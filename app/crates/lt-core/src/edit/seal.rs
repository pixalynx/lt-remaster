//! Sealing retail furniture for the Interiors shelf (screen 23). Tamsoft
//! modelled a room's furniture for the field camera: the sides that stood
//! against a wall, or that the camera never saw, were left out, and the
//! field culls back faces (NCLIP in `WorldChunk_DrawObjectMesh`). Cut out and
//! turned, a piece shows holes: a cupboard without its back, a table leg
//! that vanishes from behind, a chimney open at the top.
//!
//! [`seal`] finds the open edges (by corner position; retail windings are
//! consistent, so an edge with no partner running the other way borders a
//! hole) and closes them:
//! - a missing face: the open edges whose hole lies in one plane (across the
//!   edge from their polygon, at right angles to it), chained, closed and
//!   filled, in the texture of its largest neighbour;
//! - a thin part (a table leg, a crate's side, a leaf): when the outline
//!   would be mostly made up, or the edge slants, its polygons get their
//!   backs instead; so do cards turned over and quads folded past flat.
//!
//! Undersides are left open: nothing looks up at them.

use std::collections::{BTreeSet, HashMap};

use super::prefab::{Prefab, PrefabPrim};

type P = [i16; 3];
/// A missing face's plane: the axis it is across, the way it faces along it, and where.
type Group = (usize, i8, i16);

/// What sealing added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sealed {
    /// Polygons filling missing faces.
    pub faces: usize,
    /// Backs given to thin parts.
    pub backs: usize,
}

/// Corners in order round a polygon: quads are in PSX strip order (triangles 0-1-2 and 1-3-2).
fn ring(n: u8) -> &'static [usize] {
    if n == 4 {
        &[0, 1, 3, 2]
    } else {
        &[0, 1, 2]
    }
}

fn sub(a: P, b: P) -> [f32; 3] {
    [a[0] as f32 - b[0] as f32, a[1] as f32 - b[1] as f32, a[2] as f32 - b[2] as f32]
}

fn cross(u: [f32; 3], w: [f32; 3]) -> [f32; 3] {
    [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]]
}

fn dot(u: [f32; 3], w: [f32; 3]) -> f32 {
    u[0] * w[0] + u[1] * w[1] + u[2] * w[2]
}

fn len(u: [f32; 3]) -> f32 {
    dot(u, u).sqrt()
}

fn dist(a: P, b: P) -> f32 {
    len(sub(a, b))
}

/// The side a polygon shows (Y down): the one NCLIP keeps, from its first three corners.
fn outward(p: &Prefab, f: usize) -> Option<[f32; 3]> {
    let q = &p.prims[f].prim;
    let [a, b, c] = [0, 1, 2].map(|k| p.pool[q.idx[k] as usize]);
    let n = cross(sub(c, a), sub(b, a));
    let l = len(n);
    (l > 1e-3).then(|| n.map(|v| v / l))
}

fn corners(p: &Prefab, f: usize) -> Vec<P> {
    let q = &p.prims[f].prim;
    q.idx[..q.n as usize].iter().map(|&i| p.pool[i as usize]).collect()
}

/// Area of a polygon in its plane, `k` being the axis the plane is across.
fn area(poly: &[P], k: usize) -> f32 {
    let (u, v) = plane_axes(k);
    let mut a = 0.0;
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        a += p[u] as f32 * q[v] as f32 - q[u] as f32 * p[v] as f32;
    }
    a / 2.0
}

fn plane_axes(k: usize) -> (usize, usize) {
    match k {
        0 => (2, 1),
        1 => (0, 2),
        _ => (0, 1),
    }
}

/// Seals `p` (see the module). `open_top`: a piece meant to stay open (a crate) gets the backs of its sides
/// rather than a lid.
pub fn seal(p: &mut Prefab, open_top: bool) -> Sealed {
    let mut half: HashMap<(P, P), usize> = HashMap::new();
    let mut index: HashMap<P, u16> = HashMap::new();
    let mut thin: BTreeSet<usize> = BTreeSet::new();
    for (f, q) in p.prims.iter().enumerate() {
        let r = ring(q.prim.n);
        for k in 0..r.len() {
            let (ia, ib) = (q.prim.idx[r[k]], q.prim.idx[r[(k + 1) % r.len()]]);
            let (a, b) = (p.pool[ia as usize], p.pool[ib as usize]);
            index.entry(a).or_insert(ia);
            if a == b {
                continue;
            }
            // Two polygons running an edge the same way face opposite ways across it (a leaf turned over):
            // a card seen from both sides, so both get their backs.
            if let Some(&g) = half.get(&(a, b)) {
                if g != f && !half.contains_key(&(b, a)) {
                    thin.extend([g, f]);
                }
                continue;
            }
            half.insert((a, b), f);
        }
    }
    // A quad folded past flat: NCLIP judges it by corners 0, 1, 2, so its other half vanishes from one side.
    for (f, q) in p.prims.iter().enumerate().filter(|(_, q)| q.prim.n == 4) {
        let c = q.prim.idx.map(|i| p.pool[i as usize]);
        let (n1, n2) = (cross(sub(c[2], c[0]), sub(c[1], c[0])), cross(sub(c[2], c[1]), sub(c[3], c[1])));
        if dot(n1, n2) < 0.0 {
            thin.insert(f);
        }
    }
    let open: Vec<(P, P, usize)> = half.iter().filter(|(&(a, b), _)| !half.contains_key(&(b, a))).map(|(&(a, b), &f)| (a, b, f)).collect();
    // An edge that only looks open: it runs back along part of another open edge (a T-junction, a top that
    // overhangs its sides).
    let covered = |a: P, b: P| {
        let e = sub(b, a);
        open.iter().any(|&(c, d, _)| {
            let g = sub(d, c);
            if (c, d) == (a, b) || dot(e, g) >= 0.0 || len(cross(e, sub(c, a))) > 1.0 || len(cross(e, sub(d, a))) > 1.0 {
                return false;
            }
            // Overlap along e.
            let t = |x: P| dot(sub(x, a), e) / dot(e, e);
            let (lo, hi) = (t(c).min(t(d)), t(c).max(t(d)));
            hi.min(1.0) - lo.max(0.0) > 0.01
        })
    };
    let mut groups: HashMap<Group, Vec<(P, P, usize)>> = HashMap::new();
    for &(a, b, f) in &open {
        let Some(n) = outward(p, f) else { continue };
        let cs = corners(p, f);
        let c = [0, 1, 2].map(|k| cs.iter().map(|v| v[k] as f32).sum::<f32>() / cs.len() as f32);
        let e = sub(b, a);
        let mut h = cross(e, n);
        let m = [0, 1, 2].map(|k| (a[k] as f32 + b[k] as f32) / 2.0 - c[k]);
        if dot(h, m) < 0.0 {
            h = h.map(|v| -v);
        }
        let l = len(h).max(1e-6);
        if covered(a, b) {
            continue;
        }
        if h[1] >= 0.9 * l {
            continue;
        }
        // The plane the missing face lies in: across the axis the hole leans along most, the edge lying in it
        // (a chamfered top's edge belongs to the end face below it). Never an underside.
        let pick = (0..3).filter(|&k| a[k] == b[k] && h[k].abs() >= 0.6 * l && !(k == 1 && h[k] > 0.0)).max_by(|&x, &y| h[x].abs().total_cmp(&h[y].abs()));
        let Some(k) = pick.filter(|&k| !(open_top && k == 1)) else {
            // A slanting edge with the hole beside or above it (a leaf, a log's end, a sack's mouth): its back.
            if h[1] < 0.5 * l {
                thin.insert(f);
            }
            continue;
        };
        let s: i8 = if h[k] > 0.0 { 1 } else { -1 };
        // The missing face runs b → a, the other way from its neighbour.
        groups.entry((k, s, a[k])).or_default().push((b, a, f));
    }

    // Faces to fill: corners in order round them, the axis their plane is across, the way they face, and the
    // polygon they borrow a texture from.
    let mut faces: Vec<(Vec<P>, usize, i8, usize)> = Vec::new();
    let mut keys: Vec<_> = groups.keys().copied().collect();
    keys.sort();
    for key in keys {
        let edges = &groups[&key];
        let (k, s, _) = key;
        let (u, v) = plane_axes(k);
        // Chains of edges end to start; each closed, or joined to the next by a made-up edge.
        let mut used = vec![false; edges.len()];
        let (mut closed, mut loose): (Vec<Vec<P>>, Vec<Vec<P>>) = (Vec::new(), Vec::new());
        for i in 0..edges.len() {
            if used[i] {
                continue;
            }
            used[i] = true;
            let mut chain = vec![edges[i].0, edges[i].1];
            while chain.last() != chain.first() {
                let end = *chain.last().unwrap();
                let Some(j) = (0..edges.len()).find(|&j| !used[j] && edges[j].0 == end) else { break };
                used[j] = true;
                chain.push(edges[j].1);
            }
            if chain.last() == chain.first() {
                chain.pop();
                closed.push(chain);
                continue;
            }
            while let Some(j) = (0..edges.len()).find(|&j| !used[j] && edges[j].1 == chain[0]) {
                used[j] = true;
                chain.insert(0, edges[j].0);
            }
            loose.push(chain);
        }
        let mut polys = closed;
        if !loose.is_empty() {
            let real: f32 = loose.iter().map(|c| c.windows(2).map(|w| dist(w[0], w[1])).sum::<f32>()).sum();
            let n = loose.iter().map(Vec::len).sum::<usize>() as f32;
            let mid = [0, 1, 2].map(|c| loose.iter().flatten().map(|p| p[c] as f32).sum::<f32>() / n);
            // A made-up edge from `a` to `b`: straight along the plane's axes, else round the corner farther out
            // (a box's corner the retail mesh never had, at the back of a cupboard's floor).
            let route = |a: P, b: P| -> Vec<P> {
                if a[u] == b[u] || a[v] == b[v] {
                    return Vec::new();
                }
                let (mut c1, mut c2) = (a, a);
                c1[v] = b[v];
                c2[u] = b[u];
                let far = |c: P| (c[u] as f32 - mid[u]).powi(2) + (c[v] as f32 - mid[v]).powi(2);
                vec![if far(c1) >= far(c2) { c1 } else { c2 }]
            };
            let walk = |a: P, b: P| (a[u] - b[u]).abs() as f32 + (a[v] - b[v]).abs() as f32;
            let mut poly = loose.remove(0);
            let mut made = 0.0;
            while !loose.is_empty() {
                let end = *poly.last().unwrap();
                let j = (0..loose.len()).min_by(|&x, &y| dist(end, loose[x][0]).total_cmp(&dist(end, loose[y][0]))).unwrap();
                made += walk(end, loose[j][0]);
                poly.extend(route(end, loose[j][0]));
                poly.extend(loose.remove(j));
            }
            made += walk(*poly.last().unwrap(), poly[0]);
            poly.extend(route(*poly.last().unwrap(), poly[0]));
            // A face under the open rim of a top (a cupboard's back, below its lid) is real however much of its
            // outline is made up; otherwise mostly made up (the tops of a table's legs) means thin parts.
            let rim = edges.iter().any(|e| outward(p, e.2).is_some_and(|n| n[1] <= -0.7));
            if made <= 0.6 * real || rim {
                polys.push(poly);
            } else {
                thin.extend(edges.iter().map(|e| e.2));
            }
        }
        for mut poly in polys {
            poly.dedup();
            // Drop corners in a straight line with their neighbours.
            let mut i = 0;
            while poly.len() > 3 && i < poly.len() {
                let (a, b, c) = (poly[(i + poly.len() - 1) % poly.len()], poly[i], poly[(i + 1) % poly.len()]);
                if len(cross(sub(b, a), sub(c, b))) < 1.0 {
                    poly.remove(i);
                } else {
                    i += 1;
                }
            }
            if poly.len() < 3 || area(&poly, k).abs() < 16.0 {
                thin.extend(edges.iter().map(|e| e.2));
                continue;
            }
            let donor = edges.iter().map(|e| e.2).max_by(|&x, &y| face_area(p, x).total_cmp(&face_area(p, y))).unwrap();
            faces.push((poly, k, s, donor));
        }
    }
    // An open piece's floor, just above the room's, so it isn't the room's planks seen inside.
    if open_top && !p.prims.is_empty() {
        let used: Vec<P> = p.prims.iter().flat_map(|q| q.prim.idx[..q.prim.n as usize].iter().map(|&i| p.pool[i as usize])).collect();
        let lo = [0, 1, 2].map(|c| used.iter().map(|v| v[c]).min().unwrap());
        let hi = [0, 1, 2].map(|c| used.iter().map(|v| v[c]).max().unwrap());
        let y = hi[1] - 2;
        let donor = (0..p.prims.len()).max_by(|&x, &y| face_area(p, x).total_cmp(&face_area(p, y))).unwrap();
        faces.push((vec![[lo[0], y, lo[2]], [hi[0], y, lo[2]], [hi[0], y, hi[2]], [lo[0], y, hi[2]]], 1, -1, donor));
    }

    let mut added: Vec<PrefabPrim> = Vec::new();
    let mut out = Sealed::default();
    for (mut poly, k, s, donor) in faces {
        // Facing the hole's way out: the outward normal is −(Newell normal) in this file's convention.
        let mut newell = [0f32; 3];
        for i in 0..poly.len() {
            let (p0, p1) = (poly[i], poly[(i + 1) % poly.len()]);
            newell = [0, 1, 2].map(|c| newell[c] + cross(p0.map(|v| v as f32), p1.map(|v| v as f32))[c]);
        }
        if -newell[k] * s as f32 <= 0.0 {
            poly.reverse();
        }
        for &c in &poly {
            if let std::collections::hash_map::Entry::Vacant(e) = index.entry(c) {
                e.insert(p.pool.len() as u16);
                p.pool.push(c);
            }
        }
        for t in triangulate(&poly, k) {
            added.push(fill(p, donor, &t, k, &poly, &index));
            out.faces += 1;
        }
    }
    for f in thin {
        added.push(back(&p.prims[f]));
        out.backs += 1;
    }
    p.prims.extend(added);
    out
}

fn face_area(p: &Prefab, f: usize) -> f32 {
    let c = corners(p, f);
    let r = ring(c.len() as u8);
    let mut n = [0f32; 3];
    for i in 0..r.len() {
        n = [0, 1, 2].map(|k| n[k] + cross(c[r[i]].map(|v| v as f32), c[r[(i + 1) % r.len()]].map(|v| v as f32))[k]);
    }
    len(n) / 2.0
}

/// A polygon into convex parts: itself when it is a convex quad or a triangle, else triangles cut off at
/// its ears.
fn triangulate(poly: &[P], k: usize) -> Vec<Vec<P>> {
    let (u, v) = plane_axes(k);
    let sign = area(poly, k).signum();
    let turn = |a: P, b: P, c: P| ((b[u] - a[u]) as f32 * (c[v] - a[v]) as f32 - (b[v] - a[v]) as f32 * (c[u] - a[u]) as f32) * sign;
    let convex = (0..poly.len()).all(|i| turn(poly[i], poly[(i + 1) % poly.len()], poly[(i + 2) % poly.len()]) > 0.0);
    if poly.len() == 3 || (poly.len() == 4 && convex) {
        return vec![poly.to_vec()];
    }
    let mut rest = poly.to_vec();
    let mut out = Vec::new();
    let inside = |p: P, a: P, b: P, c: P| turn(a, b, p) > 0.0 && turn(b, c, p) > 0.0 && turn(c, a, p) > 0.0;
    while rest.len() > 3 {
        let n = rest.len();
        let ear = (0..n).find(|&i| {
            let (a, b, c) = (rest[(i + n - 1) % n], rest[i], rest[(i + 1) % n]);
            turn(a, b, c) > 0.0 && !rest.iter().any(|&p| p != a && p != b && p != c && inside(p, a, b, c))
        });
        let Some(i) = ear else { break };
        out.push(vec![rest[(i + n - 1) % n], rest[i], rest[(i + 1) % n]]);
        rest.remove(i);
    }
    if rest.len() == 3 {
        out.push(rest);
    }
    out
}

/// A polygon over `t` (corners in order round it) in `donor`'s texture and colour: the donor's texels stretched
/// over the face's extent in its plane.
fn fill(p: &Prefab, donor: usize, t: &[P], k: usize, face: &[P], index: &HashMap<P, u16>) -> PrefabPrim {
    let d = p.prims[donor];
    let q = d.prim;
    let n = q.n as usize;
    let quad = t.len() == 4;
    // Lists 0..7 = F3 FT3 F4 FT4 G3 GT3 G4 GT4: the donor's kind with this face's corner count.
    let list = (q.list & !2) | if quad { 2 } else { 0 };
    let code = 0x20 | (list << 2) | (q.code & 3);
    let (u, v) = plane_axes(k);
    let span = |a: usize| (face.iter().map(|c| c[a]).min().unwrap() as f32, face.iter().map(|c| c[a]).max().unwrap() as f32);
    let ((u0, u1), (v0, v1)) = (span(u), span(v));
    let tex = |c: usize| (q.uv[..n].iter().map(|t| t[c]).min().unwrap() as f32, q.uv[..n].iter().map(|t| t[c]).max().unwrap() as f32);
    let ((s0, s1), (t0, t1)) = (tex(0), tex(1));
    let at = |x: f32, lo: f32, hi: f32, a: f32, b: f32| (a + (x - lo) / (hi - lo).max(1.0) * (b - a)).round().clamp(0.0, 255.0) as u8;
    let mut rgb = [0u32; 3];
    for c in &q.rgb[..n] {
        for i in 0..3 {
            rgb[i] += c[i] as u32;
        }
    }
    let rgb = rgb.map(|c| (c / n as u32) as u8);
    // Strip order for a quad: corners 0, 1, 3, 2 round it.
    let order: &[usize] = if quad { &[0, 1, 3, 2] } else { &[0, 1, 2] };
    let mut prim = q;
    prim.list = list;
    prim.code = code;
    prim.n = t.len() as u8;
    prim.idx = [0; 4];
    prim.uv = [[0; 2]; 4];
    prim.rgb = [rgb; 4];
    for (slot, &o) in order.iter().enumerate() {
        let c = t[o];
        prim.idx[slot] = index[&c];
        // Texture v runs down the page; Y runs down too, and across a floor or a top, north is up the page.
        let (fu, fv) = (c[u] as f32, c[v] as f32);
        prim.uv[slot] = [at(fu, u0, u1, s0, s1), if k == 1 { at(v1 - fv + v0, v0, v1, t0, t1) } else { at(fv, v0, v1, t0, t1) }];
    }
    PrefabPrim { prim, tile: d.tile }
}

/// `f` facing the other way: its corners in reverse order round it.
fn back(f: &PrefabPrim) -> PrefabPrim {
    let mut b = *f;
    let q = &f.prim;
    let order: &[usize] = if q.n == 4 { &[1, 0, 3, 2] } else { &[0, 2, 1] };
    for (slot, &o) in order.iter().enumerate() {
        b.prim.idx[slot] = q.idx[o];
        b.prim.uv[slot] = q.uv[o];
        b.prim.rgb[slot] = q.rgb[o];
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::prefab::{CellRect, PrimData};

    fn prefab(pool: Vec<P>, quads: &[[u16; 4]]) -> Prefab {
        let prims = quads
            .iter()
            .map(|&idx| PrefabPrim { prim: PrimData { list: 3, code: 0x2C, n: 4, idx, rgb: [[128; 3]; 4], uv: [[0, 0], [31, 0], [0, 31], [31, 31]], clut: 0, tpage: 0 }, tile: None })
            .collect();
        Prefab { name: "t".into(), source_chunk: 0, area: CellRect { c0: 0, r0: 0, c1: 1, r1: 1 }, ground_y: 0, pool, prims, tiles: Vec::new(), heights: Vec::new(), category: String::new(), stock: None, stock_rev: 0, design: None }
    }

    /// Whether the polygon shows from `from` (a direction towards the viewer), as NCLIP decides.
    fn shows(p: &Prefab, f: usize, from: [f32; 3]) -> bool {
        outward(p, f).is_some_and(|n| dot(n, from) > 0.0)
    }

    /// A 64-wide, 64-deep, 64-tall box with its top, front (south) and sides, its back (north) left out as a
    /// cupboard against a wall has it: sealing adds the back, facing north, and nothing else.
    #[test]
    fn a_missing_back_is_filled_facing_out() {
        // Y down: the top at −64, the floor at 0; z = 64 is the back.
        let pool = vec![
            [0, -64, 0], [64, -64, 0], [0, 0, 0], [64, 0, 0], // front corners
            [0, -64, 64], [64, -64, 64], [0, 0, 64], [64, 0, 64], // back corners
        ];
        // Each quad in strip order, wound to show outward (checked below).
        let mut p = prefab(pool, &[[0, 1, 2, 3], [4, 5, 0, 1], [4, 0, 6, 2], [1, 5, 3, 7]]);
        assert!(shows(&p, 0, [0.0, 0.0, -1.0]) && shows(&p, 1, [0.0, -1.0, 0.0]) && shows(&p, 2, [-1.0, 0.0, 0.0]) && shows(&p, 3, [1.0, 0.0, 0.0]));
        let sealed = seal(&mut p, false);
        assert_eq!(sealed, Sealed { faces: 1, backs: 0 });
        assert!(shows(&p, 4, [0.0, 0.0, 1.0]));
        assert!(corners(&p, 4).iter().all(|c| c[2] == 64));
    }

    /// A lone upright panel (a table leg): no face to fill, so it gets its back.
    #[test]
    fn a_thin_part_gets_its_back() {
        let mut p = prefab(vec![[0, -64, 0], [8, -64, 0], [0, 0, 0], [8, 0, 0]], &[[0, 1, 2, 3]]);
        let sealed = seal(&mut p, false);
        assert_eq!(sealed, Sealed { faces: 0, backs: 1 });
        assert!(shows(&p, 0, [0.0, 0.0, -1.0]) && shows(&p, 1, [0.0, 0.0, 1.0]));
    }
}
