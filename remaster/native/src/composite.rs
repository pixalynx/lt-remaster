//! The native view (LT-111): native terrain and characters, then the 2D layer over them, into one
//! picture of the display area at N× its size.
//!
//! **Pass 1:**
//! 1. **The background.** Clear to the draw environment's background colour. When it doesn't clear
//!    (no isbg), the PS1 draws over what the buffer already holds, so the native view starts from
//!    that VRAM: a frozen field under a menu, say (15- or 24-bit, enlarged).
//! 2. **Terrain and characters**, drawn natively with a depth buffer (`raster`'s shader): the scene
//!    `crate::draw` projects. At N× use [`crate::gte::Precision::Float`]: whole-pixel GTE positions
//!    would be N×-sized steps. Each pixel also keeps its polygon's ordering-table bucket + 1 (an
//!    R32Float "order" target, 0 where there's no polygon).
//!
//! **Pass 2, the 2D layer:** the table's other packets in the GPU's order (far bucket to near).
//! - **Sorting:** a fragment shows only where the native polygon under it is in a farther bucket
//!   (`crate::layer2d`), as the GPU's painting gives.
//! - **Blending:** the PS1's blend, into what's there. A semi-transparent effect lands on the native
//!   3D, not on a flat image.
//! - **Shader:** PSoXide's hardware renderer's (`shaders/layer2d.wgsl`).
//!
//! **Lit (LT-112):** with [`crate::light::Look::Lit`] and a scene built by [`crate::draw::lit`],
//! pass 1 first draws the casters into a 2048² shadow map from the main light (`shaders/shadow.wgsl`;
//! the chunks' kept on the GPU per mesh), then the terrain and characters through `shaders/lit.wgsl`:
//! their depth first (cut-out texels tested there), then the lit colour where the depth is equal, so
//! the lit shader runs once a pixel. Same triangles, mask and order as the original look: only the
//! colour changes. A last pass resolves the colour codes; [`Compositor::render`] leaves the picture
//! on the GPU (the app's window shows it from there), [`Compositor::draw_lit`] reads it back.
//!
//! A mask target records coverage: red where the native 3D drew, green where the 2D layer did.
//!
//! **Colour codes.** The 2D pipelines hand the blender `8 × code` (`blend_encode`), and only a
//! value's top five bits are its colour, as in PSoXide, whose VRAM holds the code. The low bits a
//! blend leaves aren't colour: a full-screen subtract fade of 31 over 255 leaves 7, and sparkles
//! glowed through the black of a fade-in. So:
//! - A pixel only the 2D layer drew is read back as its code, replicated (as VRAM is decoded). The
//!   clear is a code too.
//! - Where the 2D layer blended over the native 3D, the 3D's own low bits are colour (the remaster
//!   keeps them), and only a channel below 8 (code 0) is taken as black, so fades end at black.
//! - A pixel only the native 3D drew keeps all eight bits.

use std::collections::HashMap;

use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::{MeshKey, TextureKey};

use crate::draw::Scene;
use crate::layer2d::{Layer, Vertex};
use crate::light::{caster_bytes, Uniforms, CASTER_BYTES, SHADOW_SIZE};
use crate::raster::{pack, shader_with_order, VERTEX_BYTES};
use crate::takeover::{Shown, Table};

/// A lit corner: the original look's 28 bytes, then world position and normal.
const LIT_BYTES: u64 = VERTEX_BYTES + 24;
/// A shadow caster draw's transform (mesh to world), at this stride in the draws buffer.
const DRAW_STRIDE: u64 = 256;

/// A buffer kept between draws, grown when a draw needs more.
#[derive(Default)]
struct Grow(Option<wgpu::Buffer>);

impl Grow {
    fn put(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, label: &str, bytes: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
        let need = (bytes.len() as u64).max(256).next_power_of_two();
        if self.0.as_ref().is_none_or(|b| b.size() < need) {
            self.0 = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: need, usage: usage | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
        }
        let b = self.0.clone().expect("buffer");
        if !bytes.is_empty() {
            queue.write_buffer(&b, 0, bytes);
        }
        b
    }
}

/// An 8-bit channel as the GPU keeps it: its top five bits (the 5-bit code), shown with the code
/// replicated into the low bits, as VRAM is decoded.
pub fn code(c: u8) -> u8 {
    let c5 = c >> 3;
    (c5 << 3) | (c5 >> 2)
}

/// PSoXide's average blend: the destination factor (`pipeline.rs`, `AVERAGE_DST_FACTOR`).
const AVERAGE_DST: f64 = 0.5 - 1.0 / 1024.0;
const COLOUR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MASK: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Unorm;
const ORDER: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The base layer: a picture of the display area, enlarged nearest to the target (black in a wider
/// view's margins).
const BASE: &str = r#"
@group(0) @binding(0) var picture: texture_2d<f32>;
// x the 2D layer's texture filter (unused here), y the scale, z the margin left of the display
// area (target pixels; LT-114).
@group(0) @binding(1) var<uniform> u_scale: vec4<u32>;

struct Out {
    @builtin(position) pos: vec4<f32>,
};

@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    var o: Out;
    let x = f32(i32((i & 1u) * 4u) - 1);
    let y = f32(i32((i >> 1u) * 4u) - 1);
    o.pos = vec4<f32>(x, y, 1.0, 1.0);
    return o;
}

struct Frag {
    @location(0) colour: vec4<f32>,
    @location(1) mask: vec4<f32>,
    @location(2) order: vec4<f32>,
};

@fragment fn fs(i: Out) -> Frag {
    let s = max(u_scale.y, 1u);
    let left = f32(u_scale.z);
    let p = vec2<u32>(floor(vec2<f32>(max(i.pos.x - left, 0.0), i.pos.y))) / vec2<u32>(s, s);
    let size = textureDimensions(picture);
    var f: Frag;
    f.colour = vec4<f32>(textureLoad(picture, min(p, size - 1u), 0).rgb, 1.0);
    if (i.pos.x < left || p.x >= size.x) {
        f.colour = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    f.mask = vec4<f32>(0.0);
    f.order = vec4<f32>(0.0);
    return f;
}
"#;

/// The last pass: each pixel as the GPU keeps it (module docs, "Colour codes"): a pixel only the
/// 2D layer drew becomes its 5-bit code, replicated; where the 2D layer blended over the native 3D,
/// a channel below 8 is black; the rest stays. Alpha is 1.
const RESOLVE: &str = r#"
@group(0) @binding(0) var colour: texture_2d<f32>;
@group(0) @binding(1) var mask: texture_2d<f32>;

@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32((i & 1u) * 4u) - 1);
    let y = f32(i32((i >> 1u) * 4u) - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let c8 = floor(textureLoad(colour, p, 0).rgb * 255.0 + 0.5);
    let m = textureLoad(mask, p, 0).rg;
    var out = c8;
    if (m.x < 0.5 && m.y > 0.5) {
        let c5 = floor(c8 / 8.0);
        out = c5 * 8.0 + floor(c5 / 4.0);
    } else if (m.x > 0.5 && m.y > 0.5) {
        out = select(c8, vec3<f32>(0.0), c8 < vec3<f32>(8.0));
    }
    return vec4<f32>(out / 255.0, 1.0);
}
"#;

/// A tick composited on the GPU ([`Compositor::render`]): the picture (`colour`, resolved; three
/// are used in turn) and the coverage mask (one, reused: read it before the next render), `w` × `h`.
#[derive(Clone, Debug)]
pub struct Rendered {
    pub colour: wgpu::Texture,
    pub mask: wgpu::Texture,
    pub w: u32,
    pub h: u32,
}

/// A composited picture: RGBA, and per pixel whether the native 3D and the 2D layer drew there.
pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    pub native: Vec<bool>,
    pub layer: Vec<bool>,
}

pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout3d: wgpu::BindGroupLayout,
    pipe3d: wgpu::RenderPipeline,
    binds3d: HashMap<TextureKey, wgpu::BindGroup>,
    blank3d: wgpu::BindGroup,
    base_layout: wgpu::BindGroupLayout,
    base_pipe: wgpu::RenderPipeline,
    layout2d: wgpu::BindGroupLayout,
    pipes2d: Vec<wgpu::RenderPipeline>,
    vram: wgpu::Texture,
    filter: wgpu::Buffer,
    view: wgpu::Buffer,
    lit_layout: wgpu::BindGroupLayout,
    pipe_lit: wgpu::RenderPipeline,
    /// The lit pass's depth first (cut-outs tested there), so the lit shader runs once a pixel.
    pipe_depth: wgpu::RenderPipeline,
    caster_layout: wgpu::BindGroupLayout,
    draw_layout: wgpu::BindGroupLayout,
    pipe_shadow: wgpu::RenderPipeline,
    /// Each chunk mesh's casters on the GPU: vertex buffer and corners.
    caster_meshes: HashMap<MeshKey, (wgpu::Buffer, u32)>,
    /// Kept between draws: the 3D corners, the 2D layer's, the characters' casters, the casters'
    /// transforms.
    vb3: Grow,
    vb2: Grow,
    vbc: Grow,
    draws: Grow,
    light: wgpu::Buffer,
    shadow_map: wgpu::Texture,
    shadow_cmp: wgpu::Sampler,
    /// The last draw's targets, kept while the size stays: colour, mask, order, depth.
    targets: Option<(u32, u32, [wgpu::Texture; 4])>,
    resolve_layout: wgpu::BindGroupLayout,
    resolve_pipe: wgpu::RenderPipeline,
    /// The resolved pictures, three in turn (a window may still be showing the last), and which is
    /// next.
    finals: Vec<wgpu::Texture>,
    next_final: usize,
    /// Chunk normals, kept across ticks (`crate::draw::lit`).
    pub normals: crate::light::Normals,
    /// The parts of the lighting that are on (LT-117's Lights toggles).
    pub lights: crate::light::Lights,
}

/// The PS1's blend for `kind`, on colour; alpha stays the destination's (opaque), or a subtract
/// would leave the picture transparent.
fn blend(kind: usize) -> wgpu::BlendState {
    let c = |dst: wgpu::BlendFactor, op: wgpu::BlendOperation| wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: dst, operation: op };
    let keep = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add };
    let s = |c: wgpu::BlendComponent| wgpu::BlendState { color: c, alpha: keep };
    match kind {
        1 => s(c(wgpu::BlendFactor::Constant, wgpu::BlendOperation::Add)),
        2 | 4 => s(c(wgpu::BlendFactor::One, wgpu::BlendOperation::Add)),
        3 => s(c(wgpu::BlendFactor::One, wgpu::BlendOperation::ReverseSubtract)),
        _ => wgpu::BlendState::REPLACE,
    }
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }
}

const UNFILTERED: wgpu::TextureSampleType = wgpu::TextureSampleType::Float { filterable: false };

impl Compositor {
    pub fn new(gpu: &Wgpu) -> Compositor {
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let pass1_targets = |mask: wgpu::ColorWrites, order: wgpu::ColorWrites| {
            [Some(COLOUR.into()), Some(wgpu::ColorTargetState { format: MASK, blend: None, write_mask: mask }), Some(wgpu::ColorTargetState { format: ORDER, blend: None, write_mask: order })]
        };

        // Pass 1: terrain and characters.
        let m3 = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("native 3D"), source: wgpu::ShaderSource::Wgsl(shader_with_order().into()) });
        let layout3d = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("atlas"), entries: &[texture_entry(0, UNFILTERED)] });
        let pl3 = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout3d], push_constant_ranges: &[] });
        let targets3 = pass1_targets(wgpu::ColorWrites::RED, wgpu::ColorWrites::RED);
        let pipe3d = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("native 3D"),
            layout: Some(&pl3),
            vertex: wgpu::VertexState {
                module: &m3,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: VERTEX_BYTES, step_mode: wgpu::VertexStepMode::Vertex, attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint8x4, 3 => Float32] }],
            },
            fragment: Some(wgpu::FragmentState { module: &m3, entry_point: Some("fs"), compilation_options: Default::default(), targets: &targets3 }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH, depth_write_enabled: true, depth_compare: wgpu::CompareFunction::LessEqual, stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let blank_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blank"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOUR,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let blank3d = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout3d,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&blank_tex.create_view(&Default::default())) }],
        });

        // Pass 1: the background picture.
        let mb = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("base"), source: wgpu::ShaderSource::Wgsl(BASE.into()) });
        let base_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("base"), entries: &[texture_entry(0, UNFILTERED), uniform_entry(1)] });
        let plb = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&base_layout], push_constant_ranges: &[] });
        let targets_base = pass1_targets(wgpu::ColorWrites::empty(), wgpu::ColorWrites::empty());
        let base_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("base"),
            layout: Some(&plb),
            vertex: wgpu::VertexState { module: &mb, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState { module: &mb, entry_point: Some("fs"), compilation_options: Default::default(), targets: &targets_base }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH, depth_write_enabled: false, depth_compare: wgpu::CompareFunction::Always, stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });

        // Pass 2: the 2D layer.
        let m2 = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("2D layer"), source: wgpu::ShaderSource::Wgsl(include_str!("shaders/layer2d.wgsl").into()) });
        let layout2d = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("2D layer"),
            entries: &[texture_entry(0, wgpu::TextureSampleType::Uint), uniform_entry(1), uniform_entry(2), texture_entry(3, UNFILTERED)],
        });
        let pl2 = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout2d], push_constant_ranges: &[] });
        let mut attrs = vec![
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Sint16x2, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Unorm8x4, offset: 4, shader_location: 1 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint16x2, offset: 8, shader_location: 2 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 12, shader_location: 3 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 16, shader_location: 4 },
        ];
        for k in 0..4 {
            attrs.push(wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32x4, offset: 20 + 16 * k, shader_location: 5 + k as u32 });
        }
        attrs.push(wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32, offset: 84, shader_location: 9 });
        let pipes2d = (0..5)
            .map(|kind| {
                let constants = HashMap::from([("BLEND_KIND".to_string(), kind as f64)]);
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("2D layer"),
                    layout: Some(&pl2),
                    vertex: wgpu::VertexState {
                        module: &m2,
                        entry_point: Some("vs_main"),
                        compilation_options: Default::default(),
                        buffers: &[wgpu::VertexBufferLayout { array_stride: Vertex::BYTES as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs }],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &m2,
                        entry_point: Some("fs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions { constants: &constants, ..Default::default() },
                        targets: &[Some(wgpu::ColorTargetState { format: COLOUR, blend: Some(blend(kind)), write_mask: wgpu::ColorWrites::ALL }), Some(wgpu::ColorTargetState { format: MASK, blend: None, write_mask: wgpu::ColorWrites::GREEN })],
                    }),
                    primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
                    depth_stencil: None,
                    multisample: Default::default(),
                    multiview: None,
                    cache: None,
                })
            })
            .collect();
        let vram = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("VRAM"),
            size: wgpu::Extent3d { width: 1024, height: 512, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let buffer = |label| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let (filter, view) = (buffer("texture filter"), buffer("view"));

        // Lit (LT-112): the lit pass and the shadow map's.
        let ml = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("lit 3D"), source: wgpu::ShaderSource::Wgsl(include_str!("shaders/lit.wgsl").into()) });
        let lit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light"),
            entries: &[
                uniform_entry(0),
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison), count: None },
            ],
        });
        let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout3d, &lit_layout], push_constant_ranges: &[] });
        let pipe_lit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lit 3D"),
            layout: Some(&pll),
            vertex: wgpu::VertexState {
                module: &ml,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: LIT_BYTES, step_mode: wgpu::VertexStepMode::Vertex, attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint8x4, 3 => Float32, 4 => Float32x3, 5 => Float32x3] }],
            },
            fragment: Some(wgpu::FragmentState { module: &ml, entry_point: Some("fs"), compilation_options: Default::default(), targets: &targets3 }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH, depth_write_enabled: false, depth_compare: wgpu::CompareFunction::Equal, stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let pld = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout3d], push_constant_ranges: &[] });
        let no_writes = [COLOUR, MASK, ORDER].map(|format| Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::empty() }));
        let pipe_depth = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lit 3D depth"),
            layout: Some(&pld),
            vertex: wgpu::VertexState {
                module: &ml,
                entry_point: Some("vs_depth"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: LIT_BYTES, step_mode: wgpu::VertexStepMode::Vertex, attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint8x4, 3 => Float32, 4 => Float32x3, 5 => Float32x3] }],
            },
            fragment: Some(wgpu::FragmentState { module: &ml, entry_point: Some("fs_depth"), compilation_options: Default::default(), targets: &no_writes }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH, depth_write_enabled: true, depth_compare: wgpu::CompareFunction::LessEqual, stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let ms = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("shadow map"), source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shadow.wgsl").into()) });
        let caster_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("casters"), entries: &[uniform_entry(0)] });
        let draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("caster draws"),
            entries: &[wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::VERTEX, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: wgpu::BufferSize::new(64) }, count: None }],
        });
        let pls = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout3d, &caster_layout, &draw_layout], push_constant_ranges: &[] });
        let pipe_shadow = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow map"),
            layout: Some(&pls),
            vertex: wgpu::VertexState {
                module: &ms,
                entry_point: Some("vs_shadow"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: CASTER_BYTES, step_mode: wgpu::VertexStepMode::Vertex, attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint32] }],
            },
            fragment: Some(wgpu::FragmentState { module: &ms, entry_point: Some("fs_shadow"), compilation_options: Default::default(), targets: &[] }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: wgpu::DepthBiasState { constant: 2, slope_scale: 1.5, clamp: 0.0 },
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let light = device.create_buffer(&wgpu::BufferDescriptor { label: Some("light"), size: Uniforms::BYTES as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let shadow_map = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow map"),
            size: wgpu::Extent3d { width: SHADOW_SIZE, height: SHADOW_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_cmp = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("shadow"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, compare: Some(wgpu::CompareFunction::LessEqual), ..Default::default() });
        let mr = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("resolve"), source: wgpu::ShaderSource::Wgsl(RESOLVE.into()) });
        let resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("resolve"), entries: &[texture_entry(0, UNFILTERED), texture_entry(1, UNFILTERED)] });
        let plr = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&resolve_layout], push_constant_ranges: &[] });
        let resolve_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("resolve"),
            layout: Some(&plr),
            vertex: wgpu::VertexState { module: &mr, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState { module: &mr, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[Some(COLOUR.into())] }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        Compositor {
            device,
            queue,
            layout3d,
            pipe3d,
            binds3d: HashMap::new(),
            blank3d,
            base_layout,
            base_pipe,
            layout2d,
            pipes2d,
            vram,
            filter,
            view,
            lit_layout,
            pipe_lit,
            pipe_depth,
            caster_layout,
            draw_layout,
            pipe_shadow,
            caster_meshes: HashMap::new(),
            vb3: Grow::default(),
            vb2: Grow::default(),
            vbc: Grow::default(),
            draws: Grow::default(),
            light,
            shadow_map,
            shadow_cmp,
            targets: None,
            resolve_layout,
            resolve_pipe,
            finals: Vec::new(),
            next_final: 0,
            normals: Default::default(),
            lights: Default::default(),
        }
    }

    /// The colour, mask, order and depth targets for a `w` × `h` view (kept between draws).
    fn targets(&mut self, w: u32, h: u32) -> [wgpu::Texture; 4] {
        if !matches!(&self.targets, Some((tw, th, _)) if *tw == w && *th == h) {
            let attach = wgpu::TextureUsages::RENDER_ATTACHMENT;
            let t = [
                self.texture("colour", w, h, COLOUR, attach | wgpu::TextureUsages::TEXTURE_BINDING),
                self.texture("mask", w, h, MASK, attach | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC),
                self.texture("order", w, h, ORDER, attach | wgpu::TextureUsages::TEXTURE_BINDING),
                self.texture("depth", w, h, DEPTH, attach),
            ];
            self.targets = Some((w, h, t));
            self.finals = (0..3).map(|_| self.texture("picture", w, h, COLOUR, attach | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC)).collect();
        }
        self.targets.as_ref().map(|(_, _, t)| t.clone()).expect("targets")
    }

    fn bind3d(&mut self, cache: &Cache<Wgpu>, key: TextureKey) {
        if !self.binds3d.contains_key(&key) {
            if let Some(t) = cache.texture(key) {
                let g = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.layout3d,
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&t.create_view(&Default::default())) }],
                });
                self.binds3d.insert(key, g);
            }
        }
    }

    /// The displayed picture enlarged `scale` times (nearest): the native view while the game shows
    /// something its tables don't draw (a movie).
    pub fn present(&mut self, shown: &Shown, scale: u32) -> Picture {
        let (w, h) = (shown.w * scale, shown.h * scale);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let s = (((y / scale) * shown.w + x / scale) * 4) as usize;
                let d = ((y * w + x) * 4) as usize;
                rgba[d..d + 4].copy_from_slice(&shown.rgba[s..s + 4]);
            }
        }
        Picture { w, h, rgba, native: vec![false; (w * h) as usize], layer: vec![false; (w * h) as usize] }
    }

    fn texture(&self, label: &str, w: u32, h: u32, format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    /// The native view of a tick: `scene` (its terrain and characters, `None` for a tick with no
    /// field), then `layer` (the table's other packets), at `scale` × the display area. With
    /// `light` (and a scene built by [`crate::draw::lit`]) the 3D is lit; without, it's the
    /// original look.
    pub fn draw(&mut self, scene: Option<&Scene>, table: &Table, layer: &Layer, cache: &Cache<Wgpu>, scale: u32) -> Picture {
        self.draw_lit(scene, table, layer, cache, scale, None)
    }

    pub fn draw_lit(&mut self, scene: Option<&Scene>, table: &Table, layer: &Layer, cache: &Cache<Wgpu>, scale: u32, light: Option<&Uniforms>) -> Picture {
        let left = scene.map_or(0, |s| s.left);
        let r = self.render(scene, table, layer, cache, scale, light, left);
        self.read(&r)
    }

    /// Waits for the GPU to finish what's been submitted.
    pub fn wait(&self) {
        let _ = self.device.poll(wgpu::Maintain::Wait);
    }

    /// A rendered tick read back: its picture and coverage.
    pub fn read(&self, r: &Rendered) -> Picture {
        let mut enc = self.device.create_command_encoder(&Default::default());
        let rgba = read(&self.device, &mut enc, &r.colour, r.w, r.h, 4);
        let m = read(&self.device, &mut enc, &r.mask, r.w, r.h, 2);
        self.queue.submit([enc.finish()]);
        let (rgba, m) = (rgba(), m());
        Picture { w: r.w, h: r.h, rgba, native: m.chunks(2).map(|c| c[0] > 127).collect(), layer: m.chunks(2).map(|c| c[1] > 127).collect() }
    }

    /// [`Compositor::draw_lit`] without the read-back: the work is submitted and the picture stays
    /// on the GPU, for a window on the same device to show.
    ///
    /// `left` widens the picture by that many columns each side of the display area (LT-114): the
    /// scene's 3D reaches into them (its own `left` should match), the 2D layer stays in the middle,
    /// at the game's 4:3, clipped to its draw areas as the game has them.
    #[allow(clippy::too_many_arguments)]
    pub fn render(&mut self, scene: Option<&Scene>, table: &Table, layer: &Layer, cache: &Cache<Wgpu>, scale: u32, light: Option<&Uniforms>, left: u32) -> Rendered {
        use wgpu::util::DeviceExt;
        let [ox, oy, dw, dh] = table.display;
        let (w, h) = ((dw.max(1) as u32 + 2 * left) * scale, dh.max(1) as u32 * scale);
        let [colour, mask, order, depth] = self.targets(w, h);
        let adjusted = light.map(|u| self.lights.apply(u.clone()));
        let lit = adjusted.as_ref().zip(scene.and_then(|s| s.lit.as_ref()));

        // VRAM as the table starts, and the view's uniforms.
        if table.vram.len() == 1024 * 512 {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &self.vram, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                bytemuck::cast_slice(&table.vram),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(2048), rows_per_image: Some(512) },
                wgpu::Extent3d { width: 1024, height: 512, depth_or_array_layers: 1 },
            );
        }
        let u32s: Vec<u8> = [0u32, scale, left * scale, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
        self.queue.write_buffer(&self.filter, 0, &u32s);
        // The 2D layer's view: the display area, and the margins either side of it.
        let f32s: Vec<u8> = [ox as f32 - left as f32, oy as f32, (dw as u32 + 2 * left) as f32, dh as f32].iter().flat_map(|v| v.to_le_bytes()).collect();
        self.queue.write_buffer(&self.view, 0, &f32s);

        let (bytes3, ranges3) = match (scene, lit.is_some()) {
            (Some(s), true) => pack_lit(s),
            (Some(s), false) => pack(s),
            _ => Default::default(),
        };
        let vb3 = self.vb3.put(&self.device, &self.queue, "native 3D", &bytes3, wgpu::BufferUsages::VERTEX);
        for &(key, _, _) in &ranges3 {
            self.bind3d(cache, key);
        }
        // Lit: the uniforms, the casters, and the bind groups.
        let mut shadow_draw = None;
        let lit_bind = lit.map(|(u, l)| {
            self.queue.write_buffer(&self.light, 0, &u.bytes());
            if u.shadow_on {
                // The chunks' casters stay on the GPU per mesh, each drawn with its chunk's map to
                // world space; the characters' are this tick's, in world space already.
                let mut transforms = vec![0u8; (DRAW_STRIDE as usize) * (l.chunk_casters.len() + 1)];
                let identity = [[1.0f32, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
                let cols = l.chunk_casters.iter().map(|c| c.to_world.columns()).chain(std::iter::once(identity));
                for (i, m) in cols.enumerate() {
                    let at = i * DRAW_STRIDE as usize;
                    transforms[at..at + 64].copy_from_slice(bytemuck::cast_slice(&m));
                }
                for c in &l.chunk_casters {
                    self.bind3d(cache, c.texture);
                    if !self.caster_meshes.contains_key(&c.mesh) {
                        let b = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("chunk casters"), contents: if c.bytes.is_empty() { &[0u8; CASTER_BYTES as usize] } else { &c.bytes }, usage: wgpu::BufferUsages::VERTEX });
                        self.caster_meshes.insert(c.mesh, (b, (c.bytes.len() as u64 / CASTER_BYTES) as u32));
                    }
                }
                let mut bytes = Vec::new();
                let mut ranges = Vec::new();
                for (key, corners) in &l.casters {
                    self.bind3d(cache, *key);
                    let first = (bytes.len() as u64 / CASTER_BYTES) as u32;
                    bytes.extend(caster_bytes(corners.iter().map(|c| (c.world, c.uv, c.flags))));
                    ranges.push((*key, first, corners.len() as u32));
                }
                let vb = self.vbc.put(&self.device, &self.queue, "character casters", &bytes, wgpu::BufferUsages::VERTEX);
                let db = self.draws.put(&self.device, &self.queue, "caster draws", &transforms, wgpu::BufferUsages::UNIFORM);
                let cb = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("casters"), layout: &self.caster_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: self.light.as_entire_binding() }] });
                let draw_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("caster draws"),
                    layout: &self.draw_layout,
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &db, offset: 0, size: wgpu::BufferSize::new(64) }) }],
                });
                let chunks: Vec<(MeshKey, TextureKey)> = l.chunk_casters.iter().map(|c| (c.mesh, c.texture)).collect();
                shadow_draw = Some((chunks, vb, ranges, cb, draw_bind));
            }
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("light"),
                layout: &self.lit_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.light.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.shadow_map.create_view(&Default::default())) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.shadow_cmp) },
                ],
            })
        });
        let mut bytes2 = Vec::with_capacity(layer.vertices.len() * Vertex::BYTES);
        for v in &layer.vertices {
            v.write(&mut bytes2);
        }
        let vb2 = self.vb2.put(&self.device, &self.queue, "2D layer", &bytes2, wgpu::BufferUsages::VERTEX);

        // No clear: the buffer's own VRAM under everything.
        let base = table.clear.is_none().then(|| {
            let rgba = crate::takeover::decode(&table.vram, [ox, oy, dw, dh], table.bpp24);
            let t = self.device.create_texture_with_data(
                &self.queue,
                &wgpu::TextureDescriptor {
                    label: Some("base"),
                    size: wgpu::Extent3d { width: dw.max(1) as u32, height: dh.max(1) as u32, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: COLOUR,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &rgba,
            );
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("base"),
                layout: &self.base_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&t.create_view(&Default::default())) }, wgpu::BindGroupEntry { binding: 1, resource: self.filter.as_entire_binding() }],
            })
        });
        let clear = table.clear.map(|c| c.map(code)).map_or(wgpu::Color::BLACK, |c| wgpu::Color { r: c[0] as f64 / 255.0, g: c[1] as f64 / 255.0, b: c[2] as f64 / 255.0, a: 1.0 });
        let (cv, mv, ov, dv) = (colour.create_view(&Default::default()), mask.create_view(&Default::default()), order.create_view(&Default::default()), depth.create_view(&Default::default()));
        fn attachment(view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> Option<wgpu::RenderPassColorAttachment<'_>> {
            Some(wgpu::RenderPassColorAttachment { view, resolve_target: None, ops: wgpu::Operations { load, store: wgpu::StoreOp::Store } })
        }
        let mut enc = self.device.create_command_encoder(&Default::default());
        if let Some((chunks, vb, ranges, cb, draw_bind)) = &shadow_draw {
            let sv = self.shadow_map.create_view(&Default::default());
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow map"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &sv, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipe_shadow);
            pass.set_bind_group(1, cb, &[]);
            for (i, (mesh, key)) in chunks.iter().enumerate() {
                let Some((b, n)) = self.caster_meshes.get(mesh) else { continue };
                if *n > 0 {
                    pass.set_bind_group(0, self.binds3d.get(key).unwrap_or(&self.blank3d), &[]);
                    pass.set_bind_group(2, draw_bind, &[(i as u64 * DRAW_STRIDE) as u32]);
                    pass.set_vertex_buffer(0, b.slice(..));
                    pass.draw(0..*n, 0..1);
                }
            }
            pass.set_bind_group(2, draw_bind, &[(chunks.len() as u64 * DRAW_STRIDE) as u32]);
            pass.set_vertex_buffer(0, vb.slice(..));
            for &(key, first, n) in ranges {
                if n > 0 {
                    pass.set_bind_group(0, self.binds3d.get(&key).unwrap_or(&self.blank3d), &[]);
                    pass.draw(first..first + n, 0..1);
                }
            }
        }
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("native 3D"),
                color_attachments: &[
                    attachment(&cv, wgpu::LoadOp::Clear(clear)),
                    attachment(&mv, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)),
                    attachment(&ov, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &dv, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(b) = &base {
                pass.set_pipeline(&self.base_pipe);
                pass.set_bind_group(0, b, &[]);
                pass.draw(0..3, 0..1);
            }
            pass.set_vertex_buffer(0, vb3.slice(..));
            let draw3d = |pass: &mut wgpu::RenderPass<'_>| {
                for &(key, first, n) in &ranges3 {
                    if n > 0 {
                        pass.set_bind_group(0, self.binds3d.get(&key).unwrap_or(&self.blank3d), &[]);
                        pass.draw(first..first + n, 0..1);
                    }
                }
            };
            match &lit_bind {
                Some(b) => {
                    pass.set_pipeline(&self.pipe_depth);
                    draw3d(&mut pass);
                    pass.set_pipeline(&self.pipe_lit);
                    pass.set_bind_group(1, b, &[]);
                    draw3d(&mut pass);
                }
                None => {
                    pass.set_pipeline(&self.pipe3d);
                    draw3d(&mut pass);
                }
            }
        }
        let bind2d = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("2D layer"),
            layout: &self.layout2d,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&self.vram.create_view(&Default::default())) },
                wgpu::BindGroupEntry { binding: 1, resource: self.filter.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.view.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&ov) },
            ],
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("2D layer"),
                color_attachments: &[attachment(&cv, wgpu::LoadOp::Load), attachment(&mv, wgpu::LoadOp::Load)],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_vertex_buffer(0, vb2.slice(..));
            pass.set_bind_group(0, &bind2d, &[]);
            for r in &layer.runs {
                if r.count == 0 {
                    continue;
                }
                // The draw area, from VRAM to the target.
                let x0 = ((r.clip[0] as i32 - ox as i32 + left as i32).max(0) as u32 * scale).min(w);
                let y0 = ((r.clip[1] as i32 - oy as i32).max(0) as u32 * scale).min(h);
                let x1 = (((r.clip[2] as i32 + 1 - ox as i32 + left as i32).max(0) as u32) * scale).min(w);
                let y1 = (((r.clip[3] as i32 + 1 - oy as i32).max(0) as u32) * scale).min(h);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
                pass.set_pipeline(&self.pipes2d[r.blend.min(4)]);
                if r.blend == 1 {
                    pass.set_blend_constant(wgpu::Color { r: AVERAGE_DST, g: AVERAGE_DST, b: AVERAGE_DST, a: AVERAGE_DST });
                }
                pass.draw(r.first..r.first + r.count, 0..1);
            }
        }
        let picture = self.finals[self.next_final].clone();
        self.next_final = (self.next_final + 1) % self.finals.len();
        {
            let resolve = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("resolve"),
                layout: &self.resolve_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&cv) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&mv) },
                ],
            });
            let pv = picture.create_view(&Default::default());
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("resolve"),
                color_attachments: &[attachment(&pv, wgpu::LoadOp::Clear(wgpu::Color::BLACK))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.resolve_pipe);
            pass.set_bind_group(0, &resolve, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        Rendered { colour: picture, mask, w, h }
    }
}

/// [`pack`] with each corner's world position and normal after it.
fn pack_lit(scene: &Scene) -> (Vec<u8>, Vec<(TextureKey, u32, u32)>) {
    let (w, h, left) = (scene.width.max(1) + 2 * scene.left, scene.height.max(1), scene.left as f32);
    let n: usize = scene.batches.iter().map(|b| b.corners.len()).sum();
    let mut out = Vec::with_capacity(n * LIT_BYTES as usize);
    let mut ranges = Vec::with_capacity(scene.batches.len());
    for b in &scene.batches {
        let first = (out.len() as u64 / LIT_BYTES) as u32;
        for v in &b.corners {
            let x = (v.screen.x + left + 0.5) / w as f32 * 2.0 - 1.0;
            let y = 1.0 - (v.screen.y + 0.5) / h as f32 * 2.0;
            let z = (v.screen.z / 65536.0).clamp(0.0, 1.0);
            out.extend_from_slice(bytemuck::cast_slice(&[x, y, z, v.uv[0] as f32, v.uv[1] as f32]));
            out.extend_from_slice(&[v.rgb[0], v.rgb[1], v.rgb[2], v.flags]);
            out.extend_from_slice(bytemuck::cast_slice(&[v.bucket as f32, v.world[0], v.world[1], v.world[2], v.normal[0], v.normal[1], v.normal[2]]));
        }
        ranges.push((b.texture, first, b.corners.len() as u32));
    }
    (out, ranges)
}

/// Queues a copy of `t` into a buffer; the closure waits and unpacks it.
fn read<'a>(device: &'a wgpu::Device, enc: &mut wgpu::CommandEncoder, t: &wgpu::Texture, w: u32, h: u32, bpp: u32) -> impl FnOnce() -> Vec<u8> + 'a {
    let row = (w * bpp).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    move || {
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * bpp) as usize);
        for y in 0..h as usize {
            out.extend_from_slice(&data[y * row as usize..y * row as usize + (w * bpp) as usize]);
        }
        out
    }
}
