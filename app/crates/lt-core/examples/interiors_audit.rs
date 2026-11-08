//! Audit of the Interiors shelf (screen 23): is every piece cut whole, and
//! does it look whole from every side once the game culls its back faces?
//!
//! ```text
//! cargo run --release --example interiors_audit -- <disc.bin> <out dir> [id…]
//! ```
//!
//! Per item it prints the polygons cut (sealed, `edit::seal`), what stays
//! behind in the room inside the piece's bounds (room walls and levels, or
//! another piece), the open edges left, and how much of the piece's outline is
//! see-through from each view. `<out>/<id>.png` shows it from the south, east,
//! north and west (35° down) and steeply from the south-west, back faces culled
//! as the field draws them (NCLIP, `WorldChunk_DrawObjectMesh`), see-through in
//! magenta; `<out>/<id>-left.png` what stayed behind. Research 13 §4.4.
//!
//! `DETAIL=1` lists each polygon left behind; `EDGES=1` sorts the open edges by
//! the way their polygon faces and the way the hole lies.
use std::collections::{HashMap, HashSet};

use lt_core::edit::chunk::Chunk;
use lt_core::edit::prefab::{self, Placement, Prefab};
use lt_core::edit::{props, stock};
use lt_core::raster::{self, Canvas, Ortho, Style};
use lt_core::worldmap::{WorldDisc, WorldTextures};
use lt_core::{DiscImage, FileTable, PsxExe};

const VIEW: usize = 200;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let tex = WorldTextures::load(&wd);
    let out = std::path::Path::new(&a[2]);
    std::fs::create_dir_all(out).unwrap();
    let only: HashSet<&str> = a[3..].iter().map(String::as_str).collect();
    for item in stock::catalogue().into_iter().filter(|i| i.category == stock::INTERIORS) {
        if !only.is_empty() && !only.contains(item.id.as_str()) {
            continue;
        }
        let n = item.source.chunk();
        let raw = wd.chunk(n).unwrap();
        let c = Chunk::parse(&raw).unwrap();
        let p = match stock::make(&item, &c, Some(&tex.vram_for(&wd, n, &raw))) {
            Ok(p) => p,
            Err(e) => {
                println!("{:14} FAILED {}", item.id, e.message);
                continue;
            }
        };
        let (left, found) = left_behind(&c, &item);
        if !found.is_empty() {
            let opts = prefab::HarvestOptions { skip_ground: false, prims: found, ..Default::default() };
            if let Ok(lp) = prefab::harvest_in(&c, n, prefab::CellRect { c0: 0, r0: 0, c1: 127, r1: 127 }, "left", &opts, Some(&tex.vram_for(&wd, n, &raw))) {
                std::fs::write(out.join(format!("{}-left.png", item.id)), prefab::thumbnail(&lp, 240).unwrap()).unwrap();
            }
        }
        let open = open_edges(&p);
        let want = stock::interior_size(&item.id).unwrap_or([0, 0]);
        let fits = if [p.area.w() as u8, p.area.h() as u8] == want { String::new() } else { format!(" · cut {}×{} vs recipe {}×{}", p.area.w(), p.area.h(), want[0], want[1]) };
        println!("{:14} {:3} polys · {:3} open edges · left behind: {}{fits}", item.id, p.prims.len(), open, if left.is_empty() { "none".into() } else { left.join(", ") });
        if std::env::var("EDGES").is_ok() {
            println!("    {}", open_detail(&p));
        }
        let (png, holes) = views(&p);
        println!("    see-through S {:.1}% · E {:.1}% · N {:.1}% · W {:.1}% · steep {:.1}%", holes[0], holes[1], holes[2], holes[3], holes[4]);
        std::fs::write(out.join(format!("{}.png", item.id)), png).unwrap();
    }
}

/// The source polygons whose corners all lie in the cut piece's bounds (2 units of slack) but which the cut
/// left out, by why: a wall plane or level the room filter dropped, or another piece of furniture.
fn left_behind(c: &Chunk, item: &stock::Item) -> (Vec<String>, Vec<(usize, usize)>) {
    let (cut, bounds) = match item.source {
        stock::Source::Piece { rect, .. } => {
            let Some(prims) = stock::piece_parts(c, rect) else { return (vec!["(no piece)".into()], Vec::new()) };
            let vs = prims.iter().flat_map(|&(g, i)| {
                let p = &c.groups[g].prims[i];
                p.idx[..p.n as usize].iter().map(|&v| c.pool[v as usize]).collect::<Vec<_>>()
            });
            let (mut lo, mut hi) = ([i16::MAX; 3], [i16::MIN; 3]);
            for v in vs {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
            (prims, (lo, hi))
        }
        stock::Source::Object { at, .. } => {
            let in_cell = |o: &props::Object| {
                let (x, z) = o.centre();
                (x + 2048).div_euclid(32) == at[0] as i32 && (z + 2048).div_euclid(32) == at[1] as i32
            };
            let objs: Vec<props::Object> = props::objects(c).into_iter().filter(in_cell).collect();
            let prims: Vec<(usize, usize)> = objs.iter().flat_map(|o| o.prims.iter().copied()).collect();
            let min = [0, 1, 2].map(|k| objs.iter().map(|o| o.min[k]).min().unwrap_or(0));
            let max = [0, 1, 2].map(|k| objs.iter().map(|o| o.max[k]).max().unwrap_or(0));
            (prims, (min, max))
        }
        _ => return (Vec::new(), Vec::new()),
    };
    let cut: HashSet<(usize, usize)> = cut.into_iter().collect();
    let pieces: Vec<props::Object> = props::furniture(c);
    let piece_of: HashMap<(usize, usize), usize> = pieces.iter().enumerate().flat_map(|(n, o)| o.prims.iter().map(move |&k| (k, n))).collect();
    let in_room: HashSet<(usize, usize)> = props::objects(c).into_iter().filter(props::is_room).flat_map(|o| o.prims).collect();
    let (lo, hi) = bounds;
    let mut why: HashMap<String, usize> = HashMap::new();
    let mut found = Vec::new();
    for (g, grp) in c.groups.iter().enumerate() {
        for (i, pr) in grp.prims.iter().enumerate() {
            if cut.contains(&(g, i)) {
                continue;
            }
            let vs: Vec<[i16; 3]> = pr.idx[..pr.n as usize].iter().map(|&v| c.pool[v as usize]).collect();
            let inside = vs.iter().all(|v| (0..3).all(|k| v[k] >= lo[k] - 2 && v[k] <= hi[k] + 2));
            if !inside {
                continue;
            }
            let flat = vs.iter().all(|v| v[1] == vs[0][1]);
            let label = match piece_of.get(&(g, i)) {
                Some(&n) => format!("piece of {} at {:?}", pieces[n].prims.len(), props::cells_of(&pieces[n])),
                None if props::is_terrain(c, pr) => "terrain".into(),
                None if !in_room.contains(&(g, i)) => "loose prop".into(),
                None if flat && vs[0][1] == hi[1] => "floor-level".into(),
                None if flat => "room level".into(),
                None => "room wall".into(),
            };
            if std::env::var("DETAIL").is_ok() {
                let (u0, v0, u1, v1) = (pr.uv[..pr.n as usize].iter().map(|t| t[0]).min().unwrap(), pr.uv[..pr.n as usize].iter().map(|t| t[1]).min().unwrap(), pr.uv[..pr.n as usize].iter().map(|t| t[0]).max().unwrap(), pr.uv[..pr.n as usize].iter().map(|t| t[1]).max().unwrap());
                let mn = [0, 1, 2].map(|k| vs.iter().map(|v| v[k]).min().unwrap());
                let mx = [0, 1, 2].map(|k| vs.iter().map(|v| v[k]).max().unwrap());
                println!("    {label:28} g{g:<2} code {:02x} tp {:04x} clut {:04x} uv {u0:3},{v0:3}..{u1:3},{v1:3}  {:?}..{:?}", pr.code, pr.tpage, pr.clut, mn, mx);
            }
            *why.entry(label).or_default() += 1;
            found.push((g, i));
        }
    }
    let mut v: Vec<(String, usize)> = why.into_iter().collect();
    v.sort();
    (v.into_iter().map(|(k, n)| format!("{n} {k}")).collect(), found)
}

/// A polygon's outward normal (Y down): the side NCLIP shows.
fn normal(vs: &[[i16; 3]]) -> [f32; 3] {
    let f = |v: [i16; 3]| v.map(|c| c as f32);
    let (a, b, c) = (f(vs[0]), f(vs[1]), f(vs[2]));
    let (u, w) = ([c[0] - a[0], c[1] - a[1], c[2] - a[2]], [b[0] - a[0], b[1] - a[1], b[2] - a[2]]);
    let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
    n.map(|c| c / l)
}

fn class(v: [f32; 3]) -> &'static str {
    let a = v.map(f32::abs);
    if a[1] >= a[0] && a[1] >= a[2] {
        if v[1] < 0.0 { "up" } else { "down" }
    } else if a[0] >= a[2] {
        if v[0] > 0.0 { "east" } else { "west" }
    } else if v[2] > 0.0 {
        "north"
    } else {
        "south"
    }
}

/// Each open edge: which way its polygon faces, and which way the hole lies (across the edge in its plane).
fn open_detail(p: &Prefab) -> String {
    let mut half: HashMap<([i16; 3], [i16; 3]), (usize, usize)> = HashMap::new();
    let ring = |n: u8| if n == 4 { vec![0usize, 1, 3, 2] } else { vec![0, 1, 2] };
    for (f, q) in p.prims.iter().enumerate() {
        let r = ring(q.prim.n);
        for k in 0..r.len() {
            let (a, b) = (p.pool[q.prim.idx[r[k]] as usize], p.pool[q.prim.idx[r[(k + 1) % r.len()]] as usize]);
            *half.entry((a, b)).or_insert((f, 0)) = (f, half.get(&(a, b)).map_or(0, |h| h.1) + 1);
        }
    }
    let same = half.iter().filter(|((a, b), h)| h.1 > 1 || half.contains_key(&(*a, *b)) && half.get(&(*a, *b)).unwrap().1 > 1).count();
    let mut tally: HashMap<String, usize> = HashMap::new();
    for (&(a, b), &(f, _)) in &half {
        if a == b || half.contains_key(&(b, a)) {
            continue;
        }
        let q = &p.prims[f].prim;
        let vs: Vec<[i16; 3]> = q.idx[..q.n as usize].iter().map(|&i| p.pool[i as usize]).collect();
        let n = normal(&vs);
        let e = [b[0] as f32 - a[0] as f32, b[1] as f32 - a[1] as f32, b[2] as f32 - a[2] as f32];
        let mut h = [e[1] * n[2] - e[2] * n[1], e[2] * n[0] - e[0] * n[2], e[0] * n[1] - e[1] * n[0]];
        let c = [0, 1, 2].map(|k| vs.iter().map(|v| v[k] as f32).sum::<f32>() / vs.len() as f32);
        let m = [0, 1, 2].map(|k| (a[k] as f32 + b[k] as f32) / 2.0 - c[k]);
        if h[0] * m[0] + h[1] * m[1] + h[2] * m[2] < 0.0 {
            h = h.map(|v| -v);
        }
        *tally.entry(format!("{}→{}", class(n), class(h))).or_default() += 1;
    }
    let mut v: Vec<_> = tally.into_iter().collect();
    v.sort();
    format!("same-way edges {same}; {}", v.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", "))
}

/// Edges (by corner position) used by one polygon only: holes, or a side never modelled.
fn open_edges(p: &Prefab) -> usize {
    let mut count: HashMap<([i16; 3], [i16; 3]), u32> = HashMap::new();
    for q in &p.prims {
        let n = q.prim.n as usize;
        // Quads go 0-1-3-2 round their rim, triangles 0-1-2.
        let ring: Vec<usize> = if n == 4 { vec![0, 1, 3, 2] } else { vec![0, 1, 2] };
        for k in 0..ring.len() {
            let (a, b) = (p.pool[q.prim.idx[ring[k]] as usize], p.pool[q.prim.idx[ring[(k + 1) % ring.len()]] as usize]);
            if a == b {
                continue;
            }
            *count.entry(if a < b { (a, b) } else { (b, a) }).or_default() += 1;
        }
    }
    count.values().filter(|&&n| n == 1).count()
}

/// South, east, north, west at 35° down, then steeply from the south-west: back faces culled, as the field draws them. Where the
/// piece's outline shows nothing once they are (a side never modelled, an open top) is painted magenta; the share
/// of the outline that is, per view.
fn views(p: &Prefab) -> (Vec<u8>, [f32; 5]) {
    let mut c = Chunk::blank(0);
    c.full_page();
    prefab::place(&mut c, p, Placement { col: 60, row: 60, rot: 0, ground_y: Some(0), write_ground: false, lift: 0 }).unwrap();
    let bytes = c.serialize().unwrap();
    let mesh = lt_core::world::mesh(&bytes).unwrap();
    let mut vram = lt_core::vram::Vram::new();
    vram.paint_tim(&bytes[lt_core::world::TIM_OFFSET..]);
    let shots: [(f32, f32); 5] = [(0.0, 35.0), (270.0, 35.0), (180.0, 35.0), (90.0, 35.0), (45.0, 65.0)];
    let mut sheet = Canvas::new(VIEW * shots.len(), VIEW, [24, 24, 28, 255]);
    let mut holes = [0f32; 5];
    for (s, &(yaw, pitch)) in shots.iter().enumerate() {
        // Camera basis as the editor's orbit has it (x right, y down, z forward): yaw 0 looks north (+z).
        let (y, e) = (yaw.to_radians(), pitch.to_radians());
        let r = [y.cos(), 0.0, -y.sin()];
        let d = [-e.sin() * y.sin(), e.cos(), -e.sin() * y.cos()];
        let f = [y.sin() * e.cos(), e.sin(), y.cos() * e.cos()];
        let dot = |v: [i16; 3], w: [f32; 3]| v[0] as f32 * w[0] + v[1] as f32 * w[1] + v[2] as f32 * w[2];
        let mut all = mesh.clone();
        // The rasteriser draws x across, z up and keeps the smallest y: screen right, screen up, depth.
        for v in &mut all.pool {
            let o = *v;
            *v = [dot(o, r).round() as i16, dot(o, f).round() as i16, (-dot(o, d)).round() as i16];
        }
        // NCLIP on corners 0, 1, 2 in screen space (y down): area ≤ 0 is dropped.
        let mut shown = all.clone();
        shown.prims.retain(|q| {
            let [a, b, cc] = [0, 1, 2].map(|k| mesh.pool[q.idx[k] as usize]);
            let (ax, ay, bx, by, cx, cy) = (dot(a, r), dot(a, d), dot(b, r), dot(b, d), dot(cc, r), dot(cc, d));
            (bx - ax) * (cy - ay) - (by - ay) * (cx - ax) > 0.0
        });
        let ortho = Ortho::fit(&all, VIEW);
        let (mut outline, mut solid, mut cv) = (Canvas::new(VIEW, VIEW, [0, 0, 0, 0]), Canvas::new(VIEW, VIEW, [0, 0, 0, 0]), Canvas::new(VIEW, VIEW, [0, 0, 0, 0]));
        raster::draw_mesh(&mut outline, &all, None, ortho, Style::Textured);
        raster::draw_mesh(&mut solid, &shown, None, ortho, Style::Textured);
        raster::draw_mesh(&mut cv, &shown, Some(&vram), ortho, Style::Textured);
        // The outline with what it encloses (an open top seen from above has no polygon over it): all but what the
        // border reaches.
        let mut outside = vec![false; VIEW * VIEW];
        let mut todo: Vec<usize> = (0..VIEW).flat_map(|k| [k, (VIEW - 1) * VIEW + k, k * VIEW, k * VIEW + VIEW - 1]).collect();
        while let Some(k) = todo.pop() {
            if outside[k] || outline.rgba[k * 4 + 3] != 0 {
                continue;
            }
            outside[k] = true;
            let (x, y) = (k % VIEW, k / VIEW);
            if x > 0 { todo.push(k - 1); }
            if x + 1 < VIEW { todo.push(k + 1); }
            if y > 0 { todo.push(k - VIEW); }
            if y + 1 < VIEW { todo.push(k + VIEW); }
        }
        let (mut area, mut hole) = (0, 0);
        for py in 0..VIEW {
            for px in 0..VIEW {
                let i = (py * VIEW + px) * 4;
                let o = (py * VIEW * shots.len() + s * VIEW + px) * 4;
                if outside[py * VIEW + px] {
                    continue;
                }
                area += 1;
                if solid.rgba[i + 3] == 0 {
                    hole += 1;
                    sheet.rgba[o..o + 4].copy_from_slice(&[255, 0, 255, 255]);
                } else if cv.rgba[i + 3] != 0 {
                    sheet.rgba[o..o + 4].copy_from_slice(&cv.rgba[i..i + 4]);
                }
            }
        }
        holes[s] = 100.0 * hole as f32 / area.max(1) as f32;
    }
    (lt_core::movie::png((VIEW * shots.len()) as u32, VIEW as u32, &sheet.rgba), holes)
}
