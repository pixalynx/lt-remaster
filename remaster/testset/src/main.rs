//! Builds and checks Living Tool Remaster's test set (LT-106).
//!
//! ```text
//! lt-testset build <disc.bin> <card.mcd> <set dir>   every scene, from power-on (about 4 minutes)
//! lt-testset check <disc.bin> <set dir>              replays each scene twice; the digests must match
//! ```
//!
//! The recipes follow `lt-emu/examples/compat_tour.rs`: research 16's New Game tape to the village;
//! warps by `lt_warp` (ctx+0x50 = chunk | 0x8000 while idle), with the Knight then stood on the
//! chunk's walk map; random battles found by walking squares; the late game from the card's first
//! save; flight by the World mod's root-mode switch. Every tape is at least 1,200 frames (600 game
//! ticks).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lt_core::world::walk::Walk;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};
use lt_testset::{pad, sha1, Image, Machine, Scene, CORE, CTX, KNIGHT, MODE_BATTLE, SCENES};

/// Frames in each tape (600 ticks), longer where the scene needs it.
const TAPE: u64 = 1200;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let r = match a.get(1).map(String::as_str) {
        Some("build") if a.len() == 5 => build(Path::new(&a[2]), Path::new(&a[3]), Path::new(&a[4])),
        Some("check") if a.len() == 4 => check(Path::new(&a[2]), Path::new(&a[3])),
        _ => Err("usage: lt-testset build <disc.bin> <card.mcd> <set dir> | check <disc.bin> <set dir>".into()),
    };
    if let Err(e) = r {
        eprintln!("lt-testset: {e}");
        std::process::exit(1);
    }
}

/// research 16's New Game tape: START pulses through the opening, then ✕.
fn new_game_tape(f: u64) -> u16 {
    if f % 120 >= 8 {
        0
    } else if f < 2300 {
        pad::START
    } else {
        pad::CROSS
    }
}

/// Walking: four directions in turn, ✕ now and then (talk, examine).
fn walk(t: u64) -> u16 {
    [pad::UP, pad::RIGHT, pad::DOWN, pad::LEFT][((t / 90) % 4) as usize] | if t % 200 < 6 { pad::CROSS } else { 0 }
}

/// Fighting: ✕ every 40 frames.
fn fight(t: u64) -> u16 {
    if t % 40 < 6 {
        pad::CROSS
    } else {
        0
    }
}

struct Builder {
    image: Arc<Image>,
    disc: DiscImage,
    exe: PsxExe,
    table: FileTable,
    set: PathBuf,
    log: Vec<String>,
}

impl Builder {
    /// Saves `m` as scene `name`, then plays `tape` (frames pad by pad) from it and stores the digest.
    fn scene(&mut self, name: &str, about: &str, m: &mut Machine, tape: impl Fn(u64) -> u16, frames: u64) -> Result<(), String> {
        let state = m.save_state()?;
        let f = m.field();
        let card = m.card_now();
        let mut scene = Scene { name: name.into(), about: about.into(), frame: m.frame, chunk: f.cur, mode: f.mode, card, tape: (0..frames).map(tape).collect(), digest: 0 };
        scene.digest = scene.replay(&self.image, &state)?;
        scene.save(&self.set, &state)?;
        let line = format!("{name:<12} frame {:>6}  chunk {:>3}  mode {}  {} frames  digest {:016x}  {about}", scene.frame, scene.chunk, scene.mode, frames, scene.digest);
        println!("{line}");
        self.log.push(line);
        Ok(())
    }

    /// lt_warp, then waits until idle on `chunk` and stands the Knight on the walk map nearest (0, 0).
    fn warp(&self, m: &mut Machine, chunk: u16) -> Result<(), String> {
        self.warp_near(m, chunk, 0, 0)
    }

    /// [`Builder::warp`], standing the Knight nearest (`x`, `z`).
    fn warp_near(&self, m: &mut Machine, chunk: u16, x: i32, z: i32) -> Result<(), String> {
        m.until(600, |_| 0, |m| m.field().idle()).map(|_| ())?;
        m.write(CTX + 0x50, &[(chunk & 0xFF) as u8, (chunk >> 8) as u8 | 0x80]);
        m.run(10, |_| 0)?;
        m.until(900, |_| 0, |m| m.field().on_field() && m.field().cur == chunk)?.ok_or(format!("never idle on chunk {chunk}"))?;
        let w = WorldDisc { image: &self.disc, table: &self.table, exe: &self.exe };
        if let Ok(c) = w.chunk(chunk as u32) {
            let walk = Walk::of_chunk(chunk as u32, &c, &[]);
            if let Some((x, z)) = walk.nearest_standing(x, z, 0x800) {
                if let Ok(h) = walk.stand(x, z) {
                    let mut b = Vec::new();
                    for v in [x, h, z] {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                    m.write(KNIGHT, &b);
                }
            }
        }
        m.run(30, |_| 0)?;
        Ok(())
    }

    /// Walks squares until a random battle starts (up to 2 minutes).
    fn encounter(&self, m: &mut Machine) -> Result<(), String> {
        m.until(7200, walk, |m| m.field().mode == MODE_BATTLE)?.ok_or("no encounter in 2 minutes of walking")?;
        m.run(60, |_| 0)?;
        Ok(())
    }
}

fn build(disc: &Path, card_path: &Path, set: &Path) -> Result<(), String> {
    let image = Image::open(disc)?;
    let card = std::fs::read(card_path).map_err(|e| format!("{}: {e}", card_path.display()))?;
    let d = DiscImage::open(disc).map_err(|e| e.to_string())?;
    let exe = PsxExe::from_disc(&d).map_err(|e| e.to_string())?;
    let table = FileTable::parse(&exe, d.sectors()).map_err(|e| e.to_string())?;
    let exe_sha1 = sha1(&exe.bytes);
    std::fs::create_dir_all(set).map_err(|e| e.to_string())?;
    let mut b = Builder { image: image.clone(), disc: d, exe, table, set: set.to_path_buf(), log: Vec::new() };
    let t0 = std::time::Instant::now();

    // Power-on: the boot movies play with no input.
    let mut m = Machine::boot(&image, &[])?;
    b.scene("boot", "power-on, the boot movies (no input)", &mut m, |_| 0, 2400)?;

    // New Game to the Knight's bedroom (chunk 326): the opening scene, ✕ through its lines.
    m.until(6000, new_game_tape, |m| m.field().cur == 326 && m.field().idle())?.ok_or("New Game never reached the bedroom")?;
    let at = m.frame;
    m.run(30, |i| new_game_tape(at + i))?;
    let at = m.frame;
    b.scene("cutscene", "the opening scene in the Knight's bedroom, ✕ through it", &mut m, move |i| new_game_tape(at + i), 1500)?;
    // The opening goes on out of the house into the village (LT-113's doors): kept to build from a
    // copy, so the scenes after are as they were.
    let (door, door_frame, door_card) = (m.save_state()?, m.frame, m.card_now());

    // On to the village, idle a second.
    let mut idle = 0;
    let base = m.frame;
    m.until(6000, move |i| new_game_tape(base + i), |m| {
        let f = m.field();
        idle = if f.on_field() && f.cur == 78 { idle + 1 } else { 0 };
        idle >= 60
    })?
    .ok_or("New Game never reached the village")?;
    let village = m.save_state()?;
    let village_frame = m.frame;
    let village_card = m.card_now();
    b.scene("village", "the village (chunk 78), walking about", &mut m, walk, TAPE)?;
    b.scene(
        "menu",
        "the field menu: open, down twice, into a page, back out, closed",
        &mut m,
        |t| match t {
            30..=37 => pad::START,
            150..=157 | 230..=237 => pad::DOWN,
            310..=317 => pad::CROSS,
            480..=487 | 560..=567 | 640..=647 => pad::TRIANGLE,
            _ => 0,
        },
        TAPE,
    )?;
    let from_village = |b: &Builder| Machine::from_state(&b.image, &village_card, &village, village_frame);

    // The overworld next to the village, walking across chunk seams; then its first random battle.
    let mut m = from_village(&b)?;
    b.warp(&mut m, 80)?;
    b.scene("overworld", "the overworld (chunk 80), walking across chunk seams", &mut m, |t| [pad::RIGHT, pad::UP, pad::LEFT][((t / 400) % 3) as usize], TAPE)?;
    b.encounter(&mut m)?;
    b.scene("battle-1", "a level-1 random battle on chunk 80, ✕ throughout", &mut m, fight, 2400)?;

    // An interior and a dungeon.
    let mut m = from_village(&b)?;
    b.warp(&mut m, 330)?;
    b.scene("interior", "an interior (chunk 330), walking about", &mut m, walk, TAPE)?;
    let mut m = from_village(&b)?;
    b.warp(&mut m, 372)?;
    b.scene("dungeon", "a dungeon (chunk 372), walking about", &mut m, walk, TAPE)?;
    // Two real dungeons (LT-112's lighting): "dungeon" above turned out to be Trisken, a town
    // past the grid. Its name stays, so earlier results still read the same.
    let mut m = from_village(&b)?;
    b.warp(&mut m, 454)?;
    b.scene("cave", "the Holy Armor dungeon's ice cave (chunk 454), walking about", &mut m, walk, TAPE)?;
    let mut m = from_village(&b)?;
    b.warp(&mut m, 433)?;
    b.scene("tower", "the North Tower (chunk 433), walking about", &mut m, walk, TAPE)?;
    // An inn's bar, its table lamps painted as warm light pools (LT-112's lamps), standing still.
    let mut m = from_village(&b)?;
    b.warp_near(&mut m, 348, 909, 904)?;
    b.scene("lamps", "an inn's bar with table lamps (chunk 348), standing", &mut m, |_| 0, TAPE)?;

    // The way out of the house: the New Game tape from the cutscene's start, from 300 frames before
    // the village is first the current chunk.
    let mut m = Machine::from_state(&image, &door_card, &door, door_frame)?;
    let out = m.until(6000, move |i| new_game_tape(door_frame + i), |m| m.field().cur == 78)?.ok_or("New Game never left the bedroom")?;
    let mut m = Machine::from_state(&image, &door_card, &door, door_frame)?;
    m.run(out.saturating_sub(300), move |i| new_game_tape(door_frame + i))?;
    let at = m.frame;
    b.scene("door", "the opening's way out of the house into the village (chunk 326 to 78), 300 frames before", &mut m, move |i| new_game_tape(at + i), 1200)?;

    // The late game: the card's first save, from the title (Continue, slot 1, the first save, Yes).
    let mut m = Machine::boot(&image, &card)?;
    m.until(20000, |_| 0, |m| m.frame > 600 && m.display_size().0 == 640 && m.display_size().1 > 400)?.ok_or("no title screen")?;
    m.run(90, |_| 0)?;
    for (key, wait) in [(pad::DOWN, 90u64), (pad::CROSS, 240), (pad::CROSS, 420), (pad::CROSS, 150), (pad::UP, 40), (pad::CROSS, 240), (pad::CROSS, 60)] {
        m.run(8, |_| key)?;
        m.run(wait, |_| 0)?;
    }
    m.until(1800, |_| 0, |m| m.field().on_field())?.ok_or("the save didn't load")?;
    m.run(60, |_| 0)?;
    let late = m.save_state()?;
    let (late_frame, late_card) = (m.frame, m.card_now());
    b.scene("late", "the card's first save, loaded (level 40s), walking about", &mut m, walk, TAPE)?;

    // Flight: the world map's flying mode (root task mode 0x17, as the World mod's fly_check does).
    let mut m = Machine::from_state(&image, &late_card, &late, late_frame)?;
    if m.field().cur >= 324 {
        b.warp(&mut m, 80)?;
    }
    m.write(0x801C_69C4, &0x17u32.to_le_bytes());
    m.write(0x801C_69E8, &[1]);
    m.until(900, |_| 0, |m| u16::from_le_bytes(m.read(CTX + 0xDE, 2).try_into().unwrap()) & 4 != 0)?.ok_or("flight never started")?;
    m.run(300, |_| 0)?;
    b.scene("flight", "flying over the world map", &mut m, |t| [pad::UP, pad::LEFT, pad::UP, pad::RIGHT][((t / 300) % 4) as usize], TAPE)?;

    // A late random battle on chunk 114.
    let mut m = Machine::from_state(&image, &late_card, &late, late_frame)?;
    b.warp(&mut m, 114)?;
    b.encounter(&mut m)?;
    b.scene("battle-late", "a random battle on chunk 114 with the late party, ✕ throughout", &mut m, fight, 2400)?;

    let manifest = format!(
        "core {CORE}\nexe_sha1 {exe_sha1}\ncard {} sha1 {}\nbuilt_in {:.0} s\nscenes {}\n{}\n",
        card_path.display(),
        sha1(&card),
        t0.elapsed().as_secs_f64(),
        SCENES.join(" "),
        b.log.join("\n")
    );
    std::fs::write(set.join("manifest.txt"), manifest).map_err(|e| e.to_string())?;
    println!("built {} scenes in {:.0} s", SCENES.len(), t0.elapsed().as_secs_f64());
    Ok(())
}

fn check(disc: &Path, set: &Path) -> Result<(), String> {
    let image = Image::open(disc)?;
    let manifest = std::fs::read_to_string(set.join("manifest.txt")).map_err(|e| format!("{}: {e}", set.display()))?;
    if !manifest.lines().any(|l| l == format!("core {CORE}")) {
        return Err(format!("the set was built with another core (this is {CORE}); build it again"));
    }
    let mut ok = true;
    for name in SCENES {
        let (scene, state) = Scene::load(set, name)?;
        let a = scene.replay(&image, &state)?;
        let b = scene.replay(&image, &state)?;
        let good = a == b && a == scene.digest && scene.tape.len() >= 1200;
        ok &= good;
        println!(
            "{} {name:<12} {:>5} frames ({} ticks)  replay {a:016x} / {b:016x}  built {:016x}",
            if good { "ok  " } else { "FAIL" },
            scene.tape.len(),
            scene.tape.len() / 2,
            scene.digest
        );
    }
    if ok {
        println!("all {} scenes replay to the same machine", SCENES.len());
        Ok(())
    } else {
        Err("a scene didn't replay the same".into())
    }
}
