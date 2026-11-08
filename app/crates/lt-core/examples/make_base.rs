//! Makes a base package (research 06 §6): a BPS from the retail image to a
//! base image built from it, and `base.json` with the source and target
//! filled in. How a HackerGC release (phase 3) is packaged.
//!
//! ```text
//! cargo run --release --example make_base -- <retail.bin> <base.bin> <out_dir> <id> <version>
//!     [--name "Name"] [--new-area-first N] [--flags flags.json] [--savestate boot.sstate]
//!     [--reserve-flags 0x0EE-0x0FF,...] [--demo]
//! ```
//!
//! `--demo` first writes `<base.bin>` itself: retail with chunk 326's bytes
//! appended as chunk 1328 (new areas from 1329), a stand-in base for tests.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use lt_core::base;
use lt_core::cdrom;
use lt_core::world::CHUNK_SECTORS;
use lt_core::{DiscImage, FileTable, PsxExe};

fn demo_image(retail: &Path, out: &Path) {
    let image = DiscImage::open(retail).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let world = table.by_id("world").unwrap().extent.lba;
    let chunk = image.read_sectors(world + 326 * CHUNK_SECTORS, CHUNK_SECTORS).unwrap();
    let at = world + 1328 * CHUNK_SECTORS;
    std::fs::copy(retail, out).unwrap();
    let mut f = std::fs::OpenOptions::new().append(true).open(out).unwrap();
    for lba in image.sectors()..at {
        f.write_all(&cdrom::postgap(lba)).unwrap();
    }
    for k in 0..CHUNK_SECTORS {
        let mut raw = cdrom::blank(at + k, if k == CHUNK_SECTORS - 1 { cdrom::SUB_LAST } else { cdrom::SUB_DATA });
        cdrom::encode_form1(&mut raw, &chunk[k as usize * 2048..(k as usize + 1) * 2048]);
        f.write_all(&raw).unwrap();
    }
    eprintln!("demo base image: retail + chunk 1328 (a copy of 326) at LBA {at}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if pos.len() < 5 {
        eprintln!("usage: make_base <retail.bin> <base.bin> <out_dir> <id> <version> [--name N] [--new-area-first N] [--flags F] [--savestate S] [--reserve-flags R] [--demo]");
        std::process::exit(2);
    }
    // Flag values aren't positional.
    let values: Vec<String> = ["--name", "--new-area-first", "--flags", "--savestate", "--reserve-flags"].iter().filter_map(|f| flag(f)).collect();
    let pos: Vec<&String> = pos.into_iter().filter(|p| !values.contains(p)).collect();
    let (retail, image, out) = (PathBuf::from(pos[0]), PathBuf::from(pos[1]), PathBuf::from(pos[2]));
    if args.iter().any(|a| a == "--demo") {
        demo_image(&retail, &image);
    }
    let mut m = base::manifest_template(pos[3], pos[4]);
    m.name = flag("--name").or_else(|| args.iter().any(|a| a == "--demo").then(|| "Demo base".into()));
    m.layout.new_area_first = flag("--new-area-first").and_then(|v| v.parse().ok()).or_else(|| args.iter().any(|a| a == "--demo").then_some(1329));
    if let Some(r) = flag("--reserve-flags") {
        m.reserved.flags = r.split(',').map(String::from).collect();
    }
    let mut extras = None;
    for (key, slot) in [("--flags", 0), ("--savestate", 1)] {
        if let Some(p) = flag(key).map(PathBuf::from) {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if slot == 0 {
                m.catalogues.flags = Some(name);
            } else {
                m.savestates.boot = Some(name);
            }
            extras = p.parent().map(Path::to_path_buf);
        }
    }
    let t = std::time::Instant::now();
    let pkg = base::make_package(&retail, &image, &out, m, extras.as_deref(), &AtomicBool::new(false)).unwrap();
    let bps = std::fs::metadata(pkg.file(&pkg.manifest.patch.bps)).unwrap().len();
    println!(
        "{} in {}: {} ({bps} bytes), target SHA-1 {}, new areas from {} ({:?})",
        pkg.manifest.tag(),
        out.display(),
        pkg.manifest.patch.bps,
        pkg.manifest.patch.target_sha1,
        pkg.manifest.new_area_first(),
        t.elapsed()
    );
}
