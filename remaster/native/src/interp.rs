//! Frames between game ticks (LT-113, research 16 §6.4).
//!
//! The game ticks at 30 a second (`VSync(2)`), and its timers count ticks, so its logic stays at 30.
//! The native view draws at the display's rate by blending the last two ticks, one tick behind:
//! frame `t` of the way from tick A to tick B.
//!
//! **In world space**, as SoH does, so a turning character doesn't flatten like paper:
//! - **The camera:** its rotation slerped (the 512 mode's 1.6 x-stretch kept apart, as row scales),
//!   its eye lerped.
//! - **Chunks** don't move: B's place in the world, seen through the camera in between. A chunk group
//!   either tick drew is drawn, so nothing pops in at the screen's edge.
//! - **Character parts:** each part's world rotation slerped (its scale kept apart, as column
//!   scales), its position lerped, and its posed vertices lerped one by one: a character's vertex
//!   count is the same from tick to tick, so its morph animation blends too.
//!
//! **The two ticks' world frames.** The game's world is the current chunk's: crossing into a
//! neighbour moves every position by whole chunks. A chunk both ticks drew gives the shift, so walking
//! across seams blends like anything else.
//!
//! **Snaps** (B as it is, no frame in between):
//! - no chunk in common (a warp, a door into another area);
//! - another mode, or a tick with no camera;
//! - the camera's eye moving more than [`EYE_CUT`] or turning more than [`TURN_CUT`] in one tick (a
//!   cutscene cut);
//! - a character moving more than [`ACTOR_JUMP`] (a character placed elsewhere): that character only.
//!
//! The emulated 2D layer stays at 30: every frame takes it from tick B.

use lt_assets::Frame;
use lt_scene::Record;

use crate::light::{Affine, V3};

/// An eye moving further than this in one tick is a cut (world units). Walking moves it about 30.
pub const EYE_CUT: f64 = 768.0;
/// A camera turning more than this in one tick is a cut (degrees).
pub const TURN_CUT: f64 = 45.0;
/// A character moving further than this in one tick was put elsewhere (world units).
pub const ACTOR_JUMP: f64 = 256.0;

/// What a frame between two ticks draws: tick B's draws, each with its transform for the frame.
#[derive(Clone, Debug)]
pub struct Pose {
    /// The camera: world to view.
    pub view: Affine,
    /// Per `frame.chunks[i]` of tick B: model to view, and the groups to draw (either tick's).
    pub chunks: Vec<(Affine, Vec<u32>)>,
    /// Per `frame.characters[i]` of tick B, per part: model to view, and its posed vertices.
    pub parts: Vec<Vec<(Affine, Vec<V3>)>>,
    /// Parts whose vertices were blended, and characters drawn at B because they jumped or their
    /// parts didn't match.
    pub morphed: u32,
    pub snapped_characters: u32,
}

/// Why a frame snaps to tick B.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    NoCamera,
    /// Another mode (`ctx+0x43`) or screen width.
    Mode,
    /// No chunk both ticks drew: a warp or a door.
    NoSharedChunk,
    /// The camera jumped or turned too far for one tick.
    CameraCut,
}

#[derive(Clone, Debug)]
pub enum Between {
    Pose(Pose),
    Snap(Snap),
}

type M3 = [[f64; 3]; 3];
type Q = [f64; 4];

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}
fn len3(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}
fn apply(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// A rotation matrix (orthonormal, det +1) as a unit quaternion (w, x, y, z).
fn quat(m: &M3) -> Q {
    let tr = m[0][0] + m[1][1] + m[2][2];
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [0.25 * s, (m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [(m[2][1] - m[1][2]) / s, 0.25 * s, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [(m[0][2] - m[2][0]) / s, (m[0][1] + m[1][0]) / s, 0.25 * s, (m[1][2] + m[2][1]) / s]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [(m[1][0] - m[0][1]) / s, (m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, 0.25 * s]
    };
    let l = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    q.map(|v| v / l)
}

fn matrix(q: Q) -> M3 {
    let [w, x, y, z] = q;
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - w * z), 2.0 * (x * z + w * y)],
        [2.0 * (x * y + w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - w * x)],
        [2.0 * (x * z - w * y), 2.0 * (y * z + w * x), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

fn slerp(a: Q, mut b: Q, t: f64) -> Q {
    let mut d: f64 = (0..4).map(|i| a[i] * b[i]).sum();
    if d < 0.0 {
        b = b.map(|v| -v);
        d = -d;
    }
    let q = if d > 0.9995 {
        [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t)
    } else {
        let th = d.acos();
        let (sa, sb) = (((1.0 - t) * th).sin() / th.sin(), (t * th).sin() / th.sin());
        [0, 1, 2, 3].map(|i| a[i] * sa + b[i] * sb)
    };
    let l = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    q.map(|v| v / l)
}

/// The angle between two rotations (degrees).
fn turn(a: Q, b: Q) -> f64 {
    let d: f64 = (0..4).map(|i| a[i] * b[i]).sum::<f64>().abs().min(1.0);
    2.0 * d.acos().to_degrees()
}

/// `m` as scales times a rotation: by rows (`diag(s) · R`, a view with its stretch) or by columns
/// (`R · diag(s)`, a scaled model). `None` when what's left isn't a rotation.
fn split(m: &M3, rows: bool) -> Option<([f64; 3], M3)> {
    let s: [f64; 3] = if rows { [0, 1, 2].map(|i| len3(m[i])) } else { [0, 1, 2].map(|j| len3([m[0][j], m[1][j], m[2][j]])) };
    if s.iter().any(|&v| v < 1e-6) {
        return None;
    }
    let r: M3 = if rows { [0, 1, 2].map(|i| m[i].map(|v| v / s[i])) } else { [0, 1, 2].map(|i| [0, 1, 2].map(|j| m[i][j] / s[j])) };
    // Orthonormal (R · Rᵀ ≈ I) with det +1.
    let rt = [0, 1, 2].map(|i| [0, 1, 2].map(|j| r[j][i]));
    let i3 = mul(&r, &rt);
    let off = (0..3).flat_map(|i| (0..3).map(move |j| (i, j))).map(|(i, j)| (i3[i][j] - if i == j { 1.0 } else { 0.0 }).abs()).fold(0.0, f64::max);
    let det = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1]) - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0]) + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
    (off < 0.05 && det > 0.0).then_some((s, r))
}

/// `a` to `b` at `t`: scales lerped, rotation slerped (elementwise when either isn't a rotation).
fn blend(a: &M3, b: &M3, t: f64, rows: bool) -> M3 {
    match (split(a, rows), split(b, rows)) {
        (Some((sa, ra)), Some((sb, rb))) => {
            let r = matrix(slerp(quat(&ra), quat(&rb), t));
            let s = lerp3(sa, sb, t);
            if rows {
                [0, 1, 2].map(|i| r[i].map(|v| v * s[i]))
            } else {
                [0, 1, 2].map(|i| [0, 1, 2].map(|j| r[i][j] * s[j]))
            }
        }
        _ => [0, 1, 2].map(|i| [0, 1, 2].map(|j| a[i][j] + (b[i][j] - a[i][j]) * t)),
    }
}

/// The frame `t` (0 = tick A, 1 = tick B) of the way from tick A to tick B.
pub fn between(a: (&Record, &Frame), b: (&Record, &Frame), t: f64) -> Between {
    let ((ra, fa), (rb, fb)) = (a, b);
    let (Some(ca), Some(cb)) = (ra.camera.as_ref(), rb.camera.as_ref()) else { return Between::Snap(Snap::NoCamera) };
    if ra.mode != rb.mode || ca.wide != cb.wide {
        return Between::Snap(Snap::Mode);
    }
    let (va, vb) = (Affine::of(&ca.view), Affine::of(&cb.view));
    let (Some(ia), Some(ib)) = (va.inverse(), vb.inverse()) else { return Between::Snap(Snap::NoCamera) };

    // The shift from A's world frame to B's, from a chunk both drew (the current one if it can).
    let pairs = || fb.chunks.iter().filter_map(|c| fa.chunks.iter().find(|d| d.chunk == c.chunk).map(|d| (c, d)));
    let Some((cb_, ca_)) = pairs().find(|(c, _)| c.chunk == rb.chunk).or_else(|| pairs().next()) else { return Between::Snap(Snap::NoSharedChunk) };
    let shift = sub3(ib.after(&Affine::of(&cb_.model_view)).t, ia.after(&Affine::of(&ca_.model_view)).t);

    // The camera: its eye (where the inverse view puts the origin) and its rotation.
    let (eye_a, eye_b) = (add3(ia.t, shift), ib.t);
    if len3(sub3(eye_b, eye_a)) > EYE_CUT {
        return Between::Snap(Snap::CameraCut);
    }
    if let (Some((_, qa)), Some((_, qb))) = (split(&va.m, true), split(&vb.m, true)) {
        if turn(quat(&qa), quat(&qb)) > TURN_CUT {
            return Between::Snap(Snap::CameraCut);
        }
    }
    let m = blend(&va.m, &vb.m, t, true);
    let eye = lerp3(eye_a, eye_b, t);
    let view = Affine { m, t: apply(&m, eye).map(|v| -v) };

    let chunks = fb
        .chunks
        .iter()
        .map(|c| {
            let mut groups = c.groups.clone();
            if let Some(d) = fa.chunks.iter().find(|d| d.chunk == c.chunk) {
                groups.extend(d.groups.iter().copied());
                groups.sort_unstable();
                groups.dedup();
            }
            (view.after(&ib.after(&Affine::of(&c.model_view))), groups)
        })
        .collect();

    let (mut morphed, mut snapped) = (0, 0);
    let parts = fb
        .characters
        .iter()
        .map(|cb| {
            let at_b = |p: &lt_assets::PartUse| (view.after(&ib.after(&Affine::of(&p.model_view))), p.vertices.iter().map(|v| v.map(|x| x as f32)).collect::<Vec<V3>>());
            let ca = fa.characters.iter().find(|c| c.index == cb.index && c.mesh == cb.mesh && c.parts.len() == cb.parts.len() && c.parts.iter().zip(&cb.parts).all(|(x, y)| x.object == y.object));
            let Some(ca) = ca else { return cb.parts.iter().map(at_b).collect() };
            let worlds: Vec<(Affine, Affine)> = ca
                .parts
                .iter()
                .zip(&cb.parts)
                .map(|(pa, pb)| {
                    let mut wa = ia.after(&Affine::of(&pa.model_view));
                    wa.t = add3(wa.t, shift);
                    (wa, ib.after(&Affine::of(&pb.model_view)))
                })
                .collect();
            if worlds.iter().any(|(wa, wb)| len3(sub3(wb.t, wa.t)) > ACTOR_JUMP) {
                snapped += 1;
                return cb.parts.iter().map(at_b).collect();
            }
            ca.parts
                .iter()
                .zip(&cb.parts)
                .zip(worlds)
                .map(|((pa, pb), (wa, wb))| {
                    let w = Affine { m: blend(&wa.m, &wb.m, t, false), t: lerp3(wa.t, wb.t, t) };
                    let verts: Vec<V3> = if pa.vertices.len() == pb.vertices.len() {
                        morphed += 1;
                        pa.vertices.iter().zip(&pb.vertices).map(|(x, y)| [0, 1, 2].map(|i| x[i] as f32 + (y[i] as f32 - x[i] as f32) * t as f32)).collect()
                    } else {
                        pb.vertices.iter().map(|v| v.map(|x| x as f32)).collect()
                    };
                    (view.after(&w), verts)
                })
                .collect()
        })
        .collect();
    Between::Pose(Pose { view, chunks, parts, morphed, snapped_characters: snapped })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rot_y(deg: f64) -> M3 {
        let (s, c) = deg.to_radians().sin_cos();
        [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
    }

    #[test]
    fn slerp_halves_a_turn_and_keeps_scales_apart() {
        let a = rot_y(0.0);
        let b = rot_y(40.0);
        let mid = blend(&a, &b, 0.5, false);
        let want = rot_y(20.0);
        assert!((0..3).all(|i| (0..3).all(|j| (mid[i][j] - want[i][j]).abs() < 1e-9)), "{mid:?}");
        // A view with the 512 mode's stretch on its first row keeps it, and turns.
        let stretch = |m: M3| [m[0].map(|v| v * 1.6), m[1], m[2]];
        let v = blend(&stretch(a), &stretch(b), 0.5, true);
        let w = stretch(want);
        assert!((0..3).all(|i| (0..3).all(|j| (v[i][j] - w[i][j]).abs() < 1e-9)), "{v:?}");
        assert!((turn(quat(&a), quat(&b)) - 40.0).abs() < 1e-6);
    }

    #[test]
    fn quaternions_round_trip() {
        for m in [rot_y(0.0), rot_y(170.0), [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]], [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]]] {
            let back = matrix(quat(&m));
            assert!((0..3).all(|i| (0..3).all(|j| (back[i][j] - m[i][j]).abs() < 1e-9)), "{m:?} → {back:?}");
        }
    }
}
