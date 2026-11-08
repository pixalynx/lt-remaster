//! Walk paths for the World editor (`scene::walk`, LT-79) on retail packs. Opt in
//! with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`.

use lt_core::npc::NpcPack;
use lt_core::placement::{active, decode, Body};
use lt_core::scene::math::Tables;
use lt_core::scene::walk::{walk_path, Mark, Start, WalkPath};
use lt_core::world;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

const FLAGS: [u16; 3] = [0xFC, 0xFE, 0xFF];

fn disc() -> Option<(DiscImage, PsxExe, FileTable)> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some((image, exe, table))
}

/// The branch the New-game flags place `slot` with, as a walk start.
fn start_of(p: &NpcPack, b: &[u8], slot: u32) -> Option<Start> {
    let d = p.d_block(b);
    let br = decode(d, p.actors[slot as usize].placement);
    let a = active(&br, &|f| FLAGS.contains(&f))?;
    match a.body {
        Body::Place { x, h, z, dir } => Some(Start { x, h, z, dir, walk: a.at + 10 }),
        _ => None,
    }
}

fn path(w: &WorldDisc, t: &Tables, pack: u32, slot: u32) -> Option<WalkPath> {
    path_from(w, t, pack, slot, |s| s)
}

/// The walk of `slot` from its New-game branch, its start changed by `at` (a move).
fn path_from(w: &WorldDisc, t: &Tables, pack: u32, slot: u32, at: impl Fn(Start) -> Start) -> Option<WalkPath> {
    let b = w.file(&format!("npc/{pack}")).unwrap();
    let p = NpcPack::parse(&b).unwrap();
    let home = (p.actors[slot as usize].b & 0xFFFF) as u16;
    let chunk = w.chunk(home as u32).ok()?;
    walk_path(t, &p, &b, home, world::heightmap(&chunk), &FLAGS, slot, at(start_of(&p, &b, slot)?), 3600)
}

/// @LT-79/1 @LT-79/4: npc/4's Boy (actor 3) patrols chunk 39: walks 110 steps, turns, walks
/// 100, turns, … and loops back to the start of his walk; the walker gives the same path
/// every time.
#[test]
fn the_boy_patrols_and_loops() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let t = Tables::from_exe(&exe).unwrap();
    let p = path(&w, &t, 4, 3).expect("the Boy walks");
    let marks: Vec<Mark> = p.waypoints.iter().map(|w| w.mark).collect();
    assert_eq!(marks[..4], [Mark::Walk { steps: 110 }, Mark::Turn { dir: 6 << 9 }, Mark::Walk { steps: 100 }, Mark::Turn { dir: 4 << 9 }]);
    assert!(p.loops, "{marks:?}");
    assert_eq!(*marks.last().unwrap(), Mark::Loop);
    assert!(p.reach > 300, "{}", p.reach);
    assert!(p.points.len() >= 3, "{:?}", p.points);
    assert_eq!(p.points[0], [190, -129, -500], "it starts at his spot");
    let again = path(&w, &t, 4, 3).unwrap();
    assert_eq!(again.points, p.points);
    assert_eq!(again.frames, p.frames);
}

/// @LT-79/2: a walk is relative to its spot: the Boy moved 64 east and 32 south walks the same
/// path moved by as much, with the same marks on the same frames.
#[test]
fn a_moved_walk_moves_with_its_spot() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let t = Tables::from_exe(&exe).unwrap();
    let home = path(&w, &t, 4, 3).unwrap();
    let moved = path_from(&w, &t, 4, 3, |s| Start { x: s.x + 64, z: s.z - 32, ..s }).unwrap();
    assert_eq!(moved.points.len(), home.points.len());
    for (a, b) in home.points.iter().zip(&moved.points) {
        assert_eq!((b[0] - a[0], b[2] - a[2]), (64, -32), "{a:?} → {b:?}");
    }
    let marks = |p: &WalkPath| p.waypoints.iter().map(|w| (w.mark, w.frame)).collect::<Vec<_>>();
    assert_eq!(marks(&moved), marks(&home));
    assert_eq!(moved.loops, home.loops);
}

/// @LT-79/1: Meg in Orgo (npc/6 actor 5) walks somewhere from her spot.
#[test]
fn meg_walks() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let t = Tables::from_exe(&exe).unwrap();
    let p = path(&w, &t, 6, 5).expect("Meg has a placing branch");
    assert!(p.reach > 0, "{:?}", p.waypoints);
    assert!(p.waypoints.iter().any(|w| matches!(w.mark, Mark::Walk { .. })));
}

/// @LT-79/4: every actor in npc/0–49 whose New-game branch walks gets a path that ends (a
/// loop, a stop, a wait, a hide or the frame cap) with its points on the chunk.
#[test]
fn every_walking_actor_has_a_path() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let t = Tables::from_exe(&exe).unwrap();
    let (mut walkers, mut loops) = (0, 0);
    for n in 0..50u32 {
        if n == 17 {
            continue;
        }
        let b = w.file(&format!("npc/{n}")).unwrap();
        let p = NpcPack::parse(&b).unwrap();
        let d = p.d_block(&b);
        for a in p.actors.iter().filter(|a| a.slot >= 2 && !matches!((a.d >> 8) & 0xFF, 4 | 5)) {
            let Some(s) = start_of(&p, &b, a.slot) else { continue };
            // Only branches whose walk has a `2nnn` move.
            let walks = (0..64).map(|k| lt_core_word(d, s.walk as usize + k * 2)).take_while(|&w| w & 0xF000 != 0xA000).any(|w| w & 0xF000 == 0x2000 && w & 0xFFF != 0);
            if !walks {
                continue;
            }
            let Some(path) = path(&w, &t, n, a.slot) else { continue };
            walkers += 1;
            loops += path.loops as u32;
            assert!(path.frames <= 3600);
            // A walk may cross into the next chunk (coordinates stay local to the home chunk).
            for q in &path.points {
                assert!((-0x2000..0x2000).contains(&q[0]) && (-0x2000..0x2000).contains(&q[2]), "npc/{n} actor {}: {q:?}", a.slot);
            }
        }
    }
    assert!(walkers > 50, "{walkers}");
    assert!(loops > 20, "{loops} of {walkers}");
}

fn lt_core_word(d: &[u8], at: usize) -> u16 {
    if at + 2 <= d.len() {
        u16::from_le_bytes([d[at], d[at + 1]])
    } else {
        0
    }
}
