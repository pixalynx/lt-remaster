//! A new game that starts in a project's new area, on a HackerGC base with a boot map (LT-14,
//! `edit::boot`): what the overview's start dialog does, as a disc image for PCSX-Redux.
//!
//! ```text
//! cargo run --release --example boot_area_demo -- <retail.bin> <base package dir> <out.bin> [pack] [--control]
//! ```
//!
//! The project makes the base's first new area (flat ground, `Chunk::new_area`) and moves the
//! start to its middle, facing down. The pack (default npc/23) is named for the area in the
//! base's area table and gets the starter scene on the spot. A new game should then arrive in
//! the area with the pack loaded: the starter scene, which lives in that pack, shows the Knight.
//! `--control` leaves the area table alone: no pack loads, the scene never runs, and the Knight
//! stays hidden.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::base::{self, BasePackage};
use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::edit::blank;
use lt_core::edit::boot::{self, Boot, EIGHTH};
use lt_core::edit::chunk::Chunk;
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let control = args.iter().position(|a| a == "--control").map(|i| args.remove(i)).is_some();
    if args.len() < 3 {
        eprintln!("usage: boot_area_demo <retail.bin> <base package dir> <out.bin> [pack] [--control]");
        std::process::exit(2);
    }
    let (retail, dir, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    let pack: u16 = args.get(3).map_or(23, |p| p.parse().unwrap());
    let pkg = BasePackage::open(&dir).unwrap();
    let cache = out.with_extension("cache");
    let cancel = AtomicBool::new(false);
    let mat = base::materialise(&pkg, &retail, &cache, &cancel, |_, _| {}).unwrap();
    let image = DiscImage::open(&mat.image).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let src = Source { image: &image, exe: &exe, table: &table };

    let area = pkg.manifest.new_area_first() as u16;
    let ground = Chunk::new_area(area as u32).serialize().unwrap();
    let (x, z) = (0, 0);
    let h = boot::ground(&ground, x, z).expect("the new area's middle is walkable");
    let base_exe = src.retail("exe").unwrap();
    assert!(boot::supported(&base_exe), "{} has no boot map (a HackerGC v1 base built with src/hgc/hgc_boot.c)", pkg.manifest.tag());
    let was = boot::read(&base_exe).unwrap();
    let start = Boot { chunk: area, x, h, z, facing: 4 * EIGHTH };
    let mut moved = boot::set(&base_exe, &start).unwrap();
    if !control {
        moved = boot::set_area_pack(&moved, area, Some(pack)).unwrap();
    }
    // The base's pack is retail's plus the blank slate, which the pack encoders read against
    // retail's (LT-42), as the app's start command does.
    let npc = format!("npc/{pack}");
    let pack_bytes = src.retail(&npc).unwrap();
    let r_image = DiscImage::open(&retail).unwrap();
    let r_exe = PsxExe::from_disc(&r_image).unwrap();
    let r_table = FileTable::parse(&r_exe, r_image.sectors()).unwrap();
    let retail_pack = Source { image: &r_image, exe: &r_exe, table: &r_table }.retail(&npc).unwrap();
    let (with_starter, slot) = blank::starter_at(&retail_pack, &pack_bytes, area, (x, h, z), 4, &[area as u32]).unwrap();
    eprintln!(
        "{}: start chunk/{} -> chunk/{area} ({x}, {h}, {z}), {npc} {} the area table, starter scene director {slot}",
        pkg.manifest.tag(),
        was.chunk,
        if control { "left out of" } else { "named in" }
    );
    let staged = vec![
        Staged { id: format!("chunk/{area}"), bytes: Arc::new(ground), base_sha256: None },
        Staged { id: "exe".into(), bytes: Arc::new(moved), base_sha256: None },
        Staged { id: npc, bytes: Arc::new(with_starter), base_sha256: None },
    ];
    let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, new_area_first: area as u32 });
    assert!(!plan.blocked_included, "{:?}", plan.items.iter().filter_map(|i| i.refusal.as_ref()).collect::<Vec<_>>());
    let rep = build::write_image(&src, &plan, &staged, &out, &cancel, &mut |_| {}).unwrap();
    build::verify_image(&out, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
    drop(image);
    let built = DiscImage::open(&out).unwrap();
    let bexe = PsxExe::from_disc(&built).unwrap();
    let b = boot::read(&bexe.bytes).unwrap();
    println!("{}: starts on chunk/{} ({}, {}, {}), area packs {:?}", out.display(), b.chunk, b.x, b.h, b.z, boot::area_packs(&bexe.bytes));
}
