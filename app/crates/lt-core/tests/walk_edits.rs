//! Walks the World editor writes (`edit::walk` through `edit::ladder` and
//! `edit::actor`, LT-95) on retail packs, checked against the walker
//! (`scene::walk::walk_path`). Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`.

use lt_core::edit::actor::{self, AddedActor};
use lt_core::edit::ladder::{self, Rung, RungEdit};
use lt_core::edit::{text, walk};
use lt_core::npc::NpcPack;
use lt_core::placement::{active, decode, roster, Body, Branch};
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

/// The edits that keep `rungs`, with rung `i` walking `words`.
fn with_walk(rungs: &[Rung], i: usize, words: &[u16]) -> Vec<RungEdit> {
    rungs.iter().enumerate().map(|(k, r)| RungEdit { from: Some(k as u32), cond: r.cond, body: None, walk: (k == i).then(|| words.to_vec()) }).collect()
}

/// The branch the New-game flags pick for `slot` of `b`.
fn shown(b: &[u8], slot: u32) -> Option<Branch> {
    let p = NpcPack::parse(b).unwrap();
    active(&decode(p.d_block(b), p.actors[slot as usize].placement), &|f| f == 0 || FLAGS.contains(&f)).cloned()
}

/// `slot` of `b` walking from `br` on `chunk`.
fn simulate(w: &WorldDisc, t: &Tables, b: &[u8], chunk: u16, slot: u32, br: &Branch) -> Option<WalkPath> {
    let Body::Place { x, h, z, dir } = br.body else { return None };
    let p = NpcPack::parse(b).unwrap();
    let c = w.chunk(chunk as u32).ok()?;
    walk_path(t, &p, b, chunk, world::heightmap(&c), &FLAGS, slot, Start { x, h, z, dir, walk: br.at + 10 }, 3600)
}

/// The points the walker bends at for `legs`: each leg's end where the next leg that moves
/// steps differently, then where it ends.
fn bends(start: (i32, i32), legs: &[walk::Leg]) -> Vec<(i32, i32)> {
    let moving: Vec<&walk::Leg> = legs.iter().filter(|l| l.steps > 0 && l.step != (0, 0)).collect();
    let mut out = vec![start];
    for (k, l) in moving.iter().enumerate() {
        if moving.get(k + 1).is_none_or(|n| n.step != l.step) && out.last() != Some(&l.end) {
            out.push(l.end);
        }
    }
    out
}

/// @LT-95/2: the editor's leg arithmetic is the walker's. The sine table at the eight
/// headings is what the frontend uses (0, 2896, 4096), and for every retail walker in
/// npc/0–49 the bends of `walk::legs` are the simulated path's.
#[test]
fn legs_are_the_walkers() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let t = Tables::from_exe(&exe).unwrap();
    let s = [0, 2896, 4096, 2896, 0, -2896, -4096, -2896];
    for k in 0..8 {
        assert_eq!((t.sin(k * 512), t.cos(k * 512)), (s[k as usize], s[((k as usize) + 2) % 8]), "heading {k}");
    }
    let (mut checked, mut whole) = (0, 0);
    for n in (0..50).filter(|&n| n != 17) {
        let b = w.file(&format!("npc/{n}")).unwrap();
        let p = NpcPack::parse(&b).unwrap();
        for a in p.actors.iter().filter(|a| a.slot >= 2 && !actor::is_object(a.d)) {
            let Some(br) = shown(&b, a.slot) else { continue };
            let Body::Place { x, z, dir, .. } = br.body else { continue };
            if !br.walk.iter().any(|&v| v & 0xF000 == 0x2000 && v & 0xFFF != 0) {
                continue;
            }
            let Some(path) = simulate(&w, &t, &b, (a.b & 0xFFFF) as u16, a.slot, &br) else { continue };
            let want = bends((x as i32, z as i32), &walk::legs(&t, &br.walk, (x as i32, z as i32), dir, 0));
            let got: Vec<(i32, i32)> = path.points.iter().map(|q| (q[0] as i32, q[2] as i32)).collect();
            let done = matches!(path.waypoints.last().map(|w| w.mark), Some(Mark::Loop | Mark::Stop | Mark::Replace { .. }));
            if done {
                assert_eq!(got, want, "npc/{n} actor {}: {:04X?}", a.slot, br.walk);
                whole += 1;
            } else {
                // Cut short (a wait, leaving the map, the frame cap): the bends so far.
                let k = got.len().saturating_sub(1).min(want.len());
                assert_eq!(got[..k], want[..k], "npc/{n} actor {}", a.slot);
            }
            checked += 1;
        }
    }
    assert!(checked > 50 && whole > 20, "{checked} walkers, {whole} whole");
}

/// The Boy in npc/4 (actor 3), whose one rung patrols chunk 39.
const BOY: (u32, u32) = (4, 3);
/// A new walk: speed 4, east 32 steps, north 16, west 32, south 16, loop.
const SQUARE: [u16; 10] = [0x8004, 0x1002, 0x2020, 0x1000, 0x2010, 0x1006, 0x2020, 0x1004, 0x2010, walk::LOOP];

/// @LT-95/1: the Boy's walk is edited (his ladder goes to the relocation area with the new
/// walk), reads back, a text save keeps it, and the walker walks it: four bends and back
/// to his spot. Sending his own walk back gives the retail pack, byte for byte.
#[test]
fn a_retail_walk_is_edited_and_put_back() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let t = Tables::from_exe(&exe).unwrap();
    let retail = w.file(&format!("npc/{}", BOY.0)).unwrap();
    let slot = BOY.1;
    let rungs = ladder::retail_rungs(&retail, slot).unwrap();
    let i = rungs.len() - 1;
    let own = walk::of(&rungs[i].words).to_vec();
    walk::check(&own).expect("his retail walk is one the editor writes");
    let (b, report) = ladder::apply(&retail, &retail, slot, Some(&with_walk(&rungs, i, &SQUARE))).unwrap();
    assert!(report.ladder_bytes > 0);
    let br = shown(&b, slot).unwrap();
    assert_eq!(br.walk, SQUARE);
    assert_eq!(walk::of(&ladder::recover(&retail, &b).unwrap()[&slot][i].words), SQUARE);
    let laid = text::apply(&retail, &b, &text::edit_set(&retail, &b).unwrap()).unwrap().0;
    assert_eq!(shown(&laid, slot).unwrap().walk, SQUARE, "a later save keeps it");

    let home = (NpcPack::parse(&retail).unwrap().actors[slot as usize].b & 0xFFFF) as u16;
    let path = simulate(&w, &t, &b, home, slot, &br).unwrap();
    let Body::Place { x, z, .. } = br.body else { panic!() };
    let (x, z) = (x as i32, z as i32);
    let got: Vec<(i32, i32)> = path.points.iter().map(|q| (q[0] as i32, q[2] as i32)).collect();
    assert_eq!(got, [(x, z), (x + 128, z), (x + 128, z + 64), (x, z + 64), (x, z)]);
    assert!(path.loops);

    // His own walk back: no edit at all.
    let (back, _) = ladder::apply(&retail, &b, slot, Some(&with_walk(&rungs, i, &own))).unwrap();
    assert_eq!(back, retail, "the walk edited back to retail");
}

/// @LT-95/1: every retail walk the editor can write (one ending, last) sent back unchanged
/// leaves its pack's retail bytes, for every placing rung of every editable actor.
#[test]
fn every_retail_walk_sent_back_is_no_edit() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let (mut same, mut other) = (0, 0);
    for n in (0..50).filter(|&n| n != 17) {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let p = NpcPack::parse(&retail).unwrap();
        for a in p.actors.iter().filter(|a| a.slot >= 2 && !actor::is_object(a.d)) {
            let Ok(rungs) = ladder::retail_rungs(&retail, a.slot) else { continue };
            if rungs.last().is_none_or(|r| r.cond != 0) {
                continue;
            }
            for (i, r) in rungs.iter().enumerate() {
                let words = walk::of(&r.words);
                if r.words.len() < 5 || r.words[0] == 0x7FFF || r.words[0] == 10000 {
                    continue;
                }
                if walk::check(words).is_err() {
                    other += 1;
                    continue;
                }
                let (b, _) = ladder::apply(&retail, &retail, a.slot, Some(&with_walk(&rungs, i, words))).unwrap();
                assert!(b == retail, "npc/{n} actor {} rung {i}", a.slot);
                same += 1;
            }
        }
    }
    assert!(same > 500, "{same} walks round-trip, {other} the editor wouldn't write");
}

/// @LT-95/3: the first actor of npc/6 that stands still (its last rung's walk `0000`) gets a
/// walk there, the walker walks it, and *Remove walk* (`0000`) gives the retail pack back.
/// Only a rung that places can walk.
#[test]
fn a_standing_actor_gets_a_walk_and_loses_it() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let t = Tables::from_exe(&exe).unwrap();
    let retail = w.file("npc/6").unwrap();
    let p = NpcPack::parse(&retail).unwrap();
    let ladder_of = |slot: u32| ladder::retail_rungs(&retail, slot).unwrap();
    let editable = |a: &&lt_core::npc::ActorRecord| a.slot >= 2 && !actor::is_object(a.d) && ladder_of(a.slot).last().is_some_and(|r| r.cond == 0);
    let stander = p.actors.iter().filter(editable).find(|a| shown(&retail, a.slot).is_some_and(|b| b.walk == walk::STAND && matches!(b.body, Body::Place { .. }))).expect("one stands still").slot;
    let rungs = ladder_of(stander);
    let i = rungs.iter().position(|r| r.words.len() >= 5 && walk::of(&r.words) == walk::STAND && shown(&retail, stander).is_some_and(|b| b.cond == r.cond)).unwrap();
    let (b, _) = ladder::apply(&retail, &retail, stander, Some(&with_walk(&rungs, i, &SQUARE))).unwrap();
    let br = shown(&b, stander).unwrap();
    assert_eq!(br.walk, SQUARE);
    let path = simulate(&w, &t, &b, (p.actors[stander as usize].b & 0xFFFF) as u16, stander, &br).unwrap();
    assert_eq!(path.reach, 143, "the square's far corner: √(128² + 64²)");
    assert!(path.loops);
    let (back, _) = ladder::apply(&retail, &b, stander, Some(&with_walk(&rungs, i, &walk::STAND))).unwrap();
    assert_eq!(back, retail);
    // Only a rung that places can walk.
    let (who, hidden) = p.actors.iter().filter(editable).find_map(|a| ladder_of(a.slot).iter().position(|r| r.words.first() == Some(&0x7FFF)).map(|k| (a.slot, k))).expect("a hidden rung");
    let err = ladder::apply(&retail, &retail, who, Some(&with_walk(&ladder_of(who), hidden, &SQUARE))).unwrap_err();
    assert_eq!(err.code, "WALK_STANDS");
}

fn copy(from: u32, home: u16) -> AddedActor {
    AddedActor { from, home, x: -100, h: -129, z: 200, dir: 1024, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false }
}

/// @LT-95/3: a copy walks. A copy of Meg on chunk 78 with the square walk: its program puts
/// `"LT" from` after the walk, it reads back, a text save and another copy keep it, the
/// walker walks it, a flag branch goes on in front of it, and removing the walk gives the
/// 16-byte program back. A copy hidden otherwise, or a spot, can't walk.
#[test]
fn a_copy_walks() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let t = Tables::from_exe(&exe).unwrap();
    let retail = w.file("npc/6").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let meg = roster(&r, &retail).into_iter().find(|a| a.speaker.as_deref() == Some("Meg")).unwrap().slot;
    let slot = r.actors.len() as u32;
    let walker = AddedActor { walk: SQUARE.to_vec(), ..copy(meg, 78) };
    assert_eq!(actor::program(&walker).len(), ((7 + SQUARE.len()) * 2).next_multiple_of(4), "the walk where `0000` was, padded");
    let (b, _) = actor::apply(&retail, &retail, &[walker.clone(), copy(meg, 78)]).unwrap();
    let back = actor::recover(&retail, &b).unwrap();
    assert_eq!(back[0].walk, SQUARE);
    assert!(back[1].walk.is_empty(), "the other copy stands still");
    let br = shown(&b, slot).unwrap();
    assert_eq!(br.walk, SQUARE);
    let laid = text::apply(&retail, &b, &text::edit_set(&retail, &b).unwrap()).unwrap().0;
    assert_eq!(actor::recover(&retail, &laid).unwrap()[0].walk, SQUARE, "a later save keeps it");
    let path = simulate(&w, &t, &b, 78, slot, &br).unwrap();
    let got: Vec<(i32, i32)> = path.points.iter().map(|q| (q[0] as i32, q[2] as i32)).collect();
    assert_eq!(got, [(-100, 200), (28, 200), (28, 264), (-100, 264), (-100, 200)]);

    // A branch in front (stands elsewhere while 0x1F3 is set); its own rung still walks.
    let now = ladder::copy_rungs(&back[0]);
    let mut edits = with_walk(&now, 0, &SQUARE);
    edits.insert(0, RungEdit { from: None, cond: 0x1F3, body: Some(ladder::NewBody::Place { x: 300, h: -129, z: 300, dir: 0 }), walk: None });
    let (b2, _) = ladder::apply(&retail, &b, slot, Some(&edits)).unwrap();
    let a2 = &actor::recover(&retail, &b2).unwrap()[0];
    assert_eq!((a2.branches.len(), a2.walk.as_slice()), (1, &SQUARE[..]));
    // The walk on the copy's own rung, edited through the ladder, then taken off.
    let short = [0x8002, 0x1002, 0x2008, 0x0000];
    let (b3, _) = ladder::apply(&retail, &b2, slot, Some(&with_walk(&ladder::copy_rungs(a2), 1, &short))).unwrap();
    assert_eq!(actor::recover(&retail, &b3).unwrap()[0].walk, short);
    let (b4, _) = ladder::apply(&retail, &b3, slot, Some(&with_walk(&ladder::copy_rungs(&actor::recover(&retail, &b3).unwrap()[0]), 1, &walk::STAND))).unwrap();
    let a4 = &actor::recover(&retail, &b4).unwrap()[0];
    assert!(a4.walk.is_empty());
    assert_eq!(actor::program(&AddedActor { branches: Vec::new(), ..a4.clone() }).len(), actor::PROGRAM_LEN);

    // Refusals.
    let refuse = |a: AddedActor| actor::apply(&retail, &retail, &[a]).unwrap_err().code;
    assert_eq!(refuse(AddedActor { otherwise_hidden: true, ..walker.clone() }), "WALK_STANDS");
    assert_eq!(refuse(AddedActor { walk: vec![0x2010, 0x0000, 0x4000], ..walker.clone() }), "WALK_WORDS");
}
