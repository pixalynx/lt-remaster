//! Sculpting across chunk seams (`edit::sculpt`) against the user's own retail
//! image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test sculpt`.

use std::collections::{BTreeMap, BTreeSet};

use lt_core::edit::chunk::Chunk;
use lt_core::edit::sculpt::{self, SculptMap, POINTS, SPACING};
use lt_core::edit::unbake;
use lt_core::world;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn chunks(ids: &[u32]) -> Option<BTreeMap<u32, Chunk>> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    Some(ids.iter().map(|&id| (id, Chunk::parse(&w.chunk(id).unwrap()).unwrap())).collect())
}

/// World origin of a grid chunk (PRD 09 §4.1).
fn origin(id: u32) -> (i32, i32) {
    let (x, y) = world::grid_cell(id).unwrap();
    (2048 * x as i32, -4096 * y as i32)
}

/// A hill in world space, as the editor's brush makes one: `peak` at (cx, cz), cos² down to 0 at `r`.
fn hill(cx: i32, cz: i32, r: f32, peak: f32) -> impl Fn(i32, i32) -> i16 {
    move |x, z| {
        let d = (((x - cx) as f32).powi(2) + ((z - cz) as f32).powi(2)).sqrt();
        if d >= r {
            0
        } else {
            (peak * (std::f32::consts::FRAC_PI_2 * d / r).cos().powi(2)).round() as i16
        }
    }
}

/// `id`'s map of a world-space rise.
fn map_of(id: u32, f: &impl Fn(i32, i32) -> i16) -> SculptMap {
    let (ox, oz) = origin(id);
    let mut m = SculptMap::flat();
    for j in 0..POINTS {
        for i in 0..POINTS {
            m.points[j * POINTS + i] = f(ox - 2048 + SPACING * i as i32, oz - 2048 + SPACING * j as i32);
        }
    }
    m
}

/// The (position along the edge, y) of every vertex on `axis` = `at`, the position shifted by `shift`.
fn edge(c: &Chunk, axis: usize, at: i16, shift: i32) -> BTreeSet<(i32, i16)> {
    let along = if axis == 0 { 2 } else { 0 };
    c.pool.iter().filter(|v| v[axis] == at).map(|v| (v[along] as i32 + shift, v[1])).collect()
}

/// A seam: chunk `a`'s edge and chunk `b`'s, as (axis, a's edge, b's edge, b's shift into a's frame).
struct Seam {
    a: u32,
    b: u32,
    axis: usize,
    at_a: i16,
    at_b: i16,
    shift: i32,
}

fn matches(cs: &BTreeMap<u32, Chunk>, s: &Seam) -> (usize, usize) {
    let a = edge(&cs[&s.a], s.axis, s.at_a, 0);
    let b = edge(&cs[&s.b], s.axis, s.at_b, s.shift);
    (a.intersection(&b).count(), a.len())
}

#[test]
fn a_hill_on_a_corner_keeps_the_three_seams_closed() {
    // 136's south-east corner is 138's south-west corner and the middle of 137's north edge (open grass, walkable
    // on all three sides of the corner).
    let ids = [136, 138, 137];
    let Some(mut cs) = chunks(&ids) else { return };
    let seams = [
        Seam { a: 136, b: 138, axis: 0, at_a: 2051, at_b: -2048, shift: 0 },
        Seam { a: 136, b: 137, axis: 2, at_a: -2051, at_b: 2048, shift: 2048 },
        Seam { a: 138, b: 137, axis: 2, at_a: -2051, at_b: 2048, shift: -2048 },
    ];
    let before: Vec<_> = seams.iter().map(|s| matches(&cs, s)).collect();
    let walk_before: Vec<i16> = (0..128).map(|r| cs[&136].heights[r * 128 + 127] - cs[&138].heights[r * 128]).collect();
    let corner0: Vec<i16> = cs[&136].pool.iter().filter(|v| v[0] == 2051 && v[2] == -2051).map(|v| v[1]).collect();
    assert!(!corner0.is_empty());
    let (ox, oz) = origin(136);
    let f = hill(ox + 2048, oz - 2048, 768.0, 96.0);
    for id in ids {
        let map = map_of(id, &f);
        assert!(!map.is_flat(), "the hill reaches chunk {id}");
        let c = cs.get_mut(&id).unwrap();
        let split = unbake::split(c);
        let r = sculpt::apply(c, &map, &split).unwrap();
        assert!(r.vertices > 0 && r.cells > 0, "chunk {id}: {r:?}");
        c.serialize().unwrap_or_else(|e| panic!("chunk {id}: {e}"));
    }
    for (s, (n0, len0)) in seams.iter().zip(&before) {
        assert!(*n0 >= 16, "{}|{}: the retail edges meet ({n0} of {len0})", s.a, s.b);
        let (n, len) = matches(&cs, s);
        assert_eq!(len, *len0, "{}|{}: the edge keeps its vertices", s.a, s.b);
        assert_eq!(n, *n0, "{}|{}: every vertex that met its neighbour still does", s.a, s.b);
    }
    // The seam rose with the hill: 136's corner, under its top, went up by the whole peak.
    let corner = |c: &Chunk| c.pool.iter().filter(|v| v[0] == 2051 && v[2] == -2051).map(|v| v[1]).collect::<Vec<_>>();
    assert_eq!(corner(&cs[&136]), corner0.iter().map(|y| y - 96).collect::<Vec<_>>());
    // Walk heights across 136|138 stay a step the Knight takes where they were level.
    for (row, d0) in walk_before.iter().enumerate() {
        let (a, b) = (cs[&136].heights[row * 128 + 127], cs[&138].heights[row * 128]);
        if *d0 == 0 && a < -1 && b < -1 {
            assert!((a - b).abs() <= 32, "row {row}: {a} | {b}");
        }
    }
}
