//! The field's billboards (LT-144, research 16 §4.5 paths 4–7): the quads it places in 3D and
//! projects with libgte's `RotTransPers4` and `RotAverage4`. They are the Baby, its sparkle trail
//! and its shadow (`FUN_801821ac`), chimney smoke (`Field_DrawTorchFlames` through
//! `Particle_BuildBillboardPrim`), the transform sparkles in flight, the flight shadow, the
//! characters' blob shadows, dust and rain.
//!
//! The game builds each quad in RAM (its texture, CLUT and colour), and the projection writes its
//! four screen positions into the packet. The takeover notes each projection as the call starts:
//! the GTE's matrix and screen set-up, the four corners and where their positions go
//! ([`Projection`]). When the table goes to the GPU, a packet whose corners all came from one
//! projection is a billboard, and the 2D layer draws it:
//! - with sub-pixel corners, the same corners projected again in floating point, above 1×;
//! - in a widened view, unclipped by the game's 4:3 draw area, so it reaches into the margins;
//! - and, in a widened view, with the trail sparkles the game drops near the 4:3 edges put back
//!   ([`restore`]). `FUN_801821ac` links one only when its centre is inside x 20..341,
//!   y −10..281.
//!
//! A corner is the projection's only when the packet's word is the GTE's result for it. Most
//! packets are matched by the word's address. A quad copied from another is matched by value,
//! all its corners from one projection: the Baby's sprite takes its head's corners, maybe
//! mirrored.
//!
//! Smoke the game doesn't draw (emitters past its screen test or its three a tick) is rebuilt by
//! [`crate::smoke`] for a widened view, puff for puff as `Field_DrawTorchFlames` builds them.
//!
//! Between ticks (LT-145, [`between`]) each billboard is placed in the world from its view-space
//! corners, blended with tick A's when the two pair, and projected through the camera in between.

use std::collections::HashMap;

use lt_emu_hooks::Call;
use lt_scene::Matrix;

use crate::gte::Gte;
use crate::interp::{Pose, ACTOR_JUMP};
use crate::light::Affine;
use crate::takeover::Linked;

/// libgte's `RotTransPers4(v0, v1, v2, v3, sxy0, sxy1, sxy2, sxy3, p, flag)`.
pub const ROT_TRANS_PERS4: u32 = 0x801A_A118;
/// libgte's `RotAverage4`: the same arguments, and the average depth.
pub const ROT_AVERAGE4: u32 = 0x801A_A198;

/// The functions whose `RotTransPers4`/`RotAverage4` calls place billboards. Their other callers
/// (battle effects, the battle transition, OVR24's sky) stay the game's.
pub const CALLERS: [&str; 10] = [
    "FUN_801821ac",
    "Npc_DrawIfVisible",
    "Particle_BuildBillboardPrim",
    "BabyTransform_DrawSparkles",
    "func_801FB9E0",
    "Dust_DrawPuffs",
    "DustTrail_DrawParticles",
    "Weather_UpdateRainParticles",
    "func_801E2F84",
    "func_801E5330",
];

/// Where `FUN_801821ac`'s trail `RotAverage4` returns (its second call, at 0x80182FF8).
pub const TRAIL_RETURN: u32 = 0x8018_3000;
/// The flight shadow's function (`RotTransPers4` into the blob shadows' first slot, root mode 23):
/// not a character's blob shadow, so the lit look keeps it.
pub const FLIGHT_SHADOW: &str = "func_801FB9E0";

/// The GTE flags a billboard's game drops it on (`FUN_801821ac`, `Field_DrawTorchFlames`).
pub const GAME_FLAGS: u32 = 0x7FC5_8000;
/// The flags after which the floating-point corners would be far from the GTE's: those, the
/// divide overflowing (SZ under H / 2) and the screen position saturating.
pub const FAR_FLAGS: u32 = GAME_FLAGS | (1 << 17) | (1 << 14) | (1 << 13);

/// One corner as the GTE projected it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Corner {
    /// The screen position as the packet word holds it: y << 16 | x.
    pub word: u32,
    /// The screen position in floating point.
    pub float: [f32; 2],
    /// Its SZ.
    pub z: u16,
}

/// One `RotTransPers4`/`RotAverage4` call by a billboard's function.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projection {
    /// The function that made it ([`CALLERS`]; `crate::smoke::REBUILT` for a puff the game didn't
    /// draw).
    pub caller: &'static str,
    /// Where the call returns.
    pub ra: u32,
    /// Where each corner's screen position goes.
    pub at: [u32; 4],
    pub corners: [Corner; 4],
    /// The GTE's flags over the four (each corner's RTPS).
    pub flags: u32,
    /// Each corner in view space (`RT · v + TR`, before the divide), and the GTE's projection
    /// distance and screen offset (16.16): what a frame between ticks projects again through the
    /// camera in between (LT-145, [`between`]).
    pub view: [[f64; 3]; 4],
    pub h: u16,
    pub offset: [i32; 2],
}

struct Ram<'a, 'b>(&'a Call<'b>);

impl gc_sys::Memory for Ram<'_, '_> {
    fn read(&self, addr: u32, out: &mut [u8]) {
        out.copy_from_slice(&self.0.read(addr, out.len()));
    }
}

fn kseg0(a: u32) -> u32 {
    (a & 0x1FFF_FFFF) | 0x8000_0000
}

/// The function holding `ra`, as the game has its code now.
pub fn caller(c: &Call, ra: u32) -> Option<&'static str> {
    gc_sys::function_at(&Ram(c), ra).map(|(s, _)| s.name)
}

impl Projection {
    /// The call `c` (at `RotTransPers4` or `RotAverage4`'s entry), if a billboard's function made
    /// it.
    pub fn from_call(c: &Call) -> Option<Projection> {
        let ra = c.ra();
        let caller = caller(c, ra).filter(|n| CALLERS.contains(n))?;
        let g = c.cpu().cop2();
        let gte = Gte { h: g.read_control(26) as u16, ofx: g.read_control(24) as i32, ofy: g.read_control(25) as i32, ..Gte::new(&Matrix::from_gte(c.cpu()), 0, [0, 0]) };
        let verts = [0u8, 1, 2, 3].map(|i| {
            let b = c.read(c.arg(i), 6);
            [0, 1, 2].map(|k| i16::from_le_bytes([b[k * 2], b[k * 2 + 1]]))
        });
        let at = [4u8, 5, 6, 7].map(|i| kseg0(c.arg(i)));
        Some(Projection { caller, ..Projection::of(&gte, verts, at, ra) })
    }

    /// `verts` through `gte`, their positions going to `at`.
    pub fn of(gte: &Gte, verts: [[i16; 3]; 4], at: [u32; 4], ra: u32) -> Projection {
        let mut flags = 0;
        let corners = verts.map(|v| {
            let (x, y, z, f) = gte.rtps(v);
            flags |= f;
            let s = gte.float(v.map(f32::from));
            Corner { word: ((y as u16 as u32) << 16) | x as u16 as u32, float: [s.x, s.y], z }
        });
        let view = verts.map(|v| gte.view(v.map(f32::from)));
        Projection { caller: "", ra, at, corners, flags, view, h: gte.h, offset: [gte.ofx, gte.ofy] }
    }

    /// A view-space point on screen through this projection's set-up, in floating point.
    pub fn screen(&self, p: [f64; 3]) -> [f32; 2] {
        let g = Gte { rt: [[0; 3]; 3], tr: [0; 3], h: self.h, ofx: self.offset[0], ofy: self.offset[1], exact: None };
        let s = g.screen(p);
        [s.x, s.y]
    }

    /// The prim whose four positions this projection wrote, if it wrote a `POLY_FT4`'s: the
    /// positions at +8, +0x10, +0x18 and +0x20.
    pub fn ft4(&self) -> Option<u32> {
        let prim = self.at[0].wrapping_sub(8);
        (self.at == [prim + 8, prim + 0x10, prim + 0x18, prim + 0x20]).then_some(prim)
    }
}

/// The indices of a GP0 polygon command's position words (its words from the command word on),
/// or none for any other command.
pub fn position_words(opcode: u8) -> Option<Vec<usize>> {
    if !(0x20..0x40).contains(&opcode) {
        return None;
    }
    let (gouraud, quad, textured) = (opcode & 0x10 != 0, opcode & 0x08 != 0, opcode & 0x04 != 0);
    let mut out = Vec::with_capacity(4);
    let mut i = 1;
    for k in 0..if quad { 4 } else { 3 } {
        if gouraud && k > 0 {
            i += 1;
        }
        out.push(i);
        i += 1 + textured as usize;
    }
    Some(out)
}

/// A billboard command's corners: each position word, and its floating-point screen position.
pub type Corners = Vec<(u32, [f32; 2])>;

/// A table's projections, looked up by where their corners went and by value.
pub struct Lookup<'a> {
    projections: &'a [Projection],
    at: HashMap<u32, (usize, usize)>,
    by_value: HashMap<u32, Vec<(usize, usize)>>,
}

impl<'a> Lookup<'a> {
    /// The projections whose floating-point corners stay near the GTE's ([`FAR_FLAGS`] clear);
    /// a later projection to an address replaces an earlier one, as its writes did.
    pub fn new(projections: &'a [Projection]) -> Lookup<'a> {
        let mut at = HashMap::new();
        let mut by_value: HashMap<u32, Vec<(usize, usize)>> = HashMap::new();
        for (i, p) in projections.iter().enumerate() {
            if p.flags & FAR_FLAGS != 0 {
                for a in p.at {
                    at.remove(&a);
                }
                continue;
            }
            for (k, c) in p.corners.iter().enumerate() {
                at.insert(p.at[k], (i, k));
                by_value.entry(c.word).or_default().push((i, k));
            }
        }
        Lookup { projections, at, by_value }
    }

    pub fn is_empty(&self) -> bool {
        self.at.is_empty()
    }

    /// The corners of the command at word `at` of `packet` (`words` its words, from the command
    /// word), if it is a polygon whose every position came from one projection: that projection's
    /// caller, and each position word with its floating-point screen position.
    pub fn command(&self, packet: &Linked, at: usize, words: &[u32]) -> Option<(&'static str, Corners)> {
        let idx = position_words((words[0] >> 24) as u8)?;
        if idx.iter().any(|&i| i >= words.len()) {
            return None;
        }
        // By address: the projection that wrote each word last, with the word still its result.
        let mut caller = "";
        let by_addr: Option<Corners> = idx
            .iter()
            .map(|&i| {
                let addr = packet.addr + 4 + 4 * (at + i) as u32;
                let &(p, k) = self.at.get(&addr)?;
                let c = self.projections[p].corners[k];
                caller = self.projections[p].caller;
                (c.word == words[i]).then_some((c.word, c.float))
            })
            .collect();
        if let Some(c) = by_addr {
            return Some((caller, c));
        }
        // By value: every word one projection's corner.
        let first = self.by_value.get(&words[idx[0]])?;
        first.iter().find_map(|&(p, _)| {
            let c: Option<Corners> = idx
                .iter()
                .map(|&i| {
                    let k = self.by_value.get(&words[i])?.iter().find(|&&(q, _)| q == p)?.1;
                    Some((words[i], self.projections[p].corners[k].float))
                })
                .collect();
            c.map(|c| (self.projections[p].caller, c))
        })
    }
}

/// The trail sparkles `FUN_801821ac` projected this tick and didn't link (their centres outside
/// its bounds near the 4:3 edges), as packets for a widened view: the prim's words from RAM
/// (`read`), with the CLUT and bucket of a sparkle it did link. Each goes after that sparkle in
/// the table (`packets` its packets); with none linked, after the packets of its own bucket.
pub fn restore(projections: &[Projection], packets: &[Linked], read: &dyn Fn(u32, usize) -> Vec<u8>) -> Vec<(usize, Linked)> {
    let linked: HashMap<u32, usize> = packets.iter().enumerate().map(|(i, p)| (p.addr, i)).collect();
    // The game drops those with its flags set; those with the screen position saturated (far off
    // to a side) or too near couldn't be drawn where they are.
    let trail: Vec<&Projection> = projections.iter().filter(|p| p.ra == TRAIL_RETURN && p.flags & FAR_FLAGS == 0).collect();
    let sibling = trail.iter().filter_map(|p| linked.get(&p.ft4()?)).copied().max();
    let mut out = Vec::new();
    for p in trail {
        let Some(prim) = p.ft4() else { continue };
        if linked.contains_key(&prim) {
            continue;
        }
        let b = read(prim, 40);
        let w: Vec<u32> = b.as_chunks::<4>().0.iter().map(|&q| u32::from_le_bytes(q)).collect();
        // A POLY_FT4: nine words, command 0x2C–0x2F.
        if w[0] >> 24 != 9 || (w[1] >> 26) != 0x2C >> 2 {
            continue;
        }
        let mut words = w[1..].to_vec();
        let (bucket, after) = match sibling {
            Some(s) => {
                words[2] = (packets[s].words[2] & 0xFFFF_0000) | (words[2] & 0xFFFF);
                (packets[s].bucket, s + 1)
            }
            None => {
                let z = p.corners.iter().map(|c| c.z).min().unwrap_or(0) as u32 / 4;
                let bucket = z.clamp(2, 0xBFF);
                (bucket, packets.iter().position(|q| q.bucket < bucket).unwrap_or(packets.len()))
            }
        };
        out.push((after, Linked { addr: prim, bucket, words, aside: false, shadow: false }));
    }
    out.sort_by_key(|(after, _)| *after);
    out
}

/// Tick B's billboards for a frame between ticks A and B ([`between`]).
#[derive(Clone, Debug, Default)]
pub struct Moved {
    /// Tick B's projections, each with its floating-point corners where the frame puts them (its
    /// GTE words are B's: the packets are B's).
    pub projections: Vec<Projection>,
    /// Billboards blended with tick A's; those drawn where tick B has them in the world (tick A had
    /// none to pair: new this tick, or stamped where they stay); and those paired but too far apart
    /// in one tick (put elsewhere), drawn as tick B has them.
    pub paired: u32,
    pub held: u32,
    pub jumped: u32,
    /// Per projection of B: the projection of A it was blended with.
    pub with: Vec<Option<usize>>,
}

type World = ([[f64; 3]; 4], [f64; 3]);

fn far(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Each drawn projection's corners in B's world frame through `from` (its tick's view space to the
/// world), and their centre.
fn worlds(ps: &[Projection], from: &Affine) -> Vec<Option<World>> {
    ps.iter()
        .map(|p| {
            (p.flags & FAR_FLAGS == 0).then(|| {
                let w = p.view.map(|v| from.apply(v));
                let c = [0, 1, 2].map(|i| w.iter().map(|v| v[i]).sum::<f64>() / 4.0);
                (w, c)
            })
        })
        .collect()
}

/// The projection in `ps` with the same caller and return (the same call in the code) as `p` whose
/// centre is nearest `c`, and how far.
fn nearest(p: &Projection, c: [f64; 3], ps: &[Projection], ws: &[Option<World>]) -> Option<(usize, f64)> {
    ps.iter().zip(ws).enumerate().filter(|(_, (q, _))| q.caller == p.caller && q.ra == p.ra).filter_map(|(i, (_, w))| w.as_ref().map(|w| (i, far(c, w.1)))).min_by(|x, y| x.1.total_cmp(&y.1))
}

/// Tick B's billboards (`b`) as a frame `pose` between ticks A and B draws them (LT-145): each in
/// the world, seen through the camera in between.
///
/// A billboard is paired with tick A's when each is the other's nearest (by their centres in the
/// world) among the same call's projections: the Baby, its shadow, the characters' blob shadows,
/// the flight shadow and the transform sparkles move, and their corners are blended in the world.
/// Trail sparkles and smoke puffs stay or barely move, so each pairs with itself a tick before; one
/// new this tick has no partner (the one it's nearest pairs with another) and is drawn where tick B
/// has it. A pair further apart than [`crate::interp::ACTOR_JUMP`] is drawn as tick B has it too.
/// The packet words stay B's: only the floating-point corners move.
pub fn between(a: &[Projection], b: &[Projection], pose: &Pose) -> Moved {
    let (wa, wb) = (worlds(a, &pose.from_a), worlds(b, &pose.from_b));
    let mut out = Moved { projections: Vec::with_capacity(b.len()), ..Moved::default() };
    for (i, (p, w)) in b.iter().zip(&wb).enumerate() {
        out.with.push(None);
        let Some((corners, c)) = w else {
            out.projections.push(p.clone());
            continue;
        };
        let pair = nearest(p, *c, a, &wa).filter(|&(j, _)| wa[j].as_ref().and_then(|(_, ca)| nearest(&a[j], *ca, b, &wb)).is_some_and(|(k, _)| k == i));
        let world: [[f64; 3]; 4] = match pair {
            Some((j, d)) if d <= ACTOR_JUMP => {
                out.paired += 1;
                out.with[i] = Some(j);
                let from = &wa[j].as_ref().unwrap().0;
                [0, 1, 2, 3].map(|k| [0, 1, 2].map(|x| from[k][x] + (corners[k][x] - from[k][x]) * pose.t))
            }
            Some(_) => {
                out.jumped += 1;
                *corners
            }
            None => {
                out.held += 1;
                *corners
            }
        };
        let mut q = p.clone();
        for (k, v) in world.iter().enumerate() {
            let v = pose.view.apply(*v);
            if v[2] >= 1.0 {
                q.corners[k].float = p.screen(v);
            }
        }
        out.projections.push(q);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gte() -> Gte {
        Gte { rt: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], tr: [0, 0, 1000], h: 892, ofx: 160 << 16, ofy: 112 << 16, exact: None }
    }

    /// A pose between two ticks for the tests: the camera's eye moved by `eye` (world units) from
    /// tick A to tick B, looking down +z with no turn, `t` of the way.
    fn pose(eye: [f64; 3], t: f64) -> Pose {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        // View A: world − 0; view B: world − eye. Each view space back to the world.
        let (from_a, from_b) = (Affine { m: id, t: [0.0; 3] }, Affine { m: id, t: eye });
        let view = Affine { m: id, t: eye.map(|v| -v * t) };
        Pose { view, chunks: vec![], parts: vec![], morphed: 0, snapped_characters: 0, t, from_a, from_b }
    }

    /// A quad 32 across at `centre` in tick `eye`'s view space (the GTE at the origin, screen
    /// centre 160, 112), its prim at `prim`, from `caller`'s call returning to `ra`.
    fn quad(caller: &'static str, ra: u32, prim: u32, centre: [i16; 3], eye: [f64; 3]) -> Projection {
        let g = Gte { tr: [centre[0] as i32 - eye[0] as i32, centre[1] as i32 - eye[1] as i32, centre[2] as i32 - eye[2] as i32], ..gte() };
        Projection { caller, ..Projection::of(&g, [[-16, -16, 0], [16, -16, 0], [-16, 16, 0], [16, 16, 0]], [prim + 8, prim + 0x10, prim + 0x18, prim + 0x20], ra) }
    }

    fn centre(p: &Projection) -> [f32; 2] {
        [0, 1].map(|x| p.corners.iter().map(|c| c.float[x]).sum::<f32>() / 4.0)
    }

    /// @LT-145/1 (the billboards' part): halfway between two ticks, a billboard that stays in the
    /// world is where the camera in between sees it; one that moved is blended in the world; one
    /// new this tick is held where tick B has it; and at t = 0 and 1 they are where each tick drew
    /// them.
    #[test]
    fn lt145_1_billboards_in_the_world_through_the_camera_in_between() {
        let eye = [40.0, 0.0, 0.0];
        // A trail sparkle stamped at x 100 in both ticks; the Baby moving from x 0 to x 30; a
        // sparkle new in tick B at x 120. The double buffer: tick B's prims elsewhere.
        let a = vec![quad("FUN_801821ac", TRAIL_RETURN, 0x801C_E320, [100, 0, 1000], [0.0; 3]), quad("FUN_801821ac", 0x8018_2770, 0x801C_E2D0, [0, 0, 1000], [0.0; 3])];
        let b = vec![
            quad("FUN_801821ac", TRAIL_RETURN, 0x801C_E4B0, [100, 0, 1000], eye),
            quad("FUN_801821ac", 0x8018_2770, 0x801C_E460, [30, 0, 1000], eye),
            quad("FUN_801821ac", TRAIL_RETURN, 0x801C_E4D8, [120, 0, 1000], eye),
        ];
        let half = between(&a, &b, &pose(eye, 0.5));
        assert_eq!((half.paired, half.held, half.jumped), (2, 1, 0));
        assert_eq!(half.with, vec![Some(0), Some(1), None]);
        // Through the camera halfway (its eye at x 20): x 100 → 80 in view space, 892 / 1000 a unit.
        let at = |x: f32| 160.0 + 892.0 * x / 1000.0;
        let near = |p: [f32; 2], x: f32| (p[0] - at(x)).abs() < 1e-3 && (p[1] - 112.0).abs() < 1e-3;
        assert!(near(centre(&half.projections[0]), 100.0 - 20.0), "the stamped sparkle stays: {:?}", centre(&half.projections[0]));
        assert!(near(centre(&half.projections[1]), 15.0 - 20.0), "the Baby halfway in the world");
        assert!(near(centre(&half.projections[2]), 120.0 - 20.0), "the new sparkle where tick B has it");
        // The packet words stay tick B's.
        assert!(half.projections.iter().zip(&b).all(|(m, p)| m.corners.iter().zip(&p.corners).all(|(x, y)| x.word == y.word)));
        // At the ticks: as tick A drew the pair, as tick B drew everything.
        let start = between(&a, &b, &pose(eye, 0.0));
        for (k, (m, p)) in start.projections.iter().zip(&a).enumerate().take(2) {
            assert!(m.corners.iter().zip(&p.corners).all(|(x, y)| (x.float[0] - y.float[0]).abs() < 1e-3 && (x.float[1] - y.float[1]).abs() < 1e-3), "{k} at t = 0");
        }
        let end = between(&a, &b, &pose(eye, 1.0));
        assert!(end.projections.iter().zip(&b).all(|(m, p)| m.corners.iter().zip(&p.corners).all(|(x, y)| (x.float[0] - y.float[0]).abs() < 1e-3)));
    }

    /// @LT-145/2 (the billboards' part): a billboard put elsewhere between two ticks (further than
    /// a character can move in one) is drawn where tick B has it; another call's never pairs.
    #[test]
    fn lt145_2_a_billboard_put_elsewhere_is_drawn_where_tick_b_has_it() {
        let eye = [0.0; 3];
        let a = vec![quad("Npc_DrawIfVisible", 0x8018_A3B0, BLOB, [-400, 0, 1000], eye), quad("Npc_DrawIfVisible", 0x8018_A3B0, BLOB + 0x28, [600, 0, 3000], eye)];
        let b = vec![quad("Npc_DrawIfVisible", 0x8018_A3B0, BLOB + 0x118, [0, 0, 1000], eye), quad("Particle_BuildBillboardPrim", 0x8018_23A4, 0x801F_DEB0, [600, 0, 3000], eye)];
        let half = between(&a, &b, &pose(eye, 0.5));
        assert_eq!((half.paired, half.held, half.jumped), (0, 1, 1));
        assert_eq!(half.with, vec![None, None]);
        assert!(half.projections.iter().zip(&b).all(|(m, p)| m.corners == p.corners), "both where tick B drew them");
    }

    const BLOB: u32 = crate::takeover::BLOB_SHADOWS;

    #[test]
    fn polygon_position_words() {
        assert_eq!(position_words(0x2C), Some(vec![1, 3, 5, 7]), "POLY_FT4");
        assert_eq!(position_words(0x28), Some(vec![1, 2, 3, 4]), "POLY_F4");
        assert_eq!(position_words(0x3C), Some(vec![1, 4, 7, 10]), "POLY_GT4");
        assert_eq!(position_words(0x30), Some(vec![1, 3, 5]), "POLY_G3");
        assert_eq!(position_words(0x24), Some(vec![1, 3, 5]), "POLY_FT3");
        assert_eq!(position_words(0x64), None, "a sprite");
    }

    #[test]
    fn a_quad_is_matched_by_address_and_a_copy_by_value() {
        let prim = 0x801C_E2D0;
        let p = Projection::of(&gte(), [[-16, -16, 0], [16, -16, 0], [-16, 16, 0], [16, 16, 0]], [prim + 8, prim + 0x10, prim + 0x18, prim + 0x20], TRAIL_RETURN);
        assert_eq!(p.flags & FAR_FLAGS, 0);
        assert_eq!(p.ft4(), Some(prim));
        // 892 / 1000 × 16 = 14.272: the GTE floors to whole pixels (112 − 14.272 → 97), the float
        // keeps the fraction.
        assert_eq!(p.corners[1].word, (97 << 16) | 174);
        assert!((p.corners[1].float[0] - 174.272).abs() < 1e-3);
        let words = |c: [usize; 4]| vec![0x2C80_8080, p.corners[c[0]].word, 0x7F83_0000, p.corners[c[1]].word, 0x003F_0000, p.corners[c[2]].word, 0, p.corners[c[3]].word, 0];
        let projections = [p.clone()];
        let look = Lookup::new(&projections);
        let at = Linked { addr: prim, bucket: 100, words: words([0, 1, 2, 3]), aside: false, shadow: false };
        let (_, got) = look.command(&at, 0, &at.words).expect("matched by address");
        assert_eq!(got.iter().map(|g| g.1).collect::<Vec<_>>(), p.corners.iter().map(|c| c.float).collect::<Vec<_>>());
        // The copy elsewhere, mirrored: corners 1, 0, 3, 2.
        let copy = Linked { addr: prim + 0x28, words: words([1, 0, 3, 2]), ..at.clone() };
        let (_, got) = look.command(&copy, 0, &copy.words).expect("matched by value");
        assert_eq!(got[0].1, p.corners[1].float);
        // A packet with one corner moved isn't a billboard.
        let mut moved = at.clone();
        moved.words[3] += 1;
        assert!(look.command(&moved, 0, &moved.words).is_none());
    }

    #[test]
    fn a_corner_behind_the_camera_keeps_the_gtes_positions() {
        let p = Projection::of(&gte(), [[0, 0, -1500], [16, 0, 0], [0, 16, 0], [16, 16, 0]], [8, 0x10, 0x18, 0x20], 0);
        assert_ne!(p.flags & FAR_FLAGS, 0);
        assert!(Lookup::new(&[p]).is_empty());
    }

    #[test]
    fn an_unlinked_trail_sparkle_comes_back_with_a_linked_ones_clut_and_bucket() {
        let (a, b) = (0x801C_E320, 0x801C_E348);
        let quad = [[-16, -16, 0], [16, -16, 0], [-16, 16, 0], [16, 16, 0]];
        let ps: Vec<Projection> = [a, b].iter().map(|&prim| Projection::of(&gte(), quad, [prim + 8, prim + 0x10, prim + 0x18, prim + 0x20], TRAIL_RETURN)).collect();
        let linked = Linked { addr: a, bucket: 230, words: vec![0x2E80_8080, 0, 0x7C42_1020, 0, 0x003F_0000, 0, 0, 0, 0], aside: false, shadow: false };
        let other = Linked { addr: 0x8010_0000, bucket: 10, words: vec![0], aside: false, shadow: false };
        let ram = |addr: u32, n: usize| -> Vec<u8> {
            assert_eq!((addr, n), (b, 40));
            [0x0900_0000u32, 0x2E70_7070, 0, 0x7C02_1030, 0, 0x003F_0000, 0, 0, 0, 0].iter().flat_map(|w| w.to_le_bytes()).collect()
        };
        let back = restore(&ps, &[linked, other], &ram);
        assert_eq!(back.len(), 1);
        let (after, p) = &back[0];
        assert_eq!((*after, p.addr, p.bucket), (1, b, 230));
        assert_eq!(p.words[2], 0x7C42_1030, "the linked sparkle's CLUT, its own UV");
    }
}
