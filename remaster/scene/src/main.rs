//! `lt-scene`: LT-108's scene capture on the test set.
//!
//! ```text
//! lt-scene capture <disc.bin> <set> [scene] [out.jsonl]    records for a scene's tape, checked (AC1, AC3)
//! lt-scene match <disc.bin> <hooks.jsonl> <boot> <observe> against draw_hooks.rs's log of the same frames (AC2)
//! lt-scene cost <disc.bin> <set> [scene] [runs]             what the capture adds per tick (AC4)
//! lt-scene census <disc.bin> <set> [scene…]                 every function that projects, and its calls by caller per scene (LT-118)
//! lt-scene probe <disc.bin> <set> first-flight [out dir]    the same for a path the test set doesn't play (LT-118)
//! lt-scene refs <disc.bin> <function>…                      what calls a function, and where its address is stored (LT-118)
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
            "census" => census(&args[1], &image, &args[2..]),
            "probe" => probe(&args[1], &image, &args[2..]),
            "refs" => refs(&args[1], &args[2..]),
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

/// LT-118: the functions that project (from the code), then each scene's calls to them by caller.
fn census(disc: &str, image: &Arc<Image>, rest: &[String]) -> Result<bool, String> {
    use lt_scene::census::{self, Census};
    let d = lt_core::DiscImage::open(disc).map_err(|e| e.to_string())?;
    let exe = lt_core::PsxExe::from_disc(&d).map_err(|e| e.to_string())?;
    let table = lt_core::FileTable::parse(&exe, d.sectors()).map_err(|e| e.to_string())?;
    let wd = lt_core::worldmap::WorldDisc { image: &d, table: &table, exe: &exe };
    let code = census::Code::new(exe.clone(), |id| wd.file(id).ok());
    let found = census::scan_code(&code);
    println!("## Functions that project ({})\n\n| Function | Unit | Address | GTE | Called from (jal) | Stored as a pointer |\n|---|---|---|---|---|---|", found.len());
    let named = |v: Vec<&'static gc_sys::Symbol>| v.iter().map(|s| format!("{} ({})", s.name, census::unit_name(s.unit))).collect::<Vec<_>>();
    for f in &found {
        let mut callers = named(census::static_callers(&code, f.symbol.addr));
        // Model_DrawParts has a second entry 8 bytes before it (the arena's way in).
        if f.symbol.name == "Model_DrawParts" {
            callers.extend(named(census::static_callers(&code, f.symbol.addr - 8)).into_iter().map(|c| format!("{c} via 0x8019dd68")));
        }
        let pointers: Vec<String> = census::pointers_to(&code, f.symbol.addr).iter().map(|(u, a)| format!("{} {a:#010x}", census::unit_name(*u))).collect();
        println!(
            "| `{}` | {} | {:#010x} | {} | {} | {} |",
            f.symbol.name,
            f.unit(),
            f.symbol.addr,
            f.summary(),
            if callers.is_empty() { "none".to_string() } else { callers.join(", ") },
            if pointers.is_empty() { "none".to_string() } else { pointers.join(", ") }
        );
    }
    let set = PathBuf::from(&rest[0]);
    let names: Vec<String> = if rest.len() > 1 {
        rest[1..].to_vec()
    } else {
        let manifest = std::fs::read_to_string(set.join("manifest.txt")).map_err(|e| e.to_string())?;
        manifest.lines().find_map(|l| l.strip_prefix("scenes ")).unwrap_or("").split_whitespace().map(String::from).collect()
    };
    let mut c = Census::of(&code);
    for (stub, i) in &c.stubs {
        println!("\nEntry stub: `{}` ({}) {:#010x} enters `{}`", stub.name, census::unit_name(stub.unit), stub.addr, c.functions[*i].symbol.name);
    }
    for name in &names {
        let (scene, state) = Scene::load(&set, name)?;
        c.reset();
        census::scene(image, &code, &scene, &state, &mut c)?;
        println!("\n## {name} ({} frames)\n\n| Function | Calls | Callers |\n|---|---:|---|", scene.tape.len());
        for (f, calls) in c.functions.iter().zip(&c.calls) {
            if calls.is_empty() {
                continue;
            }
            let total: u64 = calls.values().sum();
            let mut by: Vec<(&String, &u64)> = calls.iter().collect();
            by.sort_by(|a, b| b.1.cmp(a.1));
            let callers: Vec<String> = by.iter().map(|(n, k)| format!("{n} {k}")).collect();
            println!("| `{}` ({}) | {total} | {} |", f.symbol.name, f.unit(), callers.join("; "));
        }
    }
    Ok(true)
}

/// The census's view of the disc's code.
fn census_code(disc: &str) -> Result<lt_scene::census::Code, String> {
    let d = lt_core::DiscImage::open(disc).map_err(|e| e.to_string())?;
    let exe = lt_core::PsxExe::from_disc(&d).map_err(|e| e.to_string())?;
    let table = lt_core::FileTable::parse(&exe, d.sectors()).map_err(|e| e.to_string())?;
    let wd = lt_core::worldmap::WorldDisc { image: &d, table: &table, exe: &exe };
    Ok(lt_scene::census::Code::new(exe.clone(), |id| wd.file(id).ok()))
}

/// The static callers of each named function (or address) and where its address is stored.
fn refs(disc: &str, rest: &[String]) -> Result<bool, String> {
    use lt_scene::census;
    let code = census_code(disc)?;
    for name in rest {
        let syms: Vec<&gc_sys::Symbol> = match u32::from_str_radix(name.trim_start_matches("0x"), 16) {
            Ok(a) if name.starts_with("0x") => gc_sys::SYMBOLS.iter().filter(|s| s.addr == a).collect(),
            _ => gc_sys::SYMBOLS.iter().filter(|s| s.name == name).collect(),
        };
        for s in syms {
            let callers: Vec<String> = census::static_callers(&code, s.addr).iter().map(|c| format!("{} ({})", c.name, census::unit_name(c.unit))).collect();
            let pointers: Vec<String> = census::pointers_to(&code, s.addr).iter().map(|(u, a)| {
                let at = gc_sys::in_unit(*u, *a).map(|(f, _)| f.name).unwrap_or("data");
                format!("{} {a:#010x} ({at})", census::unit_name(*u))
            }).collect();
            println!("{} ({}) {:#010x}: called from [{}]; address in [{}]", s.name, census::unit_name(s.unit), s.addr, callers.join(", "), pointers.join(", "));
        }
    }
    Ok(true)
}

/// A path the test set doesn't play, counted like a scene. `first-flight`: the overworld scene's
/// state with story flag 0xBA clear (as before the game's first flight) and root mode 28, as
/// `Event_StartFlightSceneTask` asks for a flight: `GameMode_FlightSceneSetupTask` then loads
/// OVR24 and runs mode 27. Writes the display every 300 frames to `out dir` (PPM).
fn probe(disc: &str, image: &Arc<Image>, rest: &[String]) -> Result<bool, String> {
    use lt_scene::census::{self, Census};
    let code = census_code(disc)?;
    let set = PathBuf::from(&rest[0]);
    let which = rest.get(1).map(String::as_str).unwrap_or("first-flight");
    let out = rest.get(2).map(PathBuf::from);
    if which != "first-flight" {
        return Err(format!("no probe {which}"));
    }
    let (scene, state) = Scene::load(&set, "overworld")?;
    let mut m = lt_testset::Machine::from_state(image, &scene.card, &state, scene.frame)?;
    // Flag 0xBA (word 5, bit 26 of g_EventFlags) clear: the flight before the world opens up.
    let w = 0x801C_E810 + 5 * 4;
    let flags = u32::from_le_bytes(m.read(w, 4).try_into().unwrap());
    println!("flag 0xBA was {}", flags >> 26 & 1);
    m.write(w, &(flags & !(1 << 26)).to_le_bytes());
    // Root mode 28 (`GameMode_FlightSceneSetupTask`), as `Event_StartFlightSceneTask` asks for it:
    // the root task's next mode (+4) and its "change" byte (+0x28), destination 0 at ctx+0x4A.
    m.write(0x801C_69C4, &28u32.to_le_bytes());
    m.write(0x801C_69E8, &[1]);
    m.write(0x1F80_004A, &[0, 0]);
    let mut c = Census::of(&code);
    let pads = [0u16, 1 << 4, 1 << 7, 1 << 4, 1 << 5, 1 << 14];
    for step in 0..12u64 {
        census::play(&mut m, &code, &mut c, 300, |i| if (i / 60) % 2 == 0 { pads[((step + i / 60) % 6) as usize] } else { 0 })?;
        let mem = |a: u32, o: &mut [u8]| o.copy_from_slice(&m.read(a, o.len()));
        let ovr: Vec<String> = gc_sys::loaded_overlays(&mem).iter().map(|n| format!("OVR{n:02}")).collect();
        let ctx = m.read(0x1F80_0000, 0x100);
        println!("frame {}: mode {} chunk {} overlays {}", m.frame, ctx[0x43], u16::from_le_bytes([ctx[0xE8], ctx[0xE9]]), ovr.join(" "));
        if let Some(dir) = &out {
            let (rgba, w, h) = m.bus.gpu.display_rgba8_with(emulator_core::Deinterlace::Weave);
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            ppm.extend(rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]));
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join(format!("{which}-{:05}.ppm", m.frame)), ppm).map_err(|e| e.to_string())?;
        }
    }
    println!("\n## {which}\n\n| Function | Calls | Callers |\n|---|---:|---|");
    for (f, calls) in c.functions.iter().zip(&c.calls) {
        if calls.is_empty() {
            continue;
        }
        let total: u64 = calls.values().sum();
        let callers: Vec<String> = calls.iter().map(|(n, k)| format!("{n} {k}")).collect();
        println!("| `{}` ({}) | {total} | {} |", f.symbol.name, f.unit(), callers.join("; "));
    }
    Ok(true)
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
