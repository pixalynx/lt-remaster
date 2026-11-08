//! Export sizes against the user's own retail image (screen 05, Export raw).
//! Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test files`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use lt_core::extract::{content_len, shape_ok, SECTOR};
use lt_core::{DiscImage, FileTable, Kind, PsxExe};

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

#[test]
fn real_sizes_only_drop_zero_padding() {
    let Some(d) = disc() else { return };
    let t0 = Instant::now();
    // kind → (files, trimmed)
    let mut stats: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for e in &d.table.entries {
        if e.extent.sectors == 0 || matches!(e.kind, Kind::Movie | Kind::World) {
            continue;
        }
        let b = d.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
        assert!(shape_ok(e.kind, e.index, &b), "{}: retail fails its own shape check", e.id);
        let s = stats.entry(format!("{:?}", e.kind)).or_default();
        s.0 += 1;
        if let Some(len) = content_len(e.kind, e.index, &b) {
            assert!(len <= b.len() && b.len() - len < SECTOR, "{}: {len} of {}", e.id, b.len());
            assert!(b[len..].iter().all(|&x| x == 0), "{}: non-zero tail", e.id);
            // The trimmed file still needs every sector of the extent.
            assert_eq!(len.div_ceil(SECTOR), e.extent.sectors as usize, "{}", e.id);
            s.1 += 1;
        }
    }
    eprintln!("real sizes in {:?}: {stats:?}", t0.elapsed());
    let got = |k: &str| stats.get(k).copied().unwrap_or_default();
    // Every field texture set is a plain TIM; every music file ends in its TVB.
    assert_eq!(got("Fe"), (57, 57));
    assert_eq!(got("Bgm").0, got("Bgm").1);
    // One toy and the enemies whose last section isn't a model or the profile keep whole sectors.
    assert!(got("Toy").1 >= 72);
    assert!(got("Enemy").1 >= 90);
    // NPC packs and overlays have no stated length: whole sectors.
    assert_eq!(got("Npc").1, 0);
    assert_eq!(got("Ovr").1, 0);
}

/// `gc_decomp/disk/{TOY,NPC,OVR}` (carve_filetable.py, whole sectors) start
/// with exactly the bytes an export writes, followed only by zeros.
#[test]
fn exports_match_the_decomp_carve() {
    let Some(d) = disc() else { return };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../psx-dev/gc_decomp/disk");
    if !root.exists() {
        return;
    }
    let mut checked = 0;
    for e in &d.table.entries {
        let name = match e.kind {
            Kind::Toy => format!("TOY/TOY{:02}.BIN", e.index),
            Kind::Npc => format!("NPC/NPC{:02}.BIN", e.index),
            Kind::Ovr => format!("OVR/OVR{:02}.BIN", e.index + 1),
            _ => continue,
        };
        let Ok(carved) = std::fs::read(root.join(&name)) else { continue };
        let b = d.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
        let out = &b[..content_len(e.kind, e.index, &b).unwrap_or(b.len())];
        assert!(carved.starts_with(out), "{} differs from {name}", e.id);
        assert!(carved[out.len()..].iter().all(|&x| x == 0), "{name} has data past the export");
        checked += 1;
    }
    eprintln!("checked {checked} files against the decomp carve");
    assert!(checked >= 140, "only {checked} carved files found");
}
