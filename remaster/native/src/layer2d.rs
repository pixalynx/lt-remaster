//! The 2D layer: the ordering table's packets the native view doesn't draw itself (HUD, text,
//! menus, effects, shadows), decoded the way PSoXide's hardware renderer decodes them
//! (`psx_gpu_render::Translator`), each with its bucket.
//!
//! **Sorting by bucket.** The game puts each primitive in bucket screen Z / 4 (the farthest vertex
//! for chunk polygons, the nearest for models), and the GPU draws from the farthest bucket to the
//! nearest. The native 3D records each pixel's polygon bucket, and a 2D primitive shows where that
//! polygon's bucket is farther than its own: exactly what the GPU's painting gives.
//!
//! Testing against per-pixel depth instead (bucket `b` as depth `4b`) gets it wrong wherever a
//! polygon spans depth. A full-screen fade in bucket b covers every terrain polygon whose farthest
//! vertex is beyond 4b, including those polygons' nearer pixels; per-pixel depth let them through.
//! Within one bucket the GPU draws the later-linked packet first, so an effect linked after the
//! terrain is behind it. The HUD (`HUD_FlushTextBufferToOT`) and the `SetDrawMove`s are in bucket 0,
//! in front of everything.

use emulator_core::gpu::{gp0_command_word_count, GpuCmdFifo, GpuCmdLogEntry};
use psx_gpu_render::Translator;

use crate::takeover::Table;

/// One vertex of the 2D layer: PSoXide's `HwVertex` (84 bytes, its layout) and its bucket.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub pos: [i16; 2],
    pub color: [u8; 4],
    pub uv: [u16; 2],
    pub flags: u32,
    pub tex_window: u32,
    pub planes: [[u32; 4]; 4],
    /// The packet's ordering-table bucket.
    pub bucket: f32,
}

impl Vertex {
    pub const BYTES: usize = 88;

    pub fn write(&self, out: &mut Vec<u8>) {
        for v in self.pos {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.color);
        for v in self.uv {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.tex_window.to_le_bytes());
        for p in self.planes {
            for w in p {
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
        out.extend_from_slice(&self.bucket.to_le_bytes());
    }
}

/// A run of vertices drawn with one blend and one draw-area clip (inclusive VRAM rect).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// 0 opaque, 1 average, 2 add, 3 subtract, 4 add-quarter.
    pub blend: usize,
    pub first: u32,
    pub count: u32,
    pub clip: [u16; 4],
}

/// The table's kept packets as triangles.
#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub vertices: Vec<Vertex>,
    pub runs: Vec<Run>,
    /// GP0 commands decoded, and those that drew nothing (environment, VRAM copies, NOPs).
    pub commands: u32,
    pub silent: u32,
}


/// Decodes `table` at internal scale `scale`: every packet goes through the decoder in the GPU's
/// order, so its state (texture page, blend mode, draw area) is the GPU's; only the packets not set
/// aside give vertices.
pub fn layer(table: &Table, scale: u32) -> Layer {
    layer_with(table, scale, false)
}

/// [`layer`], leaving out the characters' blob shadows when `no_blob_shadows` (the lit look casts
/// real ones).
pub fn layer_with(table: &Table, scale: u32, no_blob_shadows: bool) -> Layer {
    let mut t = Translator::new();
    t.restore_environment(table.env);
    t.set_internal_scale(scale);
    let mut out = Layer::default();
    let mut index = 0u32;
    for p in &table.packets {
        let bucket = p.bucket as f32;
        let mut at = 0usize;
        while at < p.words.len() {
            let opcode = (p.words[at] >> 24) as u8;
            let n = gp0_command_word_count(opcode).max(1).min(p.words.len() - at);
            let entry = GpuCmdLogEntry { index, opcode, fifo: GpuCmdFifo::from_slice(&p.words[at..at + n]) };
            index += 1;
            at += n;
            out.commands += 1;
            let frame = t.translate(std::slice::from_ref(&entry));
            if p.aside || (no_blob_shadows && p.shadow) {
                continue;
            }
            if frame.vertices.is_empty() {
                out.silent += 1;
                continue;
            }
            let base = out.vertices.len() as u32;
            out.vertices.extend(frame.vertices.iter().map(|v| Vertex { pos: v.pos, color: v.color, uv: v.uv, flags: v.flags, tex_window: v.tex_window, planes: v.planes, bucket }));
            out.runs.extend(frame.runs.iter().map(|r| Run { blend: r.kind.index(), first: base + r.start, count: r.count, clip: r.clip }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::takeover::Linked;

    #[test]
    fn a_flat_rectangle_decodes_with_its_bucket() {
        // GP0(60h): a monochrome rectangle, colour 0x0000FF, at (10, 20), 8 × 4.
        let table = Table { packets: vec![Linked { addr: 0x8010_0000, bucket: 100, words: vec![0x6000_00FF, (20 << 16) | 10, (4 << 16) | 8], aside: false, shadow: false }], env: [0xE100_0000, 0xE200_0000, 0xE300_0000, 0xE407_FFFF | (511 << 10), 0xE500_0000, 0xE600_0000], ..Table::default() };
        let l = layer(&table, 1);
        assert_eq!(l.commands, 1);
        assert!(!l.vertices.is_empty() && l.vertices.len().is_multiple_of(3));
        assert!(l.vertices.iter().all(|v| v.bucket == 100.0));
        let xs: Vec<i16> = l.vertices.iter().map(|v| v.pos[0]).collect();
        assert_eq!((xs.iter().min(), xs.iter().max()), (Some(&10), Some(&18)));
        assert_eq!(l.runs.iter().map(|r| r.count).sum::<u32>() as usize, l.vertices.len());
        let mut b = Vec::new();
        l.vertices[0].write(&mut b);
        assert_eq!(b.len(), Vertex::BYTES);
    }
}
