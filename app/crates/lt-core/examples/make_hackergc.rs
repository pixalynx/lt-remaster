//! Makes the HackerGC v0 blank base package (research 06 phase 3): the decomp's
//! `blank` profile build (story checks in C pointed at ids nothing uses), plus the
//! data half Living Tool does itself: every NPC pack blank-slated (actors, examine
//! spots, scene and flag triggers out, their models freed; doors kept) and the
//! starter scene in npc/7, so a new game shows the Knight in an empty house.
//!
//! ```text
//! # in gc_decomp (branch hackergc):
//! make hgc-blank-disc DISC="/path/to/Guardian's Crusade.bin"   # -> build/mod/hackergc-blank-code.bin
//! # here:
//! cargo run --release --example make_hackergc -- <retail.bin> <hackergc-blank-code.bin> <out_dir> [version]
//!     [--layout build/mod/hgc_layout.json] [--savestate boot.sstate]
//! ```
//!
//! `--savestate <file>`: a PCSX-Redux save state (gzipped `.sstate` or raw `state-raw`) taken on
//! the image this run writes, idle on a field map after a new game; it ships as the package's boot
//! save state (`savestates.boot`, LT-11). The build is deterministic, so a state taken on the
//! previous run's image of the same inputs fits. `base_savestate` adds one to a finished package.
//! `--layout`: the code image comes from the shift-safe build (`tools/hgc_build.py`), which moved
//! main's tables; the layout goes into `base.json` (`profile.exeLayout`) so Living Tool reads them,
//! and so does the EXT opcode's version when the build has it (`tier2Ext`, `profile.tier2Ext`).
//! So does its event-flag count (`flagCount`, `profile.flagCount`).
//! `--ext-demo` (a test): the starter scene first runs `ext 0 0 500` -- new overlay 0, which the
//! decomp's `src/hgc_ovr/00_rubies` makes add 500 rubies.
//! `--flags-demo` (a test): the starter scene first sets flags 0x210 and 0x29F, clears 0x29F, adds
//! 7 rubies if 0x210 reads set, and rests at the bed (INN), which packs a save and offers to write it.
//! `--battle-demo` (a test): story battle 0x109 gets the SOLO behaviour in the base's table
//! (`g_HgcBattleFlags`; retail 0x109 has the Baby), and the starter scene starts it before the warp.
//! SOLO puts the formation's first enemy on the player's side, so 0x109 (the Winged Lion alone) also
//! gets a second enemy, Ramal; with nobody to fight, the game hangs at the first move at every enemy
//! (LT-45). `--battle-control` starts the battle without changing the table or the formation.
//! `--battle <id>` (hex) starts another story battle; `--formation <id>=<species>x<count>,...` writes
//! a story battle's formation as given, unchecked (`--formation 105=102x1` is retail's solo 0x105
//! with Glor alone, which hangs the retail code the same way).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::base::{self, BaseBoot};
use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::edit::blank::{self, Options, Report, BOOT_CHUNK, BOOT_PACK, BOOT_SPOT, KNIGHT_HIDDEN};
use lt_core::edit::scene::{self, NewScene};
use lt_core::edit::script::{Arg, Node};
use lt_core::{DiscImage, FileTable, PsxExe};

/// The flags the blank profile's story checks point at (gc_decomp include/config/profile_blank.h).
const NEVER_FLAGS: &str = "0x17D-0x17F";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let ext_demo = args.iter().position(|a| a == "--ext-demo").map(|i| args.remove(i)).is_some();
    let flags_demo = args.iter().position(|a| a == "--flags-demo").map(|i| args.remove(i)).is_some();
    let battle_demo = args.iter().position(|a| a == "--battle-demo").map(|i| args.remove(i)).is_some();
    // The same scene with the table left as it is: the demo's control run.
    let battle_control = args.iter().position(|a| a == "--battle-control").map(|i| args.remove(i)).is_some();
    // The story battle the starter scene starts (0x109 unless `--battle <id>`).
    let battle = args.iter().position(|a| a == "--battle").map_or(0x109, |i| {
        let v = args.remove(i + 1);
        args.remove(i);
        let v = v.trim_start_matches("0x");
        i64::from_str_radix(v, 16).expect("--battle takes a hex story-battle id")
    });
    // Story-battle formations written as given, unchecked (`--formation 105=66x1,57x1`, hex id, decimal species).
    let mut formations: Vec<(usize, [[u8; 2]; 3])> = Vec::new();
    while let Some(i) = args.iter().position(|a| a == "--formation") {
        let v = args.remove(i + 1);
        args.remove(i);
        let (id, list) = v.split_once('=').expect("--formation <id>=<species>x<count>,...");
        let k = usize::from_str_radix(id.trim_start_matches("0x"), 16).unwrap() - 0x100;
        let mut f = [[0u8; 2]; 3];
        for (slot, e) in list.split(',').enumerate() {
            let (sp, n) = e.split_once('x').expect("<species>x<count>");
            f[slot] = [sp.parse().unwrap(), n.parse().unwrap()];
        }
        formations.push((k, f));
    }
    // SOLO puts the formation's first entry on the player's side, so the demo's battle needs a second
    // entry to fight: 0x109 (the Winged Lion alone) gets Ramal, its partner in 0x104 on the same arena.
    if battle_demo && battle == 0x109 && !formations.iter().any(|(k, _)| *k == 9) {
        formations.push((9, [[11, 1], [87, 1], [0, 0]]));
    }
    let mut tier2_ext = 0u64;
    let mut flag_count = lt_core::flags::COUNT as u64;
    // Retail ids the shift-safe blank build no longer reacts to (hgc_layout.json `released`).
    let mut released: Option<lt_core::base::BaseReserved> = None;
    let savestate = args.iter().position(|a| a == "--savestate").map(|i| {
        let path = args.remove(i + 1);
        args.remove(i);
        let bytes = std::fs::read(&path).expect("reading the --savestate file");
        lt_core::savestate::SaveState::read(&bytes).unwrap_or_else(|e| panic!("{path}: {}", e.message))
    });
    let layout = args.iter().position(|a| a == "--layout").map(|i| {
        let path = args.remove(i + 1);
        args.remove(i);
        let text = std::fs::read_to_string(&path).unwrap();
        let l: lt_core::layout::Layout = serde_json::from_str(&text).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&text).unwrap();
        tier2_ext = raw.get("tier2Ext").and_then(|v| v.as_u64()).unwrap_or(0);
        released = raw.get("released").map(|v| serde_json::from_value(v.clone()).unwrap());
        flag_count = raw.get("flagCount").and_then(|v| v.as_u64()).unwrap_or(flag_count);
        // Registered before the code image is read, so its tables are found.
        lt_core::layout::register(l.clone());
        l
    });
    assert!(!ext_demo || tier2_ext > 0, "--ext-demo needs a --layout from a build with EXT (tier2Ext)");
    assert!(!flags_demo || flag_count > 0x29F, "--flags-demo needs a --layout from a build with 0x2A0 flags (flagCount)");
    // The scripts below are checked against the base they're for.
    lt_core::engine::set_active(Some(lt_core::engine::Profile {
        tier2_ext: tier2_ext as u32,
        flag_count: flag_count as u32,
        ..lt_core::engine::retail().clone()
    }));
    if args.len() < 3 {
        eprintln!("usage: make_hackergc <retail.bin> <hackergc-blank-code.bin> <out_dir> [version]");
        std::process::exit(2);
    }
    let (retail, code, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    let version = args.get(3).cloned().unwrap_or_else(|| "0.1.0".into());
    std::fs::create_dir_all(&out).unwrap();

    // The data half, on top of the code build.
    let image = DiscImage::open(&code).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let src = Source { image: &image, exe: &exe, table: &table };
    let opts = Options { free_models: true, ..Options::default() };
    let mut staged = Vec::new();
    let mut total = Report::default();
    let mut packs = 0;
    for n in 0..50u32 {
        let id = format!("npc/{n}");
        let bytes = src.retail(&id).unwrap();
        let (mut pack, _) = blank::blank(&bytes, &bytes, &opts).unwrap_or_else(|e| panic!("{id}: {e}"));
        if n == BOOT_PACK && (battle_demo || battle_control) {
            let num = |v: i64| Arg::Num { value: v };
            let mut program = blank::starter_program();
            program.insert(2, Node::Op { op: 0x08, args: vec![num(battle)], at: None });
            let (x, h, z) = BOOT_SPOT;
            pack = scene::create(&bytes, &pack, &NewScene { chunk: BOOT_CHUNK, x, h, z, cue: KNIGHT_HIDDEN }, &program, &[]).unwrap().0;
        } else if n == BOOT_PACK && (ext_demo || flags_demo) {
            let num = |v: i64| Arg::Num { value: v };
            let op = |op: u8, args: &[i64]| Node::Op { op, args: args.iter().map(|&v| num(v)).collect(), at: None };
            let mut program = Vec::new();
            if ext_demo {
                program.push(op(lt_core::tier2::OP_EXT, &[0, 0, 500]));
            }
            if flags_demo {
                let (x, _, z) = BOOT_SPOT;
                program.extend([op(0x04, &[0x210]), op(0x04, &[0x29F]), op(0x05, &[0x29F])]);
                program.push(Node::If { op: 0x0B, arg: Some(num(0x210)), then: vec![op(0x33, &[7])], els: vec![], at: None, else_at: None });
                program.push(op(0x06, &[BOOT_CHUNK as i64, x as i64, z as i64, 0]));
            }
            program.extend(blank::starter_program());
            let (x, h, z) = BOOT_SPOT;
            pack = scene::create(&bytes, &pack, &NewScene { chunk: BOOT_CHUNK, x, h, z, cue: KNIGHT_HIDDEN }, &program, &[]).unwrap().0;
        } else if n == BOOT_PACK {
            pack = blank::starter(&bytes, &pack).unwrap().0;
        }
        total.add(&blank::report(&bytes, &pack).unwrap());
        if pack != bytes {
            staged.push(Staged { id, bytes: Arc::new(pack), base_sha256: None });
            packs += 1;
        }
    }
    if !formations.is_empty() {
        for id in [lt_core::gamedata::encounters::PRIMARY, lt_core::gamedata::encounters::MIRROR] {
            let mut file = src.retail(id).unwrap();
            let t = u32::from_le_bytes(file[0x20..0x24].try_into().unwrap()) as usize;
            for (k, f) in &formations {
                let at = t + 0xF80 + k * 9;
                for (i, e) in f.iter().enumerate() {
                    file[at + 2 * i..at + 2 * i + 2].copy_from_slice(e);
                }
            }
            staged.push(Staged { id: id.into(), bytes: Arc::new(file), base_sha256: None });
        }
        for (k, f) in &formations {
            eprintln!("battle {:#x} formation {f:?}", 0x100 + k);
        }
    }
    if battle_demo {
        let k = battle as u32 - 0x100;
        let (at, _) = exe.layout.as_ref().and_then(|l| l.battle_flags).expect("--battle-demo needs a --layout with battleFlags");
        let old = u32::from_le_bytes(exe.slice(at + 4 * k, 4).unwrap().try_into().unwrap());
        let solo = lt_core::gamedata::encounters::BEHAVIOUR_BITS[0].0;
        let bytes = lt_core::edit::tables::set_behaviour(&src.retail("exe").unwrap(), battle as u16, old | solo).unwrap();
        eprintln!("battle {battle:#x} behaviour {old:#010x} -> {:#010x}", old | solo);
        staged.push(Staged { id: "exe".into(), bytes: Arc::new(bytes), base_sha256: None });
    }
    eprintln!(
        "blank slate: {packs} packs, {} actors, {} examine spots, {} scene and {} flag records out",
        total.actors,
        total.objects,
        total.scenes,
        total.flags
    );
    let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, ..Default::default() });
    assert!(!plan.blocked_included, "{:?}", plan.items.iter().filter_map(|i| i.refusal.as_ref()).collect::<Vec<_>>());
    let img = out.join(format!("hackergc-blank-{version}.bin"));
    let cancel = AtomicBool::new(false);
    let rep = build::write_image(&src, &plan, &staged, &img, &cancel, &mut |_| {}).unwrap();
    build::verify_image(&img, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
    drop(image);

    // The package: a BPS against retail, base.json, and the base's flag names.
    let extras = out.join("extras");
    std::fs::create_dir_all(&extras).unwrap();
    let flags = r#"{
  "version": 1,
  "flags": {
    "0x17D": { "name": "hgc_never_set_c", "summary": "HackerGC: the demo pack-swap check points here; never set it" },
    "0x17E": { "name": "hgc_never_set_b", "summary": "HackerGC: the alt-NPC pack-swap check points here; never set it" },
    "0x17F": { "name": "hgc_never_set_a", "summary": "HackerGC: the Xizan pack-swap check points here; never set it" }
  }
}
"#;
    std::fs::write(extras.join("flags.json"), flags).unwrap();
    let mut m = base::manifest_template("hackergc-blank", &version);
    m.name = Some(format!("HackerGC v{version} · blank"));
    let (x, h, z) = BOOT_SPOT;
    m.boot = Some(BaseBoot { chunk: BOOT_CHUNK as u32, x: x as i32, h: h as i32, z: z as i32, dir: 0x9FD });
    m.reserved.flags = vec![NEVER_FLAGS.into()];
    if let Some(r) = released {
        m.released = r;
    } else {
        // An unshifted blank build releases the pack swaps and the chunk effects (profile_blank.h).
        m.released = lt_core::base::BaseReserved {
            chunks: ["188", "413", "414", "437-438"].map(String::from).to_vec(),
            packs: ["4", "6", "10", "22"].map(String::from).to_vec(),
            ..Default::default()
        };
    }
    m.catalogues.flags = Some("flags.json".into());
    if let Some(state) = &savestate {
        std::fs::write(extras.join(base::BOOT_STATE), state.gzip()).unwrap();
        m.savestates.boot = Some(base::BOOT_STATE.into());
    }
    if let Some(l) = &layout {
        m.profile.insert("exeLayout".into(), serde_json::to_value(l).unwrap());
    }
    if tier2_ext > 0 {
        m.profile.insert("tier2Ext".into(), tier2_ext.into());
    }
    if flag_count > lt_core::flags::COUNT as u64 {
        m.profile.insert("flagCount".into(), flag_count.into());
    }
    let pkg_dir = out.join("package");
    let pkg = base::make_package(&retail, &img, &pkg_dir, m, Some(&extras), &cancel).unwrap();
    let bps = std::fs::metadata(pkg.file(&pkg.manifest.patch.bps)).unwrap().len();
    println!("{} in {}: {} ({bps} bytes), image SHA-1 {}", pkg.manifest.tag(), pkg_dir.display(), pkg.manifest.patch.bps, pkg.manifest.patch.target_sha1);
}
