//! The GPU cache: each mesh and texture a frame needs is uploaded the first time it's drawn and
//! kept; the only per-tick traffic is the animated tiles ([`crate::TextureWrite`]).
//!
//! [`Gpu`] is the device side. [`Wgpu`] puts meshes in vertex buffers (16-byte
//! [`crate::mesh::Corner`]s) and atlases in RGBA8 textures; [`Counting`] keeps nothing and counts,
//! for tests and machines with no GPU.

use std::collections::HashMap;

use crate::mesh::Corner;
use crate::{Assets, Frame, MeshKey, TextureKey};

/// A device the cache uploads to.
pub trait Gpu {
    type Mesh;
    type Texture;
    fn upload_mesh(&mut self, label: &str, corners: &[u8]) -> Self::Mesh;
    fn upload_texture(&mut self, label: &str, w: u32, h: u32, rgba: &[u8]) -> Self::Texture;
    fn write_texture(&mut self, t: &Self::Texture, x: u32, y: u32, w: u32, h: u32, rgba: &[u8]);
}

/// What was uploaded, when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Upload {
    Mesh {
        tick: u64,
        key: MeshKey,
        label: String,
        bytes: usize,
    },
    Texture {
        tick: u64,
        key: TextureKey,
        label: String,
        bytes: usize,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Every upload, in order.
    pub uploads: Vec<Upload>,
    /// Animated tiles written, and their bytes.
    pub writes: u64,
    pub write_bytes: u64,
}

impl Stats {
    pub fn upload_bytes(&self) -> usize {
        self.uploads
            .iter()
            .map(|u| match u {
                Upload::Mesh { bytes, .. } | Upload::Texture { bytes, .. } => *bytes,
            })
            .sum()
    }
}

pub struct Cache<G: Gpu> {
    pub gpu: G,
    meshes: HashMap<MeshKey, G::Mesh>,
    textures: HashMap<TextureKey, G::Texture>,
    pub stats: Stats,
}

impl<G: Gpu> Cache<G> {
    pub fn new(gpu: G) -> Self {
        Cache {
            gpu,
            meshes: HashMap::new(),
            textures: HashMap::new(),
            stats: Stats::default(),
        }
    }

    /// Uploads what `frame` draws that isn't on the GPU yet, then writes its animated tiles.
    pub fn prepare(&mut self, assets: &Assets, frame: &Frame) {
        for key in frame.meshes() {
            if self.meshes.contains_key(&key) {
                continue;
            }
            let Some(m) = assets.mesh(key) else { continue };
            let bytes = Corner::bytes(&m.corners);
            let label = assets.label(key);
            let handle = self.gpu.upload_mesh(&label, &bytes);
            self.meshes.insert(key, handle);
            self.stats.uploads.push(Upload::Mesh {
                tick: frame.tick,
                key,
                label,
                bytes: bytes.len(),
            });
        }
        for key in frame.textures() {
            if self.textures.contains_key(&key) {
                continue;
            }
            let Some(t) = assets.texture(key) else {
                continue;
            };
            let label = format!("atlas {key:016x}");
            let handle = self.gpu.upload_texture(&label, t.w, t.h, &t.rgba);
            self.textures.insert(key, handle);
            self.stats.uploads.push(Upload::Texture {
                tick: frame.tick,
                key,
                label,
                bytes: t.rgba.len(),
            });
        }
        for w in &frame.writes {
            if let Some(t) = self.textures.get(&w.texture) {
                self.gpu.write_texture(t, w.x, w.y, w.w, w.h, &w.rgba);
                self.stats.writes += 1;
                self.stats.write_bytes += w.rgba.len() as u64;
            }
        }
    }

    pub fn mesh(&self, key: MeshKey) -> Option<&G::Mesh> {
        self.meshes.get(&key)
    }

    pub fn texture(&self, key: TextureKey) -> Option<&G::Texture> {
        self.textures.get(&key)
    }

    /// Every texture on the GPU.
    pub fn texture_keys(&self) -> impl Iterator<Item = TextureKey> + '_ {
        self.textures.keys().copied()
    }
}

/// Uploads nothing; counts bytes.
#[derive(Default)]
pub struct Counting {
    pub bytes: u64,
}

impl Gpu for Counting {
    type Mesh = usize;
    type Texture = (u32, u32);
    fn upload_mesh(&mut self, _: &str, corners: &[u8]) -> usize {
        self.bytes += corners.len() as u64;
        corners.len()
    }
    fn upload_texture(&mut self, _: &str, w: u32, h: u32, rgba: &[u8]) -> (u32, u32) {
        self.bytes += rgba.len() as u64;
        (w, h)
    }
    fn write_texture(&mut self, t: &(u32, u32), x: u32, y: u32, w: u32, h: u32, rgba: &[u8]) {
        assert!(
            x + w <= t.0 && y + h <= t.1 && rgba.len() == (w * h * 4) as usize,
            "a write inside its texture"
        );
        self.bytes += rgba.len() as u64;
    }
}

/// A wgpu device and queue.
pub struct Wgpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// A mesh on the GPU: its corners in a vertex buffer.
pub struct GpuMesh {
    pub buffer: wgpu::Buffer,
    pub corners: u32,
}

impl Wgpu {
    /// A headless device (no window), or `None` without a GPU.
    pub fn headless() -> Option<Wgpu> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
                .ok()?;
        Some(Wgpu { device, queue })
    }

    /// A texture's texels read back (RGBA8, `w` × `h`): what the GPU holds after the uploads and
    /// writes so far.
    pub fn read_texture(&self, t: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
        let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: t,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            out.extend_from_slice(&data[y * row as usize..y * row as usize + w as usize * 4]);
        }
        out
    }
}

impl Gpu for Wgpu {
    type Mesh = GpuMesh;
    type Texture = wgpu::Texture;

    fn upload_mesh(&mut self, label: &str, corners: &[u8]) -> GpuMesh {
        use wgpu::util::DeviceExt;
        // wgpu refuses empty buffers; an empty mesh keeps one corner's worth.
        let contents = if corners.is_empty() {
            &[0u8; Corner::BYTES][..]
        } else {
            corners
        };
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::VERTEX,
            });
        GpuMesh {
            buffer,
            corners: (corners.len() / Corner::BYTES) as u32,
        }
    }

    fn upload_texture(&mut self, label: &str, w: u32, h: u32, rgba: &[u8]) -> wgpu::Texture {
        let size = wgpu::Extent3d {
            width: w.max(1),
            height: h.max(1),
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            // COPY_SRC: read back (checks, texture-pack export).
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        if rgba.len() as u64 >= w as u64 * h as u64 * 4 && w > 0 && h > 0 {
            self.write_texture(&texture, 0, 0, w, h, rgba);
        }
        texture
    }

    fn write_texture(&mut self, t: &wgpu::Texture, x: u32, y: u32, w: u32, h: u32, rgba: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: t,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }
}
