//! Draws a [`Scene`] with wgpu into a colour image and a coverage mask, at the PS1's resolution.
//!
//! - **Pixels:** a GTE vertex at integer (x, y) is placed at (x + 0.5, y + 0.5), so the GPU's
//!   pixel-centre test samples the points the PS1 does.
//! - **Texels:** texture coordinates interpolate affinely in texels (no perspective), as on the
//!   PS1. A texel whose atlas alpha is 0 (the PS1's 0x0000) isn't drawn, so cut-outs leave holes in
//!   the mask too.
//! - **Depth:** a depth buffer sorts by SZ. Coverage doesn't depend on it; the colour image does.
//!
//! Textures are the GPU cache's atlases (`lt_assets::gpu::Cache<Wgpu>`).

use std::collections::HashMap;

use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::TextureKey;

use crate::draw::Scene;

pub(crate) const SHADER: &str = r#"
struct In {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) rgbf: vec4<u32>,
};
struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(linear) uv: vec2<f32>,
    @location(1) @interpolate(linear) rgb: vec3<f32>,
    @location(2) @interpolate(flat) flags: u32,
};
@group(0) @binding(0) var atlas: texture_2d<f32>;

@vertex fn vs(v: In) -> Out {
    var o: Out;
    o.pos = vec4<f32>(v.pos, 1.0);
    o.uv = v.uv;
    o.rgb = vec3<f32>(v.rgbf.xyz);
    o.flags = v.rgbf.w;
    return o;
}

struct Frag {
    @location(0) colour: vec4<f32>,
    @location(1) mask: vec4<f32>,
};

@fragment fn fs(i: Out) -> Frag {
    var c: vec3<f32>;
    if ((i.flags & 1u) != 0u) {
        let size = vec2<i32>(textureDimensions(atlas));
        let t = textureLoad(atlas, clamp(vec2<i32>(floor(i.uv)), vec2<i32>(0), size - 1), 0);
        if (t.a == 0.0) { discard; }
        if ((i.flags & 4u) != 0u) { c = t.rgb; } else { c = min(t.rgb * i.rgb / 128.0, vec3<f32>(1.0)); }
    } else {
        c = i.rgb / 255.0;
    }
    var f: Frag;
    f.colour = vec4<f32>(c, 1.0);
    f.mask = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    return f;
}
"#;

/// A drawn tick: RGBA colour and a coverage mask (1 where anything was drawn), row-major.
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    pub mask: Vec<bool>,
}

pub struct Raster {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    binds: HashMap<TextureKey, wgpu::BindGroup>,
    blank: wgpu::BindGroup,
}

pub(crate) const VERTEX_BYTES: u64 = 28;

/// [`SHADER`] with the polygon's bucket + 1 carried to a third output (an R32Float "order" target,
/// 0 where no polygon is): what the composite's 2D layer sorts against.
pub(crate) fn shader_with_order() -> String {
    SHADER
        .replace("    @location(2) rgbf: vec4<u32>,\n};", "    @location(2) rgbf: vec4<u32>,\n    @location(3) bucket: f32,\n};")
        .replace("    @location(2) @interpolate(flat) flags: u32,\n};", "    @location(2) @interpolate(flat) flags: u32,\n    @location(3) @interpolate(flat) bucket: f32,\n};")
        .replace("    o.flags = v.rgbf.w;\n", "    o.flags = v.rgbf.w;\n    o.bucket = v.bucket;\n")
        .replace("    @location(1) mask: vec4<f32>,\n};", "    @location(1) mask: vec4<f32>,\n    @location(2) order: vec4<f32>,\n};")
        .replace("    f.mask = vec4<f32>(1.0, 0.0, 0.0, 1.0);\n", "    f.mask = vec4<f32>(1.0, 0.0, 0.0, 1.0);\n    f.order = vec4<f32>(i.bucket + 1.0, 0.0, 0.0, 0.0);\n")
}

/// A scene's vertices for [`SHADER`] (NDC x, y, depth = SZ / 2¹⁶; texel u, v; r, g, b, flags), and
/// each batch's texture and vertex range.
pub(crate) fn pack(scene: &Scene) -> (Vec<u8>, Vec<(TextureKey, u32, u32)>) {
    let (w, h, left) = (scene.width.max(1) + 2 * scene.left, scene.height.max(1), scene.left as f32);
    let mut bytes = Vec::new();
    let mut ranges = Vec::new();
    for b in &scene.batches {
        let first = (bytes.len() as u64 / VERTEX_BYTES) as u32;
        for v in &b.corners {
            let x = (v.screen.x + left + 0.5) / w as f32 * 2.0 - 1.0;
            let y = 1.0 - (v.screen.y + 0.5) / h as f32 * 2.0;
            let z = (v.screen.z / 65536.0).clamp(0.0, 1.0);
            for f in [x, y, z, v.uv[0] as f32, v.uv[1] as f32] {
                bytes.extend_from_slice(&f.to_le_bytes());
            }
            bytes.extend_from_slice(&[v.rgb[0], v.rgb[1], v.rgb[2], v.flags]);
            bytes.extend_from_slice(&(v.bucket as f32).to_le_bytes());
        }
        ranges.push((b.texture, first, b.corners.len() as u32));
    }
    (bytes, ranges)
}

impl Raster {
    pub fn new(gpu: &Wgpu) -> Raster {
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("lt-native"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&layout], push_constant_ranges: &[] });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("unlit"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: VERTEX_BYTES,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Uint8x4, 3 => Float32],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into()), Some(wgpu::TextureFormat::R8Unorm.into())],
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
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
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let blank = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&blank_tex.create_view(&Default::default())) }],
        });
        Raster { device, queue, layout, pipeline, binds: HashMap::new(), blank }
    }

    fn bind(&mut self, cache: &Cache<Wgpu>, key: TextureKey) -> Option<&wgpu::BindGroup> {
        if !self.binds.contains_key(&key) {
            let t = cache.texture(key)?;
            let g = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&t.create_view(&Default::default())) }],
            });
            self.binds.insert(key, g);
        }
        self.binds.get(&key)
    }

    /// Draws `scene`, its textures from `cache` (prepared for the tick).
    pub fn draw(&mut self, scene: &Scene, cache: &Cache<Wgpu>) -> Image {
        let (w, h) = (scene.width.max(1) + 2 * scene.left, scene.height.max(1));
        let target = |format: wgpu::TextureFormat, copy: bool| {
            let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
            if copy {
                usage |= wgpu::TextureUsages::COPY_SRC;
            }
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let colour = target(wgpu::TextureFormat::Rgba8Unorm, true);
        let mask = target(wgpu::TextureFormat::R8Unorm, true);
        let depth = target(wgpu::TextureFormat::Depth32Float, false);
        let (bytes, ranges) = pack(scene);
        use wgpu::util::DeviceExt;
        let vb = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tick"),
            contents: if bytes.is_empty() { &[0u8; VERTEX_BYTES as usize] } else { &bytes },
            usage: wgpu::BufferUsages::VERTEX,
        });
        for &(key, _, _) in &ranges {
            let _ = self.bind(cache, key);
        }
        let views = (colour.create_view(&Default::default()), mask.create_view(&Default::default()), depth.create_view(&Default::default()));
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("unlit"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment { view: &views.0, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } }),
                    Some(wgpu::RenderPassColorAttachment { view: &views.1, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &views.2,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, vb.slice(..));
            for &(key, first, n) in &ranges {
                if n == 0 {
                    continue;
                }
                pass.set_bind_group(0, self.binds.get(&key).unwrap_or(&self.blank), &[]);
                pass.draw(first..first + n, 0..1);
            }
        }
        let rgba = self.read(&mut enc, &colour, w, h, 4);
        let m = self.read(&mut enc, &mask, w, h, 1);
        self.queue.submit([enc.finish()]);
        let rgba = rgba();
        let m = m();
        Image { w, h, rgba, mask: m.into_iter().map(|v| v > 127).collect() }
    }

    /// Queues a copy of `t` into a buffer; the returned closure waits and unpacks it.
    fn read<'a>(&'a self, enc: &mut wgpu::CommandEncoder, t: &wgpu::Texture, w: u32, h: u32, bpp: u32) -> impl FnOnce() -> Vec<u8> + 'a {
        let row = (w * bpp).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        move || {
            let slice = buffer.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            self.device.poll(wgpu::Maintain::Wait);
            let data = slice.get_mapped_range();
            let mut out = Vec::with_capacity((w * h * bpp) as usize);
            for y in 0..h as usize {
                out.extend_from_slice(&data[y * row as usize..y * row as usize + (w * bpp) as usize]);
            }
            out
        }
    }
}
