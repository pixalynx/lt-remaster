//! A pack's character budget (`edit::actor::budget`, LT-55) on every retail
//! pack, blank-slated packs and packs filled to the limit. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`.

use std::collections::BTreeSet;

use lt_core::edit::actor::{self, AddedActor, NEW_SPOT, NPC_SLOTS};
use lt_core::edit::{remove, scene, spot};
use lt_core::npc::NpcPack;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn disc() -> Option<(DiscImage, PsxExe, FileTable)> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some((image, exe, table))
}

fn copy(from: u32, k: i16) -> AddedActor {
    AddedActor { from, home: 1328, x: 64 * k, h: -129, z: 0, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false }
}

fn sign(k: i16) -> AddedActor {
    let own = Some(spot::program(&spot::Preset::Sign { line: "Hi.".into() }, 1328).unwrap());
    AddedActor { from: NEW_SPOT, home: 1328, x: 64 * k, h: -129, z: 256, dir: 0, hidden: false, own, character: None, spot: true, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false }
}

/// @LT-55/1: every pack's budget: characters of 80 (records that aren't examine spots), the
/// records and characters that still fit; on a blank slate the taken-out retail actors are
/// counted separately and keep their slots.
#[test]
fn every_pack_reports_its_character_budget() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut seen = std::collections::BTreeMap::new();
    for n in 0..50u32 {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let r = NpcPack::parse(&retail).unwrap();
        let chars = r.actors.iter().filter(|a| !actor::is_object(a.d)).count();
        let room = actor::room(&retail).unwrap();
        let b = actor::budget(&retail, &retail).unwrap();
        assert_eq!((b.characters, b.limit, b.taken_out, b.added, b.spots), (chars, NPC_SLOTS, 0, 0, 0), "npc/{n}");
        assert_eq!((b.records_left, b.characters_left), (room, room.min(NPC_SLOTS - chars)), "npc/{n}");
        // Blank-slated: every retail actor out, and still in the count.
        let all: BTreeSet<u32> = remove::removable(&retail).unwrap().into_iter().map(|(s, _)| s).collect();
        let blank = remove::apply(&retail, &retail, &all).unwrap();
        let bb = actor::budget(&retail, &blank).unwrap();
        assert_eq!((bb.characters, bb.taken_out, bb.characters_left), (chars, chars - 2, b.characters_left), "npc/{n}");
        seen.insert(n, (b.characters, b.characters_left));
    }
    // The packs the LT-49 slice named.
    assert_eq!(seen[&22], (77, 3));
    assert_eq!(seen[&23], (67, 3));
    assert_eq!(seen[&21], (63, 17));
}

/// @LT-55/2 @LT-55/4: what the budget says still fits is exactly what the encoder takes. npc/22
/// (77 characters) takes three copies; the third is character slot 79, and a fourth is refused
/// with ACTOR_SLOTS, while examine spots still fit. npc/7, where the records run out first,
/// refuses with ACTOR_ROOM.
#[test]
fn the_budget_says_what_the_encoder_refuses() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };

    let retail = w.file("npc/22").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let src = r.actors.iter().skip(2).find(|a| !actor::is_object(a.d) && a.script != 0).unwrap().slot;
    let three: Vec<AddedActor> = (0..3).map(|k| copy(src, k)).collect();
    let (out, _) = actor::apply(&retail, &retail, &three).unwrap();
    let b = actor::budget(&retail, &out).unwrap();
    assert_eq!((b.characters, b.added, b.characters_left), (80, 3, 0));
    assert!(b.records_left > 0);
    let o = NpcPack::parse(&out).unwrap();
    assert_eq!(scene::char_slot(&o.actors, o.actors.len() - 1), Some(79), "the third copy is character slot 79");
    let mut four = three.clone();
    four.push(copy(src, 3));
    assert_eq!(actor::apply(&retail, &out, &four).unwrap_err().code, "ACTOR_SLOTS");
    let mut spots = three.clone();
    spots.push(sign(0));
    let (with_spot, _) = actor::apply(&retail, &out, &spots).unwrap();
    let bs = actor::budget(&retail, &with_spot).unwrap();
    assert_eq!((bs.characters, bs.spots, bs.records_left), (80, 1, b.records_left - 1));

    let retail = w.file("npc/7").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let src = r.actors.iter().skip(2).find(|a| !actor::is_object(a.d) && a.script != 0).unwrap().slot;
    let room = actor::room(&retail).unwrap();
    let full: Vec<AddedActor> = (0..room as i16).map(|k| copy(src, k)).collect();
    let (out, _) = actor::apply(&retail, &retail, &full).unwrap();
    let b = actor::budget(&retail, &out).unwrap();
    assert_eq!((b.records_left, b.characters_left), (0, 0));
    assert!(b.characters < NPC_SLOTS);
    let mut more = full.clone();
    more.push(copy(src, room as i16));
    assert_eq!(actor::apply(&retail, &out, &more).unwrap_err().code, "ACTOR_ROOM");
}
