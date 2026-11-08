//! Walk heights from geometry (screen 10, PRD 10 §5.2 "bake heights from
//! mesh"). Layer A is a 128×128 grid of 32-unit cells, `cell = (coord +
//! 0x800) >> 5`, sampled with no interpolation (`GetTerrainHeightAt`).
//! [`surfaces`] finds the polygons under each cell centre, [`bake`] turns
//! them into heights, and [`rebake_changed`] applies that only where an
//! edit changed the geometry.

use crate::world::Prim;

use super::chunk::{Chunk, CELLS};

/// A polygon crossing a cell centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    /// Height of the polygon at the cell centre (PSX Y, down is positive).
    pub y: f32,
    /// |normal.y| / |normal|: 1 = level floor, 0 = wall.
    pub flat: f32,
    /// The normal's Y points up (towards −Y) for the stored vertex order.
    pub up: bool,
}

/// Centre of cell `(col, row)` in chunk-local units.
pub fn cell_centre(col: usize, row: usize) -> (f32, f32) {
    (col as f32 * 32.0 - 2048.0 + 16.0, row as f32 * 32.0 - 2048.0 + 16.0)
}

/// The triangles of a prim (quads in PSX strip order: 0-1-2 and 1-3-2).
fn triangles(p: &Prim) -> impl Iterator<Item = [u16; 3]> + '_ {
    let quad = p.n == 4;
    [[p.idx[0], p.idx[1], p.idx[2]], [p.idx[1], p.idx[3], p.idx[2]]].into_iter().take(if quad { 2 } else { 1 })
}

/// Every surface under every cell centre, row-major (`row·128 + col`).
pub fn surfaces(c: &Chunk) -> Vec<Vec<Surface>> {
    let mut out: Vec<Vec<Surface>> = vec![Vec::new(); CELLS];
    for g in &c.groups {
        for p in &g.prims {
            for t in triangles(p) {
                let v = t.map(|i| c.pool[i as usize].map(|a| a as f32));
                rasterise(&v, &mut out);
            }
        }
    }
    out
}

fn rasterise(v: &[[f32; 3]; 3], out: &mut [Vec<Surface>]) {
    let (a, b, c) = (v[0], v[1], v[2]);
    // Normal of (b − a) × (c − a) in x, y, z.
    let (u, w) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
    let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    // Signed area in the XZ plane; zero means the triangle stands vertical.
    let area = (b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2]);
    if len == 0.0 || area.abs() < 1e-3 {
        return;
    }
    let flat = n[1].abs() / len;
    let up = n[1] < 0.0;
    let to_cell = |x: f32| ((x + 2048.0 - 16.0) / 32.0).ceil().max(0.0) as i32;
    let to_cell_hi = |x: f32| (((x + 2048.0 - 16.0) / 32.0).floor() as i32).min(127);
    let (minx, maxx) = (a[0].min(b[0]).min(c[0]), a[0].max(b[0]).max(c[0]));
    let (minz, maxz) = (a[2].min(b[2]).min(c[2]), a[2].max(b[2]).max(c[2]));
    for row in to_cell(minz)..=to_cell_hi(maxz) {
        for col in to_cell(minx)..=to_cell_hi(maxx) {
            let (x, z) = cell_centre(col as usize, row as usize);
            // Barycentric weights in XZ; edges count (a centre on a shared edge belongs to both).
            let w1 = ((b[0] - x) * (c[2] - z) - (c[0] - x) * (b[2] - z)) / area;
            let w2 = ((c[0] - x) * (a[2] - z) - (a[0] - x) * (c[2] - z)) / area;
            let w3 = 1.0 - w1 - w2;
            const E: f32 = -1e-4;
            if w1 >= E && w2 >= E && w3 >= E {
                let y = w1 * a[1] + w2 * b[1] + w3 * c[1];
                out[row as usize * 128 + col as usize].push(Surface { y, flat, up });
            }
        }
    }
}

/// Polygons count as floor when |normal.y| / |normal| ≥ this (about 45°).
pub const FLOOR_FLAT: f32 = 0.7;

/// Walk heights from geometry, the rule Tamsoft's tool used as far as all
/// 524 chunks show (`tests/chunks.rs`): the top floor polygon under the cell
/// centre, minus 1, and 0 where no floor crosses the centre. A floor at
/// y = −128 gives −129; furniture tops give their height − 1, so the Knight
/// can't step onto them (> 0x20), and anything over 0x60 is a wall he slides
/// along. Sea level is y = 0, which gives −1, the "blocked" value.
///
/// What it doesn't reproduce is hand authoring: unreachable plateaus Tamsoft
/// set to −1, and objects whose top face isn't modelled.
pub fn bake(c: &Chunk) -> Vec<i16> {
    surfaces(c)
        .iter()
        .map(|cell| {
            let top = cell.iter().filter(|f| f.flat >= FLOOR_FLAT).map(|f| f.y).fold(f32::INFINITY, f32::min);
            if top.is_finite() {
                (top.round() as i32 - 1).clamp(i16::MIN as i32, i16::MAX as i32) as i16
            } else {
                0
            }
        })
        .collect()
}

/// Re-bakes only the cells whose baked height differs between `old` and
/// `new` (the geometry under them changed), so the walk heights everywhere
/// else, hand authoring included, stay as they are. Returns the cells set.
pub fn rebake_changed(old: &Chunk, new: &mut Chunk) -> Vec<usize> {
    let (before, after) = (bake(old), bake(new));
    let changed: Vec<usize> = (0..CELLS).filter(|&i| before[i] != after[i]).collect();
    for &i in &changed {
        new.heights[i] = after[i];
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::chunk::Group;

    #[test]
    fn a_floor_quad_covers_its_cells_at_its_height() {
        let mut c = Chunk::blank(0);
        // A 64 × 64 floor at y = −128 over cells (64..65, 64..65).
        c.pool = vec![[0, -128, 0], [64, -128, 0], [0, -128, 64], [64, -128, 64]];
        let q = Prim { list: 2, code: 0x28, n: 4, idx: [0, 1, 2, 3], rgb: [[128; 3]; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
        c.groups.push(Group { centre: [0; 3], prims: vec![q] });
        let s = surfaces(&c);
        let hit: Vec<usize> = (0..CELLS).filter(|&i| !s[i].is_empty()).collect();
        assert_eq!(hit, vec![64 * 128 + 64, 64 * 128 + 65, 65 * 128 + 64, 65 * 128 + 65]);
        let f = s[64 * 128 + 64][0];
        assert!((f.y + 128.0).abs() < 1e-3 && f.flat > 0.999);
        let h = bake(&c);
        assert_eq!((h[64 * 128 + 64], h[0]), (-129, 0));
    }

    #[test]
    fn only_cells_under_changed_geometry_are_rebaked() {
        let mut old = Chunk::blank(0);
        old.pool = vec![[0, -128, 0], [64, -128, 0], [0, -128, 64], [64, -128, 64]];
        let q = Prim { list: 2, code: 0x28, n: 4, idx: [0, 1, 2, 3], rgb: [[128; 3]; 4], uv: [[0; 2]; 4], clut: 0, tpage: 0 };
        old.groups.push(Group { centre: [0; 3], prims: vec![q] });
        // Hand authoring the bake can't know: a blocked cell far away.
        old.heights[5] = -1;
        let mut new = old.clone();
        // Raise the floor to a 64-unit platform.
        new.pool.iter_mut().for_each(|v| v[1] = -192);
        let cells = rebake_changed(&old, &mut new);
        assert_eq!(cells, vec![64 * 128 + 64, 64 * 128 + 65, 65 * 128 + 64, 65 * 128 + 65]);
        assert_eq!((new.heights[64 * 128 + 64], new.heights[5]), (-193, -1));
    }
}
