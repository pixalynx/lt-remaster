//! Chimney smoke the game doesn't draw, rebuilt for a widened view (LT-144).
//!
//! `Field_DrawTorchFlames` (OVR02) walks the field's object records each tick. For each smoke
//! emitter (type 5) it projects the anchor with `RotTransPers` and draws eight puffs only when:
//! - the GTE raised none of the flags it checks;
//! - the anchor lands within 60 pixels of the 4:3 screen (64 in the 512-wide mode);
//! - it hasn't drawn three emitters already.
//!
//! A widened view sees emitters past that test, and more of them than three. The takeover notes
//! each anchor test (`RotTransPers` from `Field_DrawTorchFlames`) with the state the puffs are
//! built from ([`Torch`]). For each emitter the game left out, [`rebuild`] then makes the eight
//! puffs as the game would have, in the game's integer arithmetic:
//! - the billboard turn from the camera's angles (`FUN_801a0efc`, through the game's own sine
//!   table), composed with the view (`FUN_801a11cc`);
//! - each puff's rise and drift, its jitter (`Particle_JitterBillboardQuad`), and its texture and
//!   CLUT by phase;
//! - its bucket, the nearest corner's SZ over 4 (`Gte_MinOtz4`), at least 3.
//!
//! An emitter the game leaves out keeps its phase in RAM (the game steps it only when it draws).
//! A rebuilt one takes the task's tick counter instead, so its smoke still rises.
//!
//! The rebuild of the emitters the game did draw is the game's puffs, corner for corner (the
//! test set's check). That is how the arithmetic is known to be the game's.

use lt_emu_hooks::Call;
use lt_scene::Matrix;

use crate::billboard::{Corner, Projection, FAR_FLAGS, GAME_FLAGS};
use crate::gte::Gte;
use crate::takeover::Linked;

/// libgte's `RotTransPers(v, sxy, p, flag)`.
pub const ROT_TRANS_PERS: u32 = 0x801A_A028;
/// Main's sine table for `FUN_801a1040`: 1,025 halfwords, a quarter turn.
pub const SINE: u32 = 0x8017_602C;
pub const SINE_LEN: usize = 1025;
/// `Field_DrawTorchFlames`' camera (`D_801CDBD8`): its angles at +4, the drift at +0x20, +0x22.
pub const CAMERA: u32 = 0x801C_DBD8;
/// The field's object records (`D_801FD638`, 16 bytes each) and their count (`D_801FD758`), OVR02.
pub const RECORDS: u32 = 0x801F_D638;
pub const RECORD_COUNT: u32 = 0x801F_D758;
/// The caller of a puff the game didn't draw.
pub const REBUILT: &str = "smoke (rebuilt)";
/// Where rebuilt puffs' prims would be: no RAM there, so nothing the game wrote matches them.
pub const REBUILT_PRIMS: u32 = 0xF000_0000;

/// A smoke emitter: its record's index in the list, its anchor and its phase (as RAM had them when
/// the game tested the first anchor).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Emitter {
    pub index: usize,
    pub at: [i16; 3],
    pub phase: u8,
}

/// One tick's `Field_DrawTorchFlames`: what its puffs are built from, and the anchors it tested.
#[derive(Clone, Debug)]
pub struct Torch {
    /// The task's tick counter (`obj->f24`): the puffs' jitter.
    pub tick: i32,
    /// `obj->f98`: the view.
    pub view: Matrix,
    /// The camera's angles (rx, ry, rz) and its drift (`f20` the size, `f22` the angle).
    pub angles: [i16; 3],
    pub drift: [i16; 2],
    /// The 512-wide screen (`obj->f4C`).
    pub wide: bool,
    /// The GTE as the anchors were tested: the view's turn, the translation it had, H, offsets.
    pub gte: Gte,
    pub emitters: Vec<Emitter>,
    /// The anchors tested, in the game's order: the record index, the GTE's screen position and
    /// flags.
    pub tested: Vec<(usize, [i32; 2], u32)>,
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn i16_at(b: &[u8], i: usize) -> i16 {
    i16::from_le_bytes([b[i], b[i + 1]])
}

impl Torch {
    /// The state at the first anchor test of a tick (`c` at `RotTransPers`' entry, from
    /// `Field_DrawTorchFlames`): `obj` is its task context (the call's `p` is `obj + 0x13C`).
    pub fn at_call(c: &Call) -> Torch {
        let obj = c.arg(2).wrapping_sub(0x13C);
        let o = c.read(obj, 0xB8);
        let cam = c.read(CAMERA, 0x24);
        let list = u32_at(&c.read(RECORDS, 4), 0);
        let n = u16::from_le_bytes(c.read(RECORD_COUNT, 2).try_into().unwrap()) as usize;
        let recs = c.read(list, n.min(1024) * 16);
        let emitters = (0..n.min(1024)).filter(|&i| recs[i * 16 + 8] == 5).map(|i| Emitter { index: i, at: [0, 1, 2].map(|k| i16_at(&recs, i * 16 + k * 2)), phase: recs[i * 16 + 9] }).collect();
        let g = c.cpu().cop2();
        let gte = Gte { h: g.read_control(26) as u16, ofx: g.read_control(24) as i32, ofy: g.read_control(25) as i32, ..Gte::new(&Matrix::from_gte(c.cpu()), 0, [0, 0]) };
        Torch {
            tick: u32_at(&o, 0x24) as i32,
            view: Matrix::from_bytes(&o[0x98..0xB8]),
            angles: [i16_at(&cam, 4), i16_at(&cam, 6), i16_at(&cam, 8)],
            drift: [i16_at(&cam, 0x20), i16_at(&cam, 0x22)],
            wide: o[0x4C] != 0,
            gte,
            emitters,
            tested: Vec::new(),
        }
    }

    /// The record `c` (an anchor test) tests: its index in the list.
    pub fn record(c: &Call) -> usize {
        let list = u32_at(&c.read(RECORDS, 4), 0);
        c.arg(0).wrapping_sub(list) as usize / 16
    }

    /// The anchor test `c` (another `RotTransPers` of the same call) noted.
    pub fn test(&mut self, c: &Call) {
        let rec = Self::record(c);
        let b = c.read(c.arg(0), 6);
        let (x, y, _, flags) = self.gte.rtps([0, 1, 2].map(|k| i16_at(&b, k * 2)));
        self.tested.push((rec, [x, y], flags));
    }

    /// Whether the game draws an anchor at `screen` with `flags`.
    pub fn passes(&self, screen: [i32; 2], flags: u32) -> bool {
        let [x, y] = screen;
        let across = if self.wide { (-0x40..0x241).contains(&x) } else { (-0x3C..0x17D).contains(&x) };
        flags & GAME_FLAGS == 0 && across && (-0x28..0x12D).contains(&y)
    }

    /// The emitters the game drew (their record indices): those whose anchors passed.
    pub fn drawn(&self) -> Vec<usize> {
        self.tested.iter().filter(|t| self.passes(t.1, t.2)).map(|t| t.0).collect()
    }
}

/// `FUN_801a1040`: the sine of `a` (4096 a turn) from main's table `sine`, × 4096.
pub fn sin(sine: &[i16], a: i32) -> i32 {
    let i = (a & 0x3FF) as usize;
    let v = if a & 0x400 != 0 { sine[1024 - i] } else { sine[i] } as i32;
    if a & 0x800 != 0 {
        -v
    } else {
        v
    }
}

/// `FUN_801a103c`.
pub fn cos(sine: &[i16], a: i32) -> i32 {
    sin(sine, a + 0x400)
}

/// `FUN_801a0efc(x, y, z)`: the rotation the smoke's quads turn by, as its handwritten code builds
/// it (each product shifted down on its own, as the code does).
pub fn turn(sine: &[i16], x: i32, y: i32, z: i32) -> [[i16; 3]; 3] {
    let (sx, cx, sy, cy, sz, cz) = (sin(sine, x), cos(sine, x), sin(sine, y), cos(sine, y), sin(sine, z), cos(sine, z));
    let (sxsz, sxcz) = ((sx * sz) >> 12, (sx * cz) >> 12);
    let m = [
        [((cy * cz) >> 12) + ((-sy * sxsz) >> 12), ((-cy * sz) >> 12) + ((-sy * sxcz) >> 12), -((sy * cx) >> 12)],
        [(cx * sz) >> 12, (cx * cz) >> 12, -sx],
        [((sy * cz) >> 12) + ((cy * sxsz) >> 12), ((-sy * sz) >> 12) + ((cy * sxcz) >> 12), (cy * cx) >> 12],
    ];
    m.map(|r| r.map(|v| v as i16))
}

/// The GTE's MVMVA (rotation `a`, `sf` = 1, `lm` = 0) on `v`: IR1–3.
fn mvmva(a: &[[i16; 3]; 3], v: [i16; 3]) -> [i32; 3] {
    [0, 1, 2].map(|r| ((0..3).map(|c| a[r][c] as i64 * v[c] as i64).sum::<i64>() >> 12).clamp(-0x8000, 0x7FFF) as i32)
}

/// `FUN_801a11cc(a, b)`: the GTE's rotation `a · b` (column by column) and translation
/// `a.t + a · b.t` (b.t as halfwords), as it leaves them.
pub fn compose(a: &Matrix, b: &[[i16; 3]; 3], bt: [i32; 3]) -> ([[i16; 3]; 3], [i32; 3]) {
    let mut r = [[0i16; 3]; 3];
    for j in 0..3 {
        let col = mvmva(&a.m, [b[0][j], b[1][j], b[2][j]]);
        for i in 0..3 {
            r[i][j] = col[i] as i16;
        }
    }
    let t = mvmva(&a.m, bt.map(|v| v as i16));
    (r, [0, 1, 2].map(|i| a.t[i].wrapping_add(t[i])))
}

/// The puff quad before its jitter (`Field_DrawTorchFlames`' `verts`).
const QUAD: [[i16; 3]; 4] = [[-32, -24, -32], [32, -24, -32], [-32, 0, 32], [32, 0, 32]];

/// One puff: its projection and packet words (a `POLY_FT4`, without its tag).
pub struct Puff {
    pub projection: Projection,
    pub words: Vec<u32>,
    pub bucket: u32,
}

/// The eight puffs of `e` at `phase` (the value `Field_DrawTorchFlames` reads after stepping it),
/// their positions going to prims from `prim` on (0x28 bytes each).
pub fn puffs(torch: &Torch, sine: &[i16], e: &Emitter, phase: u8, prim: u32) -> Vec<Puff> {
    let [rx, ry, rz] = torch.angles.map(i32::from);
    let m = turn(sine, -rx, -ry, rz);
    let [size, angle] = torch.drift.map(i32::from);
    let (ax, az, h) = ((sin(sine, angle) * size) >> 12, (cos(sine, angle) * size) >> 12, size >> 3);
    let mut ph = phase as i32;
    let mut m4 = ((torch.tick >> 2) + e.index as i32 * 4) % 16;
    if 8 < m4 {
        m4 = 16 - m4;
    }
    let m4 = m4 as i16;
    let jitter = [[-m4, -m4], [m4, -m4], [-m4, m4], [m4, m4]];
    let corners: [[i16; 3]; 4] = std::array::from_fn(|i| [QUAD[i][0] + jitter[i][0], QUAD[i][1] + jitter[i][1], QUAD[i][2]]);
    (0..8)
        .map(|k| {
            let t = (ph / 32).min(3);
            let (u, v) = (((t & 1) << 5) as u32, ((t / 2) << 5) as u32);
            let clut = match t {
                0 => ((0x1FA - ph / 8) << 6) | 4,
                3 => (((ph / 10).min(15) + 0x1F0) << 6) | 4,
                _ => 0x7DC4,
            } as u32;
            let s = sin(sine, ph * 8);
            let d = h / (8 - k);
            let pos = [e.at[0] as i32 + ((s * ax) >> 12), e.at[1] as i32 - ph - d, e.at[2] as i32 + ((s * az) >> 12)];
            ph += 16;
            let (rt, tr) = compose(&torch.view, &m, pos);
            let gte = Gte { rt, tr, ..torch.gte };
            let at = prim + 0x28 * k as u32;
            let p = Projection { caller: REBUILT, ..Projection::of(&gte, corners, [at + 8, at + 0x10, at + 0x18, at + 0x20], 0) };
            let xy: Vec<u32> = p.corners.iter().map(|c: &Corner| c.word).collect();
            let words = vec![0x2F80_8080, xy[0], (clut << 16) | ((v + 0x40) << 8) | (u + 0x80), xy[1], (0x3F << 16) | ((v + 0x40) << 8) | (u + 0x9F), xy[2], ((v + 0x5F) << 8) | (u + 0x80), xy[3], ((v + 0x5F) << 8) | (u + 0x9F)];
            let bucket = ((p.corners.iter().map(|c| c.z).min().unwrap_or(0) >> 2) as u32).max(3);
            Puff { projection: p, words, bucket }
        })
        .collect()
}

/// The puffs of every emitter `torch` didn't draw (its anchor in front of the camera, unsaturated
/// and within the game's lines), as projections and packets for a widened view: each packet before the first of
/// `packets` nearer than it.
pub fn rebuild(torches: &[Torch], sine: &[i16], packets: &[Linked]) -> (Vec<Projection>, Vec<(usize, Linked)>) {
    let mut projections = Vec::new();
    let mut out = Vec::new();
    let mut prim = REBUILT_PRIMS;
    for torch in torches {
        let drawn = torch.drawn();
        for e in torch.emitters.iter().filter(|e| !drawn.contains(&e.index)) {
            let (_, y, _, flags) = torch.gte.rtps(e.at);
            if flags & FAR_FLAGS != 0 || !(-0x28..0x12D).contains(&y) {
                continue;
            }
            let phase = (torch.tick & 15) as u8;
            for puff in puffs(torch, sine, e, phase, prim) {
                let after = packets.iter().position(|q| q.bucket < puff.bucket).unwrap_or(packets.len());
                out.push((after, Linked { addr: prim, bucket: puff.bucket, words: puff.words, aside: false, shadow: false }));
                projections.push(puff.projection);
                prim += 0x28;
            }
        }
    }
    out.sort_by_key(|(after, _)| *after);
    (projections, out)
}

/// The rebuild of the emitters `torch` drew against the game's own puffs (`projections`, the
/// table's from `Particle_BuildBillboardPrim`, in the order it made them): corners compared and
/// those the same.
pub fn verify(torch: &Torch, sine: &[i16], projections: &[&Projection]) -> (u64, u64) {
    let drawn = torch.drawn();
    let mut game = projections.iter();
    let (mut n, mut same) = (0, 0);
    for e in drawn.iter().filter_map(|i| torch.emitters.iter().find(|e| e.index == *i)) {
        // The game steps the phase before it builds the puffs.
        let phase = (e.phase + 1) % 16;
        for puff in puffs(torch, sine, e, phase, REBUILT_PRIMS) {
            let Some(g) = game.next() else { return (n, same) };
            for k in 0..4 {
                n += 1;
                same += u64::from(g.corners[k].word == puff.projection.corners[k].word);
            }
        }
    }
    (n, same)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine table like main's: sin over a quarter turn, × 4096.
    fn table() -> Vec<i16> {
        (0..SINE_LEN).map(|i| ((i as f64 / 1024.0 * std::f64::consts::FRAC_PI_2).sin() * 4096.0).round() as i16).collect()
    }

    #[test]
    fn sine_and_cosine_wrap_by_quadrant() {
        let t = table();
        assert_eq!((sin(&t, 0), sin(&t, 0x400), sin(&t, 0x800), sin(&t, 0xC00)), (0, 4096, 0, -4096));
        assert_eq!(cos(&t, 0), 4096);
        assert_eq!(sin(&t, 0x200), -sin(&t, 0xA00));
    }

    #[test]
    fn no_angles_turn_nothing_and_compose_keeps_the_view() {
        let t = table();
        let m = turn(&t, 0, 0, 0);
        assert_eq!(m, [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]]);
        let view = Matrix { m: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], t: [10, 20, 3000] };
        let (r, tr) = compose(&view, &m, [100, -50, 7]);
        assert_eq!(r, m);
        assert_eq!(tr, [110, -30, 3007]);
    }

    #[test]
    fn a_quarter_turn_about_x_matches_the_handwritten_code() {
        // x = 0x400: sx = 4096, cx = 0; the code's row 1 is (cx·sz, cx·cz, −sx).
        let m = turn(&table(), 0x400, 0, 0);
        assert_eq!(m[1], [0, 0, -4096]);
        assert_eq!(m[2][1], 4096);
    }

    #[test]
    fn puffs_rise_and_take_their_texture_by_phase() {
        let t = table();
        let torch = Torch {
            tick: 0,
            view: Matrix { m: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], t: [0, 0, 2000] },
            angles: [0, 0, 0],
            drift: [0, 0],
            wide: false,
            gte: Gte { rt: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], tr: [0, 0, 2000], h: 892, ofx: 160 << 16, ofy: 112 << 16, exact: None },
            emitters: vec![Emitter { index: 0, at: [0, 0, 0], phase: 0 }],
            tested: vec![],
        };
        let p = puffs(&torch, &t, &torch.emitters[0], 0, REBUILT_PRIMS);
        assert_eq!(p.len(), 8);
        // Each puff is 16 higher than the last (y up is negative): screen y falls.
        let ys: Vec<i32> = p.iter().map(|q| (q.projection.corners[0].word >> 16) as i16 as i32).collect();
        assert!(ys.windows(2).all(|w| w[1] < w[0]), "{ys:?}");
        // Phase 0: the first texture, CLUT row 0x1FA; the last puff (phase 112) the fourth.
        assert_eq!(p[0].words[2] >> 16, (0x1FA << 6) | 4);
        assert_eq!(p[0].words[2] & 0xFFFF, (0x40 << 8) | 0x80);
        assert_eq!(p[7].words[2] & 0xFFFF, (0x60 << 8) | 0xA0);
        assert_eq!(p[0].words[4] >> 16, 0x3F, "tpage");
        assert!(p.iter().all(|q| q.bucket >= 3));
    }

    #[test]
    fn an_emitter_the_game_drew_isnt_rebuilt() {
        let t = table();
        let gte = Gte { rt: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], tr: [0, 0, 2000], h: 892, ofx: 160 << 16, ofy: 112 << 16, exact: None };
        let torch = Torch {
            tick: 8,
            view: Matrix { m: gte.rt, t: gte.tr },
            angles: [0, 0, 0],
            drift: [0, 0],
            wide: false,
            gte,
            // Emitter 0 on screen and drawn; emitter 3 far to the right, past the game's test.
            emitters: vec![Emitter { index: 0, at: [0, 0, 0], phase: 0 }, Emitter { index: 3, at: [1200, 0, 0], phase: 0 }],
            tested: vec![(0, [160, 112], 0), (3, [695, 112], 0)],
        };
        assert_eq!(torch.drawn(), vec![0]);
        let (ps, packets) = rebuild(&[torch], &t, &[]);
        assert_eq!((ps.len(), packets.len()), (8, 8));
        assert!(ps.iter().all(|p| p.caller == REBUILT));
        assert!((ps[0].corners[0].float[0] - 695.0).abs() < 20.0);
    }
}
