//! Gives a base package its boot save state (LT-11), or says what the state it has holds.
//!
//! ```text
//! cargo run --release --example base_savestate -- <package_dir> [<state>]
//! cargo run --release --example base_savestate -- <package_dir> --image <retail.bin> <out_dir>
//! ```
//!
//! `--image` applies the package to the retail image into `<out_dir>` (as Living Tool's
//! base cache does) and prints the `.cue` to boot in Redux, for a package that came alone.
//!
//! `<state>` is a PCSX-Redux save state taken on the image the package's BPS makes (the
//! `.bin` make_hackergc writes next to the package, or the base Living Tool materialised):
//! boot it, start a new game, wait for free roam, then either save a state from Redux's
//! menu (a gzipped `.sstate`) or fetch the raw one from its web server:
//!
//! ```text
//! curl -o boot.raw http://127.0.0.1:<port>/api/v1/state-raw
//! ```
//!
//! The state is stored gzipped as `boot.sstate` and named in `base.json`
//! (`savestates.boot`). Take it idle on a field map: states taken during the opening or a
//! transition have crashed Redux on load.

use std::path::PathBuf;

use lt_core::base::{self, BasePackage};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first().map(PathBuf::from) else {
        eprintln!("usage: base_savestate <package_dir> [<state>]");
        std::process::exit(2);
    };
    if args.get(1).is_some_and(|a| a == "--image") {
        let (Some(retail), Some(out)) = (args.get(2), args.get(3)) else {
            eprintln!("usage: base_savestate <package_dir> --image <retail.bin> <out_dir>");
            std::process::exit(2);
        };
        let pkg = BasePackage::open(&dir).expect("opening the package");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let m = base::materialise(&pkg, std::path::Path::new(retail), std::path::Path::new(out), &cancel, |_, _| {}).expect("applying the base");
        println!("{}: {} ({} sectors, SHA-1 {})", m.tag, m.cue.display(), m.sectors, m.sha1);
        return;
    }
    let pkg = match args.get(1) {
        Some(state) => base::set_boot_state(&dir, &std::fs::read(state).expect("reading the state")),
        None => BasePackage::open(&dir),
    }
    .unwrap_or_else(|e| {
        eprintln!("{}: {}", e.code, e.message);
        std::process::exit(1);
    });
    match pkg.boot_state() {
        Ok(Some(s)) => {
            let file = pkg.file(pkg.manifest.savestates.boot.as_deref().unwrap_or_default());
            let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            println!(
                "{}: {} ({bytes} bytes, {} raw), {}, saved on chunk {}",
                pkg.manifest.tag(),
                file.display(),
                s.raw().len(),
                s.version_string,
                s.chunk()
            );
        }
        Ok(None) => println!("{}: no boot save state", pkg.manifest.tag()),
        Err(e) => {
            eprintln!("{}: {}", e.code, e.message);
            std::process::exit(1);
        }
    }
}
