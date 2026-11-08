//! Textures no file holds: VRAM the game animates while it runs.
//!
//! The field moves 13 rects a tick with `SetDrawMove` (`Field_ChunkStreamTask`): eight strips
//! of texture pages (the water at (544, 256) among them) and five palette rows at (160, 480..484);
//! battle arenas move about 21. The GPU runs a tick's moves with that tick's ordering table, during
//! the next tick, so the VRAM a tick's frame samples is the VRAM at the tick's end with its own
//! moves applied ([`view`]).
//!
//! An atlas tile is animated when the texels or palette it decodes from overlap a rect the game
//! moves to. [`Animated`] starts from `lt_core::vram::WATER_RECT` and learns every destination it
//! sees; those tiles are decoded again each tick from the view.

use lt_core::mesh::{OwnedVram, Tile};
use lt_core::vram::{VRAM_H, VRAM_W, WATER_RECT};
use lt_scene::VramMove;

/// A VRAM rect in halfwords.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// The VRAM a tick's frame samples: `vram` (read at the tick's end) with the tick's moves applied,
/// in order, as the GPU will.
pub fn view(vram: &[u16], moves: &[VramMove]) -> Vec<u16> {
    let mut v = vram.to_vec();
    for m in moves {
        let [sx, sy, w, h] = m.from.map(usize::from);
        let [dx, dy] = m.to.map(usize::from);
        // The GPU reads the whole source before writing (a move within one strip copies the old texels).
        let src: Vec<u16> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| v[((sy + y) % VRAM_H) * VRAM_W + (sx + x) % VRAM_W])
            .collect();
        for y in 0..h {
            for x in 0..w {
                v[((dy + y) % VRAM_H) * VRAM_W + (dx + x) % VRAM_W] = src[y * w + x];
            }
        }
    }
    v
}

/// The halfwords a tile decodes from: its texels, and its palette row unless it's 15-bit.
pub fn sources(t: &Tile) -> (Rect, Option<Rect>) {
    let px = (t.tpage & 0xF) * 64;
    let py = ((t.tpage >> 4) & 1) * 256;
    let depth = (t.tpage >> 7) & 3;
    let shift = match depth {
        0 => 2,
        1 => 1,
        _ => 0,
    };
    let (u0, u1) = (t.u0, t.u0 + t.w.max(1) - 1);
    let texels = Rect {
        x: px + (u0 >> shift),
        y: py + t.v0,
        w: (u1 >> shift) - (u0 >> shift) + 1,
        h: t.h.max(1),
    };
    let clut = match depth {
        0 => Some(16),
        1 => Some(256),
        _ => None,
    }
    .map(|w| Rect {
        x: (t.clut & 0x3F) * 16,
        y: (t.clut >> 6) & 0x1FF,
        w,
        h: 1,
    });
    (texels, clut)
}

/// The rects the game animates.
#[derive(Clone, Debug)]
pub struct Animated {
    rects: Vec<Rect>,
    /// Steps when a rect is learnt (a texture's animated tiles are then found again).
    pub version: u32,
}

impl Default for Animated {
    fn default() -> Self {
        let (x, y, w, h) = WATER_RECT;
        Animated {
            rects: vec![Rect {
                x: x as u16,
                y: y as u16,
                w: w as u16,
                h: h as u16,
            }],
            version: 1,
        }
    }
}

impl Animated {
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Learns the destinations of a tick's moves.
    pub fn learn(&mut self, moves: &[VramMove]) {
        for m in moves {
            let r = Rect {
                x: m.to[0],
                y: m.to[1],
                w: m.from[2],
                h: m.from[3],
            };
            if !self.rects.iter().any(|k| {
                k.x <= r.x && k.y <= r.y && k.x + k.w >= r.x + r.w && k.y + k.h >= r.y + r.h
            }) {
                self.rects.push(r);
                self.version += 1;
            }
        }
    }

    /// Whether `t` decodes from anything animated.
    pub fn touches(&self, t: &Tile) -> bool {
        let (texels, clut) = sources(t);
        self.rects
            .iter()
            .any(|r| r.overlaps(&texels) || clut.is_some_and(|c| r.overlaps(&c)))
    }
}

/// A tile's RGBA (atlas alpha encoding: 0 clear, 255 opaque, 128 STP set) decoded from `vram`.
pub fn decode(t: &Tile, vram: &OwnedVram) -> Vec<u8> {
    let mut out = Vec::with_capacity(t.w as usize * t.h as usize * 4);
    for y in 0..t.h as usize {
        for x in 0..t.w as usize {
            out.extend_from_slice(&vram.texel_rgba(
                t.tpage,
                t.clut,
                (t.u0 as usize + x) as u8,
                (t.v0 as usize + y) as u8,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_lands_where_the_gpu_puts_it() {
        let mut v = vec![0u16; VRAM_W * VRAM_H];
        for (i, x) in (512..528).enumerate() {
            v[300 * VRAM_W + x] = i as u16 + 1;
        }
        let m = VramMove {
            from: [512, 300, 16, 1],
            to: [512, 256],
        };
        let out = view(&v, &[m]);
        assert_eq!(out[256 * VRAM_W + 512], 1);
        assert_eq!(out[256 * VRAM_W + 527], 16);
        assert_eq!(v[256 * VRAM_W + 512], 0, "the read VRAM is left alone");
    }

    #[test]
    fn tiles_know_their_texels_and_palette() {
        // 4bpp page 8 (x 512, y 256), CLUT at (160, 480): u 0..31 is halfwords 512..519.
        let t = Tile {
            tpage: 8 | (1 << 4),
            clut: (480 << 6) | 10,
            x: 0,
            y: 0,
            w: 32,
            h: 8,
            u0: 0,
            v0: 0,
        };
        let (tex, clut) = sources(&t);
        assert_eq!(
            tex,
            Rect {
                x: 512,
                y: 256,
                w: 8,
                h: 8
            }
        );
        assert_eq!(
            clut,
            Some(Rect {
                x: 160,
                y: 480,
                w: 16,
                h: 1
            })
        );
        let mut a = Animated::default();
        assert!(
            !a.touches(&Tile { tpage: 3, ..t }),
            "page 3 (192, 0), palette (160, 480) not yet learnt"
        );
        a.learn(&[VramMove {
            from: [176, 485, 16, 1],
            to: [160, 480],
        }]);
        assert!(
            a.touches(&Tile { tpage: 3, ..t }),
            "its palette row is animated"
        );
        let v = a.version;
        a.learn(&[VramMove {
            from: [176, 485, 16, 1],
            to: [160, 480],
        }]);
        assert_eq!(a.version, v, "known rects don't count twice");
    }
}
