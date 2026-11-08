//! What the field camera sees of the Knight (screen 23's Game camera, phase 5).
//!
//! The field camera follows the Knight: `FUN_8019f88c` (`scene::world::camera_task`) turns the
//! world by R = Rz·Rx(pitch)·Ry(−yaw) about him and pushes it `distance` away, so the eye always
//! sits at the same offset from him. A floor cell hides him when a polygon the game would draw
//! (its screen winding positive: NCLIP, as `otSort` keeps them) lies between his head there and
//! that eye. Polygons facing away, a room's near walls among them, are culled and hide nothing.

use serde::Serialize;

use super::chunk::Chunk;
use crate::world::walk::Walk;

/// The field camera's angles and distances (`D_801CDBD8`), in the game's units (0x1000 = a turn).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldCamera {
    /// Rx: 0x200 looks 45° down.
    pub pitch: i16,
    /// Ry: 0 looks north (+z), and it grows toward the east; the towns' camera is −0x200 (north-west),
    /// and the turn carries through doors and warps (retail's interiors set 0).
    pub yaw: i16,
    /// The view's z offset (`f10`): how far the eye sits from the Knight.
    pub distance: i16,
    /// The view's y offset (`fe`).
    pub lift: i16,
    /// The projection distance (SetGeomScreen).
    pub h: i16,
}

impl FieldCamera {
    /// The camera at its defaults (`scene::world::World::new`), turned to `yaw`.
    pub const fn at(yaw: i16) -> Self {
        FieldCamera { pitch: 0x200, yaw, distance: 0x7D0, lift: 0x14, h: 0x37C }
    }

    /// R = Rz(0)·Rx(pitch)·Ry(−yaw): rows are the camera's right, down and forward axes.
    pub fn rotation(&self) -> [[f32; 3]; 3] {
        let a = |u: i16| u as f32 * std::f32::consts::PI / 2048.0;
        let (sx, cx) = a(self.pitch).sin_cos();
        let (sy, cy) = a(self.yaw).sin_cos();
        [[cy, 0.0, -sy], [-sy * sx, cx, -cy * sx], [sy * cx, sx, cy * cx]]
    }

    /// From the point the camera follows to the eye: −Rᵀ·(0, lift, distance).
    pub fn eye_offset(&self) -> [f32; 3] {
        let r = self.rotation();
        let t = [0.0, self.lift as f32, self.distance as f32];
        std::array::from_fn(|k| -(r[0][k] * t[0] + r[1][k] * t[1] + r[2][k] * t[2]))
    }
}

/// How far above the floor the sight line starts: the top of the Knight's head (the field model
/// stands about 90 units tall; a little below its crown, so a hat brim doesn't count).
pub const HEAD: f32 = 80.0;

/// A floor cell where the camera can't see the Knight's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hidden {
    pub col: u8,
    pub row: u8,
    /// The draw group of the nearest polygon in the way.
    pub group: u32,
}

/// The floor cells of `c` within cells `within` (col0, row0, col1, row1, inclusive): how many the
/// Knight can stand on, and the ones where he would stand out of the camera's sight.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sight {
    pub floor: u32,
    pub hidden: Vec<Hidden>,
}

/// The whole chunk.
pub const EVERYWHERE: [u8; 4] = [0, 0, 127, 127];

pub fn hidden(c: &Chunk, cam: &FieldCamera, within: [u8; 4]) -> Sight {
    let walk = Walk::new(&c.heights, false, &[]);
    let off = cam.eye_offset();
    let v = |i: u16| c.pool.get(i as usize).map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]);
    // Triangles (a quad's two, wound as its first three corners), each with its group and its box.
    let mut tris: Vec<([[f32; 3]; 3], u32, [f32; 6])> = Vec::new();
    for (g, group) in c.groups.iter().enumerate() {
        for p in &group.prims {
            let n = p.n as usize;
            let corners: Vec<[f32; 3]> = p.idx[..n.min(4)].iter().filter_map(|&i| v(i)).collect();
            if corners.len() < 3 {
                continue;
            }
            let mut push = |t: [[f32; 3]; 3]| {
                let lo = |k: usize| t.iter().map(|q| q[k]).fold(f32::MAX, f32::min);
                let hi = |k: usize| t.iter().map(|q| q[k]).fold(f32::MIN, f32::max);
                tris.push((t, g as u32, [lo(0), lo(1), lo(2), hi(0), hi(1), hi(2)]));
            };
            push([corners[0], corners[1], corners[2]]);
            if corners.len() == 4 {
                push([corners[1], corners[3], corners[2]]);
            }
        }
    }
    let mut out = Sight::default();
    for row in within[1] as u32..=(within[3] as u32).min(127) {
        for col in within[0] as u32..=(within[2] as u32).min(127) {
            let (x, z) = (col as i32 * 32 - 2048 + 16, row as i32 * 32 - 2048 + 16);
            if !walk.can_stand(x, z) {
                continue;
            }
            let Ok(h) = walk.stand(x as i16, z as i16) else { continue };
            out.floor += 1;
            let head = [x as f32, h as f32 - HEAD, z as f32];
            let eye = [x as f32 + off[0], h as f32 + off[1], z as f32 + off[2]];
            let d = sub(eye, head);
            let (blo, bhi) = (std::array::from_fn::<f32, 3, _>(|k| head[k].min(eye[k])), std::array::from_fn::<f32, 3, _>(|k| head[k].max(eye[k])));
            let mut best: Option<(f32, u32)> = None;
            for (t, g, b) in &tris {
                if (0..3).any(|k| b[k + 3] < blo[k] || b[k] > bhi[k]) {
                    continue;
                }
                // Drawn only when it winds forwards on screen, seen from this eye.
                let nrm = cross(sub(t[1], t[0]), sub(t[2], t[0]));
                if dot(nrm, sub(t[0], eye)) <= 0.0 {
                    continue;
                }
                if let Some(s) = segment_hits(head, d, t) {
                    if best.is_none_or(|(b, _)| s < b) {
                        best = Some((s, *g));
                    }
                }
            }
            if let Some((_, group)) = best {
                out.hidden.push(Hidden { col: col as u8, row: row as u8, group });
            }
        }
    }
    out
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Where along `o + s·d` (0 < s < 1) the segment crosses triangle `t` (Möller–Trumbore).
fn segment_hits(o: [f32; 3], d: [f32; 3], t: &[[f32; 3]; 3]) -> Option<f32> {
    let (e1, e2) = (sub(t[1], t[0]), sub(t[2], t[0]));
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-6 {
        return None;
    }
    let inv = 1.0 / det;
    let q = sub(o, t[0]);
    let u = dot(q, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qe = cross(q, e1);
    let w = dot(d, qe) * inv;
    if w < 0.0 || u + w > 1.0 {
        return None;
    }
    let s = dot(e2, qe) * inv;
    (s > 1e-4 && s < 1.0).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Prim;

    /// A flat floor over the whole chunk (walk height −129, as a new area's), and nothing else.
    fn floor() -> Chunk {
        let mut c = Chunk::blank(1328);
        c.heights = vec![-129; 128 * 128];
        c
    }

    fn quad(c: &mut Chunk, corners: [[i16; 3]; 4]) {
        let base = c.pool.len() as u16;
        c.pool.extend(corners);
        let prim = Prim { list: 2, code: 0x28, n: 4, idx: [base, base + 1, base + 2, base + 3], rgb: [[128; 3]; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
        c.groups.push(super::super::chunk::Group { centre: [0; 3], prims: vec![prim] });
    }

    #[test]
    fn the_eye_sits_up_and_behind_the_knight() {
        let off = FieldCamera::at(0).eye_offset();
        // Looking north and 45° down: the eye is south (−z) and up (−y), 2000 or so away.
        assert!(off[0].abs() < 1.0, "{off:?}");
        assert!(off[1] < -1300.0 && off[2] < -1300.0, "{off:?}");
        let turned = FieldCamera::at(0x400).eye_offset();
        assert!(turned[0] < -1300.0 && turned[2].abs() < 1.0, "{turned:?}");
    }

    #[test]
    fn a_wall_facing_the_camera_hides_the_cells_north_of_it() {
        // A wall across z = 0, x −256…256, 272 high: wound to wind forwards seen from the south, where the camera is.
        let mut c = floor();
        quad(&mut c, [[-256, -400, 0], [256, -400, 0], [-256, -128, 0], [256, -128, 0]]);
        let cam = FieldCamera::at(0);
        let hid = hidden(&c, &cam, EVERYWHERE).hidden;
        let at = |x: i32, z: i32| hid.iter().any(|h| h.col as i32 == (x + 2048) / 32 && h.row as i32 == (z + 2048) / 32);
        assert!(at(0, 48), "just north of the wall");
        assert!(!at(0, -48), "south of the wall, the camera's side");
        assert!(!at(0, 400), "far enough north that the sight line clears its top");
        assert!(!at(400, 48), "beside it");
        assert!(hid.iter().all(|h| h.group == 0));
    }

    #[test]
    fn a_wall_facing_away_is_culled_and_hides_nothing() {
        let mut c = floor();
        quad(&mut c, [[256, -400, 0], [-256, -400, 0], [256, -128, 0], [-256, -128, 0]]);
        let s = hidden(&c, &FieldCamera::at(0), EVERYWHERE);
        assert!(s.hidden.is_empty());
        assert!(s.floor > 10_000, "{}", s.floor);
    }
}
