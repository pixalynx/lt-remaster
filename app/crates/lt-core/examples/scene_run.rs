//! Plays one retail scene in the scene player and prints what happened.
//! `cargo run --release --example scene_run -- <disc.bin> <pack> <slot> [flag…]`
use lt_core::npc::NpcPack;
use lt_core::scene::{self, math::Tables, SceneInput, SceneKind, Start};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let (pack_i, slot): (u32, u32) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let flags: Vec<u16> = a[4..].iter().map(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()).collect();
    let b = wd.file(&format!("npc/{pack_i}")).unwrap();
    let pack = NpcPack::parse(&b).unwrap();
    let entry = scene::catalog(&pack, &b).into_iter().find(|e| e.slot == slot).expect("no such scene");
    let chunk = wd.chunk(entry.chunk as u32).unwrap();
    let tables = Tables::from_exe(&exe).unwrap();
    let start = if entry.kind == SceneKind::Trigger { Start::Trigger { x: entry.x, z: entry.z } } else { Start::Talk };
    let mut all = vec![0xFC, 0xFE, 0xFF];
    all.extend(flags);
    if entry.flag != 0 { all.push(entry.flag); }
    let item = |n: u16| format!("item{n}").into_bytes();
    let toy = |n: u16| format!("toy{n}").into_bytes();
    let t0 = std::time::Instant::now();
    let (pb, inferred) = scene::play_inferred(&SceneInput {
        tables: &tables, pack: &pack, pack_bytes: &b, chunk: entry.chunk, chunk_bytes: &chunk, slot, start,
        flags: all, answers: vec![], baby: true, yaw: None, hold_base: 20, hold_per_glyph: 1.0,
        names: ["Knight".into(), "Baby".into(), "Nehani".into(), "K-Knight".into(), "Bonik".into()],
        item_name: &item, toy_name: &toy, max_frames: 9000,
    }, &[]);
    println!("inferred {:?}", inferred);
    let m = &pb.meta;
    println!("{} frames ({:.1} s) in {:?}; ending {:?} at frame {}; {} bytes", m.frames, m.frames as f32 / 30.0, t0.elapsed(), m.ending, m.ending_frame, pb.frames.len());
    for e in &m.events { println!("  {:5} D+{:04X} {:14} {}", e.frame, e.at, e.name, e.note); }
    println!("pages {} speakers {:?} menus {:?} presses {:?}", m.pages.len(), m.speakers, m.menus, m.presses);
    for (i, p) in m.pages.iter().enumerate().take(6) {
        let s: String = p.iter().map(|g| g[0] as u8 as char).collect();
        println!("  page {i}: {s}");
    }
    println!("cues: {:?}", m.cues.iter().take(30).map(|c| (c.frame, c.cue)).collect::<Vec<_>>());
    println!("notes {:?}", m.notes);
    // Knight track summary.
    let stride = scene::FRAME_HEAD + scene::ACTOR_BYTES * m.actors.len();
    for f in (0..m.frames as usize).step_by(30) {
        let base = f * stride + scene::FRAME_HEAD;
        let rd = |o: usize| i16::from_le_bytes([pb.frames[base + o], pb.frames[base + o + 1]]);
        let fade = pb.frames[f * stride + 68];
        let flags = pb.frames[f * stride + 70];
        print!("f{f}: K({},{},{} f{:#x}) fade {fade} ui {flags:#x} | ", rd(0), rd(2), rd(4), rd(6));
    }
    println!();
}
