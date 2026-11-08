//! Flag branches on actors the project added (`edit::actor` copies through
//! `edit::ladder`, LT-77). Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`.

use lt_core::edit::actor::{self, AddedActor, NEW_SPOT};
use lt_core::edit::ladder::{self, NewBody, Rung, RungEdit};
use lt_core::edit::placement::{self, Place, PlaceWrite};
use lt_core::edit::{remove, spot, text};
use lt_core::npc::NpcPack;
use lt_core::placement::{active, decode, Body};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn disc() -> Option<(DiscImage, PsxExe, FileTable)> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some((image, exe, table))
}

fn copy(from: u32, home: u16, x: i16, z: i16) -> AddedActor {
    AddedActor { from, home, x, h: -129, z, dir: 1024, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false }
}

/// The ladder the game reads for record `slot` of `b`.
fn ladder_of(b: &[u8], slot: u32) -> Vec<(u16, Body)> {
    let p = NpcPack::parse(b).unwrap();
    decode(p.d_block(b), p.actors[slot as usize].placement).into_iter().map(|br| (br.cond, br.body)).collect()
}

/// The edits that keep `rungs` as they are.
fn keep(rungs: &[Rung]) -> Vec<RungEdit> {
    rungs.iter().enumerate().map(|(i, r)| RungEdit { from: Some(i as u32), cond: r.cond, body: None, walk: None }).collect()
}

const PLACE: Body = Body::Place { x: 100, h: -129, z: 200, dir: 2048 };

/// "The cats appear after a flag": hidden otherwise, standing at a spot while 0x1F3 is set.
fn appear_after(now: &[Rung]) -> Vec<RungEdit> {
    let own = now.len() as u32 - 1;
    vec![
        RungEdit { from: None, cond: 0x1F3, body: Some(NewBody::Place { x: 100, h: -129, z: 200, dir: 2048 }), walk: None },
        RungEdit { from: Some(own), cond: 0, body: Some(NewBody::Hidden), walk: None },
    ]
}

/// @LT-77/1 @LT-77/2: a copy of Mary (npc/6) homed on a new area takes a branch and a
/// hidden fallback; the game reads them in order; the copy keeps its spot for later; a
/// branch moves where it is; other edits (a text save, another copy, taking out a retail
/// actor) keep it; and taking the branches off gives the plain 16-byte copy back.
#[test]
fn a_copy_appears_after_a_flag() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let slot = r.actors.len() as u32;
    let plain = actor::apply(&retail, &retail, &[copy(4, 1330, -64, 32)]).unwrap().0;
    assert_eq!(actor::program(&copy(4, 1330, -64, 32)).len(), actor::PROGRAM_LEN, "no branches: the old program");

    let now = ladder::copy_rungs(&actor::recover(&retail, &plain).unwrap()[0]);
    let (b, report) = ladder::apply(&retail, &plain, slot, Some(&appear_after(&now))).unwrap();
    assert!(report.actor_bytes > actor::PROGRAM_LEN as u32);
    let a = &actor::recover(&retail, &b).unwrap()[0];
    assert!(a.otherwise_hidden);
    assert_eq!((a.x, a.z), (-64, 32), "its own spot is kept");
    assert_eq!(a.branches.len(), 1);
    assert_eq!(ladder_of(&b, slot), vec![(0x1F3, PLACE), (0, Body::Absent)]);
    let br = decode(NpcPack::parse(&b).unwrap().d_block(&b), NpcPack::parse(&b).unwrap().actors[slot as usize].placement);
    assert_eq!(active(&br, &|_| false).unwrap().body, Body::Absent);
    assert_eq!(active(&br, &|f| f == 0x1F3).unwrap().body, PLACE);
    assert!(ladder::edited(&retail, &b).contains(&slot));

    // A move of the branch lands on its words; a layout pass keeps it.
    let at = br[0].at;
    let moved = placement::apply(&retail, &b, &[PlaceWrite { at, place: Some(Place { x: 300, h: -129, z: -40, dir: 0 }) }]).unwrap();
    let laid = text::apply(&retail, &moved, &text::edit_set(&retail, &moved).unwrap()).unwrap().0;
    assert!(laid == moved, "a text save changed the copy");
    assert_eq!(ladder_of(&laid, slot)[0], (0x1F3, Body::Place { x: 300, h: -129, z: -40, dir: 0 }));

    // Another copy and a retail actor taken out keep it.
    let mut added = actor::recover(&retail, &laid).unwrap();
    added.push(copy(5, 1330, 64, 32));
    let two = actor::apply(&retail, &laid, &added).unwrap().0;
    let removed = remove::apply(&retail, &two, &[7].into_iter().collect()).unwrap();
    let back = actor::recover(&retail, &removed).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!((back[0].branches.len(), back[0].otherwise_hidden), (1, true));
    assert!(back[1].branches.is_empty() && !back[1].otherwise_hidden);

    // Standing again, then no branches: the plain program.
    let now = ladder::copy_rungs(&back[0]);
    let stand: Vec<RungEdit> = vec![
        RungEdit { from: Some(0), cond: 0x1F3, body: None, walk: None },
        RungEdit { from: Some(1), cond: 0, body: Some(NewBody::Place { x: -64, h: -129, z: 32, dir: 1024 }), walk: None },
    ];
    assert_eq!(now.len(), 2);
    let standing = ladder::apply(&retail, &removed, slot, Some(&stand)).unwrap().0;
    assert_eq!(ladder_of(&standing, slot)[1], (0, Body::Place { x: -64, h: -129, z: 32, dir: 1024 }));
    let off = ladder::apply(&retail, &standing, slot, None).unwrap().0;
    let a = &actor::recover(&retail, &off).unwrap()[0];
    assert!(a.branches.is_empty() && !a.otherwise_hidden);
    let p = NpcPack::parse(&off).unwrap();
    let at = p.actors[slot as usize].placement as usize;
    let d = p.d_block(&off);
    assert_eq!(d[at..at + actor::PROGRAM_LEN], actor::program(a)[..], "the plain 16-byte program");
}

/// @LT-77/1: examine spots take no branches, a copy of a chest can't be hidden
/// otherwise (its `7FFF` is the opened shape), and the ladder rules hold for copies.
#[test]
fn copies_refuse_what_the_game_cant_read() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let slot = r.actors.len() as u32;
    // An examine spot.
    let sign = AddedActor {
        from: NEW_SPOT,
        home: 1330,
        x: 0,
        h: -129,
        z: 0,
        dir: 0,
        hidden: false,
        own: Some(spot::program(&spot::Preset::Sign { line: "Hi.".into() }, 1330).unwrap()),
        character: None,
        spot: true,
        branches: Vec::new(),
        otherwise_hidden: false,
        walk: Vec::new(),
        house: None,
        smoke: false,
    };
    let with_sign = actor::apply(&retail, &retail, &[sign]).unwrap().0;
    let now = ladder::copy_rungs(&actor::recover(&retail, &with_sign).unwrap()[0]);
    assert_eq!(ladder::apply(&retail, &with_sign, slot, Some(&appear_after(&now))).unwrap_err().code, "LADDER_OBJECT");

    // A chest (record type 3) in any pack.
    let (n, chest) = (0..50u32)
        .find_map(|n| {
            let b = w.file(&format!("npc/{n}")).ok()?;
            let p = NpcPack::parse(&b)?;
            let s = p.actors.iter().find(|a| a.slot >= 2 && ladder::record_type(a.d) == ladder::CHEST)?.slot;
            (actor::room(&b).ok()? > 0).then_some((n, s))
        })
        .expect("a pack with a chest and room");
    let cr = w.file(&format!("npc/{n}")).unwrap();
    let cslot = NpcPack::parse(&cr).unwrap().actors.len() as u32;
    let with_chest = actor::apply(&cr, &cr, &[copy(chest, 1330, 0, 0)]).unwrap().0;
    let now = ladder::copy_rungs(&actor::recover(&cr, &with_chest).unwrap()[0]);
    assert_eq!(ladder::apply(&cr, &with_chest, cslot, Some(&appear_after(&now))).unwrap_err().code, "LADDER_CHEST");
    // Standing branches are fine for a chest copy.
    let stand: Vec<RungEdit> = std::iter::once(RungEdit { from: None, cond: 0x1F3, body: Some(NewBody::Place { x: 8, h: -129, z: 8, dir: 0 }), walk: None }).chain(keep(&now)).collect();
    assert!(ladder::apply(&cr, &with_chest, cslot, Some(&stand)).is_ok());

    // Ladder rules: a fallback with a flag, an early unconditional rung, 512–999, no such copy.
    let with_copy = actor::apply(&retail, &retail, &[copy(4, 1330, 0, 0)]).unwrap().0;
    let now = ladder::copy_rungs(&actor::recover(&retail, &with_copy).unwrap()[0]);
    let err = |e: &[RungEdit]| ladder::apply(&retail, &with_copy, slot, Some(e)).unwrap_err().code;
    assert_eq!(err(&[RungEdit { from: Some(0), cond: 0x20, body: None, walk: None }]), "LADDER_NO_FALLBACK");
    assert_eq!(err(&[RungEdit { from: None, cond: 0, body: Some(NewBody::Hidden), walk: None }, RungEdit { from: Some(0), cond: 0, body: None, walk: None }]), "LADDER_EARLY_ALWAYS");
    assert_eq!(err(&[RungEdit { from: None, cond: 700, body: Some(NewBody::Hidden), walk: None }, RungEdit { from: Some(0), cond: 0, body: None, walk: None }]), "LADDER_COND");
    assert_eq!(ladder::apply(&retail, &with_copy, slot + 1, Some(&keep(&now))).unwrap_err().code, "ACTOR_UNKNOWN");
}

/// @LT-77/2: in every pack with room, a copy of its first copyable actor that appears only
/// while 0x1F3 is set lays out, reads back, survives a text save, and comes off again to
/// the plain copy's bytes.
#[test]
fn every_pack_takes_a_copy_with_branches() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut packs = 0;
    for n in 0..50u32 {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let r = NpcPack::parse(&retail).unwrap();
        if actor::room(&retail).unwrap() == 0 {
            continue;
        }
        let Some(from) = r.actors.iter().find(|a| a.slot >= 2 && a.script != 0 && !actor::is_object(a.d) && ladder::record_type(a.d) != ladder::CHEST).map(|a| a.slot) else {
            continue;
        };
        let slot = r.actors.len() as u32;
        let plain = match actor::apply(&retail, &retail, &[copy(from, 1330, 0, 0)]) {
            Ok((b, _)) => b,
            Err(e) if e.code == "ACTOR_SLOTS" => continue,
            Err(e) => panic!("npc/{n}: {e}"),
        };
        let now = ladder::copy_rungs(&actor::recover(&retail, &plain).unwrap()[0]);
        let b = ladder::apply(&retail, &plain, slot, Some(&appear_after(&now))).unwrap_or_else(|e| panic!("npc/{n}: {e}")).0;
        assert_eq!(ladder_of(&b, slot), vec![(0x1F3, PLACE), (0, Body::Absent)], "npc/{n}");
        let again = text::apply(&retail, &b, &text::edit_set(&retail, &b).unwrap()).unwrap().0;
        assert!(again == b, "npc/{n}: a text save moved the copy");
        let off = ladder::apply(&retail, &b, slot, None).unwrap().0;
        assert!(off == plain, "npc/{n}: taking the branches off left bytes behind");
        packs += 1;
    }
    assert!(packs > 20, "{packs}");
}
