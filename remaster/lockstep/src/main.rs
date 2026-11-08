//! `lt-lockstep`: LT-105's checker over the test set (`lt-testset build` makes one).
//!
//! ```text
//! lt-lockstep check <disc.bin> <set> [scene…]          observe hooks must change nothing, in every scene
//! lt-lockstep plant <disc.bin> <set> [scene] [frame]   a planted byte must be found, named and traced
//! lt-lockstep masks <disc.bin> <set> [scene]           what a masked render replacement looks like
//! ```
//!
//! **check**: side A runs the original code; side B the same with 13 observe hooks (the draw
//! functions, the field's chunk streaming, the music sequencer run from VSync, a battle function
//! and the battle camera in OVR04, the OVR02 ones guarded by their words). Every scene must stay
//! identical after every frame, and A must end on the test set's digest.
//!
//! **plant**: B's hook on `Field_ChunkStreamTask` flips one bit of the Knight's `talk` byte at the
//! given frame (village, frame 300 by default). The report must name that frame, the field
//! `CHARACTERS[0].talk` and the hook. Then the Knight's `x` is planted and masked (reason:
//! planted): the game's own code carries the difference on, and the report must name the function
//! whose store made it.
//!
//! **masks**: B replaces `WorldChunk_DrawObjectMesh` (no terrain packets), compared with
//! [`lt_lockstep::RENDER_MASKS`]: the report lists the masks with what differed in them, and the
//! first difference they don't cover.
//!
//! Exits 1 when a check fails.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use lt_lockstep::gc::{self, KNIGHT, PLANT, TALK, X};
use lt_lockstep::{original, run, Mask, RENDER_MASKS};
use lt_testset::{Image, Scene, SCENES};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "lt-lockstep check|plant|masks <disc.bin> <set> […]";
    if args.len() < 3 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let result = (|| -> Result<bool, String> {
        let image = Image::open(&PathBuf::from(&args[1]))?;
        let set = PathBuf::from(&args[2]);
        let rest = &args[3..];
        match args[0].as_str() {
            "check" => check(&image, &set, rest),
            "plant" => plant(&image, &set, rest),
            "masks" => masks(&image, &set, rest),
            _ => Err(usage.into()),
        }
    })();
    match result {
        Ok(true) => println!("PASS"),
        Ok(false) => {
            println!("FAIL");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("lt-lockstep: {e}");
            std::process::exit(2);
        }
    }
}

fn check(image: &Arc<Image>, set: &Path, rest: &[String]) -> Result<bool, String> {
    let observe = gc::observer(gc::ovr02_words(image, set)?);
    let names: Vec<String> = if rest.is_empty() { SCENES.iter().map(|s| s.to_string()).collect() } else { rest.to_vec() };
    let (mut ok, mut total, t0) = (true, 0, Instant::now());
    for name in names {
        let (scene, state) = Scene::load(set, &name)?;
        let t = Instant::now();
        let r = run(image, &scene, &state, &original, &observe, &[])?;
        let digest_ok = r.digest_a == scene.digest;
        let fired: u64 = r.hook_calls.iter().map(|(_, n)| n).sum();
        print!("{}", r.text());
        println!("  A's digest {:016x} {} the test set's ({:.1} s)", r.digest_a, if digest_ok { "matches" } else { "DOESN'T MATCH" }, t.elapsed().as_secs_f64());
        ok &= r.divergence.is_none() && digest_ok && fired > 0;
        total += r.frames;
    }
    println!("{total} frames ({} ticks) in {:.0} s", total / 2, t0.elapsed().as_secs_f64());
    Ok(ok)
}

fn plant(image: &Arc<Image>, set: &Path, rest: &[String]) -> Result<bool, String> {
    let name = rest.first().map(String::as_str).unwrap_or("village");
    let from: u64 = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    let (scene, state) = Scene::load(set, name)?;

    println!("1. One bit of CHARACTERS[0].talk flipped by B's hook from frame {from}:");
    let fired = Rc::new(Cell::new(None));
    let r = run(image, &scene, &state, &original, &gc::planter(KNIGHT + TALK, 0x01, from, fired.clone()), &[])?;
    print!("{}", r.text());
    let at = fired.get();
    let found = r.divergence.as_ref().is_some_and(|d| {
        Some(d.frame) == at
            && d.addr == KNIGHT + TALK
            && d.field.as_deref() == Some("CHARACTERS[0].talk")
            && d.cause.what.starts_with(&format!("B: hook {PLANT} wrote it"))
    });
    println!("  planted after frame {at:?}; found there, with its field and writer: {}\n", yes(found));

    println!("2. The Knight's x moved 16 units by B's hook from frame {from}, x masked: the game's code carries it on.");
    let fired = Rc::new(Cell::new(None));
    let masks = [Mask { name: "planted", addr: KNIGHT + X, len: 2, why: "the planted difference itself (the Knight's x), to see what the game's own code does with it" }];
    let r = run(image, &scene, &state, &original, &gc::planter(KNIGHT + X, 0x10, from, fired.clone()), &masks)?;
    print!("{}", r.text());
    // "A and B stored it in Model_DrawParts+0x564 (pc …)": a store, in a function gc-sys names.
    let carried = r.divergence.as_ref().is_some_and(|d| {
        fired.get().is_some_and(|f| d.frame >= f) && d.cause.what.split(" stored it in ").nth(1).is_some_and(|f| !f.starts_with("0x"))
    });
    let used = r.masks.first().is_some_and(|m| m.1 > 0);
    println!("  carried on by a named function's store: {}; the mask listed and used: {}", yes(carried), yes(used));
    Ok(found && carried && used)
}

fn masks(image: &Arc<Image>, set: &Path, rest: &[String]) -> Result<bool, String> {
    let name = rest.first().map(String::as_str).unwrap_or("village");
    let (scene, state) = Scene::load(set, name)?;
    println!("B replaces WorldChunk_DrawObjectMesh (no terrain packets), compared with the render masks:");
    let r = run(image, &scene, &state, &original, &gc::terrain_skipper, RENDER_MASKS)?;
    print!("{}", r.text());
    let listed = r.masks.len() == RENDER_MASKS.len() && r.masks.iter().all(|m| !m.2.is_empty());
    println!("  every mask listed with its reason: {}", yes(listed));
    Ok(listed && r.masks.iter().any(|m| m.1 > 0))
}

fn yes(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "NO"
    }
}
