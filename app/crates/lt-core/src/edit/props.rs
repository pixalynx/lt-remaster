//! Props baked into a chunk (screen 10's construction kit): the polygons
//! that aren't terrain, joined into objects (a tree, a rock, a house).
//!
//! Tamsoft's level tool placed props from a library and merged them into
//! the chunk mesh, sharing vertices with the ground they stand on, so
//! connectivity alone joins a whole village into one piece. The terrain is
//! recognisable though: its polygons have every corner on the 128-unit
//! lattice and are either flat or textured from a shared page (the field
//! atlas and region sets hold the grass, dirt and cliff faces). Everything
//! else is a prop, and props that share a vertex are one object.
//!
//! Measured on all 524 chunks (`examples/prop_survey.rs`): the repeated
//! shapes are the library's props, e.g. the small pine 1,162 times in 120
//! chunks (`docs/research/05-props.md`).

use std::collections::HashMap;

use crate::world::Prim;

use super::chunk::Chunk;

/// Whether `p` is terrain: every corner on the 128-unit lattice, and flat
/// or drawn from a shared page (house walls and roofs on the lattice use
/// the chunk's own page).
pub fn is_terrain(c: &Chunk, p: &Prim) -> bool {
    let vs = p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize]);
    let lattice = vs.clone().all(|v| (v[0] as i32 + 2048) % 128 == 0 && (v[2] as i32 + 2048) % 128 == 0);
    if !lattice {
        return false;
    }
    let y0 = c.pool[p.idx[0] as usize][1];
    let flat = vs.clone().all(|v| v[1] == y0);
    flat || !own_page(c, p)
}

/// Whether `p` samples the chunk's own texture page.
pub fn own_page(c: &Chunk, p: &Prim) -> bool {
    let t = &c.texture.image;
    let (tx, ty) = ((p.tpage & 0xF) * 64, ((p.tpage >> 4) & 1) * 256);
    p.textured() && tx == t.x && ty >= t.y && ty < t.y + t.h
}

/// One prop: its polygons (group, index within the group) and bounds.
#[derive(Debug, Clone)]
pub struct Object {
    pub prims: Vec<(usize, usize)>,
    /// Bounds of its vertices, chunk-local `[x, y, z]` (Y down).
    pub min: [i16; 3],
    pub max: [i16; 3],
}

impl Object {
    /// Centre of the bounds on the ground plane.
    pub fn centre(&self) -> (i32, i32) {
        ((self.min[0] as i32 + self.max[0] as i32) / 2, (self.min[2] as i32 + self.max[2] as i32) / 2)
    }
}

/// The chunk's props, largest first: non-terrain polygons joined where they
/// share a vertex position.
pub fn objects(c: &Chunk) -> Vec<Object> {
    let mut keys: Vec<(usize, usize)> = Vec::new();
    for (g, grp) in c.groups.iter().enumerate() {
        for (i, p) in grp.prims.iter().enumerate() {
            if !is_terrain(c, p) {
                keys.push((g, i));
            }
        }
    }
    let mut parent: Vec<usize> = (0..keys.len()).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while parent[r] != r {
            r = parent[r];
        }
        let mut j = i;
        while parent[j] != r {
            let n = parent[j];
            parent[j] = r;
            j = n;
        }
        r
    }
    let mut first: HashMap<[i16; 3], usize> = HashMap::new();
    for (k, &(g, i)) in keys.iter().enumerate() {
        let p = &c.groups[g].prims[i];
        for &vi in &p.idx[..p.n as usize] {
            let v = c.pool[vi as usize];
            match first.get(&v) {
                Some(&o) => {
                    let (a, b) = (find(&mut parent, k), find(&mut parent, o));
                    if a != b {
                        parent[a] = b;
                    }
                }
                None => {
                    first.insert(v, k);
                }
            }
        }
    }
    let mut by_root: HashMap<usize, Object> = HashMap::new();
    for (k, &(g, i)) in keys.iter().enumerate() {
        let r = find(&mut parent, k);
        let o = by_root.entry(r).or_insert_with(|| Object { prims: Vec::new(), min: [i16::MAX; 3], max: [i16::MIN; 3] });
        o.prims.push((g, i));
        let p = &c.groups[g].prims[i];
        for &vi in &p.idx[..p.n as usize] {
            let v = c.pool[vi as usize];
            for a in 0..3 {
                o.min[a] = o.min[a].min(v[a]);
                o.max[a] = o.max[a].max(v[a]);
            }
        }
    }
    let mut out: Vec<Object> = by_root.into_values().collect();
    out.sort_by(|a, b| b.prims.len().cmp(&a.prims.len()).then(a.min.cmp(&b.min)));
    out
}

/// An upright plane: the axis it is across (0 = x, 2 = z) and where.
type Plane = (usize, i16);
/// A sheet's height (top, bottom; Y down) and its area by texture (tpage, clut).
type Sheet = ((i16, i16), HashMap<(u16, u16), i64>);

/// Whether `o` is a room (an interior's welded mesh): 150+ polygons, 160+ units tall.
pub fn is_room(o: &Object) -> bool {
    o.prims.len() >= 150 && o.max[1] - o.min[1] >= 160
}

/// Furniture welded into a room's mesh (research 13 §4): in every room object
/// ([`is_room`]), what is left once its walls and its floor
/// and wall-top polygons are dropped, joined where polygons share a corner.
/// A wall is a sheet (upright polygons in one plane joined along their edges)
/// that stands the room's full height in the room's wall texture: a cupboard's
/// back in a wall's plane stays, and so does a brick chimney. Largest first;
/// scraps are kept (books on a shelf), for [`super::stock`] to gather.
pub fn furniture(c: &Chunk) -> Vec<Object> {
    let verts = |k: (usize, usize)| {
        let p = &c.groups[k.0].prims[k.1];
        p.idx[..p.n as usize].iter().map(|&i| c.pool[i as usize]).collect::<Vec<_>>()
    };
    // An upright polygon's plane: x or z constant.
    let plane = |vs: &[[i16; 3]]| [0usize, 2].into_iter().find(|&a| vs.iter().all(|v| v[a] == vs[0][a])).map(|a| (a, vs[0][a]));
    let mut out = Vec::new();
    for room in objects(c).into_iter().filter(is_room) {
        let (top, floor) = (room.min[1], room.max[1]);
        // Sheets: upright polygons in one plane, joined where they share an edge (two corners).
        let upright: Vec<(usize, Plane)> = room.prims.iter().enumerate().filter_map(|(n, &k)| plane(&verts(k)).map(|key| (n, key))).collect();
        let mut sheet: Vec<usize> = (0..room.prims.len()).collect();
        let mut by_edge: HashMap<(Plane, [i16; 3], [i16; 3]), usize> = HashMap::new();
        for &(n, key) in &upright {
            let vs = verts(room.prims[n]);
            for a in 0..vs.len() {
                for b in a + 1..vs.len() {
                    let (p, q) = if vs[a] < vs[b] { (vs[a], vs[b]) } else { (vs[b], vs[a]) };
                    let o = *by_edge.entry((key, p, q)).or_insert(n);
                    let (x, y) = (find(&mut sheet, n), find(&mut sheet, o));
                    sheet[x] = y;
                }
            }
        }
        // Each sheet's height and its area by texture; the room's wall textures are those of its full-height sheets.
        let mut sheets: HashMap<usize, Sheet> = HashMap::new();
        for &(n, key) in &upright {
            let k = room.prims[n];
            let vs = verts(k);
            let p = &c.groups[k.0].prims[k.1];
            let e = sheets.entry(find(&mut sheet, n)).or_insert(((i16::MAX, i16::MIN), HashMap::new()));
            for v in &vs {
                e.0 = (e.0 .0.min(v[1]), e.0 .1.max(v[1]));
            }
            let across = 2 - key.0;
            let span = vs.iter().map(|v| v[across]).max().unwrap() as i64 - vs.iter().map(|v| v[across]).min().unwrap() as i64;
            let rise = vs.iter().map(|v| v[1]).max().unwrap() as i64 - vs.iter().map(|v| v[1]).min().unwrap() as i64;
            *e.1.entry(if p.textured() { (p.tpage, p.clut) } else { (0, 0) }).or_default() += span * rise / if p.n == 3 { 2 } else { 1 };
        }
        let full = |e: &(i16, i16)| e.1 - e.0 >= floor - top - 8;
        let main = |areas: &HashMap<(u16, u16), i64>| areas.iter().max_by_key(|(t, a)| (**a, **t)).map(|(t, _)| *t);
        let mut wall_area: HashMap<(u16, u16), i64> = HashMap::new();
        // Walls are textured: an untextured sheet is a dark firebox or flue.
        for (_, areas) in sheets.values().filter(|(e, _)| full(e)) {
            if let Some(t) = main(areas).filter(|&t| t != (0, 0)) {
                *wall_area.entry(t).or_default() += areas.values().sum::<i64>();
            }
        }
        let most = wall_area.values().copied().max().unwrap_or(0);
        let walls: Vec<usize> = sheets.iter().filter(|(_, (e, areas))| full(e) && main(areas).is_some_and(|t| wall_area.get(&t).is_some_and(|&a| a * 5 >= most * 2))).map(|(&s, _)| s).collect();
        let keep: Vec<(usize, usize)> = room
            .prims
            .iter()
            .enumerate()
            .filter(|&(n, &k)| {
                let vs = verts(k);
                let wall = walls.contains(&find(&mut sheet, n));
                let level = vs.iter().all(|v| v[1] == vs[0][1]) && (vs[0][1] == top || vs[0][1] == floor);
                !wall && !level
            })
            .map(|(_, &k)| k)
            .collect();
        // Pieces: the kept polygons joined by shared corners.
        let mut parent: Vec<usize> = (0..keep.len()).collect();
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut first: HashMap<[i16; 3], usize> = HashMap::new();
        for (n, &k) in keep.iter().enumerate() {
            for v in verts(k) {
                let o = *first.entry(v).or_insert(n);
                let (a, b) = (find(&mut parent, n), find(&mut parent, o));
                parent[a] = b;
            }
        }
        let mut by_root: HashMap<usize, Object> = HashMap::new();
        for (n, &k) in keep.iter().enumerate() {
            let o = by_root.entry(find(&mut parent, n)).or_insert_with(|| Object { prims: Vec::new(), min: [i16::MAX; 3], max: [i16::MIN; 3] });
            o.prims.push(k);
            for v in verts(k) {
                for a in 0..3 {
                    o.min[a] = o.min[a].min(v[a]);
                    o.max[a] = o.max[a].max(v[a]);
                }
            }
        }
        out.extend(by_root.into_values());
    }
    out.sort_by(|a, b| b.prims.len().cmp(&a.prims.len()).then(a.min.cmp(&b.min)));
    out
}

/// The cells `[c0, r0, c1, r1]` (inclusive) an object's bounds cover.
pub fn cells_of(o: &Object) -> [i32; 4] {
    let cell = |v: i32, hi: bool| if hi { (v + 2047).div_euclid(32) } else { (v + 2048).div_euclid(32) };
    [cell(o.min[0] as i32, false), cell(o.min[2] as i32, false), cell(o.max[0] as i32, true), cell(o.max[2] as i32, true)]
}

/// The smallest prop whose bounds hold chunk-local `(x, z)`, if any.
pub fn object_at(c: &Chunk, x: i32, z: i32) -> Option<Object> {
    objects(c)
        .into_iter()
        .filter(|o| (o.min[0] as i32..=o.max[0] as i32).contains(&x) && (o.min[2] as i32..=o.max[2] as i32).contains(&z))
        .min_by_key(|o| (o.max[0] as i32 - o.min[0] as i32) * (o.max[2] as i32 - o.min[2] as i32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::chunk::Group;

    fn quad(idx: [u16; 4], textured_own: bool) -> Prim {
        Prim {
            list: if textured_own { 7 } else { 6 },
            code: if textured_own { 0x3C } else { 0x38 },
            n: 4,
            idx,
            rgb: [[128; 3]; 4],
            uv: [[0; 2]; 4],
            clut: 0x7802,
            tpage: if textured_own { 0x0A } else { 0 },
        }
    }

    #[test]
    fn terrain_is_left_out_and_props_join_by_vertex() {
        let mut c = Chunk::blank(0);
        c.pool = vec![
            // Ground quad on the lattice (flat).
            [0, -128, 0], [128, -128, 0], [0, -128, 128], [128, -128, 128],
            // A post standing on it, sharing a ground corner: two upright quads off the lattice.
            [128, -192, 128], [160, -128, 128], [160, -192, 128], [160, -128, 160], [160, -192, 160],
            // A lone box far away.
            [-500, -140, -500], [-480, -140, -500], [-500, -140, -480], [-480, -140, -480],
        ];
        c.groups.push(Group {
            centre: [0; 3],
            prims: vec![quad([0, 1, 2, 3], false), quad([3, 4, 5, 6], true), quad([5, 6, 7, 8], true), quad([9, 10, 11, 12], false)],
        });
        assert!(is_terrain(&c, &c.groups[0].prims[0]));
        let objs = objects(&c);
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].prims, vec![(0, 1), (0, 2)]);
        assert_eq!((objs[0].min, objs[0].max), ([128, -192, 128], [160, -128, 160]));
        assert_eq!(object_at(&c, -490, -490).unwrap().prims, vec![(0, 3)]);
        assert!(object_at(&c, 0, 0).is_none());
    }
}
