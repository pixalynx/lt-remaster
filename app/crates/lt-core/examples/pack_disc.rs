//! Writes a disc image with a content pack in it, without the app: the pack's files are built from
//! the image's own bytes and the pack's patches (from a folder, as `make_pack` writes them), staged
//! and written with the app's build code. A harness for packs, on retail or on a base's image
//! (docs/research/11-play-as.md).
//!
//! ```text
//! cargo run -p lt-core --example pack_disc -- <disc.bin> <pack id> <patch dir> <out.bin> [--base base.json --retail retail.bin] [--no-exe]
//! ```
//!
//! `--no-exe`: leave out the pack's executable writes (an A/B test).
//! `--base base.json --retail retail.bin`: the image is a base's (a HackerGC build); its
//! `profile.exeLayout` is registered first so the shifted executable's file table reads, and the
//! patches read their sources from the retail disc, as the app does on a base. The `.cue` is
//! written beside the image.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::pack::{self, PackFile};
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let retail_disc = args.iter().position(|a| a == "--retail").map(|i| {
        let path = args.remove(i + 1);
        args.remove(i);
        PathBuf::from(path)
    });
    // `--no-exe`: leave the executable alone (an A/B test of a pack's executable writes).
    let no_exe = args.iter().position(|a| a == "--no-exe").map(|i| args.remove(i)).is_some();
    if let Some(i) = args.iter().position(|a| a == "--base") {
        let path = args.remove(i + 1);
        args.remove(i);
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if let Some(l) = m.pointer("/profile/exeLayout") {
            lt_core::layout::register(serde_json::from_value(l.clone()).unwrap());
        }
    }
    if args.len() != 4 {
        eprintln!("usage: pack_disc <disc.bin> <pack id> <patch dir> <out.bin> [--base base.json]");
        std::process::exit(2);
    }
    let (disc, id, dir, out) = (PathBuf::from(&args[0]), &args[1], PathBuf::from(&args[2]), PathBuf::from(&args[3]));
    let image = DiscImage::open(&disc).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let src = Source { image: &image, exe: &exe, table: &table };
    let p = pack::find(id).unwrap_or_else(|| panic!("no built-in pack {id}"));
    // The retail disc, for the patches' sources (the image itself when it's retail).
    let rimage = retail_disc.as_ref().map(|r| DiscImage::open(r).unwrap());
    let rexe = rimage.as_ref().map(|i| PsxExe::from_disc(i).unwrap());
    let rtable = rimage.as_ref().zip(rexe.as_ref()).map(|(i, e)| FileTable::parse(e, i.sectors()).unwrap());
    let rsrc = rimage.as_ref().zip(rexe.as_ref()).zip(rtable.as_ref()).map(|((image, exe), table)| Source { image, exe, table });
    let retail = |f: &str| rsrc.as_ref().unwrap_or(&src).retail(f).ok();
    let base = |f: &str| src.retail(f).ok();
    let patches = |f: &PackFile| std::fs::read(dir.join(&f.patch)).ok().filter(|b| pack::patch_ok(f, b));
    eprintln!("{id} on this image: {:?}", p.state(exe.release, &retail, &base, &base, &patches));
    let built = p.build(&retail, &patches).unwrap_or_else(|e| panic!("{}: {}", e.code, e.message));
    let mut staged: Vec<Staged> = built.into_iter().map(|(id, b)| Staged { id, bytes: Arc::new(b), base_sha256: None }).collect();
    if !p.manifest.exe_writes.is_empty() && !no_exe {
        let exe = p.exe_write(&src.retail("exe").unwrap(), true).unwrap_or_else(|e| panic!("{}: {}", e.code, e.message));
        staged.push(Staged { id: "exe".into(), bytes: Arc::new(exe), base_sha256: None });
    }
    let plan = build::plan(&src, &staged, |_| true, PlanOptions::default());
    assert!(!plan.blocked_included, "{:?}", plan.items.iter().filter_map(|i| i.refusal.as_ref()).collect::<Vec<_>>());
    let cancel = AtomicBool::new(false);
    let rep = build::write_image(&src, &plan, &staged, &out, &cancel, &mut |_| {}).unwrap();
    build::verify_image(&out, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
    let name = out.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::write(out.with_extension("cue"), build::cue_sheet(&name)).unwrap();
    for s in &staged {
        let item = plan.items.iter().find(|i| i.id == s.id);
        eprintln!("  {:9} {:7} bytes{}", s.id, s.bytes.len(), item.map_or(String::new(), |i| format!("  {:?}{}", i.verdict, i.new_lba.map_or(String::new(), |l| format!(" at LBA {l}")))));
    }
    eprintln!("wrote {}", out.display());
}
