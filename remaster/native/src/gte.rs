//! The GTE's perspective transform (RTPS/RTPT with `sf = 1`, `lm = 0`) in its own integer
//! arithmetic, and the two draw paths' polygon tests, so the gate's projection can only differ from
//! the game's when the inputs do.
//!
//! - **Terrain** (`WorldChunk_DrawObjectMesh`, OVR02): flags & 0x7F818000 (MAC overflows, IR1/IR2
//!   saturation, MAC0 overflow), then `nclip` ≤ 0, then the farthest vertex's SZ & 0xFFFC must be
//!   in 1..0x3000, then the polygon must reach the screen. Quads: RTPT on vertices 0–2 (flags,
//!   `nclip`), RTPS on vertex 3 (its own flags), the depth test over all four.
//! - **Models** (`Model_DrawParts`): flags & 0xFFC40000 (bit 31 sums every error but depth cueing),
//!   then `nclip`, then the nearest SZ & 0xFFFC < 0x3000. A quad's fourth vertex is not checked.
//!
//! The SZ a path keys on, over 4, is also the polygon's ordering-table bucket ([`Drawn::bucket`]):
//! the farthest vertex for terrain, the nearest for models.
//!
//! Division is the UNR table's Newton–Raphson, as psx-spx gives it.

use lt_scene::Matrix;

/// The flags each draw path rejects a polygon on.
pub const TERRAIN_FLAGS: u32 = 0x7F81_8000;
pub const MODEL_FLAGS: u32 = 0xFFC4_0000;
/// Polygons whose farthest vertex is this deep or deeper aren't drawn.
pub const FAR: u32 = 0x3000;

/// Which of the game's draw functions a polygon goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    Terrain,
    Model,
}

/// How to project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    /// The GTE's integer arithmetic: whole-pixel screen positions, as the PS1 draws.
    Gte,
    /// Floating point, sub-pixel: what a modern renderer does with the same camera.
    Float,
}

/// One projected vertex: screen position (pixels) and depth (SZ).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Screen {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// The GTE set up for one draw: rotation and translation, projection distance and screen offset
/// (16.16, as `SetGeomOffset` puts it).
#[derive(Clone, Copy, Debug)]
pub struct Gte {
    pub rt: [[i16; 3]; 3],
    pub tr: [i32; 3],
    pub h: u16,
    pub ofx: i32,
    pub ofy: i32,
    /// The transform in double precision, for a draw between ticks (LT-113): [`Gte::float`] uses
    /// it, and `rt`/`tr` are it rounded (the integer tests stay the GTE's).
    pub exact: Option<([[f64; 3]; 3], [f64; 3])>,
}

fn unr_table() -> &'static [u8; 257] {
    static T: std::sync::OnceLock<[u8; 257]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u8; 257];
        for (i, e) in t.iter_mut().enumerate() {
            *e = ((0x40000 / (i as i64 + 0x100) + 1) / 2 - 0x101).max(0) as u8;
        }
        t
    })
}

/// `H / SZ` as the GTE divides (16.16, at most 0x1FFFF), and whether it overflowed (flag 17).
pub fn divide(h: u16, sz: u16) -> (u32, bool) {
    if (h as u32) < (sz as u32) * 2 {
        let z = sz.leading_zeros();
        let n = (h as u64) << z;
        let mut d = (sz as u64) << z;
        let u = unr_table()[((d - 0x7FC0) >> 7) as usize] as u64 + 0x101;
        d = (0x200_0080u64.wrapping_sub(d * u)) >> 8;
        d = (0x80 + d * u) >> 8;
        (((n * d + 0x8000) >> 16).min(0x1_FFFF) as u32, false)
    } else {
        (0x1_FFFF, true)
    }
}

impl Gte {
    pub fn new(m: &Matrix, h: u16, offset: [i32; 2]) -> Gte {
        Gte { rt: m.m, tr: m.t, h, ofx: offset[0] << 16, ofy: offset[1] << 16, exact: None }
    }

    /// A transform no tick had (one between two): `m · v + t` in double precision.
    pub fn exact(m: [[f64; 3]; 3], t: [f64; 3], h: u16, offset: [i32; 2]) -> Gte {
        let rt = m.map(|r| r.map(|v| (v * 4096.0).round().clamp(-32768.0, 32767.0) as i16));
        let tr = t.map(|v| v.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32);
        Gte { rt, tr, h, ofx: offset[0] << 16, ofy: offset[1] << 16, exact: Some((m, t)) }
    }

    /// RTPS on one vertex: its screen position and SZ, and the flags it raises.
    pub fn rtps(&self, v: [i16; 3]) -> (i32, i32, u16, u32) {
        let mut flag = 0u32;
        let mut mac = [0i64; 3];
        for (r, m) in mac.iter_mut().enumerate() {
            let sum = ((self.tr[r] as i64) << 12) + (0..3).map(|c| self.rt[r][c] as i64 * v[c] as i64).sum::<i64>();
            if sum > (1i64 << 43) - 1 {
                flag |= 1 << (30 - r);
            } else if sum < -(1i64 << 43) {
                flag |= 1 << (27 - r);
            }
            *m = sum >> 12;
        }
        let sat = |v: i64, bit: u32, flag: &mut u32| -> i64 {
            if !(-0x8000..=0x7FFF).contains(&v) {
                *flag |= 1 << bit;
            }
            v.clamp(-0x8000, 0x7FFF)
        };
        let ir1 = sat(mac[0], 24, &mut flag);
        let ir2 = sat(mac[1], 23, &mut flag);
        let _ir3 = sat(mac[2], 22, &mut flag);
        if !(0..=0xFFFF).contains(&mac[2]) {
            flag |= 1 << 18;
        }
        let sz = mac[2].clamp(0, 0xFFFF) as u16;
        let (n, over) = divide(self.h, sz);
        if over {
            flag |= 1 << 17;
        }
        let mut screen = |ir: i64, of: i32, bit: u32| -> i32 {
            let m0 = n as i64 * ir + of as i64;
            if m0 > i32::MAX as i64 {
                flag |= 1 << 16;
            } else if m0 < i32::MIN as i64 {
                flag |= 1 << 15;
            }
            let s = m0 >> 16;
            if !(-0x400..=0x3FF).contains(&s) {
                flag |= 1 << bit;
            }
            s.clamp(-0x400, 0x3FF) as i32
        };
        let sx = screen(ir1, self.ofx, 14);
        let sy = screen(ir2, self.ofy, 13);
        if flag & 0x7F87_E000 != 0 {
            flag |= 1 << 31;
        }
        (sx, sy, sz, flag)
    }

    /// The same transform in floating point: sub-pixel screen position and depth.
    pub fn float(&self, v: [f32; 3]) -> Screen {
        let p: Vec<f64> = match &self.exact {
            Some((m, t)) => (0..3).map(|r| t[r] + (0..3).map(|c| m[r][c] * v[c] as f64).sum::<f64>()).collect(),
            None => (0..3).map(|r| self.tr[r] as f64 + (0..3).map(|c| self.rt[r][c] as f64 * v[c] as f64).sum::<f64>() / 4096.0).collect(),
        };
        let z = p[2].max(1.0);
        let h = self.h as f64;
        Screen { x: (self.ofx as f64 / 65536.0 + h * p[0] / z) as f32, y: (self.ofy as f64 / 65536.0 + h * p[1] / z) as f32, z: p[2] as f32 }
    }
}

/// Why a draw path drops a polygon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Cull {
    /// The GTE raised a flag the path checks.
    Flags,
    /// `nclip` ≤ 0: it faces away.
    Back,
    /// Its farthest vertex is 0x3000 deep or more (or 0, for terrain).
    Depth,
    /// It doesn't reach the screen.
    Offscreen,
}

fn nclip(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> f64 {
    a.0 * b.1 + b.0 * c.1 + c.0 * a.1 - a.0 * c.1 - b.0 * a.1 - c.0 * b.1
}

/// A polygon as a draw path projects it.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawn {
    pub screen: Vec<Screen>,
    /// Its ordering-table bucket: the key SZ & 0xFFFC, over 4.
    pub bucket: u32,
    /// Why the path drops it, if it does.
    pub cull: Option<Cull>,
}

/// Where a polygon must land to be drawn: the screen's columns (`x0..x1`, 224 lines) and the depth
/// its key SZ must stay under. [`Bounds::screen`] is the game's; LT-114 widens both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x0: f32,
    pub x1: f32,
    pub far: u32,
}

impl Bounds {
    /// The game's: a `width`-wide screen (320 or 512) and [`FAR`].
    pub fn screen(width: i32) -> Bounds {
        Bounds { x0: 0.0, x1: width as f32, far: FAR }
    }
}

/// A polygon's (3 or 4) vertices through `gte` on `path`: their screen positions, or `None` where
/// the game wouldn't draw it. `width` is the screen's (320 or 512); its height is 224.
pub fn polygon(gte: &Gte, path: Path, precision: Precision, verts: &[[f32; 3]], width: i32) -> Option<Vec<Screen>> {
    let d = test(gte, path, precision, verts, width);
    d.cull.is_none().then_some(d.screen)
}

/// [`polygon`], with the screen positions either way, the bucket and why it was dropped.
pub fn test(gte: &Gte, path: Path, precision: Precision, verts: &[[f32; 3]], width: i32) -> Drawn {
    test_within(gte, path, precision, verts, Bounds::screen(width))
}

/// [`test`] against other bounds: a wider screen, a longer draw distance.
pub fn test_within(gte: &Gte, path: Path, precision: Precision, verts: &[[f32; 3]], bounds: Bounds) -> Drawn {
    let (screen, bucket, cull) = judge(gte, path, precision, verts, bounds);
    Drawn { screen, bucket, cull }
}

fn judge(gte: &Gte, path: Path, precision: Precision, verts: &[[f32; 3]], bounds: Bounds) -> (Vec<Screen>, u32, Option<Cull>) {
    let n = verts.len();
    let (mask, zero_cull, check_v3) = match path {
        Path::Terrain => (TERRAIN_FLAGS, true, true),
        Path::Model => (MODEL_FLAGS, false, false),
    };
    // The GTE's integer tests on the vertices as the GTE would have them (whole units).
    let p: Vec<(i32, i32, u16, u32)> = verts.iter().map(|&v| gte.rtps(v.map(|x| x.round().clamp(-32768.0, 32767.0) as i16))).collect();
    let out: Vec<Screen> = match precision {
        Precision::Gte => p.iter().map(|&(x, y, z, _)| Screen { x: x as f32, y: y as f32, z: z as f32 }).collect(),
        Precision::Float => verts.iter().map(|&v| gte.float(v)).collect(),
    };
    // RTPT's flags over the first three, RTPS's over the fourth.
    if (p[0].3 | p[1].3 | p[2].3) & mask != 0 {
        return (out, 0, Some(Cull::Flags));
    }
    if n == 4 && check_v3 && p[3].3 & mask != 0 {
        return (out, 0, Some(Cull::Flags));
    }
    let s = |i: usize| (out[i].x as f64, out[i].y as f64);
    if nclip(s(0), s(1), s(2)) <= 0.0 {
        return (out, 0, Some(Cull::Back));
    }
    // The key SZ: terrain's farthest vertex, a model's nearest (the GTE's SZ either way).
    let zs = p.iter().map(|q| q.2 as u32);
    let key = if path == Path::Terrain { zs.max() } else { zs.min() }.unwrap_or(0) & 0xFFFC;
    let bucket = key / 4;
    if key >= bounds.far || (zero_cull && key == 0) {
        return (out, bucket, Some(Cull::Depth));
    }
    if path == Path::Terrain {
        let (xs, ys): (Vec<f32>, Vec<f32>) = out.iter().map(|q| (q.x, q.y)).unzip();
        let max = |v: &[f32]| v.iter().copied().fold(f32::MIN, f32::max);
        let min = |v: &[f32]| v.iter().copied().fold(f32::MAX, f32::min);
        if max(&xs) <= bounds.x0 || min(&xs) >= bounds.x1 || max(&ys) <= 0.0 || min(&ys) >= 224.0 {
            return (out, bucket, Some(Cull::Offscreen));
        }
    }
    (out, bucket, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn division_matches_the_gte() {
        // psx-spx: H/SZ in 16.16, the UNR result rounds within one unit of the exact quotient.
        for (h, sz) in [(892u16, 1000u16), (892, 2473), (892, 4096), (892, 0x2FFF), (892, 447)] {
            let (q, over) = divide(h, sz);
            let exact = ((h as f64) * 65536.0 / sz as f64).round() as i64;
            assert!(!over && (q as i64 - exact).abs() <= 2, "{h}/{sz}: {q:#x} vs {exact:#x}");
        }
        assert_eq!(divide(892, 446), (0x1_FFFF, true), "SZ below H/2 overflows");
    }

    #[test]
    fn a_vertex_in_front_projects_through_the_offset() {
        let g = Gte { rt: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], tr: [0, 0, 892], h: 892, ofx: 257 << 16, ofy: 112 << 16, exact: None };
        let (sx, sy, sz, flag) = g.rtps([100, -50, 0]);
        assert_eq!((sx, sy, sz, flag), (357, 62, 892, 0));
        let f = g.float([100.0, -50.0, 0.0]);
        assert!((f.x - 357.0).abs() < 1e-3 && (f.y - 62.0).abs() < 1e-3);
        // Behind the camera: SZ saturates and the model path drops it.
        let (_, _, sz, flag) = g.rtps([0, 0, -1000]);
        assert_eq!(sz, 0);
        assert!(flag & MODEL_FLAGS != 0);
    }

    #[test]
    fn back_faces_and_far_polygons_are_dropped() {
        let g = Gte { rt: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], tr: [0, 0, 2000], h: 892, ofx: 256 << 16, ofy: 112 << 16, exact: None };
        // Clockwise on screen (y down) is the front.
        let front = [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 100.0, 0.0]];
        let back = [[0.0, 0.0, 0.0], [0.0, 100.0, 0.0], [100.0, 0.0, 0.0]];
        assert!(polygon(&g, Path::Terrain, Precision::Gte, &front, 512).is_some());
        assert!(polygon(&g, Path::Terrain, Precision::Gte, &back, 512).is_none());
        let far = Gte { tr: [0, 0, 0x3000], ..g };
        assert!(polygon(&far, Path::Model, Precision::Gte, &front, 512).is_none());
    }
}
