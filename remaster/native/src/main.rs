//! `lt-native`: LT-110's gate on the test set.
//!
//! ```text
//! lt-native gate <disc.bin> <set> [scene…] [--every N] [--out DIR] [--float] [--why] [--all]
//! ```
//!
//! `--why` attributes each mismatched pixel to a cause (the polygon there and why it was or wasn't
//! drawn).
//!
//! Each field scene is sampled every N frames (30). For each scene it prints the coverage
//! agreement and writes `DIR/<scene>.png` for its worst tick: the emulated frame, the native one,
//! and the mismatch.
//! - Grey: both covered. Black: neither.
//! - Green: emulated only. Magenta: native only.
//! - Blue: excluded (HUD, effects, the edges).
//!
//! Exits 1 when a scene agrees on less than 99 %.

use std::path::PathBuf;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::gate::{save_png, scene};
use lt_native::gte::Precision;
use lt_native::raster::Raster;
use lt_testset::{Image, Scene};

const FIELD: &[&str] = &["village", "overworld", "interior", "dungeon", "late", "flight", "cutscene", "menu"];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 || !matches!(args[0].as_str(), "gate" | "lockstep" | "composite" | "check" | "light" | "bench" | "motion" | "wide" | "characters" | "billboards" | "moving" | "events") {
        eprintln!("lt-native gate|lockstep|composite|check|light|bench|motion|wide|characters|billboards|moving|events <disc.bin> <set> [scene…] …");
        std::process::exit(2);
    }
    let run = match args[0].as_str() {
        "gate" => gate(&args[1..]),
        "composite" => composite(&args[1..]),
        "check" => check(&args[1..]),
        "light" => light(&args[1..]),
        "bench" => bench(&args[1..]),
        "motion" => motion(&args[1..]),
        "wide" => wide(&args[1..]),
        "characters" => characters(&args[1..]),
        "billboards" => billboards(&args[1..]),
        "moving" => moving(&args[1..]),
        "events" => events(&args[1..]),
        _ => lockstep(&args[1..]),
    };
    match run {
        Ok(true) => println!("PASS"),
        Ok(false) => {
            println!("FAIL");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("lt-native: {e}");
            std::process::exit(2);
        }
    }
}

/// LT-144's measurements (`billboards <disc> <set> [scene…] [--aspect A] [--every N] [--out DIR]
/// [--knight X,Z]`):
/// the field's billboards at 2× in a widened view (16:9 unless given), by kind: drawn natively,
/// put back, in the margins; the smoke rebuild against the game's puffs; the blob shadows by look.
/// Fails when a billboard packet isn't drawn as one, or the rebuild differs from the game's puffs.
fn billboards(args: &[String]) -> Result<bool, String> {
    use lt_native::wide::Reach;
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let (mut names, mut aspect, mut every, mut out, mut knight) = (Vec::new(), 16.0 / 9.0, 30u64, None, None);
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--aspect" => {
                aspect = args.get(i + 1).and_then(|a| Reach::parse_aspect(a)).ok_or("--aspect 16:9")?;
                i += 1;
            }
            "--knight" => {
                let v: Vec<i16> = args.get(i + 1).ok_or("--knight X,Z")?.split(',').filter_map(|v| v.parse().ok()).collect();
                knight = Some([*v.first().ok_or("--knight X,Z")?, *v.get(1).ok_or("--knight X,Z")?]);
                i += 1;
            }
            "--every" => {
                every = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--every N")?;
                i += 1;
            }
            "--out" => {
                out = Some(PathBuf::from(args.get(i + 1).ok_or("--out DIR")?));
                i += 1;
            }
            s => names.push(s.to_string()),
        }
        i += 1;
    }
    if names.is_empty() {
        names = FIELD.iter().map(|s| s.to_string()).collect();
    }
    if let Some(o) = &out {
        std::fs::create_dir_all(o).map_err(|e| e.to_string())?;
    }
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().ok_or("no GPU adapter")?;
    let mut comp = lt_native::composite::Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        let b = lt_native::check::billboards_at(&image, &s, &state, knight, Reach::new(aspect, 1.0), every, &mut assets, &mut cache, &mut comp, out.as_deref().map(|o| (o, name.as_str())))?;
        println!("{}", b.text(name));
        ok &= b.missed == 0 && b.puff_corners_same == b.puff_corners;
    }
    Ok(ok)
}

/// LT-145's measurements (`moving <disc> <set> [scene…]`): the field's billboards in the frames
/// between ticks at 60 and 120 Hz (2 and 4 frames a tick), with no GPU. Fails when a billboard
/// holds still for a frame while it moves over the tick, pops at a tick, or a blob shadow strays
/// from its character by half a pixel or more.
fn moving(args: &[String]) -> Result<bool, String> {
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let mut names: Vec<String> = args[2..].to_vec();
    if names.is_empty() {
        names = ["village", "overworld", "flight"].iter().map(|s| s.to_string()).collect();
    }
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        for k in [2, 4] {
            let m = lt_native::check::moving(&image, &s, &state, k, &mut assets)?;
            println!("{}", m.text(name, k));
            ok &= m.repeated == 0 && m.pop_a < 0.01 && m.pop_b < 0.01 && m.shadow_stray < 0.5;
        }
    }
    Ok(ok)
}

/// LT-142's measurements (`events <disc> <set> [scene…]`): the actors two event tasks draw
/// themselves (Mr. O'Neal's approach, Zeppetto's flicker), in the gift and zeppetto scenes. Fails
/// when one isn't captured, resolved and set aside, or the lit view doesn't draw it.
fn events(args: &[String]) -> Result<bool, String> {
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let mut names: Vec<String> = args[2..].to_vec();
    if names.is_empty() {
        names = ["gift", "zeppetto"].iter().map(|s| s.to_string()).collect();
    }
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().ok_or("no GPU")?;
    let mut comp = lt_native::composite::Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        let e = lt_native::check::event_actors(&image, &s, &state, 8, &mut assets, &mut cache, &mut comp)?;
        println!("{}", e.text(name));
        ok &= e.characters > 0 && e.resolved == e.characters && e.posed == e.parts && e.placed == e.parts && e.kept == 0 && e.aside > 0 && e.lit > 0;
    }
    Ok(ok)
}

/// LT-119's measurements (`characters <disc> <set> [scene…]`): which record fields of an NPC
/// without a packet slot stay live, and how exactly the records rebuild what the game draws. Fails
/// when a drawn character isn't rebuilt exactly.
fn characters(args: &[String]) -> Result<bool, String> {
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let mut names: Vec<String> = args[2..].to_vec();
    if names.is_empty() {
        names = ["village", "door", "dungeon", "interior", "overworld", "late"].iter().map(|s| s.to_string()).collect();
    }
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        let c = lt_native::check::characters(&image, &s, &state, &mut assets)?;
        println!("{}", c.text(name));
        ok &= c.rebuilt == c.drawn;
    }
    // Chests: the cave's moved in front of the Knight, at three lids and two turns.
    let (s, state) = Scene::load(&set, "cave")?;
    for (lid, dir) in [(0u8, 0x800i16), (0x10, 0x800), (0x20, 0x800), (0x10, 0x5A0)] {
        let c = lt_native::check::chest(&image, &s, &state, lid, dir, &mut assets)?;
        println!("chest, lid {lid:#x}, dir {dir:#x}: {}", match c {
            Some((n, true)) => format!("{n} parts drawn, rebuilt exactly"),
            Some((n, false)) => format!("{n} parts drawn, NOT rebuilt"),
            None => "not drawn".into(),
        });
        ok &= matches!(c, Some((2, true)));
    }
    Ok(ok)
}

/// The field scenes LT-114 widens (every one with a camera).
const WIDE: &[&str] = &["village", "overworld", "interior", "dungeon", "late", "flight", "cutscene", "menu", "cave", "tower", "lamps", "door"];

/// LT-114's measurements (`wide <disc> <set> [scene…] [--aspect 16:9,21:9] [--distance D]
/// [--every N] [--out DIR] [--tilt DEGREES] [--hud SCALE]`): per scene and aspect, the object test against the
/// game's, the margins' coverage, what the distance adds and the NPC gap. Fails when a scene's
/// objects don't match the game's or its margins miss what's there. With `--tilt`, the camera is
/// turned up that far and the distance measured instead (fails when it adds nothing); with `--hud`,
/// the 2D layer against the game's 4:3 at that scale (fails when it moves or leaves the middle).
fn wide(args: &[String]) -> Result<bool, String> {
    use lt_native::wide::Reach;
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let (mut names, mut aspects, mut distance, mut every, mut out, mut tilt, mut hud) = (Vec::new(), vec![16.0 / 9.0, 21.0 / 9.0], 1.0, 30u64, None, None::<f64>, None::<u32>);
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--aspect" => {
                aspects = args.get(i + 1).ok_or("--aspect A[,A…]")?.split(',').map(Reach::parse_aspect).collect::<Option<Vec<_>>>().ok_or("--aspect 16:9,21:9")?;
                i += 1;
            }
            "--distance" => {
                distance = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--distance D")?;
                i += 1;
            }
            "--every" => {
                every = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--every N")?;
                i += 1;
            }
            "--out" => {
                out = Some(PathBuf::from(args.get(i + 1).ok_or("--out DIR")?));
                i += 1;
            }
            "--hud" => {
                hud = Some(args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--hud SCALE")?);
                i += 1;
            }
            "--tilt" => {
                tilt = Some(args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--tilt DEGREES")?);
                i += 1;
            }
            s => names.push(s.to_string()),
        }
        i += 1;
    }
    if names.is_empty() {
        names = WIDE.iter().map(|s| s.to_string()).collect();
    }
    if let Some(o) = &out {
        std::fs::create_dir_all(o).map_err(|e| e.to_string())?;
    }
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().ok_or("no GPU adapter")?;
    let mut comp = lt_native::composite::Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        for &a in &aspects {
            let reach = Reach::new(a, distance);
            let dir = out.as_ref().map(|o| o.join(format!("{:.2}-d{distance}", a)));
            if let Some(d) = &dir {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            if let Some(scale) = hud {
                let h = lt_native::check::hud(&image, &s, &state, reach, every, scale, &mut assets, &mut cache, &mut comp, dir.as_deref())?;
                println!("{name} 2D layer at {:.3}:1, {scale}x: {} ticks; {} px at 4:3, {:.3}% the same in the middle; {} px in the margins", reach.aspect, h.ticks, h.layer_px, 100.0 * h.same as f64 / h.layer_px.max(1) as f64, h.outside);
                ok &= h.outside == 0 && h.same as f64 >= 0.99 * h.layer_px as f64;
                continue;
            }
            if let Some(t) = tilt {
                let f = lt_native::check::further(&image, &s, &state, reach, t, every, &mut assets, &mut cache, &mut comp, dir.as_deref())?;
                println!("{name} turned up {t}°, distance {distance}: {} ticks; chunks {} (game distance {}); {} polygons past the game's far; {:.2}% of pixels further", f.ticks, f.chunks, f.chunks_game, f.beyond_far, 100.0 * f.further_px as f64 / f.px.max(1) as f64);
                ok &= distance <= 1.0 || f.further_px > 0;
                continue;
            }
            let w = lt_native::check::widened(&image, &s, &state, reach, every, &mut assets, &mut cache, &mut comp, dir.as_deref())?;
            println!("{}", w.text(name, reach));
            ok &= w.objects_agree == w.chunk_draws && w.margin_missing as f64 <= 0.001 * w.margin_px as f64;
        }
    }
    Ok(ok)
}

fn gate(args: &[String]) -> Result<bool, String> {
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let mut names = Vec::new();
    let (mut every, mut out, mut precision, mut why, mut all) = (30usize, None, Precision::Gte, false, false);
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--every" => {
                every = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--every N")?;
                i += 1;
            }
            "--out" => {
                out = Some(PathBuf::from(args.get(i + 1).ok_or("--out DIR")?));
                i += 1;
            }
            "--float" => precision = Precision::Float,
            "--why" => why = true,
            "--all" => all = true,
            s => names.push(s.to_string()),
        }
        i += 1;
    }
    if names.is_empty() {
        names = FIELD.iter().map(|s| s.to_string()).collect();
    }
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().ok_or("no GPU adapter")?;
    let mut raster = Raster::new(&gpu);
    let mut cache = Cache::new(gpu);
    if let Some(o) = &out {
        std::fs::create_dir_all(o).map_err(|e| e.to_string())?;
    }
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        let r = scene(&image, name, &s, &state, every, &mut assets, &mut cache, &mut raster, precision, why)?;
        let (eo, no, ex): (u64, u64, u64) = r.samples.iter().fold((0, 0, 0), |a, s| (a.0 + s.emulated_only, a.1 + s.native_only, a.2 + s.excluded));
        let worst = r.worst();
        println!(
            "{name}: {} ticks compared ({} drew nothing), {:.3}% agree over {} pixels; emulated only {eo}, native only {no}, excluded {ex}; worst {:.3}% (frame {:?})",
            r.samples.len(),
            r.empty,
            r.agreement() * 100.0,
            r.compared(),
            worst.map_or(0.0, |w| w.agreement() * 100.0),
            worst.map(|w| w.tick_frame)
        );
        let (cp, cc, ce) = r.samples.iter().fold((0u64, 0u64, 0u64), |a, s| (a.0 + s.colour_pixels, a.1 + s.colour_close, a.2 + s.colour_error));
        println!("{name}: colour within {} of the game's on {:.3}% of {cp} pixels both draw (mean difference {:.2})", lt_native::gate::COLOUR_CLOSE, 100.0 * cc as f64 / cp.max(1) as f64, ce as f64 / cp.max(1) as f64);
        let mut causes: std::collections::BTreeMap<String, u64> = Default::default();
        for smp in &r.samples {
            for (k, v) in &smp.causes {
                *causes.entry(k.clone()).or_default() += v;
            }
        }
        let mut causes: Vec<(String, u64)> = causes.into_iter().collect();
        causes.sort_by_key(|c| std::cmp::Reverse(c.1));
        for (k, v) in causes.iter().take(8) {
            println!("  {v:>7}  {k}");
        }
        if why {
            // A few pixels in full: the worst tick's, and any cause other than an edge tie.
            let odd = r.samples.iter().flat_map(|s| s.details.iter()).filter(|d| !d.lines().next().unwrap_or("").contains("edge"));
            for d in worst.map(|w| w.details.iter().take(6)).into_iter().flatten().chain(odd.take(6)) {
                println!("    {d}");
            }
        }
        if let (Some(o), true) = (&out, all) {
            for smp in &r.samples {
                if let Some(p) = &smp.report {
                    save_png(&o.join(format!("{name}-{}.png", smp.tick_frame)), p)?;
                }
            }
        }
        if let (Some(o), Some(w)) = (&out, worst) {
            if let Some(p) = &w.report {
                save_png(&o.join(format!("{name}.png")), p)?;
            }
        }
        ok &= !r.samples.is_empty() && r.agreement() >= 0.99;
    }
    Ok(ok)
}

/// The takeover against the original in lockstep (AC4): no game state outside the render masks.
fn lockstep(args: &[String]) -> Result<bool, String> {
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let image = Image::open(&path)?;
    let names: Vec<String> = if args.len() > 2 { args[2..].to_vec() } else { FIELD.iter().map(|s| s.to_string()).collect() };
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        let (r, t) = lt_native::check::lockstep(&image, &s, &state)?;
        println!("{name}: {} frames; {} tables read, {} terrain and {} character packets set aside", r.frames, t.tables, t.terrain, t.characters);
        match &r.divergence {
            None => println!("  no game state differs outside the masks"),
            Some(_) => {
                println!("{}", r.text());
                ok = false;
            }
        }
    }
    Ok(ok)
}

/// The native view against the emulated picture (`composite <disc> <set> <scene> [--scale N]
/// [--every F] [--out DIR] [--look lit|original]`): every F frames, `DIR/<scene>-<frame>.png` with the emulated picture
/// (enlarged) above the native view.
fn composite(args: &[String]) -> Result<bool, String> {
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let name = args.get(2).ok_or("a scene")?.clone();
    let (mut scale, mut every, mut out, mut look) = (2u32, 120u64, PathBuf::from("."), lt_native::light::Look::Original);
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--scale" => scale = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--scale N")?,
            "--every" => every = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--every F")?,
            "--out" => out = PathBuf::from(args.get(i + 1).ok_or("--out DIR")?),
            "--look" => {
                look = match args.get(i + 1).map(String::as_str) {
                    Some("lit") => lt_native::light::Look::Lit,
                    Some("original") => lt_native::light::Look::Original,
                    _ => return Err("--look lit|original".into()),
                }
            }
            _ => {}
        }
        i += 2;
    }
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().ok_or("no GPU adapter")?;
    let mut comp = lt_native::composite::Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    let (s, state) = Scene::load(&set, &name)?;
    let mut session = lt_native::play::Session::new(&image, &s, &state)?;
    let mut last: Option<(u64, lt_native::composite::Picture, u32, u32)> = None;
    let mut written = 0;
    for (f, &pad) in s.tape.iter().enumerate() {
        for (tick, shown) in session.frame(pad, &mut assets)? {
            // The picture the GPU made of the last tick arrives with this one.
            if let Some((frame, native, kept, aside)) = last.take() {
                if frame / every != (frame + 2) / every || written == 0 {
                    let p = stack(&shown, &native, scale);
                    lt_native::gate::save_png(&out.join(format!("{name}-{frame}.png")), &p)?;
                    println!("{name} frame {frame}: {kept} packets in the 2D layer, {aside} set aside; native view {}x{}", native.w, native.h);
                    written += 1;
                }
            }
            let native = lt_native::play::native_look(&tick, &shown, &assets, &mut cache, &mut comp, scale, look);
            let aside = tick.table.terrain + tick.table.characters;
            last = Some((tick.record.frame, native, tick.table.kept().count() as u32, aside));
        }
        let _ = f;
    }
    Ok(written > 0)
}

/// The emulated picture enlarged `scale` times (nearest) above the native view.
fn stack(shown: &lt_native::takeover::Shown, native: &lt_native::composite::Picture, scale: u32) -> lt_native::gate::Picture {
    let w = native.w.max(shown.w * scale);
    let h = shown.h * scale + 4 + native.h;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..shown.h * scale {
        for x in 0..shown.w * scale {
            let s = (((y / scale) * shown.w + x / scale) * 4) as usize;
            let d = ((y * w + x) * 4) as usize;
            rgba[d..d + 4].copy_from_slice(&shown.rgba[s..s + 4]);
        }
    }
    let top = shown.h * scale + 4;
    for y in 0..native.h {
        for x in 0..native.w {
            let s = ((y * native.w + x) * 4) as usize;
            let d = (((top + y) * w + x) * 4) as usize;
            rgba[d..d + 4].copy_from_slice(&native.rgba[s..s + 4]);
        }
    }
    lt_native::gate::Picture { w, h, rgba }
}

/// LT-111's measurements (`check <disc> <set> [scene…] [--every N]`): field scenes (coverage, the 2D
/// layer, effects) and the menu, dialogue and movie scenes at 2×.
fn check(args: &[String]) -> Result<bool, String> {
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let mut names = Vec::new();
    let mut every = 120usize;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--every" {
            every = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--every N")?;
            i += 1;
        } else {
            names.push(args[i].clone());
        }
        i += 1;
    }
    if names.is_empty() {
        names = FIELD.iter().chain(["boot"].iter()).map(|s| s.to_string()).collect();
    }
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().ok_or("no GPU adapter")?;
    let mut comp = lt_native::composite::Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        if name != "boot" {
            let r = lt_native::check::field_scene(&image, &s, &state, every, &mut assets, &mut cache, &mut comp)?;
            let sum = |f: fn(&lt_native::check::Field) -> u64| r.iter().map(f).sum::<u64>();
            let pct = |a: u64, b: u64| 100.0 * a as f64 / b.max(1) as f64;
            let (cov, lay, eff) = (pct(sum(|f| f.covered_agree), sum(|f| f.pixels)), pct(sum(|f| f.layer_equal), sum(|f| f.layer_pixels)), pct(sum(|f| f.effect_agree), sum(|f| f.effect_pixels)));
            println!(
                "{name}: {} field ticks; coverage {cov:.3}%; 2D layer equal to the game without geometry {lay:.3}% of {} pixels; effects agree {eff:.3}% of {} pixels ({} effect packets)",
                r.len(),
                sum(|f| f.layer_pixels),
                sum(|f| f.effect_pixels),
                sum(|f| f.effects as u64)
            );
            ok &= r.is_empty() || (cov >= 99.0 && lay >= 99.0 && (sum(|f| f.effect_pixels) == 0 || eff >= 99.0));
        }
        if matches!(name.as_str(), "menu" | "cutscene" | "boot") {
            let l = lt_native::check::look(&image, &s, &state, every as u64, 2, &mut assets, &mut cache, &mut comp)?;
            let pct = 100.0 * l.layer_equal as f64 / l.layer_pixels.max(1) as f64;
            println!("{name} at 2x: {} ticks; 2D-only pixels within a step of the game's {pct:.3}% of {}; movie ticks passed through {}/{} exact", l.ticks, l.layer_pixels, l.passed_equal, l.passed);
            ok &= (l.layer_pixels == 0 || pct >= 99.0) && l.passed_equal == l.passed;
        }
    }
    Ok(ok)
}

/// Scene names and `--every N` / `--height H` from `args[2..]`.
fn scene_args(args: &[String], default: &[&str]) -> Result<(Vec<String>, u64, u32), String> {
    let (mut names, mut every, mut height) = (Vec::new(), 120u64, 1440u32);
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--every" => {
                every = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--every N")?;
                i += 1;
            }
            "--height" => {
                height = args.get(i + 1).and_then(|s| s.parse().ok()).ok_or("--height H")?;
                i += 1;
            }
            s => names.push(s.to_string()),
        }
        i += 1;
    }
    if names.is_empty() {
        names = default.iter().map(|s| s.to_string()).collect();
    }
    Ok((names, every, height))
}

/// The lit scenes (LT-112).
const LIT: &[&str] = &["village", "overworld", "interior", "dungeon", "cave", "tower", "lamps"];

/// LT-112's measurements (`light <disc> <set> [scene…] [--every N]`): real shadows (from anything,
/// from characters), lamps, and the original look against LT-110's unlit image.
fn light(args: &[String]) -> Result<bool, String> {
    let (names, every, _) = scene_args(args, LIT)?;
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().ok_or("no GPU adapter")?;
    let mut comp = lt_native::composite::Compositor::new(&gpu);
    let mut raster = Raster::new(&gpu);
    let mut cache = Cache::new(gpu);
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        let r = lt_native::check::lighting_scene(&image, &s, &state, every, &mut assets, &mut cache, &mut comp, &mut raster)?;
        let sum = |f: fn(&lt_native::check::Lighting) -> u64| r.iter().map(f).sum::<u64>();
        let pct = |a: u64, b: u64| 100.0 * a as f64 / b.max(1) as f64;
        let area = r.first().and_then(|m| m.area).map(|a| format!("{a:?}")).unwrap_or_default();
        let (shadow, chars, lamp) = (pct(sum(|m| m.shadowed), sum(|m| m.native)), pct(sum(|m| m.character_shadowed), sum(|m| m.native)), pct(sum(|m| m.lamp_lit), sum(|m| m.native)));
        let orig = pct(sum(|m| m.original_equal), sum(|m| m.original_pixels));
        println!(
            "{name} ({area}): {} ticks; real shadows on {shadow:.2}% of native pixels, characters' {chars:.3}%; {} lamps, lighting {lamp:.2}%; original look = unlit image on {orig:.4}% of {} pixels",
            r.len(),
            r.iter().map(|m| m.lamps).max().unwrap_or(0),
            sum(|m| m.original_pixels)
        );
        ok &= !r.is_empty() && sum(|m| m.original_equal) == sum(|m| m.original_pixels);
    }
    Ok(ok)
}

/// The lit native view's time per tick (`bench <disc> <set> [scene…] [--height H] [--every N]`) at
/// the smallest scale whose picture is at least H lines (1440: 7×, 1568 lines). PASS when every
/// scene's mean fits a 60 Hz frame (16.7 ms): 60 frames a second sustained. The 95th percentile is
/// shown too; it moves with whatever else the machine is doing.
fn bench(args: &[String]) -> Result<bool, String> {
    let (names, every, height) = scene_args(args, LIT)?;
    let scale = height.div_ceil(224).max(1);
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().ok_or("no GPU adapter")?;
    let mut comp = lt_native::composite::Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    let mut worst = 0.0f64;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        let mut ms = lt_native::check::bench(&image, &s, &state, every, scale, &mut assets, &mut cache, &mut comp)?;
        ms.sort_by(f64::total_cmp);
        let at = |q: f64| ms.get(((ms.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
        let mean = ms.iter().sum::<f64>() / ms.len().max(1) as f64;
        println!("{name} at {scale}x ({} lines): {} ticks; {mean:.2} ms mean, {:.2} ms 95th percentile, {:.2} ms max", 224 * scale, ms.len(), at(0.95), at(1.0));
        worst = worst.max(mean);
    }
    Ok(worst <= 1000.0 / 60.0)
}

/// The scenes LT-113 measures.
const MOVING: &[&str] = &["village", "overworld", "interior", "dungeon", "cave", "tower", "late", "flight", "cutscene"];

/// LT-113's measurements (`motion <disc> <set> [scene…] [--every K]`, K frames a tick: 2 for 60 Hz,
/// 4 for 120): how evenly the Knight and the camera move between ticks, frames shown twice, warps
/// and doors against snaps, and the characters' blended parts.
fn motion(args: &[String]) -> Result<bool, String> {
    let (names, k, _) = scene_args(args, MOVING)?;
    let k = if k == 120 { 2 } else { k as u32 };
    let path = PathBuf::from(&args[0]);
    let set = PathBuf::from(&args[1]);
    let image = Image::open(&path)?;
    let disc = Disc::open(&path)?;
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let mut ok = true;
    for name in &names {
        let (s, state) = Scene::load(&set, name)?;
        let m = lt_native::check::motion(&image, &s, &state, k, &mut assets)?;
        println!(
            "{name} at {k} frames a tick: {} pairs, {} blended; snaps (no camera, mode, no shared chunk, cut) {:?}; area changes {} snapped {}; moving frames {} shown twice {}; most uneven step {:.3}; turns {} most uneven {:.3}; parts {} morphed {} characters snapped {}; eye moved in {} pairs, {:.0} units",
            m.pairs, m.blended, m.snaps, m.area_changes, m.area_changes_snapped, m.moving_frames, m.repeated, m.uneven, m.turns, m.turn_uneven, m.parts, m.morphed, m.snapped_characters, m.eye_moves, m.eye_travel
        );
        ok &= m.area_changes_snapped == m.area_changes && m.repeated == 0;
    }
    Ok(ok)
}
