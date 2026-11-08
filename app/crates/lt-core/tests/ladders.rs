//! Placement ladders edited per actor (`edit::ladder`, LT-76) on every retail
//! pack. Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`.

use std::collections::BTreeMap;

use lt_core::edit::ladder::{self, NewBody, Rung, RungEdit};
use lt_core::edit::placement::{self, Place, PlaceWrite};
use lt_core::edit::{actor, remove, text};
use lt_core::npc::NpcPack;
use lt_core::placement::{active, decode, roster, Body};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn disc() -> Option<(DiscImage, PsxExe, FileTable)> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some((image, exe, table))
}

/// The edits that keep `rungs` as they are.
fn keep(rungs: &[Rung]) -> Vec<RungEdit> {
    rungs.iter().enumerate().map(|(i, r)| RungEdit { from: Some(i as u32), cond: r.cond, body: None, walk: None }).collect()
}

/// Slots a ladder can be edited for: not the party, not examine spots, and a
/// ladder that ends in an unconditional rung.
fn editable(p: &NpcPack, b: &[u8]) -> Vec<u32> {
    let d = p.d_block(b);
    p.actors
        .iter()
        .filter(|a| a.slot >= 2 && !actor::is_object(a.d) && decode(d, a.placement).last().is_some_and(|br| br.cond == 0))
        .map(|a| a.slot)
        .collect()
}

/// Mary in npc/6: her slot and the D offset of her 0x020 rung (the letter errand).
fn mary(retail: &[u8]) -> (u32, u32, u32) {
    let p = NpcPack::parse(retail).unwrap();
    let a = roster(&p, retail).into_iter().find(|a| a.speaker.as_deref() == Some("Mary") && a.branches.iter().any(|b| b.cond == 0x20)).expect("Mary with a 0x020 rung");
    let errand = a.branches.iter().find(|b| b.cond == 0x20).unwrap().at;
    let home = a.branches.last().unwrap().at;
    (a.slot, errand, home)
}

/// @LT-76/3: every retail ladder copies as it is, and keeping every rung is no edit at all
/// (the pack stays retail, byte for byte).
#[test]
fn keeping_every_rung_is_retail() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut ladders = 0;
    for n in 0..50u32 {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let p = NpcPack::parse(&retail).unwrap();
        let d = p.d_block(&retail);
        for slot in editable(&p, &retail) {
            let a = &p.actors[slot as usize];
            let rungs = ladder::retail_rungs(&retail, slot).unwrap();
            // The copy reads back as the game reads the retail program.
            let copy = ladder::program(slot, &rungs);
            let got = ladder::rungs(&copy, ladder::HEADER_LEN as u32, ladder::record_type(a.d));
            assert_eq!(got, rungs, "npc/{n} actor {slot}");
            let (want, have) = (decode(d, a.placement), decode(&copy, ladder::HEADER_LEN as u32));
            assert_eq!(want.iter().map(|b| (b.cond, b.body)).collect::<Vec<_>>(), have.iter().map(|b| (b.cond, b.body)).collect::<Vec<_>>(), "npc/{n} actor {slot}");
            ladder::check(&retail, slot, &rungs).unwrap_or_else(|e| panic!("npc/{n} actor {slot}: {e}"));
            let (out, _) = ladder::apply(&retail, &retail, slot, Some(&keep(&rungs))).unwrap();
            assert!(out == retail, "npc/{n} actor {slot}: keeping its rungs changed the pack");
            ladders += 1;
        }
    }
    assert!(ladders > 1000, "{ladders}");
}

/// @LT-76/3 @LT-76/4: on every pack, every actor with a ladder gets a new top branch
/// (hidden while 0x1FF is set; a chest stands elsewhere instead). The game reads it
/// first; with the flag clear it stands where retail does; actors that shared the
/// program keep the retail one; and reverting every ladder gives retail back.
#[test]
fn every_actor_takes_a_new_branch() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut edited = 0;
    for n in 0..50u32 {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let r = NpcPack::parse(&retail).unwrap();
        let mut b = retail.clone();
        let mut done = Vec::new();
        for slot in editable(&r, &retail) {
            let a = &r.actors[slot as usize];
            let chest = ladder::record_type(a.d) == 3;
            let rungs = ladder::retail_rungs(&retail, slot).unwrap();
            if rungs.len() >= ladder::MAX_RUNGS {
                continue;
            }
            let top = RungEdit { from: None, cond: 0x1FF, body: Some(if chest { NewBody::Place { x: 16, h: -129, z: -16, dir: 1024 } } else { NewBody::Hidden }), walk: None };
            let edits: Vec<RungEdit> = std::iter::once(top).chain(keep(&rungs)).collect();
            let out = match ladder::apply(&retail, &b, slot, Some(&edits)) {
                Ok((out, _)) => out,
                Err(e) if e.code == "NPC_CAP" => break,
                Err(e) => panic!("npc/{n} actor {slot}: {e}"),
            };
            b = out;
            done.push(slot);
            let c = NpcPack::parse(&b).unwrap();
            let cd = c.d_block(&b);
            let now = decode(cd, c.actors[slot as usize].placement);
            let before = decode(r.d_block(&retail), a.placement);
            assert_eq!(now.len(), before.len() + 1, "npc/{n} actor {slot}");
            assert_eq!(now[0].cond, 0x1FF);
            let want_top = if chest { Body::Place { x: 16, h: -129, z: -16, dir: 1024 } } else { Body::Absent };
            assert_eq!(active(&now, &|f| f == 0x1FF).unwrap().body, want_top, "npc/{n} actor {slot}");
            let flags = |f: u16| f == 0x20;
            assert_eq!(active(&now, &flags).map(|b| b.body), active(&before, &flags).map(|b| b.body), "npc/{n} actor {slot}");
            // Sharers keep retail's program.
            for o in r.actors.iter().filter(|o| o.slot != slot && o.placement == a.placement && !done.contains(&o.slot)) {
                assert_eq!(c.actors[o.slot as usize].placement, a.placement, "npc/{n} actor {} shares with {slot}", o.slot);
            }
            edited += 1;
        }
        // Everything reads back; a text-layout pass (another encoder's save) keeps it.
        let set = ladder::recover(&retail, &b).unwrap();
        assert_eq!(set.keys().copied().collect::<Vec<_>>(), done, "npc/{n}");
        let again = text::apply(&retail, &b, &text::edit_set(&retail, &b).unwrap()).unwrap().0;
        assert!(again == b, "npc/{n}: a text save moved the ladders");
        // Back to retail one at a time.
        for &slot in &done {
            b = ladder::apply(&retail, &b, slot, None).unwrap().0;
        }
        assert!(b == retail, "npc/{n}: reverting every ladder left bytes behind");
    }
    assert!(edited > 1000, "{edited}");
}

/// @LT-76/1: Mary's errand spot (npc/6, the 0x020 rung) moves without touching her home
/// spot, on the retail ladder (in place) and on an edited one (in the relocation area,
/// where a later save of the pack keeps the move).
#[test]
fn marys_errand_spot_moves_alone() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let (slot, errand, home) = mary(&retail);
    let spot = Place { x: 100, h: -33, z: 200, dir: 2048 };
    let words = |b: &[u8]| {
        let p = NpcPack::parse(b).unwrap();
        let d = p.d_block(b);
        decode(d, p.actors[slot as usize].placement).into_iter().map(|br| (br.cond, br.body)).collect::<BTreeMap<_, _>>()
    };
    let before = words(&retail);
    let moved = placement::apply(&retail, &retail, &[PlaceWrite { at: errand, place: Some(spot) }]).unwrap();
    let after = words(&moved);
    assert_eq!(after[&0x20], Body::Place { x: 100, h: -33, z: 200, dir: 2048 });
    assert_eq!(after[&0], before[&0], "her home spot");
    assert_eq!(placement::place_at(NpcPack::parse(&moved).unwrap().d_block(&moved), home), placement::place_at(NpcPack::parse(&retail).unwrap().d_block(&retail), home));

    // An edited ladder: a new top branch, then the errand rung moves where the copy is.
    let rungs = ladder::retail_rungs(&retail, slot).unwrap();
    let edits: Vec<RungEdit> = std::iter::once(RungEdit { from: None, cond: 0x1FE, body: Some(NewBody::Hidden), walk: None }).chain(keep(&rungs)).collect();
    let (b, report) = ladder::apply(&retail, &moved, slot, Some(&edits)).unwrap();
    assert!(report.ladder_bytes > 0);
    let p = NpcPack::parse(&b).unwrap();
    let copy = decode(p.d_block(&b), p.actors[slot as usize].placement);
    assert_eq!(copy[0].cond, 0x1FE);
    assert_eq!(copy.iter().find(|br| br.cond == 0x20).unwrap().body, after[&0x20], "the copy keeps the in-place move");
    let errand2 = copy.iter().find(|br| br.cond == 0x20).unwrap().at;
    let spot2 = Place { x: -300, h: -33, z: 50, dir: 0 };
    let b2 = placement::apply(&retail, &b, &[PlaceWrite { at: errand2, place: Some(spot2) }]).unwrap();
    let laid = text::apply(&retail, &b2, &text::edit_set(&retail, &b2).unwrap()).unwrap().0;
    let w2 = words(&laid);
    assert_eq!(w2[&0x20], Body::Place { x: -300, h: -33, z: 50, dir: 0 });
    assert_eq!(w2[&0], before[&0]);
    assert_eq!(w2[&0x1FE], Body::Absent);
}

/// @LT-76/3: taking an actor out wins over its edited ladder; the other encoders keep
/// ladders through their saves (an added actor copying an edited one gets its own program).
#[test]
fn ladders_live_with_the_other_edits() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let (slot, _, _) = mary(&retail);
    let rungs = ladder::retail_rungs(&retail, slot).unwrap();
    let edits: Vec<RungEdit> = std::iter::once(RungEdit { from: None, cond: 0x0C0, body: Some(NewBody::Place { x: 0, h: -33, z: 0, dir: 0 }), walk: None }).chain(keep(&rungs)).collect();
    let (b, _) = ladder::apply(&retail, &retail, slot, Some(&edits)).unwrap();
    // A copy of Mary on a new area.
    let copy = actor::AddedActor { from: slot, home: 1330, x: 10, h: -33, z: 10, dir: 0, hidden: false, character: None, own: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false };
    if actor::room(&retail).unwrap() > 0 {
        let (b2, _) = actor::apply(&retail, &b, std::slice::from_ref(&copy)).unwrap();
        assert_eq!(ladder::recover(&retail, &b2).unwrap().keys().copied().collect::<Vec<_>>(), vec![slot]);
        assert_eq!(actor::recover(&retail, &b2).unwrap().len(), 1);
    }
    // Taken out: the removal block wins and the ladder edit goes.
    let out = remove::apply(&retail, &b, &[slot].into_iter().collect()).unwrap();
    assert!(ladder::recover(&retail, &out).unwrap().is_empty());
    assert_eq!(ladder::apply(&retail, &out, slot, Some(&edits)).unwrap_err().code, "LADDER_REMOVED");
    // Refusals.
    let err = |e: &[RungEdit]| ladder::apply(&retail, &retail, slot, Some(e)).unwrap_err().code;
    assert_eq!(err(&[RungEdit { from: None, cond: 0x20, body: Some(NewBody::Hidden), walk: None }]), "LADDER_NO_FALLBACK");
    assert_eq!(err(&[RungEdit { from: Some(0), cond: 0, body: None, walk: None }, RungEdit { from: None, cond: 0, body: Some(NewBody::Hidden), walk: None }]), "LADDER_EARLY_ALWAYS");
    assert_eq!(err(&[RungEdit { from: None, cond: 700, body: Some(NewBody::Hidden), walk: None }, RungEdit { from: Some(0), cond: 0, body: None, walk: None }]), "LADDER_COND");
    assert_eq!(ladder::apply(&retail, &retail, 0, Some(&[])).unwrap_err().code, "LADDER_PARTY");
    // npc/49 actor 4's retail ladder has no unconditional rung (with 0x0BF clear the game
    // scans on into the words after it), so a copy needs one added before it saves.
    let r49 = w.file("npc/49").unwrap();
    let theirs = ladder::retail_rungs(&r49, 4).unwrap();
    assert_ne!(theirs.last().unwrap().cond, 0);
    assert_eq!(ladder::apply(&r49, &r49, 4, Some(&keep(&theirs))).unwrap_err().code, "LADDER_NO_FALLBACK");
    let fallback: Vec<RungEdit> = keep(&theirs).into_iter().chain([RungEdit { from: None, cond: 0, body: Some(NewBody::Hidden), walk: None }]).collect();
    let (b49, _) = ladder::apply(&r49, &r49, 4, Some(&fallback)).unwrap();
    assert_eq!(ladder::recover(&r49, &b49).unwrap()[&4].len(), theirs.len() + 1);
    assert_eq!(ladder::apply(&retail, &retail, slot, Some(&[])).unwrap_err().code, "LADDER_RUNGS");
}
