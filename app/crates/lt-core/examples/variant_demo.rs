//! A chunk variant on a HackerGC base (`edit::variants`), as a disc image for PCSX-Redux:
//! chunk 42's broken bridge is read from a mended copy while flag 0x200 is set, and the
//! signpost by its west end asks to fix it.
//!
//! ```text
//! cargo run --release --example bridge_spike -- <retail.bin> mend <mended.chunk>
//! cargo run --release --example variant_demo -- <retail.bin> <base package dir> <mended.chunk> <out.bin> [--control]
//! ```
//!
//! - The mended copy goes into the base's first new area in chunk 42's VRAM slot, and the
//!   variant table names it for chunk 42 while 0x200 is set.
//! - A new game starts beside the signpost, facing it (the starter scene in npc/1 shows the
//!   Knight there).
//! - The sign is an examine spot whose program asks, sets 0x200 and warps to the same spot: the
//!   warp drops the resident chunk 42, which reads the variant behind the fade.
//! - `--control` leaves the table alone: the sign sets the flag, and the bridge stays broken.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::base::{self, BasePackage};
use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::edit::actor::{self, AddedActor};
use lt_core::edit::blank;
use lt_core::edit::boot::{self, Boot, EIGHTH};
use lt_core::edit::variants::{self, Variant};
use lt_core::tier2::dsl;
use lt_core::{DiscImage, FileTable, PsxExe};

const CHUNK: u16 = 42;
const PACK: u32 = 1;
const FLAG: u16 = 0x200;
/// The signpost (a blocked cell) and the floor beside it, where the Knight stands facing west.
const SIGN: (i16, i16) = (-944, -1424);
const STAND: (i16, i16) = (-880, -1424);
const WEST: u16 = 6;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let control = args.iter().position(|a| a == "--control").map(|i| args.remove(i)).is_some();
    if args.len() < 4 {
        eprintln!("usage: variant_demo <retail.bin> <base package dir> <mended.chunk> <out.bin> [--control]");
        std::process::exit(2);
    }
    let (retail, dir, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]), PathBuf::from(&args[3]));
    let mended = std::fs::read(&args[2]).unwrap();
    let pkg = BasePackage::open(&dir).unwrap();
    let cache = out.with_extension("cache");
    let cancel = AtomicBool::new(false);
    let mat = base::materialise(&pkg, &retail, &cache, &cancel, |_, _| {}).unwrap();
    let image = DiscImage::open(&mat.image).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let src = Source { image: &image, exe: &exe, table: &table };
    let r_image = DiscImage::open(&retail).unwrap();
    let r_exe = PsxExe::from_disc(&r_image).unwrap();
    let r_table = FileTable::parse(&r_exe, r_image.sectors()).unwrap();
    let retail_src = Source { image: &r_image, exe: &r_exe, table: &r_table };

    // The variant: the first new area in chunk 42's slot (its texture page uploads there).
    let first = pkg.manifest.new_area_first();
    let area = (first..).find(|n| n & 3 == CHUNK as u32 & 3).unwrap() as u16;
    let base_exe = src.retail("exe").unwrap();
    assert!(variants::supported(&base_exe), "{} has no chunk variant table (src/hgc/hgc_variants.c)", pkg.manifest.tag());
    let ground = |x, z| boot::ground(&src.retail(&format!("chunk/{CHUNK}")).unwrap(), x, z).unwrap();
    let (sx, sz) = STAND;
    let start = Boot { chunk: CHUNK, x: sx, h: ground(sx, sz), z: sz, facing: WEST * EIGHTH };
    let mut exe_bytes = boot::set(&base_exe, &start).unwrap();
    if !control {
        exe_bytes = variants::put(&exe_bytes, Variant { chunk: CHUNK, flag: FLAG, variant: area }).unwrap();
    }

    // npc/1: the starter scene on the start, and the sign.
    let npc = format!("npc/{PACK}");
    let retail_pack = retail_src.retail(&npc).unwrap();
    let (with_starter, director) = blank::starter_at(&retail_pack, &src.retail(&npc).unwrap(), CHUNK, (start.x, start.h, start.z), WEST as u8, &[area as u32]).unwrap();
    let program = format!(
        "say3 \"The bridge is broken.{{nl}}Do you want to{{nl}}fix it?\"\nif yes {{\n  set 0x{FLAG:03X}\n  warp facing:{WEST} chunk/{CHUNK} x:{sx} z:{sz} flags:0x0000\n}}\n"
    );
    let (own, diags) = dsl::parse(&program);
    assert!(diags.iter().all(|d| d.severity != dsl::Severity::Error), "{diags:?}");
    let mut added = actor::recover(&retail_pack, &with_starter).unwrap();
    let (gx, gz) = SIGN;
    added.push(AddedActor {
        from: actor::NEW_SPOT,
        home: CHUNK,
        x: gx,
        h: start.h,
        z: gz,
        dir: 0,
        hidden: false,
        own: Some(own),
        character: None,
        spot: true,
        branches: Vec::new(),
        otherwise_hidden: false,
        walk: Vec::new(),
        house: None,
        smoke: false,
    });
    let (pack_bytes, _) = actor::apply(&retail_pack, &with_starter, &added).unwrap();
    eprintln!(
        "{}: chunk/{CHUNK} reads chunk/{area} while 0x{FLAG:03X} is set{}; start ({sx}, {}, {sz}) facing the sign, starter scene director {director}, sign at ({gx}, {gz})",
        pkg.manifest.tag(),
        if control { " (control: not in the table)" } else { "" },
        start.h
    );

    let staged = vec![
        Staged { id: format!("chunk/{area}"), bytes: Arc::new(mended), base_sha256: None },
        Staged { id: "exe".into(), bytes: Arc::new(exe_bytes), base_sha256: None },
        Staged { id: npc, bytes: Arc::new(pack_bytes), base_sha256: None },
    ];
    let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, new_area_first: first });
    assert!(!plan.blocked_included, "{:?}", plan.items.iter().filter_map(|i| i.refusal.as_ref()).collect::<Vec<_>>());
    let rep = build::write_image(&src, &plan, &staged, &out, &cancel, &mut |_| {}).unwrap();
    build::verify_image(&out, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
    drop(image);
    let built = DiscImage::open(&out).unwrap();
    let bexe = PsxExe::from_disc(&built).unwrap();
    println!("{}: starts on chunk/{}, variants {:?}", out.display(), boot::read(&bexe.bytes).unwrap().chunk, variants::list(&bexe.bytes));
}
