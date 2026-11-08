//! Scene player checks (screen 20) against the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test scenes`.

use lt_core::npc::NpcPack;
use lt_core::scene::{self, math::Tables, Ending, Playback, SceneEntry, SceneInput, SceneKind, Start};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, exe, table })
}

/// `FileTable_NpcBin` entry 17 aliases entry 0.
const PACK_ALIAS: u32 = 17;

fn run(d: &Disc, tables: &Tables, pack_i: u32, b: &[u8], pack: &NpcPack, e: &SceneEntry, answers: Vec<u8>) -> Playback {
    let wd = WorldDisc { image: &d.image, table: &d.table, exe: &d.exe };
    let chunk = wd.chunk(e.chunk as u32).unwrap_or_else(|_| panic!("npc/{pack_i} actor {}: chunk {}", e.slot, e.chunk));
    let start = if e.kind == SceneKind::Trigger { Start::Trigger { x: e.x, z: e.z } } else { Start::Talk };
    let mut flags = vec![0xFC, 0xFE, 0xFF];
    if e.flag != 0 {
        flags.push(e.flag);
    }
    let item = |n: u16| format!("item{n}").into_bytes();
    let toy = |n: u16| format!("toy{n}").into_bytes();
    let input = SceneInput {
        tables,
        pack,
        pack_bytes: b,
        chunk: e.chunk,
        chunk_bytes: &chunk,
        slot: e.slot,
        start,
        flags,
        answers,
        baby: true,
        yaw: None,
        hold_base: 20,
        hold_per_glyph: 1.0,
        names: ["Knight".into(), "Baby".into(), "Nehani".into(), "K-Knight".into(), "Bonik".into()],
        item_name: &item,
        toy_name: &toy,
        max_frames: 30 * 60 * 5,
    };
    scene::play_inferred(&input, &[]).0
}

fn packs(d: &Disc) -> Vec<(u32, Vec<u8>, NpcPack)> {
    let wd = WorldDisc { image: &d.image, table: &d.table, exe: &d.exe };
    (0..50)
        .filter(|&p| p != PACK_ALIAS)
        .filter_map(|p| {
            let b = wd.file(&format!("npc/{p}")).ok()?;
            let pack = NpcPack::parse(&b)?;
            Some((p, b, pack))
        })
        .collect()
}

/// Every walk-in scene and every conversation plays to an ending:
/// none hangs on an opcode the player does not know, every frame decodes.
#[test]
fn every_scene_plays_to_an_ending() {
    let Some(d) = disc() else { return };
    let tables = Tables::from_exe(&d.exe).unwrap();
    let (mut scenes, mut talks, mut timeouts, mut loops) = (0, 0, Vec::new(), Vec::new());
    for (p, b, pack) in packs(&d) {
        for e in &scene::catalog(&pack, &b) {
            let pb = run(&d, &tables, p, &b, &pack, e, vec![]);
            let m = &pb.meta;
            let what = format!("npc/{p} actor {} ({:?})", e.slot, e.kind);
            assert!(m.frames > 0, "{what}: no frames ({:?})", m.notes);
            assert!(!matches!(m.ending, Ending::Hang { .. }), "{what}: {:?}", m.ending);
            assert_eq!(pb.frames.len(), m.frames as usize * (scene::FRAME_HEAD + scene::ACTOR_BYTES * m.actors.len()), "{what}: frame bytes");
            assert!(m.ending_frame < m.frames, "{what}: ending frame {} of {}", m.ending_frame, m.frames);
            match m.ending {
                Ending::Timeout => timeouts.push(what),
                Ending::Loop { .. } => loops.push(what),
                _ => {}
            }
            if e.kind == SceneKind::Trigger {
                scenes += 1;
            } else {
                talks += 1;
            }
        }
    }
    println!("{scenes} scenes, {talks} conversations; still running after 5 min: {timeouts:?}; loops: {loops:?}");
    assert_eq!(scenes, 115);
    // npc/28 actor 11's first line is an empty box (`05 01 00 00`): the
    // window never opens, so the game's text task never finishes either.
    assert!(timeouts.len() <= 1, "timeouts: {timeouts:?}");
    assert!(loops.len() <= 2, "loops: {loops:?}");
}

/// The village dream (npc/6 actor 11): flag inference sets 0x0C0 (the scene
/// clears it) and 0x004 (places actor 10, the pillar's centre); the pillar
/// sequence runs, the voice speaks seven pages and the scene warps to 329.
#[test]
fn destiny_dream_infers_its_story() {
    let Some(d) = disc() else { return };
    let tables = Tables::from_exe(&d.exe).unwrap();
    let wd = WorldDisc { image: &d.image, table: &d.table, exe: &d.exe };
    let b = wd.file("npc/6").unwrap();
    let pack = NpcPack::parse(&b).unwrap();
    let e = scene::catalog(&pack, &b).into_iter().find(|e| e.slot == 11 && e.kind == SceneKind::Trigger).unwrap();
    let pb = run(&d, &tables, 6, &b, &pack, &e, vec![]);
    let m = &pb.meta;
    let inferred: Vec<u16> = m.inferred.iter().map(|i| i.flag).collect();
    assert!(inferred.contains(&0x0C0) && inferred.contains(&0x004), "{:?}", m.inferred);
    assert!(matches!(m.ending, Ending::Warp { chunk: 329, .. }), "{:?}", m.ending);
    assert_eq!(m.speakers, vec!["???".to_string()]);
    assert_eq!(m.pages.len(), 8);
    // Actor 10 is placed (present) once its rung flag is set.
    let stride = scene::FRAME_HEAD + scene::ACTOR_BYTES * m.actors.len();
    let present = |f: usize, a: usize| pb.frames[f * stride + scene::FRAME_HEAD + a * scene::ACTOR_BYTES + 11] & 1 != 0;
    assert!(present(1060, 10), "actor 10 absent at frame 1060");
    // The pillar flag (16) is up during the sequence.
    assert!(pb.frames[500 * stride + 69] & 16 != 0, "no pillar at frame 500");
}

/// Mary's walk-in (npc/6 actor 4) plays the village music under her lines.
#[test]
fn mary_scene_is_stable() {
    let Some(d) = disc() else { return };
    let tables = Tables::from_exe(&d.exe).unwrap();
    let wd = WorldDisc { image: &d.image, table: &d.table, exe: &d.exe };
    let b = wd.file("npc/6").unwrap();
    let pack = NpcPack::parse(&b).unwrap();
    let e = scene::catalog(&pack, &b).into_iter().find(|e| e.slot == 4 && e.kind == SceneKind::Trigger).unwrap();
    let pb = run(&d, &tables, 6, &b, &pack, &e, vec![]);
    let m = &pb.meta;
    assert!(matches!(m.ending, Ending::End), "{:?}", m.ending);
    assert_eq!(m.speakers, vec!["Mary".to_string()]);
    assert!(m.inferred.is_empty(), "{:?}", m.inferred);
    let g = wd.file("sys/9").unwrap();
    let t = &g[u32::from_le_bytes([g[0x20], g[0x21], g[0x22], g[0x23]]) as usize..];
    assert_eq!(scene::area_bgm(t, m.attr, &m.flags_before), Some(1));
}
