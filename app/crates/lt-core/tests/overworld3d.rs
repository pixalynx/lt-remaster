//! Overworld 3D scene checks (screen 09, 3D mode, "Overworld" scope) against
//! the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test overworld3d -- --nocapture`.
//! `LT_TEST_OUT=<dir>` also writes the payload (`overworld.bin`).

use std::time::Instant;

use lt_core::overworld::{self, Edge, EDGE_SAMPLES, NO_SURFACE};
use lt_core::world;
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, table })
}

impl Disc {
    fn chunk(&self, id: u32) -> Option<Vec<u8>> {
        let w = self.table.by_id("world")?;
        self.image.read_sectors(w.extent.lba + id * world::CHUNK_SECTORS, world::CHUNK_SECTORS).ok()
    }
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn lei32(b: &[u8], o: usize) -> i32 {
    le32(b, o) as i32
}

#[test]
fn origins_follow_the_brick_grid() {
    // PRD 09 §4.1: chunk/39 → grid (3, 3), world (6144, −12288); chunk/42 → grid (6, 2).
    assert_eq!(overworld::origin(39), Some((6144, -12288)));
    assert_eq!(overworld::origin(42), Some((12288, -8192)));
    assert_eq!(overworld::origin(324), None);
    // Edge sample ends are the chunk corners.
    assert_eq!(Edge::North.point(0), (-2048, 2048));
    assert_eq!(Edge::North.point(EDGE_SAMPLES - 1), (2048, 2048));
    assert_eq!(Edge::West.point(0), (-2048, -2048));
}

#[test]
fn overworld_payload() {
    let Some(d) = disc() else { return };
    let t = Instant::now();
    let chunks = overworld::build(|id| d.chunk(id));
    let built = t.elapsed();
    let t = Instant::now();
    let bytes = overworld::encode(&chunks);
    let encoded = t.elapsed();
    if let Some(dir) = std::env::var_os("LT_TEST_OUT") {
        std::fs::write(std::path::PathBuf::from(dir).join("overworld.bin"), &bytes).unwrap();
    }

    assert_eq!(chunks.len(), 324);
    assert!(chunks.iter().enumerate().all(|(i, c)| c.id == i as u32));
    let nv: usize = chunks.iter().map(|c| c.verts.len()).sum();
    let nq: usize = chunks.iter().map(|c| c.quads.len()).sum();
    let nt: usize = chunks.iter().map(|c| c.tris.len()).sum();
    let largest = chunks.iter().map(|c| c.verts.len()).max().unwrap();
    eprintln!(
        "overworld: built in {built:?}, encoded in {encoded:?}: {nv} vertices (largest chunk {largest}), {nq} quads + {nt} tris = {} polygons, {} triangles, payload {} B",
        nq + nt,
        nq * 2 + nt,
        bytes.len()
    );
    assert!(largest < u16::MAX as usize, "local indices fit u16");

    // Every chunk's polygon count matches the archive parser.
    for c in &chunks {
        let raw = d.chunk(c.id).unwrap();
        let s = world::stats(c.id, &raw).unwrap();
        assert_eq!(c.polygons() as u32, s.polygons, "chunk/{} polygons", c.id);
    }

    // Header and per-chunk records.
    assert_eq!(&bytes[0..4], b"LTOW");
    assert_eq!(le16(&bytes, 6), 324);
    assert_eq!(le32(&bytes, 8) as usize, nv);
    let ni = le32(&bytes, 12) as usize;
    assert_eq!(ni, nq * 4 + nt * 3);
    assert_eq!(le16(&bytes, 16) as usize, EDGE_SAMPLES);
    let expect = 32 + 324 * 48 + nv * 8 + ni.div_ceil(2) * 4 + 324 * 4 * EDGE_SAMPLES * 2;
    assert_eq!(bytes.len(), expect);
    let mut first_v = 0u32;
    for (i, c) in chunks.iter().enumerate() {
        let r = 32 + i * 48;
        let (gx, gy) = world::grid_cell(c.id).unwrap();
        assert_eq!(le16(&bytes, r) as u32, c.id);
        assert_eq!((bytes[r + 2] as u32, bytes[r + 3] as u32), (gx, gy), "chunk/{} grid", c.id);
        // Positions match grid_cell: wx = 2048·gx, wz = −4096·gy.
        assert_eq!((lei32(&bytes, r + 4), lei32(&bytes, r + 8)), (2048 * gx as i32, -4096 * gy as i32), "chunk/{} origin", c.id);
        assert_eq!(le32(&bytes, r + 12), first_v);
        assert_eq!(le32(&bytes, r + 16) as usize, c.verts.len());
        first_v += c.verts.len() as u32;
    }
    // Vertices carry their chunk id.
    let vbase = 32 + 324 * 48;
    for (i, c) in chunks.iter().enumerate().step_by(37) {
        let first = le32(&bytes, 32 + i * 48 + 12) as usize;
        if !c.verts.is_empty() {
            assert_eq!(le16(&bytes, vbase + first * 8 + 6) as u32, c.id);
        }
    }

    // Geometry stays within the chunk extent (±0x800, a few units of slack).
    let outside: Vec<u32> = chunks
        .iter()
        .filter(|c| !c.verts.is_empty() && (c.bbox[0] < -2100 || c.bbox[3] > 2100 || c.bbox[2] < -2100 || c.bbox[5] > 2100))
        .map(|c| c.id)
        .collect();
    eprintln!("chunks reaching past ±2100: {outside:?}");
    let measured = chunks.iter().filter(|c| c.ground_measured).count();
    eprintln!("ground measured at the centre on {measured} of 324 chunks");
}

/// Neighbours share their edge: the top surface on both sides lines up.
#[test]
fn seams_line_up() {
    let Some(d) = disc() else { return };
    let chunks = overworld::build(|id| d.chunk(id));
    let at = |gx: i32, gy: i32| -> &overworld::ChunkLod {
        let id = world::chunk_at(gx.rem_euclid(36) as u32, gy.rem_euclid(18) as u32).unwrap();
        &chunks[id as usize]
    };
    let half = EDGE_SAMPLES / 2;
    // (this edge, its samples) vs (neighbour edge, its samples).
    let mut pairs = 0usize;
    let mut both = 0usize;
    let mut close = 0usize;
    let mut diffs: Vec<i32> = Vec::new();
    let mut worst: Vec<(i32, u32, u32, usize)> = Vec::new();
    let mut compare = |a: &overworld::ChunkLod, ea: Edge, ia: usize, b: &overworld::ChunkLod, eb: Edge, ib: usize| {
        pairs += 1;
        let (ya, yb) = (a.edges[ea as usize][ia], b.edges[eb as usize][ib]);
        if ya == NO_SURFACE || yb == NO_SURFACE {
            return;
        }
        both += 1;
        let d = (ya as i32 - yb as i32).abs();
        diffs.push(d);
        if d <= 16 {
            close += 1;
        } else {
            worst.push((d, a.id, b.id, ia));
        }
    };
    for c in &chunks {
        let (gx, gy) = world::grid_cell(c.id).unwrap();
        let (gx, gy) = (gx as i32, gy as i32);
        // East edge vs the east neighbour's west edge (x + 2, same row), sample for sample.
        let e = at(gx + 2, gy);
        for i in 0..EDGE_SAMPLES {
            compare(c, Edge::East, i, e, Edge::West, i);
        }
        // North edge: west half against the NW neighbour's south edge east half; east half against NE's west half.
        let nw = at(gx - 1, gy - 1);
        let ne = at(gx + 1, gy - 1);
        for i in 0..=half {
            compare(c, Edge::North, i, nw, Edge::South, i + half);
            compare(c, Edge::North, i + half, ne, Edge::South, i);
        }
    }
    diffs.sort_unstable();
    let pct = |p: f64| diffs.get(((diffs.len() as f64 - 1.0) * p) as usize).copied().unwrap_or(0);
    worst.sort_unstable_by(|a, b| b.cmp(a));
    worst.dedup_by_key(|w| (w.1, w.2));
    eprintln!(
        "seams: {pairs} shared samples, {both} with a surface on both sides, {close} within 16 units ({:.1}%); |Δy| p50 {} p90 {} p99 {} max {}; worst {:?}",
        close as f64 * 100.0 / both.max(1) as f64,
        pct(0.5),
        pct(0.9),
        pct(0.99),
        diffs.last().copied().unwrap_or(0),
        &worst[..worst.len().min(12)]
    );
    // Seams of authored neighbours meet: most shared samples agree to within 16 units.
    assert!(both > pairs / 2, "most seam samples have ground on both sides");
    assert!(close as f64 >= both as f64 * 0.9, "≥ 90 % of seam samples line up");
}
