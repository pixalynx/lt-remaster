//! Turning an actor already on the map (LT-74): the world editor's facing
//! control and R / ⇧R write the fourth placement word of the active branch,
//! in place, like a move at the same spot. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test facing`.

use lt_core::edit::actor::{self, AddedActor};
use lt_core::edit::placement::{self as place_edit, Place, PlaceWrite};
use lt_core::npc::NpcPack;
use lt_core::placement::{self, Body};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn disc() -> Option<(DiscImage, PsxExe, FileTable)> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some((image, exe, table))
}

/// The placing branch of `slot` and its words.
fn placed(b: &[u8], slot: u32) -> (u32, Place) {
    let p = NpcPack::parse(b).unwrap();
    let roster = placement::roster(&p, b);
    let a = roster.iter().find(|a| a.slot == slot).unwrap();
    a.branches
        .iter()
        .find_map(|br| match br.body {
            Body::Place { x, h, z, dir } => Some((br.at, Place { x, h, z, dir })),
            _ => None,
        })
        .unwrap()
}

/// @LT-74/1: the Fisher of npc/4 (actor 05, retail facing west, dir 3072)
/// turned in each of the eight directions at the same x/h/z: only the facing
/// word changes, it reads back, `moved` reports it like a move, and Revert to
/// retail puts the retail facing back byte for byte.
#[test]
fn a_retail_actor_turns_in_place_and_reverts() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/4").unwrap();
    let (at, from) = placed(&retail, 5);
    assert_eq!(from.dir, 3072, "the Fisher faces west in retail");
    for step in 0..8u16 {
        let to = Place { dir: step * 512, ..from };
        let out = place_edit::apply(&retail, &retail, &[PlaceWrite { at, place: Some(to) }]).unwrap();
        let changed: Vec<usize> = out.iter().zip(&retail).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
        assert!(changed.len() <= 2, "step {step}: only the facing word changes ({changed:?})");
        assert_eq!(placed(&out, 5), (at, to), "step {step}");
        let moved = place_edit::moved(&retail, &out);
        if to.dir == from.dir {
            assert!(moved.is_empty() && out == retail, "west is the retail facing");
            continue;
        }
        assert!(moved.iter().any(|(slot, m)| *slot == 5 && m.at == at && m.place == to), "step {step}: listed as moved");
        // Revert to retail: the retail words, the facing among them.
        let back = place_edit::apply(&retail, &out, &[PlaceWrite { at, place: None }]).unwrap();
        assert_eq!(back, retail, "step {step}: Revert gives the retail facing back");
    }
    // A turn after a move keeps the move: x/h/z and the new facing together.
    let moved = Place { x: 448, h: -129, z: -320, dir: from.dir };
    let out = place_edit::apply(&retail, &retail, &[PlaceWrite { at, place: Some(moved) }]).unwrap();
    let out = place_edit::apply(&retail, &out, &[PlaceWrite { at, place: Some(Place { dir: 1024, ..moved }) }]).unwrap();
    assert_eq!(placed(&out, 5).1, Place { x: 448, h: -129, z: -320, dir: 1024 });
    // The word is checked (0–4095).
    let err = place_edit::apply(&retail, &retail, &[PlaceWrite { at, place: Some(Place { dir: 4096, ..from }) }]).unwrap_err();
    assert_eq!(err.code, "PLACE_DIR");
}

/// @LT-74/1: an added copy (npc/6, the village pack, has room) turns the same
/// way: its program's facing word changes and `actor::recover` reads it back.
/// Every retail facing is one of the eight 45° steps the control offers.
#[test]
fn an_added_actor_turns_and_retail_faces_the_eight_steps() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let roster = placement::roster(&r, &retail);
    let src = roster
        .iter()
        .find(|a| !a.party && !a.object && a.speaker.is_some() && a.home != 0 && !matches!(a.look.kind, 4 | 5))
        .expect("a villager to copy");
    let (_, p) = placed(&retail, src.slot);
    let copy = AddedActor { from: src.slot, home: src.home, x: p.x + 64, h: p.h, z: p.z, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false };
    let (out, _) = actor::apply(&retail, &retail, &[copy]).unwrap();
    let base = r.actors.len() as u32;
    let (at, now) = placed(&out, base);
    assert_eq!(now.dir, 0);
    let turned = place_edit::apply(&retail, &out, &[PlaceWrite { at, place: Some(Place { dir: 1536, ..now }) }]).unwrap();
    let changed = turned.iter().zip(&out).filter(|(a, b)| a != b).count();
    assert!(changed <= 2, "only the facing word changes ({changed} bytes)");
    let back = actor::recover(&retail, &turned).unwrap();
    assert_eq!((back[0].x, back[0].h, back[0].z, back[0].dir), (p.x + 64, p.h, p.z, 1536));

    // Every retail placement faces one of the eight steps (3,954 placing branches in npc/0–49: N 2,621,
    // NE 29, E 282, SE 7, S 470, SW 22, W 501, NW 22), so the 8-way control shows each exactly.
    let mut steps = [0usize; 8];
    for n in 0..50u32 {
        let b = w.file(&format!("npc/{n}")).unwrap();
        let Some(pack) = NpcPack::parse(&b) else { continue };
        for a in placement::roster(&pack, &b) {
            for br in &a.branches {
                if let Body::Place { dir, .. } = br.body {
                    assert_eq!(dir % 512, 0, "npc/{n} actor {} faces {dir}, between the steps", a.slot);
                    steps[(dir / 512) as usize] += 1;
                }
            }
        }
    }
    assert!(steps.iter().all(|&k| k > 0), "{steps:?}");
}
