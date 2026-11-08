//! A 1024×512-halfword model of PS1 VRAM in the *field* context, enough to
//! texture world chunks the way the GPU does (port of `gc_world.py` `VRAM`).
//!
//! Painting order for a chunk (`docs/screens/09-world-viewer.md` §4.3):
//!
//! ```text
//! 1  sys/7  SYS08 field atlas   512×256 4bpp at (512, 0), CLUTs (0, 480)
//! 2  fe/k   region texture set  256×256 4bpp at (512, 256), CLUTs (160.., 480)
//! 3  sibling chunk TIMs         interiors only (id & !3 … +3)
//! 4  the chunk's own TIM        header x/y verbatim
//! ```
//!
//! Region page rows 0–127, u 128–255 (halfwords x 544–575, y 256–383) hold
//! water that the game animates at runtime; no file holds it. Texels the FE set
//! leaves at 0 there are reported as [`Texel::Water`].

use crate::container::{le16, le32};

pub const VRAM_W: usize = 1024;
pub const VRAM_H: usize = 512;

/// Halfword rectangle of the runtime-animated water in the region page.
pub const WATER_RECT: (usize, usize, usize, usize) = (544, 256, 32, 128);

/// Field-mode water colour of the rendered overview (#528FDC), used where the
/// game would show its animated water frame.
pub const WATER_RGB: [u8; 3] = [0x52, 0x8F, 0xDC];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Texel {
    /// Raw value 0x0000: never drawn.
    Clear,
    /// 15-bit BGR colour; bit 15 is the STP (semi-transparency) bit.
    Colour(u16),
    /// Runtime water (no captured frame).
    Water,
}

#[derive(Clone)]
pub struct Vram {
    pub mem: Vec<u16>,
}

impl Default for Vram {
    fn default() -> Self {
        Self::new()
    }
}

impl Vram {
    pub fn new() -> Self {
        Self { mem: vec![0; VRAM_W * VRAM_H] }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u16 {
        self.mem[(y & (VRAM_H - 1)) * VRAM_W + (x & (VRAM_W - 1))]
    }

    /// Uploads a TIM's CLUT and pixel blocks to the rectangles its header names
    /// (`UploadTIMToVRAM`). Returns false if `b` is not a TIM.
    pub fn paint_tim(&mut self, b: &[u8]) -> bool {
        if b.len() < 20 || le32(b, 0) != 0x10 {
            return false;
        }
        let flags = le32(b, 4);
        let mut p = 8usize;
        if flags & 8 != 0 {
            let bnum = le32(b, p) as usize;
            if !self.paint_block(b, p) {
                return false;
            }
            p += bnum;
        }
        self.paint_block(b, p)
    }

    /// `{u32 bnum; u16 x, y, w, h; u16 data[w*h]}` at `p`.
    fn paint_block(&mut self, b: &[u8], p: usize) -> bool {
        if p + 12 > b.len() {
            return false;
        }
        let (x, y, w, h) = (le16(b, p + 4) as usize, le16(b, p + 6) as usize, le16(b, p + 8) as usize, le16(b, p + 10) as usize);
        let data = &b[p + 12..];
        for yy in 0..h {
            for xx in 0..w {
                let o = (yy * w + xx) * 2;
                if o + 2 > data.len() {
                    return true;
                }
                let (vx, vy) = ((x + xx) & (VRAM_W - 1), (y + yy) & (VRAM_H - 1));
                self.mem[vy * VRAM_W + vx] = le16(data, o);
            }
        }
        true
    }

    /// The texel at page coordinates (u, v) of `tpage` through `clut`
    /// (tpage bits 0–3 = x/64, bit 4 = y/256, bits 7–8 = depth 4/8/15 bpp;
    /// clut = `(y << 6) | (x >> 4)`).
    #[inline]
    pub fn texel(&self, tpage: u16, clut: u16, u: u8, v: u8) -> Texel {
        let px = (tpage as usize & 0xF) * 64;
        let py = ((tpage as usize >> 4) & 1) * 256;
        let (cx, cy) = ((clut as usize & 0x3F) * 16, clut as usize >> 6);
        let (u, v) = (u as usize, v as usize);
        let (hx, hy) = match (tpage >> 7) & 3 {
            0 => (px + u / 4, py + v),
            1 => (px + u / 2, py + v),
            _ => (px + u, py + v),
        };
        let hw = self.get(hx, hy);
        let index = match (tpage >> 7) & 3 {
            0 => (hw >> ((u & 3) * 4)) & 0xF,
            1 => (hw >> ((u & 1) * 8)) & 0xFF,
            _ => return if hw == 0 { Texel::Clear } else { Texel::Colour(hw) },
        };
        if hw == 0 && in_water(hx, hy) {
            return Texel::Water;
        }
        match self.get(cx + index as usize, cy) {
            0 => Texel::Clear,
            c => Texel::Colour(c),
        }
    }
}

#[inline]
fn in_water(x: usize, y: usize) -> bool {
    let (wx, wy, ww, wh) = WATER_RECT;
    (wx..wx + ww).contains(&x) && (wy..wy + wh).contains(&y)
}

/// 5-bit channel to 8-bit.
#[inline]
pub fn c5(v: u16) -> u8 {
    let v = (v & 31) as u8;
    (v << 3) | (v >> 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paints_a_tim_and_reads_4bpp_texels() {
        // 4bpp TIM: CLUT 16×1 at (0, 480), pixels 1×1 halfwords at (640, 0).
        let mut t = vec![];
        t.extend_from_slice(&0x10u32.to_le_bytes());
        t.extend_from_slice(&0x08u32.to_le_bytes());
        t.extend_from_slice(&(12u32 + 32).to_le_bytes());
        for v in [0u16, 480, 16, 1] {
            t.extend_from_slice(&v.to_le_bytes());
        }
        for i in 0..16u16 {
            t.extend_from_slice(&(i * 0x421).to_le_bytes());
        }
        t.extend_from_slice(&14u32.to_le_bytes());
        for v in [640u16, 0, 1, 1, 0x3210] {
            t.extend_from_slice(&v.to_le_bytes());
        }
        let mut v = Vram::new();
        assert!(v.paint_tim(&t));
        let clut = (480 << 6) as u16;
        assert_eq!(v.texel(10, clut, 0, 0), Texel::Clear);
        assert_eq!(v.texel(10, clut, 1, 0), Texel::Colour(0x421));
        assert_eq!(v.texel(10, clut, 3, 0), Texel::Colour(3 * 0x421));
    }
}
