//! The in-game dialogue window, composed from the resident SYS14 atlas (`sys/2`,
//! a 512×512 4bpp TIM at VRAM (896, 0), CLUT 160×16 at (0, 496)) the way OVR02
//! does it. Spec: `docs/screens/12-text-dialogue-editor.md` §4.3–4.4.
//!
//! - Glyphs (`Text_DrawGlyph`): cell `k = ch − 0x21`, `u = (k % 24)·8`,
//!   `v = (k / 24 + 7)·12`, 8×12, CLUT row 0 (0x7C00); `'` is 3 px wide (u + 1).
//! - Window (`FUN_OVR2__801e40f8`): 232×52 at x 44, built from 8×8 tiles at
//!   u = style·24 (VRAM x 0x380 + style·6), rows v 0xD8 (top), 0xE0 (middle ×5),
//!   0xE8 (bottom), 29 columns; CLUT row 6 (0x7D80); STP texels blend 50/50.
//! - Speaker bubble: caps and body at u 0x90–0xA8, v 0xD8, CLUT row 7 col 48,
//!   subtractive; the name in the 8×8 alt font at u 0xC0 (lowercase shifted −1).
//!
//! The backdrop is a neutral gradient (no field scene is captured). The bubble
//! placement over top/middle boxes is **unverified**; the screen labels the
//! preview "Approximate".

use crate::container::{le16, le32};

pub const W: usize = 320;
pub const H: usize = 240;
/// Glyph atlas: cells 0–95 (24 × 4) of the 8×12 font.
pub const ATLAS_W: usize = 192;
pub const ATLAS_H: usize = 48;

/// Box y for a `{box:n}` code: 5 middle, 3 top, else bottom.
pub fn box_y(n: u8) -> usize {
    match n {
        5 => 0x5A,
        3 => 0x10,
        _ => 0xA4,
    }
}

/// SYS14's 4bpp texel indices and raw CLUT words.
pub struct Sys14 {
    w: usize,
    h: usize,
    idx: Vec<u8>,
    clut: Vec<u16>,
    clut_w: usize,
}

impl Sys14 {
    /// Parses the TIM at the start of `sys/2`.
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 20 || le32(b, 0) != 0x10 || le32(b, 4) & 8 == 0 {
            return None;
        }
        let clut_len = le32(b, 8) as usize;
        let clut_w = le16(b, 16) as usize;
        let clut_h = le16(b, 18) as usize;
        let clut = (0..clut_w * clut_h).map(|i| le16(b, 20 + i * 2)).collect::<Vec<_>>();
        let p = 8 + clut_len;
        let (pw, ph) = (le16(b, p + 8) as usize, le16(b, p + 10) as usize);
        let pix = b.get(p + 12..p + 12 + pw * ph * 2)?;
        let mut idx = Vec::with_capacity(pw * 4 * ph);
        for &byte in pix {
            idx.push(byte & 15);
            idx.push(byte >> 4);
        }
        Some(Self { w: pw * 4, h: ph, idx, clut, clut_w })
    }

    fn texel(&self, u: usize, v: usize) -> usize {
        self.idx[v.min(self.h - 1) * self.w + u.min(self.w - 1)] as usize
    }

    /// 16 raw colours of CLUT row `row`, starting at entry `col0`.
    fn pal(&self, row: usize, col0: usize) -> [u16; 16] {
        let mut out = [0u16; 16];
        for (k, c) in out.iter_mut().enumerate() {
            *c = self.clut.get(row * self.clut_w + col0 + k).copied().unwrap_or(0);
        }
        out
    }

    /// The dialogue font cells 0–95 as RGBA (transparent where the colour is 0).
    pub fn glyph_atlas(&self) -> Vec<u8> {
        let pal = self.pal(0, 0);
        let mut out = vec![0u8; ATLAS_W * ATLAS_H * 4];
        for v in 0..ATLAS_H {
            for u in 0..ATLAS_W {
                let c = pal[self.texel(u, 84 + v)];
                if c != 0 {
                    let [r, g, b] = rgb(c);
                    out[(v * ATLAS_W + u) * 4..][..4].copy_from_slice(&[r, g, b, 255]);
                }
            }
        }
        out
    }

    /// A 320×240 RGBA frame: backdrop, the window for `{box:n}` and the
    /// speaker bubble when there is a speaker. Text is drawn by the caller.
    pub fn frame(&self, n: u8, speaker: Option<&str>) -> Vec<u8> {
        self.compose(Fb::backdrop(), n, speaker)
    }

    /// [`Self::frame`] on a clear background, for drawing over a live scene
    /// (screen 20): opaque texels keep alpha 255; semi-transparent ones become
    /// their colour at alpha 128 (the window's 50/50 average) or, for the
    /// speaker bubble's subtractive texels, black at the colour's strength.
    pub fn overlay(&self, n: u8, speaker: Option<&str>) -> Vec<u8> {
        self.compose(Fb::clear(), n, speaker)
    }

    /// The window graphic for `{box:n}` as composed in VRAM before it is drawn
    /// (232×56, `FUN_OVR2__801e40f8`), on a clear background with the overlay
    /// alpha encoding. Screen 20 scales it to the open/close sizes.
    pub fn window_texture(&self, n: u8) -> Vec<u8> {
        let comp = self.window_tiles(n as usize);
        let mut out = vec![0u8; 232 * 56 * 4];
        for (i, &c) in comp.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let f = rgb(c);
            let a = if c & 0x8000 != 0 { 128 } else { 255 };
            out[i * 4..i * 4 + 4].copy_from_slice(&[f[0], f[1], f[2], a]);
        }
        out
    }

    /// The speaker bubble with `name`, its top-left at (x, y), on a clear 320×240 frame.
    pub fn bubble_overlay(&self, name: &str, x: i32, y: i32) -> Vec<u8> {
        let mut fb = Fb::clear();
        self.bubble(&mut fb, name, x, y);
        fb.rgba()
    }

    fn compose(&self, mut fb: Fb, n: u8, speaker: Option<&str>) -> Vec<u8> {
        let y = box_y(n);
        self.window(&mut fb, n as usize, y);
        if let Some(name) = speaker.filter(|s| !s.is_empty()) {
            self.bubble(&mut fb, name, 0x33, y as i32 - 9);
        }
        fb.rgba()
    }

    fn window(&self, fb: &mut Fb, style: usize, y: usize) {
        let comp = self.window_tiles(style);
        let left = (W - 232) / 2;
        for r in 0..52 {
            let v = r * 56 / 52;
            for c in 0..232 {
                fb.put((left + c) as i32, (y + r) as i32, comp[v * 232 + c], Some(0));
            }
        }
    }

    /// The 232×56 window composed from 8×8 frame tiles (raw colours).
    fn window_tiles(&self, style: usize) -> Vec<u16> {
        let pal = self.pal(6, 0);
        let u0 = style * 24;
        let mut comp = vec![0u16; 56 * 232];
        for row in 0..7 {
            let ty = match row {
                0 => 216,
                6 => 232,
                _ => 224,
            };
            for col in 0..29 {
                let tx = u0 + match col {
                    0 => 0,
                    28 => 16,
                    _ => 8,
                };
                for dy in 0..8 {
                    for dx in 0..8 {
                        comp[(row * 8 + dy) * 232 + col * 8 + dx] = pal[self.texel(tx + dx, ty + dy)];
                    }
                }
            }
        }
        comp
    }

    fn bubble(&self, fb: &mut Fb, name: &str, x: i32, y: i32) {
        let pal = self.pal(7, 48);
        let n = (name.chars().count() as i32 - 1).clamp(1, 12);
        self.ft4(fb, [(x, y, 0x90, 0xD8), (x + 8, y, 0x98, 0xD8), (x - 12, y + 12, 0x90, 0xE4), (x + 8, y + 12, 0x98, 0xE4)], &pal);
        let mut xx = x + 8;
        for _ in 0..n {
            for dy in 0..12 {
                for dx in 0..8 {
                    fb.put(xx + dx, y + dy, pal[self.texel(0x98 + dx as usize, 0xD8 + dy as usize)], Some(2));
                }
            }
            xx += 8;
        }
        self.ft4(fb, [(xx, y, 0xA0, 0xD8), (xx + 20, y, 0xA8, 0xD8), (xx, y + 12, 0xA0, 0xE4), (xx + 8, y + 12, 0xA8, 0xE4)], &pal);
        let tp = self.pal(0, 0);
        let mut sx = x + 4;
        for ch in name.chars() {
            if ch != ' ' && ch.is_ascii() {
                let mut c = ch as i32;
                if ch.is_ascii_lowercase() {
                    c -= 1;
                }
                let v = ((c - 0x20) & 0xF8) as usize;
                let u = 0xC0 + ((c & 7) * 8) as usize;
                for dy in 0..8 {
                    for dx in 0..8 {
                        fb.put(sx + dx as i32, y + 3 + dy as i32, tp[self.texel(u + dx, v + dy)], None);
                    }
                }
            }
            sx += 8;
        }
    }

    /// Two affine triangles (v0 v1 v2, v1 v3 v2) like a GPU FT4, subtractive.
    fn ft4(&self, fb: &mut Fb, v: [(i32, i32, i32, i32); 4], pal: &[u16; 16]) {
        self.tri(fb, v[0], v[1], v[2], pal);
        self.tri(fb, v[1], v[3], v[2], pal);
    }

    fn tri(&self, fb: &mut Fb, p0: (i32, i32, i32, i32), p1: (i32, i32, i32, i32), p2: (i32, i32, i32, i32), pal: &[u16; 16]) {
        let area = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        let (a, b, c) = ((p0.0 as f32, p0.1 as f32), (p1.0 as f32, p1.1 as f32), (p2.0 as f32, p2.1 as f32));
        let total = area(a, b, c);
        if total == 0.0 {
            return;
        }
        let (x0, x1) = (p0.0.min(p1.0).min(p2.0), p0.0.max(p1.0).max(p2.0));
        let (y0, y1) = (p0.1.min(p1.1).min(p2.1), p0.1.max(p1.1).max(p2.1));
        for py in y0..y1 {
            for px in x0..x1 {
                let p = (px as f32 + 0.5, py as f32 + 0.5);
                let (w0, w1, w2) = (area(b, c, p) / total, area(c, a, p) / total, area(a, b, p) / total);
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let u = (w0 * p0.2 as f32 + w1 * p1.2 as f32 + w2 * p2.2 as f32) as usize;
                let v = (w0 * p0.3 as f32 + w1 * p1.3 as f32 + w2 * p2.3 as f32) as usize;
                fb.put(px, py, pal[self.texel(u, v)], Some(2));
            }
        }
    }
}

fn rgb(c: u16) -> [u8; 3] {
    [((c & 31) << 3) as u8, (((c >> 5) & 31) << 3) as u8, (((c >> 10) & 31) << 3) as u8]
}

struct Fb {
    px: Vec<[u8; 3]>,
    /// Overlay mode: per-pixel alpha, and STP texels stored rather than blended.
    alpha: Option<Vec<u8>>,
}

impl Fb {
    /// Neutral field backdrop: a vertical slate gradient with a darker floor.
    fn backdrop() -> Self {
        let (top, bot) = ([58.0f32, 70.0, 88.0], [20.0f32, 24.0, 31.0]);
        let mut px = vec![[0u8; 3]; W * H];
        for y in 0..H {
            let t = y as f32 / 239.0;
            let mut col = [0u8; 3];
            for i in 0..3 {
                col[i] = (top[i] + (bot[i] - top[i]) * t) as u8;
            }
            if y >= 150 {
                let t = (y - 150) as f32 / 90.0;
                let k = [1.0 - 0.25 * t, 1.0 - 0.2 * t, 1.0 - 0.15 * t];
                for i in 0..3 {
                    col[i] = (col[i] as f32 * k[i]) as u8;
                }
            }
            px[y * W..(y + 1) * W].fill(col);
        }
        Self { px, alpha: None }
    }

    /// A clear frame for [`Sys14::overlay`].
    fn clear() -> Self {
        Self { px: vec![[0u8; 3]; W * H], alpha: Some(vec![0; W * H]) }
    }

    /// Plots a raw colour; STP texels blend with `abr` (0 average, 1 add, 2 subtract, 3 add ¼).
    fn put(&mut self, x: i32, y: i32, c: u16, abr: Option<u8>) {
        if c == 0 || !(0..W as i32).contains(&x) || !(0..H as i32).contains(&y) {
            return;
        }
        let i = y as usize * W + x as usize;
        let f = rgb(c);
        if let Some(alpha) = &mut self.alpha {
            let (px, a) = match (c & 0x8000 != 0, abr) {
                (true, Some(2)) => ([0, 0, 0], f.iter().map(|&v| v as u16).max().unwrap_or(0) as u8),
                (true, Some(_)) => (f, 128),
                _ => (f, 255),
            };
            self.px[i] = px;
            alpha[i] = a;
            return;
        }
        let dst = &mut self.px[i];
        let mut out = f;
        if c & 0x8000 != 0 {
            if let Some(abr) = abr {
                for i in 0..3 {
                    let (b, f) = (dst[i] as i32, f[i] as i32);
                    out[i] = match abr {
                        0 => (b + f) / 2,
                        1 => (b + f).min(255),
                        2 => (b - f).max(0),
                        _ => (b + f / 4).min(255),
                    } as u8;
                }
            }
        }
        *dst = out;
    }

    fn rgba(&self) -> Vec<u8> {
        match &self.alpha {
            Some(a) => self.px.iter().zip(a).flat_map(|(p, &a)| [p[0], p[1], p[2], a]).collect(),
            None => self.px.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        }
    }
}
