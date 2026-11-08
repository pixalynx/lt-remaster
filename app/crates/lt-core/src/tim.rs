//! Sony TIM textures. Every texture in the game is uploaded to a fixed VRAM
//! rectangle that the TIM itself names. Tamsoft quirk: flags `0x0A` ("16bpp +
//! CLUT") appear on 4bpp data, so the CLUT decides the depth, not the flag.

use serde::Serialize;

use crate::container::{le16, le32};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VramRect {
    pub x: u16,
    pub y: u16,
    /// Width in 16-bit VRAM halfwords.
    pub w: u16,
    pub h: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimInfo {
    pub flags: u32,
    /// Depth actually used to decode (4, 8 or 16).
    pub bpp: u8,
    pub image: VramRect,
    pub clut: Option<VramRect>,
    /// Width and height in pixels.
    pub width: u32,
    pub height: u32,
    /// Total TIM length in bytes.
    pub len: usize,
}

#[derive(Debug, Clone)]
pub struct Tim<'a> {
    pub info: TimInfo,
    clut: &'a [u8],
    pixels: &'a [u8],
}

impl<'a> Tim<'a> {
    /// Parses a TIM at the start of `b`. Returns `None` if it isn't one.
    pub fn parse(b: &'a [u8]) -> Option<Self> {
        if b.len() < 20 || le32(b, 0) != 0x10 {
            return None;
        }
        let flags = le32(b, 4);
        let mut p = 8usize;
        let (clut_rect, clut) = if flags & 8 != 0 {
            let bnum = le32(b, p) as usize;
            let rect = rect_at(b, p + 4);
            let data = b.get(p + 12..p + bnum)?;
            p += bnum;
            (Some(rect), data)
        } else {
            (None, &b[0..0])
        };
        let bnum = le32(b, p) as usize;
        let image = rect_at(b, p + 4);
        let pixels = b.get(p + 12..p + bnum)?;
        let stated = (flags & 3) as u8;
        let bpp = match (clut_rect, stated) {
            (Some(_), 1) => 8,
            (Some(_), _) => 4,
            (None, _) => 16,
        };
        let per_hw = 16 / bpp as u32;
        Some(Self {
            info: TimInfo {
                flags,
                bpp,
                image,
                clut: clut_rect,
                width: image.w as u32 * per_hw,
                height: image.h as u32,
                len: p + bnum,
            },
            clut,
            pixels,
        })
    }

    /// Number of 16-colour (4bpp) or 256-colour (8bpp) palettes per CLUT row.
    pub fn palettes_per_row(&self) -> usize {
        match (self.info.clut, self.info.bpp) {
            (Some(c), 4) => c.w as usize / 16,
            (Some(c), 8) => c.w as usize / 256,
            _ => 0,
        }
    }

    /// Raw 15-bit colours of one palette: `row` within the CLUT block,
    /// `col` = which 16/256-entry palette across that row.
    pub fn palette(&self, row: usize, col: usize) -> Vec<u16> {
        let Some(c) = self.info.clut else { return Vec::new() };
        let entries = if self.info.bpp == 8 { 256 } else { 16 };
        let start = (row * c.w as usize + col * entries) * 2;
        (0..entries)
            .filter_map(|i| self.clut.get(start + i * 2..start + i * 2 + 2))
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
            .collect()
    }

    /// Decodes to RGBA8 through the given palette. Colour 0x0000 is transparent.
    pub fn rgba(&self, row: usize, col: usize) -> Vec<u8> {
        let (w, h) = (self.info.width as usize, self.info.height as usize);
        let mut out = vec![0u8; w * h * 4];
        let pal: Vec<[u8; 4]> = self.palette(row, col).into_iter().map(bgr555_to_rgba).collect();
        let lookup = |i: usize| pal.get(i).copied().unwrap_or([0, 0, 0, 0]);
        match self.info.bpp {
            4 => {
                for (i, &byte) in self.pixels.iter().enumerate().take(w * h / 2) {
                    put(&mut out, i * 2, lookup((byte & 0x0F) as usize));
                    put(&mut out, i * 2 + 1, lookup((byte >> 4) as usize));
                }
            }
            8 => {
                for (i, &byte) in self.pixels.iter().enumerate().take(w * h) {
                    put(&mut out, i, lookup(byte as usize));
                }
            }
            _ => {
                for i in 0..w * h {
                    if let Some(s) = self.pixels.get(i * 2..i * 2 + 2) {
                        put(&mut out, i, bgr555_to_rgba(u16::from_le_bytes([s[0], s[1]])));
                    }
                }
            }
        }
        out
    }
}

fn put(out: &mut [u8], px: usize, c: [u8; 4]) {
    if let Some(dst) = out.get_mut(px * 4..px * 4 + 4) {
        dst.copy_from_slice(&c);
    }
}

pub(crate) fn rect_at(b: &[u8], at: usize) -> VramRect {
    VramRect { x: le16(b, at), y: le16(b, at + 2), w: le16(b, at + 4), h: le16(b, at + 6) }
}

/// PS1 15-bit colour (bit 15 = semi-transparency) to RGBA8.
pub fn bgr555_to_rgba(c: u16) -> [u8; 4] {
    if c == 0 {
        return [0, 0, 0, 0];
    }
    let ch = |v: u16| ((v << 3) | (v >> 2)) as u8;
    [ch(c & 31), ch((c >> 5) & 31), ch((c >> 10) & 31), 255]
}

/// `#RRGGBB`, or `None` for transparent (0x0000).
pub fn bgr555_to_hex(c: u16) -> Option<String> {
    (c != 0).then(|| {
        let [r, g, b, _] = bgr555_to_rgba(c);
        format!("#{r:02X}{g:02X}{b:02X}")
    })
}
