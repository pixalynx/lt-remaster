//! Examine spots added to NPC packs (`edit::spot`, LT-57) on every retail
//! pack. Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`.

use std::collections::BTreeSet;

use lt_core::edit::actor::{self, AddedActor, NEW_SPOT, NPC_SLOTS};
use lt_core::edit::spot::{self, Preset};
use lt_core::edit::{placement, remove, script, text};
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

fn spot(home: u16, x: i16, z: i16, dir: u16, preset: &Preset) -> AddedActor {
    AddedActor { from: NEW_SPOT, home, x, h: -33, z, dir, hidden: false, own: Some(spot::program(preset, home).unwrap()), character: None, spot: true, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false }
}

/// The three presets on chunk `home`.
fn three(home: u16) -> Vec<AddedActor> {
    vec![
        spot(home, -32, 80, 2048, &Preset::Bed { wake_x: -32, wake_z: -32, wake_chunk: None }),
        spot(home, 400, -200, 0, &Preset::Sign { line: "North: the shrine.{nl}South: home.".into() }),
        spot(home, -400, 300, 1024, &Preset::Chest { item: 9, flag: 0x214 }),
    ]
}

fn same(a: &AddedActor, b: &AddedActor) -> bool {
    (a.from, a.home, a.x, a.h, a.z, a.dir, a.hidden, a.character, a.spot, a.own.is_some()) == (b.from, b.home, b.x, b.h, b.z, b.dir, b.hidden, b.character, b.spot, b.own.is_some())
}

/// Character slots a pack's records take (objects take none).
fn characters(p: &NpcPack) -> usize {
    p.actors.iter().filter(|a| !actor::is_object(a.d)).count()
}

/// @LT-57/1 @LT-57/3: every pack with room takes a bed, a sign and a chest homed on
/// a new area (1330): type-4 records after the retail ones, placed by their
/// program's words 1–3, running programs of their own; they take no character
/// slot, read back, move, come out one at a time and leave retail behind.
#[test]
fn every_pack_with_room_takes_examine_spots() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut tried = 0;
    for n in 0..50u32 {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let r = NpcPack::parse(&retail).unwrap();
        let added = three(1330);
        if actor::room(&retail).unwrap() < added.len() {
            assert_eq!(actor::apply(&retail, &retail, &added).unwrap_err().code, "ACTOR_ROOM", "npc/{n}");
            continue;
        }
        let out = match actor::apply(&retail, &retail, &added) {
            Ok((out, _)) => out,
            Err(e) if e.code == "NPC_CAP" => continue,
            Err(e) => panic!("npc/{n}: {e:?}"),
        };
        tried += 1;
        let o = NpcPack::parse(&out).unwrap();
        let od = o.d_block(&out);
        assert_eq!(o.actors.len(), r.actors.len() + 3, "npc/{n}");
        assert_eq!(characters(&o), characters(&r), "npc/{n}: a spot took a character slot");
        for (k, a) in added.iter().enumerate() {
            let rec = &o.actors[r.actors.len() + k];
            // Type 4, no model or pose set, home 1330.
            assert_eq!((rec.d, rec.b), (spot::RECORD_D, 0xFFFF_0000 | 1330), "npc/{n}");
            // What the spawn loop reads: pos[1..3] = x, h, z.
            let word = |i: usize| i16::from_le_bytes([od[rec.placement as usize + 2 * i], od[rec.placement as usize + 2 * i + 1]]);
            assert_eq!((word(1), word(2), word(3)), (a.x, a.h, a.z), "npc/{n}");
            // Its own program, which decodes where the record points.
            assert!(lt_core::tier2::disassemble(od, rec.script).is_ok(), "npc/{n}");
        }
        let ops = |slot: usize| lt_core::tier2::disassemble(od, o.actors[slot].script).unwrap().iter().map(|i| i.op).collect::<Vec<u8>>();
        let base = r.actors.len();
        assert!(ops(base).contains(&0x06), "npc/{n}: the bed has no INN");
        assert_eq!(ops(base + 1)[0], 0x01, "npc/{n}: the sign doesn't say");
        assert!(ops(base + 2).contains(&0x28), "npc/{n}: the chest has no CHEST");
        let back = actor::recover(&retail, &out).unwrap();
        assert!(back.len() == 3 && back.iter().zip(&added).all(|(a, b)| same(a, b)), "npc/{n}: {back:?}");
        // Screen 11 reads and edits the sign's program.
        let s = script::source(&retail, &out, (base + 1) as u32, &format!("npc/{n}")).unwrap();
        assert!(s.edited && s.dsl.contains("added examine spot") && s.dsl.contains("North: the shrine."), "npc/{n}: {}", s.dsl);
        let edited = s.dsl.replace("North: the shrine.", "North: the old shrine.");
        let c = script::check(&retail, &out, (base + 1) as u32, Some(&edited), &[1330]).unwrap();
        assert!(c.diagnostics.iter().all(|d| d.severity != lt_core::tier2::dsl::Severity::Error), "npc/{n}: {:?}", c.diagnostics);
        let out = c.bytes.unwrap();
        assert!(script::source(&retail, &out, (base + 1) as u32, "").unwrap().dsl.contains("North: the old shrine."), "npc/{n}");
        // Move the chest the way the Move tool does: its program's words.
        let at = NpcPack::parse(&out).unwrap().actors[base + 2].placement;
        let place = placement::Place { x: -300, h: -40, z: 260, dir: 2048 };
        let out = placement::apply(&retail, &out, &[placement::PlaceWrite { at, place: Some(place) }]).unwrap();
        let back = actor::recover(&retail, &out).unwrap();
        assert_eq!((back[2].x, back[2].h, back[2].z, back[2].dir), (-300, -40, 260, 2048), "npc/{n}");
        // A grown retail line laid out with them keeps them.
        if let Some(l) = r.dialogue(&retail).into_iter().find(|l| l.kind == "say") {
            let edits = std::collections::BTreeMap::from([(l.offset, "A retail line grown long enough to have to move to the end of the pack.".to_string())]);
            if let Ok((grown, _)) = text::apply(&retail, &out, &edits) {
                assert_eq!(actor::recover(&retail, &grown).unwrap().len(), 3, "npc/{n}");
            }
        }
        // Out one at a time (the sign first), then retail.
        let mut left = back.clone();
        left.remove(1);
        let (two, _) = actor::apply(&retail, &out, &left).unwrap();
        let after = actor::recover(&retail, &two).unwrap();
        assert!(after.len() == 2 && after[0].own.is_some() && after[1].x == -300, "npc/{n}");
        let (clean, _) = actor::apply(&retail, &two, &[]).unwrap();
        let (clean, _) = text::apply(&retail, &clean, &Default::default()).unwrap();
        assert_eq!(clean, retail, "npc/{n} back to retail");
    }
    assert!(tried >= 30, "{tried} packs tried");
}

/// @LT-57/3: npc/22 has 77 characters, room for 3 more, and spots on top: they count
/// against the record room and the loader cap, not the 80 character slots.
#[test]
fn spots_take_no_character_slot() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/22").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    assert_eq!(characters(&r), 77);
    let src = r.actors.iter().skip(2).find(|a| !actor::is_object(a.d) && a.script != 0).unwrap();
    let copy = |k: i16| AddedActor { from: src.slot, home: 1328, x: 64 * k, h: -129, z: 0, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false };
    let mut added: Vec<AddedActor> = (0..3).map(copy).collect();
    added.extend(three(1328));
    let room = actor::room(&retail).unwrap();
    assert!(room >= added.len(), "npc/22 has room for {room}");
    let (out, _) = actor::apply(&retail, &retail, &added).unwrap();
    assert_eq!(characters(&NpcPack::parse(&out).unwrap()), NPC_SLOTS);
    // A fourth character still doesn't fit; a fourth spot does while room lasts.
    let mut four = added.clone();
    four.push(copy(4));
    assert_eq!(actor::apply(&retail, &retail, &four).unwrap_err().code, "ACTOR_SLOTS");
    if room > added.len() {
        let mut more = added.clone();
        more.push(spot(1328, 0, 0, 0, &Preset::Sign { line: "One more.".into() }));
        actor::apply(&retail, &retail, &more).unwrap();
    }
    // Past the room, spots are refused like actors.
    let many: Vec<AddedActor> = (0..=room as i16).map(|k| spot(1328, 16 * k, 0, 0, &Preset::Sign { line: "Hi.".into() })).collect();
    assert_eq!(actor::apply(&retail, &retail, &many).unwrap_err().code, "ACTOR_ROOM");
}

/// @LT-57/1: on a blank slate (every retail record taken out, as a blank base has
/// its packs), npc/7 still takes spots on the Knight's house and a new area, and
/// a copy of a retail spot (the water barrel) runs the retail program until it
/// gets its own. A spot copying nothing needs a program; spots have no looks.
#[test]
fn spots_on_a_blank_slate() {
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/7").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let all: BTreeSet<u32> = remove::removable(&retail).unwrap().into_iter().map(|(s, _)| s).collect();
    let blank = remove::apply(&retail, &retail, &all).unwrap();
    let mut added = three(326);
    added[1].home = 1330;
    // A copy of the water barrel (actor 60, a retail spot).
    added.push(AddedActor { from: 60, home: 1330, x: 640, h: -33, z: 68, dir: 0, hidden: false, own: None, character: None, spot: true, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false });
    let (out, _) = actor::apply(&retail, &blank, &added).unwrap();
    let o = NpcPack::parse(&out).unwrap();
    assert_eq!(remove::recover(&retail, &out).unwrap(), all);
    let back = actor::recover(&retail, &out).unwrap();
    assert!(back.iter().zip(&added).all(|(a, b)| same(a, b)), "{back:?}");
    // The copy runs actor 60's retail program, not the removal block's END the blank slate gave 60.
    let copy = &o.actors[r.actors.len() + 3];
    assert_eq!(copy.script, r.actors[60].script);
    assert_ne!(o.actors[60].script, r.actors[60].script);
    // Refusals.
    let mut bad = spot(326, 0, 0, 0, &Preset::Sign { line: "x".into() });
    bad.own = None;
    assert_eq!(actor::apply(&retail, &blank, &[bad.clone()]).unwrap_err().code, "SPOT_NO_PROGRAM");
    bad.own = Some(spot::program(&Preset::Sign { line: "x".into() }, 326).unwrap());
    bad.hidden = true;
    assert_eq!(actor::apply(&retail, &blank, &[bad]).unwrap_err().code, "SPOT_LOOKS");
    let barrel = AddedActor { from: 60, home: 326, x: 0, h: 0, z: 0, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false };
    assert_eq!(actor::apply(&retail, &blank, &[barrel]).unwrap_err().code, "ACTOR_OBJECT");
    // Back to the blank slate exactly.
    let (clean, _) = actor::apply(&retail, &out, &[]).unwrap();
    assert_eq!(clean, blank);
}

/// @LT-57/1: spots are checked with the walk map (LT-58): the Knight must be able to stand
/// within reach, on floor (wall tops and gaps don't count). Retail's own spots sit on furniture
/// and walls far more than on floor, so the check is reach, not the spot's own cell: it accepts
/// all but a couple of retail's type-4 spots, though few of them are on floor cells.
#[test]
fn retail_spots_pass_the_reach_check() {
    use lt_core::world::walk::{Cell, Walk};
    let Some((image, exe, table)) = disc() else { return };
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut walks: std::collections::BTreeMap<u16, Walk> = Default::default();
    let (mut n, mut on_floor, mut reached) = (0, 0, 0);
    for p in (0..50u32).filter(|&p| p != 17) {
        let b = w.file(&format!("npc/{p}")).unwrap();
        let pk = NpcPack::parse(&b).unwrap();
        let d = pk.d_block(&b);
        for a in pk.actors.iter().skip(2).filter(|a| (a.d >> 8) & 0xFF == 4 && (a.b as u16) < 524) {
            let home = a.b as u16;
            let walk = walks.entry(home).or_insert_with(|| Walk::of_chunk(home as u32, &w.chunk(home as u32).unwrap(), &[]));
            let o = a.placement as usize;
            let (x, z) = (i16::from_le_bytes([d[o + 2], d[o + 3]]) as i32, i16::from_le_bytes([d[o + 6], d[o + 7]]) as i32);
            let Some((col, row)) = lt_core::world::walk::cell_of(x, z) else { continue };
            n += 1;
            on_floor += usize::from(walk.cell(col, row) == Cell::Floor);
            reached += usize::from(spot::reach(walk, x, z).is_some());
        }
    }
    println!("{n} retail spots: {on_floor} on floor cells, {reached} within reach of standing room");
    assert!(n > 2400 && on_floor * 10 < n, "{n} spots, {on_floor} on floor");
    assert!(reached + 3 >= n, "{reached} of {n} reachable");
}
