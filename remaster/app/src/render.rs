//! The window's GPU side (wgpu): the emulated picture on the left, the native view on the right
//! (a placeholder until its first picture), and the debug overlay over both. The native view is
//! composited on this device by the native thread and drawn from its texture as it is.
//!
//! Quads, in order: the picture, a black backdrop for the native view, the native view (inside the
//! backdrop, placed by the screen position), the overlay. Then the toolbar (LT-117: egui, drawn by
//! `egui-wgpu` on the same device) along the top, which the panes stay below.
//!
//! Everything is a textured quad through one pipeline. The picture texture follows the emulated
//! display's size (320 or 512 x 224, 640 x 448 interlaced) and keeps the PS1's 4:3 shape; the native
//! view takes the aspect it was drawn at (LT-114: 4:3 up to 32:9) beside it, or the whole window
//! alone. A native picture that isn't composited (a movie, the placeholder) stays 4:3 in the
//! middle of its pane.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::window::Window;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

const SHADER: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> VOut {
    var o: VOut;
    o.pos = vec4<f32>(pos, 0.0, 1.0);
    o.uv = uv;
    return o;
}
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@fragment fn fs(i: VOut) -> @location(0) vec4<f32> { return textureSample(tex, samp, i.uv); }
"#;

struct Tex {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    width: u32,
    height: u32,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    vertices: wgpu::Buffer,
    picture: Tex,
    native: Tex,
    overlay: Tex,
    backdrop: Tex,
    /// The native view as the native thread composited it: its bind group, where it sits in the
    /// view (fractions x0, y0, x1, y1) and the part of the texture that shows (u0, v0, u1, v1).
    native_gpu: Option<(wgpu::BindGroup, [f32; 4], [f32; 4])>,
    /// The native pane's aspect (width over height), and whether it fills the window alone.
    pub native_aspect: f32,
    pub native_only: bool,
    pub adapter: String,
    /// The toolbar's height (physical pixels): the panes go below it.
    pub top: f32,
    /// The toolbar: its renderer, and what it last drew.
    ui: egui_wgpu::Renderer,
    ui_jobs: Vec<egui::ClippedPrimitive>,
    ui_ppp: f32,
}

impl Renderer {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window).map_err(|e| format!("surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or("no GPU adapter for this window")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).map_err(|e| format!("device: {e}"))?;
        let caps = surface.get_capabilities(&adapter);
        // A plain (not sRGB) target: the emulated picture is display colours already.
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let pipeline = make_pipeline(&device, &layout, format);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Nearest, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quads"),
            contents: bytemuck::cast_slice(&[Vertex { pos: [0.0; 2], uv: [0.0; 2] }; 24]),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let info = adapter.get_info();
        let ui = egui_wgpu::Renderer::new(&device, format, None, 1, false);
        let mut r = Renderer {
            top: 0.0,
            ui,
            ui_jobs: Vec::new(),
            ui_ppp: 1.0,
            picture: Self::texture(&device, &layout, &sampler, 1, 1),
            native: Self::texture(&device, &layout, &sampler, 1, 1),
            overlay: Self::texture(&device, &layout, &sampler, 1, 1),
            backdrop: Self::texture(&device, &layout, &sampler, 1, 1),
            native_gpu: None,
            native_aspect: 4.0 / 3.0,
            native_only: false,
            surface,
            device,
            queue,
            config,
            pipeline,
            layout,
            sampler,
            vertices,
            adapter: format!("{} ({:?})", info.name, info.backend),
        };
        let (rgba, w, h) = native_placeholder();
        r.upload(Which::Native, &rgba, w, h);
        r.upload(Which::Backdrop, &[0, 0, 0, 255], 1, 1);
        Ok(r)
    }

    /// The device and queue, for the native thread to composite on.
    pub fn gpu(&self) -> (wgpu::Device, wgpu::Queue) {
        (self.device.clone(), self.queue.clone())
    }

    /// Shows a native view the native thread composited on this device: `texture` (`w` × `h`) at
    /// `at` in a `canvas` the size of the emulated picture (native pixels), clipped to it.
    pub fn show_native(&mut self, texture: &wgpu::Texture, w: u32, h: u32, canvas: [u32; 2], at: [i32; 2]) {
        let (cw, ch) = (canvas[0].max(1) as f32, canvas[1].max(1) as f32);
        let (x0, y0) = (at[0].max(0) as f32, at[1].max(0) as f32);
        let (x1, y1) = (((at[0] + w as i32) as f32).min(cw), ((at[1] + h as i32) as f32).min(ch));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let rect = [x0 / cw, y0 / ch, x1 / cw, y1 / ch];
        let (fx, fy) = (at[0] as f32, at[1] as f32);
        let uv = [(x0 - fx) / w as f32, (y0 - fy) / h as f32, (x1 - fx) / w as f32, (y1 - fy) / h as f32];
        let view = texture.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("native view"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
        });
        self.native_gpu = Some((bind, rect, uv));
    }

    fn texture(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, width: u32, height: u32) -> Tex {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) }],
        });
        Tex { texture, bind, width, height }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn upload(&mut self, which: Which, rgba: &[u8], width: u32, height: u32) {
        if width == 0 || height == 0 || rgba.len() < (width * height * 4) as usize {
            return;
        }
        let (device, layout, sampler) = (&self.device, &self.layout, &self.sampler);
        if matches!(which, Which::Native) {
            self.native_gpu = None;
        }
        let tex = match which {
            Which::Picture => &mut self.picture,
            Which::Native => &mut self.native,
            Which::Overlay => &mut self.overlay,
            Which::Backdrop => &mut self.backdrop,
        };
        if tex.width != width || tex.height != height {
            *tex = Self::texture(device, layout, sampler, width, height);
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * width), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }

    /// The aspect a native pane would have if it took all the room the window leaves it: the
    /// whole window below the toolbar alone, else what's beside a 4:3 picture as tall as that.
    pub fn room(&self) -> f32 {
        let (w, h) = (self.config.width as f32, (self.config.height as f32 - self.top).max(1.0));
        if self.native_only { w / h } else { w / h - 4.0 / 3.0 }
    }

    /// The emulated picture's pane and the native view's (x, y, width, height): side by side at one
    /// height (4:3, then the native aspect), centred below the toolbar; or the native view alone.
    fn panes(&self) -> ([f32; 4], [f32; 4]) {
        let (w, h) = (self.config.width as f32, (self.config.height as f32 - self.top).max(1.0));
        let a = self.native_aspect.max(0.1);
        let lead = if self.native_only { 0.0 } else { 4.0 / 3.0 };
        let ph = h.min(w / (lead + a));
        let x0 = (w - ph * (lead + a)) / 2.0;
        let y0 = self.top + (h - ph) / 2.0;
        ([x0, y0, ph * lead, ph], [x0 + ph * lead, y0, ph * a, ph])
    }

    /// The toolbar's frame from egui: its textures and shapes, at `ppp` physical pixels a point.
    pub fn set_ui(&mut self, textures: egui::TexturesDelta, jobs: Vec<egui::ClippedPrimitive>, ppp: f32) {
        for (id, delta) in &textures.set {
            self.ui.update_texture(&self.device, &self.queue, *id, delta);
        }
        for id in &textures.free {
            self.ui.free_texture(id);
        }
        self.ui_jobs = jobs;
        self.ui_ppp = ppp;
    }

    /// The toolbar over what `enc` drew to `view`.
    fn ui_pass(&mut self, enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        if self.ui_jobs.is_empty() {
            return;
        }
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [self.config.width, self.config.height], pixels_per_point: self.ui_ppp };
        let extra = self.ui.update_buffers(&self.device, &self.queue, enc, &self.ui_jobs, &screen);
        if !extra.is_empty() {
            self.queue.submit(extra);
        }
        let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("toolbar"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
            ..Default::default()
        });
        self.ui.render(&mut pass.forget_lifetime(), &self.ui_jobs, &screen);
    }

    /// The four quads for the current window size.
    fn layout(&self) {
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        let scale = (h / 360.0).clamp(1.0, 3.0).floor();
        let full = [0.0, 0.0, 1.0, 1.0];
        let (picture, view) = self.panes();
        let native = match &self.native_gpu {
            Some((_, r, uv)) => ([view[0] + view[2] * r[0], view[1] + view[3] * r[1], view[2] * (r[2] - r[0]), view[3] * (r[3] - r[1])], *uv),
            // 4:3 in the middle of the pane.
            None => {
                let vw = (view[3] * 4.0 / 3.0).min(view[2]);
                ([view[0] + (view[2] - vw) / 2.0, view[1], vw, view[3]], full)
            }
        };
        let quads = [(picture, full), (view, full), native, ([8.0, 8.0 + self.top, self.overlay.width as f32 * scale, self.overlay.height as f32 * scale], full)];
        let mut v = Vec::with_capacity(24);
        for ([x, y, rw, rh], [u0, v0, u1, v1]) in quads {
            let (l, r) = (x / w * 2.0 - 1.0, (x + rw) / w * 2.0 - 1.0);
            let (t, b) = (1.0 - y / h * 2.0, 1.0 - (y + rh) / h * 2.0);
            v.extend_from_slice(&[
                Vertex { pos: [l, t], uv: [u0, v0] },
                Vertex { pos: [l, b], uv: [u0, v1] },
                Vertex { pos: [r, b], uv: [u1, v1] },
                Vertex { pos: [l, t], uv: [u0, v0] },
                Vertex { pos: [r, b], uv: [u1, v1] },
                Vertex { pos: [r, t], uv: [u1, v0] },
            ]);
        }
        self.queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&v));
    }

    fn pass(&self, enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, pipeline: &wgpu::RenderPipeline) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.035, g: 0.035, b: 0.045, a: 1.0 }), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        let native = self.native_gpu.as_ref().map_or(&self.native.bind, |n| &n.0);
        for (i, bind) in [&self.picture.bind, &self.backdrop.bind, native, &self.overlay.bind].into_iter().enumerate() {
            pass.set_bind_group(0, bind, &[]);
            pass.draw(6 * i as u32..6 * (i as u32 + 1), 0..1);
        }
    }

    /// Draws a frame; false when the surface had to be set up again (resized, lost).
    pub fn draw(&mut self) -> bool {
        self.layout();
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.pass(&mut enc, &view, &self.pipeline);
        self.ui_pass(&mut enc, &view);
        self.queue.submit([enc.finish()]);
        frame.present();
        true
    }

    /// The window's picture as it would be drawn now, toolbar and all, as RGBA rows (width, height).
    pub fn capture(&mut self) -> (Vec<u8>, u32, u32) {
        self.layout();
        let (w, h) = (self.config.width, self.config.height);
        // The surface's own format (the toolbar's renderer is made for it); BGRA is swapped below.
        let format = self.config.format;
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        let view = target.create_view(&Default::default());
        self.pass(&mut enc, &view, &self.pipeline);
        self.ui_pass(&mut enc, &view);
        let row = (4 * w).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("capture"), size: (row * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::Maintain::Wait);
        let data = buffer.slice(..).get_mapped_range();
        let mut rgba = Vec::with_capacity((4 * w * h) as usize);
        for y in 0..h as usize {
            rgba.extend_from_slice(&data[y * row as usize..y * row as usize + 4 * w as usize]);
        }
        if matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) {
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        (rgba, w, h)
    }
}

fn make_pipeline(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("quad"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("quad"), bind_group_layouts: &[layout], push_constant_ranges: &[] });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
}

#[derive(Clone, Copy)]
pub enum Which {
    Picture,
    Native,
    Overlay,
    Backdrop,
}

/// Lines of text as an RGBA image: white 8 x 8 letters on a dark, half-clear panel.
pub fn text(lines: &[String]) -> (Vec<u8>, u32, u32) {
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(1) as u32;
    let (w, h) = (cols * 8 + 8, lines.len().max(1) as u32 * 10 + 6);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for px in rgba.chunks_mut(4) {
        px.copy_from_slice(&[8, 9, 12, 190]);
    }
    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            let glyph = font8x8::legacy::BASIC_LEGACY.get(ch as usize).copied().unwrap_or([0; 8]);
            for (gy, bits) in glyph.iter().enumerate() {
                for gx in 0..8 {
                    if bits >> gx & 1 != 0 {
                        let (x, y) = (4 + col as u32 * 8 + gx, 3 + row as u32 * 10 + gy as u32);
                        let i = ((y * w + x) * 4) as usize;
                        rgba[i..i + 4].copy_from_slice(&[236, 233, 226, 255]);
                    }
                }
            }
        }
    }
    (rgba, w, h)
}

/// The native view until its first picture: a dark 4:3 panel with a faint grid and a label.
fn native_placeholder() -> (Vec<u8>, u32, u32) {
    let (w, h) = (320u32, 240u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let grid = x % 32 == 0 || y % 32 == 0;
            let c = if grid { [30, 32, 40, 255] } else { [17, 18, 23, 255] };
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&c);
        }
    }
    let (label, lw, lh) = text(&["Native view".into(), "waiting for the game's first table".into()]);
    let (ox, oy) = ((w - lw) / 2, (h - lh) / 2);
    for y in 0..lh {
        for x in 0..lw {
            let s = ((y * lw + x) * 4) as usize;
            let d = (((oy + y) * w + ox + x) * 4) as usize;
            rgba[d..d + 4].copy_from_slice(&label[s..s + 4]);
        }
    }
    (rgba, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_8_by_8_letters_in_a_panel() {
        let (rgba, w, h) = text(&["AB".into(), "C".into()]);
        assert_eq!((w, h), (2 * 8 + 8, 2 * 10 + 6));
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        // Some pixels are letters, the corner is the panel.
        assert!(rgba.chunks(4).any(|p| p == [236, 233, 226, 255]));
        assert_eq!(&rgba[..4], &[8, 9, 12, 190]);
    }

    #[test]
    fn the_native_placeholder_is_4_by_3() {
        let (rgba, w, h) = native_placeholder();
        assert_eq!(w * 3, h * 4);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
    }
}
