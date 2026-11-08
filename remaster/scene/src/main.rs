//! `lt-scene`: LT-108's scene capture on the test set.
//!
//! ```text
//! lt-scene capture <disc.bin> <set> [scene] [out.jsonl]    records for a scene's tape, checked (AC1, AC3)
//! lt-scene match <disc.bin> <hooks.jsonl> <boot> <observe> against draw_hooks.rs's log of the same frames (AC2)
//! lt-scene cost <disc.bin> <set> [scene] [runs]             what the capture adds per tick (AC4)
//! ```
//!
//! For `match`, make the log with emu-host's example, from power-on with the same tape:
//! `cargo run --release --example draw_hooks -- <disc> <dir> <boot> <observe> <lt_scene::check::DRAW_HOOKS_ARGS…>`
//! (`lt-scene args` prints them).
//!
//! Exits 1 when a check fails.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use lt_scene::check::{capture_from_boot, capture_scene, compare, completeness, plain_scene, stability, DRAW_HOOKS_ARGS};
use lt_testset::{Image, Scene};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "lt-scene capture|match|cost|args …";
    if args.first().map(String::as_str) == Some("args") {
        println!("{}", DRAW_HOOKS_ARGS.join(" "));
        return;
    }
    if args.len() < 3 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let result = (|| -> Result<bool, String> {
        let image = Image::open(&PathBuf::from(&args[1]))?;
        match args[0].as_str() {
            "capture" => capture(&image, &args[2..]),
            "match" => matched(&image, &args[2..]),
            "cost" => cost(&image, &args[2..]),
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
            eprintln!("lt-scene: {e}");
            std::process::exit(2);
        }
    }
}

fn capture(image: &Arc<Image>, rest: &[String]) -> Result<bool, String> {
    let set = PathBuf::from(&rest[0]);
    let name = rest.get(1).map(String::as_str).unwrap_or("village");
    let (scene, state) = Scene::load(&set, name)?;
    let (records, took) = capture_scene(image, &scene, &state)?;
    if let Some(out) = rest.get(2) {
        let mut f = std::fs::File::create(out).map_err(|e| e.to_string())?;
        for r in &records {
            writeln!(f, "{}", serde_json::to_string(r).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        }
    }
    let n = records.len();
    let groups: usize = records.iter().map(|r| r.chunks.iter().map(|c| c.groups.len()).sum::<usize>()).sum();
    let chars: usize = records.iter().map(|r| r.characters.len()).sum();
    let verts: usize = records.iter().flat_map(|r| &r.characters).flat_map(|c| &c.parts).map(|p| p.vertices.len()).sum();
    let flames: usize = records.iter().map(|r| r.flames.len()).sum();
    println!(
        "{name}: {} frames, {n} records ({:.1} s): {:.1} groups, {:.1} characters, {:.0} vertices, {:.1} flames per tick",
        scene.tape.len(),
        took.as_secs_f64(),
        groups as f64 / n.max(1) as f64,
        chars as f64 / n.max(1) as f64,
        verts as f64 / n.max(1) as f64,
        flames as f64 / n.max(1) as f64
    );
    if let Some(r) = records.get(records.len() / 2) {
        let cam = r.camera.as_ref();
        println!(
            "  tick {} (frame {}): view {:?}, H {:?}, offset {:?}, wide {:?}",
            r.tick,
            r.frame,
            cam.map(|c| c.view),
            cam.map(|c| c.h),
            cam.map(|c| c.offset),
            cam.map(|c| c.wide)
        );
        for c in &r.chunks {
            println!("  chunk {} (slot {}) offset {:?}: {} groups", c.chunk, c.slot, c.offset, c.groups.len());
        }
        for c in &r.characters {
            let v: Vec<usize> = c.parts.iter().map(|p| p.vertices.len()).collect();
            println!("  character {} at {:?}: parts {:?} vertices, world t {:?}", c.index, c.position, v, c.parts[0].world.t);
        }
        println!("  torch pass {}, flames {:?}", r.torch_pass, r.flames.iter().map(|f| (f.index, f.position)).collect::<Vec<_>>());
    }
    // The village (LT-108's scene) runs at a steady 30 Hz with three chunks drawn; elsewhere a slow
    // tick can take three frames and an interior draws one chunk.
    let village = name == "village";
    let one_per_tick = n as u64 * 2 >= scene.tape.len() as u64 - 2 && n as u64 * 2 <= scene.tape.len() as u64 + 2;
    // Elsewhere, ticks that draw no field (a battle, a fade, loading) are counted, not checked.
    let field: Vec<lt_scene::Record> = records.iter().filter(|r| village || !r.chunks.is_empty()).cloned().collect();
    if !village {
        println!("  ticks drawing the field: {} of {n}", field.len());
    }
    let missing = completeness(&field, village.then_some(3));
    let (unstable, (ga, gn), (ca, cn)) = stability(&records);
    println!("  one record per tick, two frames each: {}", yes(one_per_tick));
    let chunks = if village { "3 chunks" } else { "chunks" };
    println!("  every record complete (camera, {chunks}, groups, characters with matrix and vertices): {} ({} problems)", yes(missing.is_empty()), missing.len());
    for m in missing.iter().take(5) {
        println!("    {m}");
    }
    println!(
        "  ids stable: {} ({} problems); {:.1}% of group draws and {:.1}% of character draws carry the last tick's id",
        yes(unstable.is_empty()),
        unstable.len(),
        100.0 * ga as f64 / gn.max(1) as f64,
        100.0 * ca as f64 / cn.max(1) as f64
    );
    for u in unstable.iter().take(5) {
        println!("    {u}");
    }
    Ok((one_per_tick || !village) && missing.is_empty() && unstable.is_empty())
}

fn matched(image: &Arc<Image>, rest: &[String]) -> Result<bool, String> {
    let log = std::fs::read_to_string(&rest[0]).map_err(|e| format!("{}: {e}", rest[0]))?;
    let boot: u64 = rest.get(1).and_then(|s| s.parse().ok()).ok_or("boot frames")?;
    let observe: u64 = rest.get(2).and_then(|s| s.parse().ok()).ok_or("observe frames")?;
    let records = capture_from_boot(image, boot, observe)?;
    let (ticks, diffs) = compare(&records, &log)?;
    println!("{ticks} ticks compared against draw_hooks.rs's log (frames {boot}..{}): {} differences", boot + observe, diffs.len());
    for d in diffs.iter().take(10) {
        println!("  {d}");
    }
    Ok(ticks > 0 && diffs.is_empty())
}

fn cost(image: &Arc<Image>, rest: &[String]) -> Result<bool, String> {
    let set = PathBuf::from(&rest[0]);
    let name = rest.get(1).map(String::as_str).unwrap_or("village");
    let runs: usize = rest.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    let (scene, state) = Scene::load(&set, name)?;
    let (mut with, mut without, mut ticks) = (f64::MAX, f64::MAX, 0);
    for _ in 0..runs {
        without = without.min(plain_scene(image, &scene, &state)?.as_secs_f64());
        let (records, t) = capture_scene(image, &scene, &state)?;
        with = with.min(t.as_secs_f64());
        ticks = records.len();
    }
    let added = (with - without) / ticks.max(1) as f64 * 1000.0;
    println!(
        "{name}: {} frames, {ticks} ticks; best of {runs}: {:.3} s plain, {:.3} s capturing; the capture adds {added:.3} ms per tick ({:.2} ms per tick in all)",
        scene.tape.len(),
        without,
        with,
        with / ticks.max(1) as f64 * 1000.0
    );
    Ok(added < 1.0)
}

fn yes(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "NO"
    }
}
