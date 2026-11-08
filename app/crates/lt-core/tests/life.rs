//! What a house adds to a pack besides doors and spots (`edit::life`, screen 23 phase 6): residents (one from
//! the pack, one brought in), smoke and a locked door's stand-in, read back and taken out again. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`.

use lt_core::edit::actor;
use lt_core::edit::character::Character;
use lt_core::edit::inside::{resident_walk, ResidentPlan};
use lt_core::edit::life::{self, Piece};
use lt_core::edit::{door, scene};
use lt_core::npc::NpcPack;
use lt_core::trigger::TriggerRecord;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn disc() -> Option<(DiscImage, PsxExe, FileTable)> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some((image, exe, table))
}

fn plan(area: u32, x: i16, name: &str, walk: Vec<u16>) -> ResidentPlan {
    ResidentPlan { index: 0, area, x, h: -33, z: 0, dir: 0, pack: 6, slot: 5, name: name.into(), line: "Welcome in!".into(), walk }
}

#[test]
fn a_house_adds_and_takes_out_its_own() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    // Something the project placed by hand first: a scene director on the village, which must survive.
    let (mine, _) = scene::create(&retail, &retail, &scene::NewScene { chunk: 78, x: 0, h: -33, z: 300, cue: 0 }, &[], &[]).unwrap();
    let before = actor::recover(&retail, &mine).unwrap();
    let doors_before = door::recover(&retail, &mine).unwrap();

    // The smallest character of npc/4 (the village pack is near its loader cap: the Boy himself doesn't fit).
    let src = w.file("npc/4").unwrap();
    let look = NpcPack::parse(&src)
        .unwrap()
        .actors
        .iter()
        .skip(2)
        .filter(|a| !actor::is_object(a.d) && (a.b >> 16) & 0xFF != 0xFF)
        .filter_map(|a| Character::from_actor(&src, a.slot as usize, format!("npc/4 · actor {}", a.slot)).ok())
        .min_by_key(|c| c.model.len() + c.poses.as_ref().map_or(0, Vec::len))
        .unwrap();
    let walk = resident_walk([10, 10], &[[14, 10], [14, 13]]).unwrap();
    let pieces = vec![
        Piece::Resident { plan: plan(1328, 100, "Meg", walk.clone()), from: 5, look: None },
        Piece::Resident { plan: plan(1328, -100, "", Vec::new()), from: life::carrier(&retail).unwrap(), look: Some(Box::new(look)) },
        Piece::Smoke { home: 78, x: -400, h: -353, z: -600 },
        Piece::Lock { home: 78, x: -400, h: -33, z: -700, dsl: life::lock_dsl(0x1A0, Some(504), [-400, -33, -760, 0]) },
    ];
    let (laid, slots) = life::add(&retail, &mine, 1328, &pieces, &[1328]).unwrap();
    let added = actor::recover(&retail, &laid).unwrap();
    assert_eq!(added.len(), before.len() + 4);
    let ours: Vec<_> = added.iter().filter(|a| a.house == Some(1328)).collect();
    assert_eq!(ours.len(), 4);
    assert_eq!(ours[0].walk, walk, "the resident walks");
    assert!(ours[1].character.is_some(), "the brought-in resident looks like the Boy");
    assert!(ours[2].spot && ours[2].smoke, "smoke is an object of type 5");
    assert!(ours[3].hidden, "the lock's director is hidden");
    let o = NpcPack::parse(&laid).unwrap();
    assert_eq!(actor::is_object(o.actors[slots[2] as usize].d), true);
    assert_eq!((o.actors[slots[2] as usize].d >> 8) & 0xFF, 5);
    // The lock's talk record names its director's character slot.
    let ch = scene::char_slot(&o.actors, slots[3] as usize).unwrap();
    let doors = door::recover(&retail, &laid).unwrap();
    assert!(doors.iter().any(|d| d.chunk == 78 && matches!(d.record, TriggerRecord::Talk { actor, .. } if actor == ch)), "{doors:?}");

    // Taking the house out leaves exactly what was there.
    let out = life::take_out(&retail, &laid, 1328).unwrap().unwrap();
    assert_eq!(actor::recover(&retail, &out).unwrap(), before);
    assert_eq!(door::recover(&retail, &out).unwrap(), doors_before);
    assert!(life::take_out(&retail, &out, 1328).unwrap().is_none(), "nothing left to take");
    assert_eq!(r.actors.len(), NpcPack::parse(&out).unwrap().actors.len() - before.len());
}
