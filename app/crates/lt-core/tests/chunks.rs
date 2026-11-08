//! Chunk codec checks against the user's own retail image (PRD 10 §8). Opt in
//! with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test chunks`.

use lt_core::edit::chunk::Chunk;
use lt_core::world::{self, CHUNK_COUNT};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, exe, table })
}

impl Disc {
    fn chunk(&self, id: u32) -> Vec<u8> {
        WorldDisc { image: &self.image, table: &self.table, exe: &self.exe }.chunk(id).unwrap()
    }
}

#[test]
fn every_retail_chunk_round_trips() {
    let Some(d) = disc() else { return };
    for id in 0..CHUNK_COUNT {
        let raw = d.chunk(id);
        let c = Chunk::parse(&raw).unwrap_or_else(|e| panic!("chunk/{id}: {e}"));
        let out = c.serialize().unwrap_or_else(|e| panic!("chunk/{id}: {e}"));
        if let Some(at) = raw.iter().zip(&out).position(|(a, b)| a != b) {
            panic!("chunk/{id} differs first at {at:#x}");
        }
        // The meters agree with the readers the viewer uses.
        let s = c.sizes().unwrap();
        let st = world::stats(id, &raw).unwrap();
        assert_eq!((s.mesh, s.vertices, s.polygons, s.texture), (st.mesh_bytes, st.vertices, st.polygons, st.texture_bytes), "chunk/{id}");
        assert_eq!((s.layer_a, s.layer_b), world::layer_bytes(&raw), "chunk/{id}");
        assert!(c.unencodable_heights().is_empty(), "chunk/{id}");
    }
}

/// A height edit rewrites layer A and nothing else; a cloned group adds
/// exactly its polygons and keeps every other group's.
#[test]
fn edits_to_chunk39_read_back() {
    let Some(d) = disc() else { return };
    let raw = d.chunk(39);
    let mut c = Chunk::parse(&raw).unwrap();
    // PRD 10 §8: raise a 3×3 block by 0x80 next to the village ramp.
    for z in 60..63 {
        for x in 60..63 {
            c.heights[z * 128 + x] -= 0x80;
        }
    }
    let out = c.serialize().unwrap();
    let changed: Vec<usize> = raw.iter().zip(&out).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
    assert!(!changed.is_empty());
    assert!(changed.iter().all(|&i| (0x2A800..0x2D800).contains(&i)), "outside layer A: {:#x}", changed[0]);
    assert_eq!(Chunk::parse(&out).unwrap().heights, c.heights);
    assert_eq!(world::heightmap(&out), c.heights);

    // Copy group 0 one cell east, as new vertices in a new group.
    let mut c = Chunk::parse(&raw).unwrap();
    let mut g = c.groups[0].clone();
    let base = c.pool.len() as u16;
    let used: std::collections::BTreeSet<u16> = g.prims.iter().flat_map(|p| p.idx[..p.n as usize].to_vec()).collect();
    let remap: std::collections::BTreeMap<u16, u16> = used.iter().enumerate().map(|(k, &i)| (i, base + k as u16)).collect();
    for &i in &used {
        let [x, y, z] = c.pool[i as usize];
        c.pool.push([x + 32, y, z]);
    }
    for p in &mut g.prims {
        for i in &mut p.idx[..p.n as usize] {
            *i = remap[i];
        }
    }
    let added = g.prims.len();
    c.groups.push(g);
    let last = c.groups.len() - 1;
    c.recompute_centre(last);
    let out = c.serialize().unwrap();
    let before = world::mesh(&raw).unwrap();
    let after = world::mesh(&out).unwrap();
    assert_eq!(after.prims.len(), before.prims.len() + added);
    assert_eq!(after.groups.len(), before.groups.len() + 1);
    assert_eq!(after.prims[..before.prims.len()], before.prims[..]);
    assert_eq!(world::heightmap(&out), world::heightmap(&raw));
    assert_eq!(out[world::TIM_OFFSET..], raw[world::TIM_OFFSET..]);
    let r = Chunk::parse(&out).unwrap();
    assert_eq!(r.groups[last].centre, c.groups[last].centre);
    assert_eq!(r.serialize().unwrap(), out);
}

/// The brushes' save path: painting cells and then their old values back
/// gives the retail file again, even after a save whose layers grew.
#[test]
fn painting_back_restores_retail() {
    use lt_core::edit::chunk::{paint, CellWrite};
    let Some(d) = disc() else { return };
    let retail = d.chunk(39);
    let c = Chunk::parse(&retail).unwrap();
    // A checkerboard patch: many short runs, so layer A grows.
    let cells: Vec<usize> = (40..56).flat_map(|z| (40..56).map(move |x| z * 128 + x)).collect();
    let rough: Vec<CellWrite> = cells.iter().map(|&i| CellWrite { cell: i as u32, value: c.heights[i] as i32 - 32 * ((i + i / 128) % 2) as i32 }).collect();
    let zones: Vec<CellWrite> = cells.iter().map(|&i| CellWrite { cell: i as u32, value: 68 }).collect();
    let p = paint(&retail, &retail, &rough, &zones).unwrap();
    assert!(p.sizes.layer_a > world::layer_bytes(&retail).0);
    let r = Chunk::parse(&p.bytes).unwrap();
    assert!(cells.iter().all(|&i| r.zones[i] == 68 && r.heights[i] == rough.iter().find(|w| w.cell as usize == i).unwrap().value as i16));
    let undo_h: Vec<CellWrite> = cells.iter().map(|&i| CellWrite { cell: i as u32, value: c.heights[i] as i32 }).collect();
    let undo_z: Vec<CellWrite> = cells.iter().map(|&i| CellWrite { cell: i as u32, value: c.zones[i] as i32 }).collect();
    let back = paint(&retail, &p.bytes, &undo_h, &undo_z).unwrap();
    assert!(back.bytes == retail, "painted back differs from retail");
}

/// PRD 10 §5.3: exporting any retail chunk to OBJ and importing the files
/// unchanged gives the same bytes (blend bits, page rows, list choice, pool
/// order and culling centres all survive).
#[test]
fn every_chunk_survives_an_obj_round_trip() {
    use lt_core::edit::obj;
    let Some(d) = disc() else { return };
    let mut images = 0;
    for id in 0..CHUNK_COUNT {
        let raw = d.chunk(id);
        let c = Chunk::parse(&raw).unwrap();
        let e = obj::export(&c, id);
        images += e.images.len();
        let (back, r) = obj::import(&c, &e.obj, Some(&e.meta)).unwrap_or_else(|err| panic!("chunk/{id}: {err}"));
        assert_eq!(r.recentred, 0, "chunk/{id}");
        let out = back.serialize().unwrap();
        if let Some(at) = raw.iter().zip(&out).position(|(a, b)| a != b) {
            panic!("chunk/{id} differs first at {at:#x} after the OBJ round trip");
        }
    }
    assert!(images > 524, "{images} page previews");
}

/// Step from a to b allowed (`Overworld_KnightUpdate`: |Δ| ≤ 0x20; −1 blocks).
fn step(a: i16, b: i16) -> bool {
    a != -1 && b != -1 && (a as i32 - b as i32).abs() <= 0x20
}

/// The baker against Tamsoft's heights (research of 2026-10-05, PRD 10 §11):
/// steps retail allows that the bake allows too (recall), and steps the bake
/// allows that touch a cell retail walks on but retail forbids (leaks).
/// Recall misses are mostly hand authoring: unreachable plateaus set to −1
/// and the ship chunks' water plane (414–418), which retail sets to 0.
#[test]
fn baked_heights_agree_with_retail() {
    use lt_core::edit::bake;
    let Some(d) = disc() else { return };
    // [overworld, interior] = (retail steps, also baked, leaks, baked steps)
    let mut acc = [(0u64, 0u64, 0u64, 0u64); 2];
    for id in 0..CHUNK_COUNT {
        let c = Chunk::parse(&d.chunk(id)).unwrap();
        let (b, h) = (bake::bake(&c), &c.heights);
        let mut walk = vec![false; h.len()];
        let edges: Vec<(usize, usize)> = (0..128 * 128)
            .flat_map(|i| [(i % 128 < 127).then(|| (i, i + 1)), (i / 128 < 127).then(|| (i, i + 128))])
            .flatten()
            .collect();
        for &(i, j) in &edges {
            if step(h[i], h[j]) {
                walk[i] = true;
                walk[j] = true;
            }
        }
        let a = &mut acc[(id >= 324) as usize];
        for &(i, j) in &edges {
            let (r, k) = (step(h[i], h[j]), step(b[i], b[j]));
            a.0 += r as u64;
            a.1 += (r && k) as u64;
            a.2 += (k && !r && (walk[i] || walk[j])) as u64;
            a.3 += k as u64;
        }
    }
    let pct = |n: u64, d: u64| 100.0 * n as f64 / d as f64;
    let (ow, int) = (acc[0], acc[1]);
    let (ow_recall, int_recall) = (pct(ow.1, ow.0), pct(int.1, int.0));
    let (ow_leak, int_leak) = (pct(ow.2, ow.3), pct(int.2, int.3));
    println!("overworld recall {ow_recall:.1}% leaks {ow_leak:.2}% · interior recall {int_recall:.1}% leaks {int_leak:.2}%");
    assert!(ow_recall > 98.0 && int_recall > 96.0, "recall {ow_recall:.1} / {int_recall:.1}");
    assert!(ow_leak < 0.5 && int_leak < 0.5, "leaks {ow_leak:.2} / {int_leak:.2}");
}
