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
//! **The battle transition (LT-116)** copies the frame on screen into VRAM (`MoveImage`) and swirls
//! it away. Each picture is kept with the draw area it was drawn for, so when the transition copies
//! that area, the picture is copied in its place ([`FrameCopy`], at N×), and the transition's moves
//! of columns and rows are made on it too. The 2D layer samples it wherever a 15-bit texel lies in
//! the copy: the swirl shows the remastered frame.
//!
//! **A frozen frame (LT-140).** The field menu, a chunk load and battles copy the frame on
//! screen into VRAM in two halves and show it dimmed under their panels. The native view keeps the
//! whole picture it last drew for that draw area ([`Frozen`], margins too) while VRAM still holds
//! the game's copy, and the 2D layer draws the frozen frame's quads, and their copies in a widened
//! view's margins, from it.
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

/// A lit corner: the original look's 28 bytes, then world position, normal, fog, water, the
/// distance to the shore, and where its triangle's stretches of shore are in the shores buffer
/// (LT-148: 0, none, for all but water).
const LIT_BYTES: u64 = VERTEX_BYTES + 36 + 4;
/// A lit corner's attributes (`lit.wgsl`, `In`).
const LIT_ATTRIBUTES: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint8x4, 3 => Float32, 4 => Float32x3, 5 => Float32x3, 6 => Float32x3, 7 => Uint32];
/// A shadow caster draw's transform (mesh to world), at this stride in the draws buffer.
const DRAW_STRIDE: u64 = 256;

/// The battle transition's copy of a frame (LT-116): the VRAM rect it stands for, and the native
/// picture of that frame at `scale` × (with a scratch texture for moves within it).
struct FrameCopy {
    rect: [u16; 4],
    scale: u32,
    tex: wgpu::Texture,
    scratch: wgpu::Texture,
}

/// The picture last drawn for a draw area (LT-116): one of `finals` (kept when they're made again
/// for a new size), its margin (PSX pixels) and scale, and the capture's tick it was drawn for
/// (LT-140).
#[derive(Clone)]
struct Final {
    tex: wgpu::Texture,
    left: u32,
    scale: u32,
    tick: Option<u64>,
}

/// A frame freeze's picture (LT-140): the native picture of the frozen frame, whole, the VRAM rects
/// the game copied it to, and a sum of what VRAM held there just after (it stands for the copy
/// while VRAM still does).
struct Frozen {
    tex: wgpu::Texture,
    rects: Vec<[u16; 4]>,
    sum: u64,
    /// Where its 4:3 part starts, as a share of its width.
    share: f64,
}

/// How many ticks before a freeze its picture may have been drawn (LT-140).
const FROZEN_TICKS: u64 = 6;
/// How far a frozen picture's margins may be from the view's, as a share of the width (LT-140):
/// whole pixels at 320 and 512 wide give 16:9 margins a few thousandths apart.
const SHARE_SLACK: f64 = 0.01;

/// A sum of VRAM's halfwords in `rects` (x, y, w, h), to tell when the game writes over them.
fn vram_sum(vram: &[u16], rects: &[[u16; 4]]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for &[x, y, w, ht] in rects {
        for row in y as usize..(y as usize + ht as usize).min(512) {
            let start = row * 1024 + x as usize;
            for &v in vram.get(start..(start + w as usize).min((row + 1) * 1024)).unwrap_or(&[]) {
                h = (h ^ v as u64).wrapping_mul(0x100_0000_01B3);
            }
        }
    }
    h
}

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
/// The mirrored pass's picture (LT-147): linear light, alpha where it drew.
const MIRRORED: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The mirror's uniforms (`lit.wgsl`, `struct Mirror`): the clip matrix, the plane, and the
/// shore's switch (LT-148).
const MIRROR_BYTES: u64 = 64 + 16 + 16;

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
#[derive(Clone, Default)]
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
    /// The water triangles' stretches of shore (LT-148): three `[x0, z0, x1, z1]` a triangle, the
    /// first none.
    shores: Grow,
    light: wgpu::Buffer,
    shadow_map: wgpu::Texture,
    shadow_cmp: wgpu::Sampler,
    /// The water's ripple map and its repeating, mipmapped sampler (LT-146, `crate::water`).
    ripples: wgpu::TextureView,
    ripple_sampler: wgpu::Sampler,
    /// The water's mirrored pass (LT-147): its pipeline, the mirror's uniforms, the layout the lit
    /// pass reads its picture through, that picture's sampler, and a blank one (nothing mirrored).
    pipe_mirror: wgpu::RenderPipeline,
    mirror: wgpu::Buffer,
    mirror_layout: wgpu::BindGroupLayout,
    mirror_sampler: wgpu::Sampler,
    blank_mirror: wgpu::BindGroup,
    /// The mirrored pass's targets, kept while the size stays: colour, depth.
    mirror_targets: Option<(u32, u32, [wgpu::Texture; 2])>,
    /// Whether the mirrored pass may run (on; off to measure what it adds), whether the last
    /// render ran it, and how many renders have.
    pub reflections: bool,
    pub reflected: bool,
    pub mirrored: u64,
    /// Whether the water's shallows, foam and caustics are drawn (LT-148; on, off to measure what
    /// they add).
    pub shallows: bool,
    /// The last draw's targets, kept while the size stays: colour, mask, order, depth.
    targets: Option<(u32, u32, [wgpu::Texture; 4])>,
    resolve_layout: wgpu::BindGroupLayout,
    resolve_pipe: wgpu::RenderPipeline,
    /// The resolved pictures, three in turn (a window may still be showing the last), and which is
    /// next.
    finals: Vec<wgpu::Texture>,
    next_final: usize,
    /// The picture last drawn for each draw area (x, y), kept through a change of size (LT-116,
    /// LT-140).
    final_of: HashMap<[u16; 2], Final>,
    /// The battle transition's copy of a frame, while it runs; its sampler and uniform (the rect, or
    /// zero), and a blank texture bound when there's none.
    frame_copy: Option<FrameCopy>,
    /// The table whose moves were made last (its tick and first move): a table drawn again (a frame
    /// between ticks, LT-113) doesn't make them twice.
    moves_made: Option<(Option<u64>, crate::takeover::ImageMove)>,
    copy_sampler: wgpu::Sampler,
    copy_rect: wgpu::Buffer,
    blank_copy: wgpu::Texture,
    /// Renders that drew from a frame copy (LT-116, for measuring).
    pub copied: u64,
    /// A frame freeze's picture while VRAM holds the game's copy (LT-140); the table whose copies
    /// were taken last (its tick and first copy: a table drawn again doesn't take them twice); the
    /// shader's uniform (whether there's one, the target's size).
    frozen: Option<Frozen>,
    frozen_made: Option<(Option<u64>, crate::takeover::ImageMove)>,
    frozen_info: wgpu::Buffer,
    frozen_on: bool,
    /// Renders that had a frozen frame's picture (LT-140, for measuring).
    pub frozen_renders: u64,
    /// A frozen frame drawn from the native picture of it (LT-140, the default); off, the game's
    /// own copy is drawn, as a measurement against the game's picture wants.
    pub frozen_frames: bool,
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
            entries: &[
                texture_entry(0, wgpu::TextureSampleType::Uint),
                uniform_entry(1),
                uniform_entry(2),
                texture_entry(3, UNFILTERED),
                // The battle transition's frame copy (LT-116).
                texture_entry(4, wgpu::TextureSampleType::Float { filterable: true }),
                wgpu::BindGroupLayoutEntry { binding: 5, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                uniform_entry(6),
                // A frozen frame's picture (LT-140).
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: true }),
                uniform_entry(8),
            ],
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
        attrs.push(wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 88, shader_location: 10 });
        attrs.push(wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 96, shader_location: 11 });
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
        let copy_rect = buffer("frame copy");
        let frozen_info = buffer("frozen frame");
        let copy_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("frame copy"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let blank_copy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("no frame copy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOUR,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        // Lit (LT-112): the lit pass and the shadow map's.
        let ml = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("lit 3D"), source: wgpu::ShaderSource::Wgsl(include_str!("shaders/lit.wgsl").into()) });
        let lit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("light"),
            entries: &[
                uniform_entry(0),
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison), count: None },
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: true }),
                wgpu::BindGroupLayoutEntry { binding: 4, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                uniform_entry(5),
                // LT-148: each water triangle's three nearest stretches of shore.
                wgpu::BindGroupLayoutEntry { binding: 6, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        // The mirrored pass's picture, read by the lit pass only (LT-147).
        let mirror_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mirrored"),
            entries: &[
                texture_entry(0, wgpu::TextureSampleType::Float { filterable: true }),
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout3d, &lit_layout, &mirror_layout], push_constant_ranges: &[] });
        let pipe_lit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lit 3D"),
            layout: Some(&pll),
            vertex: wgpu::VertexState {
                module: &ml,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: LIT_BYTES, step_mode: wgpu::VertexStepMode::Vertex, attributes: &LIT_ATTRIBUTES }],
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
                buffers: &[wgpu::VertexBufferLayout { array_stride: LIT_BYTES, step_mode: wgpu::VertexStepMode::Vertex, attributes: &LIT_ATTRIBUTES }],
            },
            fragment: Some(wgpu::FragmentState { module: &ml, entry_point: Some("fs_depth"), compilation_options: Default::default(), targets: &no_writes }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH, depth_write_enabled: true, depth_compare: wgpu::CompareFunction::LessEqual, stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        // The mirrored pass (LT-147): the lit corners again, through the mirror, at a fraction of
        // the picture's size.
        let plm = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout3d, &lit_layout], push_constant_ranges: &[] });
        let pipe_mirror = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mirrored"),
            layout: Some(&plm),
            vertex: wgpu::VertexState {
                module: &ml,
                entry_point: Some("vs_reflect"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: LIT_BYTES, step_mode: wgpu::VertexStepMode::Vertex, attributes: &LIT_ATTRIBUTES }],
            },
            fragment: Some(wgpu::FragmentState { module: &ml, entry_point: Some("fs_reflect"), compilation_options: Default::default(), targets: &[Some(MIRRORED.into())] }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH, depth_write_enabled: true, depth_compare: wgpu::CompareFunction::LessEqual, stencil: Default::default(), bias: Default::default() }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let mirror = device.create_buffer(&wgpu::BufferDescriptor { label: Some("mirror"), size: MIRROR_BYTES, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mirror_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("mirrored"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let blank_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nothing mirrored"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: MIRRORED,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &blank_tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &[0u8; 8],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let blank_mirror = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nothing mirrored"),
            layout: &mirror_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&blank_tex.create_view(&Default::default())) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&mirror_sampler) },
            ],
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
        // The water's ripples (LT-146): one tileable map, every mip, sampled repeating.
        let maps = crate::water::ripple_map();
        let side = crate::water::RIPPLE_SIZE as u32;
        let ripple_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ripples"),
            size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
            mip_level_count: maps.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, data) in maps.iter().enumerate() {
            let w = (side >> level).max(1);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &ripple_tex, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(w) },
                wgpu::Extent3d { width: w, height: w, depth_or_array_layers: 1 },
            );
        }
        let ripples = ripple_tex.create_view(&Default::default());
        let ripple_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ripples"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
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
            final_of: HashMap::new(),
            frame_copy: None,
            moves_made: None,
            copy_sampler,
            copy_rect,
            blank_copy,
            copied: 0,
            frozen: None,
            frozen_made: None,
            frozen_info,
            frozen_on: false,
            frozen_renders: 0,
            frozen_frames: true,
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
            shores: Grow::default(),
            light,
            shadow_map,
            shadow_cmp,
            ripples,
            ripple_sampler,
            pipe_mirror,
            mirror,
            mirror_layout,
            mirror_sampler,
            blank_mirror,
            mirror_targets: None,
            reflections: true,
            reflected: false,
            mirrored: 0,
            shallows: true,
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
            // The pictures last drawn for each draw area stay: a frame freeze (LT-140) may want
            // one drawn at the size before (the field at 512 wide, the menu at 320).
            self.finals = (0..3).map(|_| self.texture("picture", w, h, COLOUR, attach | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC)).collect();
        }
        self.targets.as_ref().map(|(_, _, t)| t.clone()).expect("targets")
    }

    /// The mirrored pass's colour and depth targets for a `w` × `h` picture (kept between draws).
    fn mirror_targets(&mut self, w: u32, h: u32) -> [wgpu::Texture; 2] {
        let (w, h) = (w.div_ceil(crate::water::MIRROR_DIVISOR), h.div_ceil(crate::water::MIRROR_DIVISOR));
        if !matches!(&self.mirror_targets, Some((tw, th, _)) if *tw == w && *th == h) {
            let attach = wgpu::TextureUsages::RENDER_ATTACHMENT;
            let t = [self.texture("mirrored", w, h, MIRRORED, attach | wgpu::TextureUsages::TEXTURE_BINDING), self.texture("mirrored depth", w, h, DEPTH, attach)];
            self.mirror_targets = Some((w, h, t));
        }
        self.mirror_targets.as_ref().map(|(_, _, t)| t.clone()).expect("mirror targets")
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

    /// The battle transition's moves for `table` (LT-116), into `enc` before the table is drawn: a
    /// copy of a draw area takes the picture last drawn for it; a move within the copy is made on
    /// the picture, `scale` ×. With no moves the copy is dropped (the transition is over). Writes
    /// the copy's rect (or zeros) for the 2D layer.
    fn frame_moves(&mut self, enc: &mut wgpu::CommandEncoder, table: &Table, scale: u32, size: [u32; 2]) {
        if table.moves.is_empty() {
            self.frame_copy = None;
            self.moves_made = None;
        }
        let this = table.moves.first().map(|&m| (table.tick, m));
        let again = this.is_some() && this == self.moves_made;
        self.moves_made = this.or(self.moves_made);
        let copy = |enc: &mut wgpu::CommandEncoder, from: &wgpu::Texture, at: [u32; 2], to: &wgpu::Texture, dst: [u32; 2], size: [u32; 2]| {
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo { texture: from, mip_level: 0, origin: wgpu::Origin3d { x: at[0], y: at[1], z: 0 }, aspect: wgpu::TextureAspect::All },
                wgpu::TexelCopyTextureInfo { texture: to, mip_level: 0, origin: wgpu::Origin3d { x: dst[0], y: dst[1], z: 0 }, aspect: wgpu::TextureAspect::All },
                wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            );
        };
        for m in table.moves.iter().filter(|_| !again) {
            let [sx, sy, mw, mh] = m.from;
            if let Some(Final { tex, left, scale: s, .. }) = self.final_of.get(&[sx, sy]).cloned() {
                // The frame on screen copied: its picture, the copied part of it.
                let picture = tex;
                let (w, h) = (mw as u32 * s, mh as u32 * s);
                // Drawn at this size: the field at another width isn't the frame copied.
                if s != scale || [picture.width(), picture.height()] != size || left * s + w > picture.width() || h > picture.height() {
                    continue;
                }
                if !self.frame_copy.as_ref().is_some_and(|c| c.tex.width() == w && c.tex.height() == h) {
                    let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
                    self.frame_copy = Some(FrameCopy { rect: [0; 4], scale, tex: self.texture("frame copy", w, h, COLOUR, usage), scratch: self.texture("frame copy scratch", w, h, COLOUR, usage) });
                }
                let c = self.frame_copy.as_mut().expect("made");
                c.rect = [m.to[0], m.to[1], mw, mh];
                copy(enc, &picture, [left * s, 0], &c.tex, [0, 0], [w, h]);
            } else if let Some(c) = &self.frame_copy {
                // A part of the copy moved within it.
                let [cx, cy, cw, ch] = c.rect.map(u32::from);
                let (fx, fy, tx, ty, mw, mh) = (sx as u32, sy as u32, m.to[0] as u32, m.to[1] as u32, mw as u32, mh as u32);
                let inside = |x: u32, y: u32| x >= cx && y >= cy && x + mw <= cx + cw && y + mh <= cy + ch;
                if mw == 0 || mh == 0 || !inside(fx, fy) || !inside(tx, ty) {
                    continue;
                }
                let s = c.scale;
                copy(enc, &c.tex, [(fx - cx) * s, (fy - cy) * s], &c.scratch, [0, 0], [mw * s, mh * s]);
                copy(enc, &c.scratch, [0, 0], &c.tex, [(tx - cx) * s, (ty - cy) * s], [mw * s, mh * s]);
            }
        }
        let rect = self.frame_copy.as_ref().map_or([0.0; 4], |c| c.rect.map(|v| v as f32));
        self.copied += u64::from(self.frame_copy.is_some());
        self.queue.write_buffer(&self.copy_rect, 0, bytemuck::cast_slice(&rect));
    }

    /// A frame freeze (LT-140): when the game copies the frame on screen (a menu or a chunk load
    /// opening), the picture last drawn for that draw area is kept, whole, margins and all. It
    /// stands for the copy until VRAM no longer holds it. Writes the shader's uniform: whether
    /// there's one, and the target's size (`size`), at which the frozen frame's quads sample it.
    fn freeze(&mut self, enc: &mut wgpu::CommandEncoder, table: &Table, scale: u32, size: [u32; 2], left: u32) {
        // Where the 4:3 picture starts in a picture, as a share of its width: the frozen frame is
        // sampled by position, so a picture of another width (the field at 512, the menu at 320)
        // stands for it if its margins are the same share.
        let share = |left: u32, scale: u32, w: u32| (left * scale) as f64 / w.max(1) as f64;
        let here = share(left, scale, size[0]);
        let this = table.frozen.first().map(|&m| (table.tick, m));
        if !self.frozen_frames {
            self.frozen = None;
            self.frozen_made = None;
        } else if this.is_some() && this != self.frozen_made {
            // The copies start at the draw area's corner: the picture last drawn for that area, if
            // it was drawn at this scale a few ticks before at most (the app may drop a tick or
            // two; a measurement drawing ticks here and there has none, and the game's own copy is
            // drawn then).
            let fresh = |at: Option<u64>| match (at, table.tick) {
                (Some(a), Some(t)) => a < t && t - a <= FROZEN_TICKS,
                _ => true,
            };
            let source = table.frozen.iter().find_map(|m| self.final_of.get(&[m.from[0], m.from[1]]).cloned()).filter(|f| {
                f.scale == scale && fresh(f.tick) && f.tex.height() == size[1] && (share(f.left, f.scale, f.tex.width()) - here).abs() < SHARE_SLACK
            });
            if let Some(f) = source {
                self.frozen_made = this;
                let (w, h) = (f.tex.width(), f.tex.height());
                let tex = match self.frozen.take() {
                    Some(old) if old.tex.width() == w && old.tex.height() == h => old.tex,
                    _ => self.texture("frozen frame", w, h, COLOUR, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST),
                };
                enc.copy_texture_to_texture(f.tex.as_image_copy(), tex.as_image_copy(), wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
                let rects: Vec<[u16; 4]> = table.frozen.iter().map(|m| [m.to[0], m.to[1], m.from[2], m.from[3]]).collect();
                self.frozen = Some(Frozen { tex, sum: vram_sum(&table.vram, &rects), rects, share: share(f.left, f.scale, w) });
            } else {
                // A new freeze with no picture of it: the old one doesn't stand for it.
                self.frozen = None;
            }
        } else if self.frozen.as_ref().is_some_and(|f| !table.vram.is_empty() && vram_sum(&table.vram, &f.rects) != f.sum) {
            // The game has drawn over its copy: the frame isn't frozen any more.
            self.frozen = None;
        }
        // Drawn from in a view of the same shape only.
        let on = self.frozen.as_ref().is_some_and(|f| f.tex.height() == size[1] && (f.share - here).abs() < SHARE_SLACK);
        self.frozen_on = on;
        self.frozen_renders += u64::from(on);
        let info = [f32::from(u8::from(on)), size[0] as f32, size[1] as f32, 0.0];
        self.queue.write_buffer(&self.frozen_info, 0, bytemuck::cast_slice(&info));
    }

    /// Whether a frozen frame's quads drew from the native picture in the last render (LT-140).
    pub fn has_frozen(&self) -> bool {
        self.frozen_on
    }

    /// A frame freeze's picture as it stands (LT-140, for measuring): RGBA, the whole widened
    /// picture of the frozen frame.
    pub fn frozen(&self) -> Option<Picture> {
        let f = self.frozen.as_ref()?;
        let (w, h) = (f.tex.width(), f.tex.height());
        let mut enc = self.device.create_command_encoder(&Default::default());
        let rgba = read(&self.device, &mut enc, &f.tex, w, h, 4);
        self.queue.submit([enc.finish()]);
        let rgba = rgba();
        Some(Picture { w, h, rgba, native: Vec::new(), layer: Vec::new() })
    }

    /// The battle transition's copy of a frame as it stands (LT-116, for measuring): the VRAM rect
    /// it stands for, and its picture (RGBA, `scale` × the rect).
    pub fn frame_copy(&self) -> Option<([u16; 4], Picture)> {
        let c = self.frame_copy.as_ref()?;
        let (w, h) = (c.tex.width(), c.tex.height());
        let mut enc = self.device.create_command_encoder(&Default::default());
        let rgba = read(&self.device, &mut enc, &c.tex, w, h, 4);
        self.queue.submit([enc.finish()]);
        let rgba = rgba();
        Some((c.rect, Picture { w, h, rgba, native: Vec::new(), layer: Vec::new() }))
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
        // The water's mirror (LT-147): when the lit look draws water, about the plane most of it
        // lies at, through this frame's camera.
        let mirror = match (lit, scene) {
            (Some((u, l)), Some(s)) if u.water && self.reflections => l.camera.zip(crate::water::plane(s)),
            _ => None,
        };
        let mut mirror_bytes = [0f32; MIRROR_BYTES as usize / 4];
        if let (Some((cam, y)), Some(s)) = (mirror, scene) {
            let clip = crate::water::mirror_clip(&cam, y, s.width as f32, s.height as f32, s.left as f32);
            for (c, col) in clip.iter().enumerate() {
                mirror_bytes[c * 4..c * 4 + 4].copy_from_slice(col);
            }
            mirror_bytes[16..20].copy_from_slice(&[y, 1.0, 1.0 / w as f32, 1.0 / h as f32]);
        }
        mirror_bytes[20] = if self.shallows { 1.0 } else { 0.0 };
        self.queue.write_buffer(&self.mirror, 0, bytemuck::cast_slice(&mirror_bytes));
        let mirror_views = mirror.map(|_| self.mirror_targets(w, h).map(|t| t.create_view(&Default::default())));

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

        let (bytes3, ranges3, shore_bytes) = match (scene, lit.is_some()) {
            (Some(s), true) => pack_lit(s, mirror.map(|m| m.1)),
            (Some(s), false) => {
                let (b, r) = pack(s);
                (b, r, Vec::new())
            }
            _ => Default::default(),
        };
        let shores = self.shores.put(&self.device, &self.queue, "shores", &shore_bytes, wgpu::BufferUsages::STORAGE);
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
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.ripples) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.ripple_sampler) },
                    wgpu::BindGroupEntry { binding: 5, resource: self.mirror.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: shores.as_entire_binding() },
                ],
            })
        });
        let mirror_bind = mirror_views.as_ref().map(|[cv, _]| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mirrored"),
                layout: &self.mirror_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(cv) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.mirror_sampler) }],
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
        // The battle transition's moves (LT-116), on the queue before the table's draw.
        let mut moves = self.device.create_command_encoder(&Default::default());
        self.frame_moves(&mut moves, table, scale, [w, h]);
        // A frame freeze (LT-140).
        self.freeze(&mut moves, table, scale, [w, h], left);
        self.queue.submit([moves.finish()]);
        let frozen_view = self.frozen.as_ref().map_or_else(|| self.blank_copy.create_view(&Default::default()), |f| f.tex.create_view(&Default::default()));
        let copy_view = self.frame_copy.as_ref().map_or_else(|| self.blank_copy.create_view(&Default::default()), |c| c.tex.create_view(&Default::default()));
        let (cv, mv, ov, dv) = (colour.create_view(&Default::default()), mask.create_view(&Default::default()), order.create_view(&Default::default()), depth.create_view(&Default::default()));
        fn attachment(view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> Option<wgpu::RenderPassColorAttachment<'_>> {
            Some(wgpu::RenderPassColorAttachment { view, resolve_target: None, ops: wgpu::Operations { load, store: wgpu::StoreOp::Store } })
        }
        let draw3d = |pass: &mut wgpu::RenderPass<'_>| {
            for &(key, first, n) in &ranges3 {
                if n > 0 {
                    pass.set_bind_group(0, self.binds3d.get(&key).unwrap_or(&self.blank3d), &[]);
                    pass.draw(first..first + n, 0..1);
                }
            }
        };
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
        // The scene mirrored in the water (LT-147), lit as the lit pass lights it (the shadow map
        // drawn just above), for the lit pass's water to read.
        self.reflected = false;
        if let (Some(b), Some([mcv, mdv])) = (&lit_bind, &mirror_views) {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mirrored"),
                color_attachments: &[attachment(mcv, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT))],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: mdv, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipe_mirror);
            pass.set_bind_group(1, b, &[]);
            pass.set_vertex_buffer(0, vb3.slice(..));
            draw3d(&mut pass);
            self.reflected = true;
            self.mirrored += 1;
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
            // The GPU's draw area for the 3D, its lines (LT-116): a widened view's columns reach the
            // margins.
            if let Some([_, y0, _, y1]) = table.clip3d {
                let top = ((y0 as i32 - oy as i32).max(0) as u32 * scale).min(h);
                let bottom = (((y1 as i32 + 1 - oy as i32).max(0) as u32) * scale).min(h);
                pass.set_scissor_rect(0, top, w, bottom.saturating_sub(top).max(1));
            }
            pass.set_vertex_buffer(0, vb3.slice(..));
            match &lit_bind {
                Some(b) => {
                    pass.set_pipeline(&self.pipe_depth);
                    draw3d(&mut pass);
                    pass.set_pipeline(&self.pipe_lit);
                    pass.set_bind_group(1, b, &[]);
                    pass.set_bind_group(2, mirror_bind.as_ref().unwrap_or(&self.blank_mirror), &[]);
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
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&copy_view) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.copy_sampler) },
                wgpu::BindGroupEntry { binding: 6, resource: self.copy_rect.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&frozen_view) },
                wgpu::BindGroupEntry { binding: 8, resource: self.frozen_info.as_entire_binding() },
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
                // The draw area, from VRAM to the target; a billboard's in a widened view reaches
                // the picture's edges (LT-144).
                let x0 = ((r.clip[0] as i32 - ox as i32 + left as i32).max(0) as u32 * scale).min(w);
                let y0 = ((r.clip[1] as i32 - oy as i32).max(0) as u32 * scale).min(h);
                let x1 = (((r.clip[2] as i32 + 1 - ox as i32 + left as i32).max(0) as u32) * scale).min(w);
                let y1 = (((r.clip[3] as i32 + 1 - oy as i32).max(0) as u32) * scale).min(h);
                let (x0, x1) = match (r.margin, r.wide) {
                    // A margin's own columns (LT-149).
                    (Some(crate::layer2d::Side::Left), _) => (0, (left * scale).min(w)),
                    (Some(crate::layer2d::Side::Right), _) => (((left + dw as u32) * scale).min(w), w),
                    (None, true) => (0, w),
                    (None, false) => (x0, x1),
                };
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
        // Kept by the draw area it's of, for the battle transition to copy (LT-116).
        self.final_of.insert([ox, oy], Final { tex: picture.clone(), left, scale, tick: table.tick });
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

/// [`pack`] with each corner's world position, normal, fog, water (1, or 2 on the mirrored
/// `plane`: LT-147), distance to the shore and where its triangle's stretches of shore are after
/// it; and those stretches (LT-148): a water triangle's three, after the first (none).
fn pack_lit(scene: &Scene, plane: Option<f32>) -> (Vec<u8>, Vec<(TextureKey, u32, u32)>, Vec<u8>) {
    let water = |v: &crate::draw::Vertex| match (v.water, plane) {
        (false, _) => 0.0f32,
        (true, Some(p)) if crate::water::on_plane(v.world[1], p) => 2.0,
        (true, _) => 1.0,
    };
    let (w, h, left) = (scene.width.max(1) + 2 * scene.left, scene.height.max(1), scene.left as f32);
    let n: usize = scene.batches.iter().map(|b| b.corners.len()).sum();
    let mut out = Vec::with_capacity(n * LIT_BYTES as usize);
    let mut ranges = Vec::with_capacity(scene.batches.len());
    let mut shores: Vec<[f32; 4]> = vec![crate::draw::NO_SHORE; 3];
    for b in &scene.batches {
        let first = (out.len() as u64 / LIT_BYTES) as u32;
        let mut slot = 0u32;
        for (k, v) in b.corners.iter().enumerate() {
            // A triangle's first corner: its stretches, when it's water.
            if k % 3 == 0 {
                slot = if v.water {
                    shores.extend_from_slice(&v.shores);
                    (shores.len() / 3 - 1) as u32
                } else {
                    0
                };
            }
            let x = (v.screen.x + left + 0.5) / w as f32 * 2.0 - 1.0;
            let y = 1.0 - (v.screen.y + 0.5) / h as f32 * 2.0;
            let z = (v.screen.z / 65536.0).clamp(0.0, 1.0);
            out.extend_from_slice(bytemuck::cast_slice(&[x, y, z, v.uv[0] as f32, v.uv[1] as f32]));
            out.extend_from_slice(&[v.rgb[0], v.rgb[1], v.rgb[2], v.flags]);
            out.extend_from_slice(bytemuck::cast_slice(&[v.bucket as f32, v.world[0], v.world[1], v.world[2], v.normal[0], v.normal[1], v.normal[2], v.fog, water(v), v.shore]));
            out.extend_from_slice(&slot.to_le_bytes());
        }
        ranges.push((b.texture, first, b.corners.len() as u32));
    }
    (out, ranges, bytemuck::cast_slice(&shores).to_vec())
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
