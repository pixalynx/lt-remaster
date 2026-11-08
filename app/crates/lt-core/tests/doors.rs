//! Doors added to NPC packs (`edit::door`) on every retail pack. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`.

use std::collections::BTreeMap;

use lt_core::edit::door::{self, Added};
use lt_core::npc::NpcPack;
use lt_core::trigger::TriggerRecord;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn door(x: i16, z: i16, to: u16, pack: u8) -> TriggerRecord {
    TriggerRecord::Door { at: 0, x, z, facing: 4, to, tx: 0, tz: 64, flags: 0x8000, pack, music: 0 }
}

#[test]
fn every_pack_takes_doors_and_gives_them_back() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut refused: BTreeMap<u32, &'static str> = BTreeMap::new();
    for n in 0..50u32 {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let r = NpcPack::parse(&retail).unwrap();
        // A door into a new area on the pack's first table-2 chunk (or chunk 1328), and its way back.
        let home = r.triggers(&retail).iter().find(|g| g.pass == 1).map_or(1328, |g| g.chunk);
        let added = vec![Added::always(home, door(-64, 96, 1330, n as u8)), Added::always(1330, door(0, -640, home as u16, n as u8))];
        let out = match door::apply(&retail, &retail, &added) {
            Ok((out, _)) => out,
            Err(e) => {
                refused.insert(n, e.code);
                continue;
            }
        };
        let o = NpcPack::parse(&out).unwrap();
        assert!(o.size <= 0x31000, "npc/{n} past the loader cap");
        let groups = o.triggers(&out);
        // Retail table 1 is untouched; table 2 has the door in every rung of `home` and a group for 1330.
        assert_eq!(groups.iter().filter(|g| g.pass == 0).count(), r.triggers(&retail).iter().filter(|g| g.pass == 0).count(), "npc/{n}");
        let g = groups.iter().find(|g| g.pass == 1 && g.chunk == home).unwrap();
        assert!(g.rungs.iter().all(|rung| rung.records.iter().any(|x| door::words(x) == door::words(&added[0].record))), "npc/{n}");
        assert!(groups.iter().any(|g| g.pass == 1 && g.chunk == 1330), "npc/{n}");
        let back = door::recover(&retail, &out).unwrap();
        assert_eq!(back.len(), 2, "npc/{n}");
        // Nothing else moved: the D-span up to the old end and the model sections are retail.
        let rfm = (r.d + r.d_span) as usize;
        let pass1 = r.d as usize + 4 + 16 * r.actors.len();
        // Only pass1 changes, and the old table 2 is cleared (pass1 points at the new one).
        let d = r.d as usize;
        let t2 = d + r.pass1 as usize..d + r.pass1 as usize + lt_core::trigger::table_len(r.d_block(&retail), r.pass1 as usize).unwrap();
        assert!(out[t2.clone()].iter().all(|&x| x == 0), "npc/{n}: old table 2 not cleared");
        let diff: Vec<usize> = (d..rfm).filter(|&i| out[i] != retail[i] && !(pass1..pass1 + 2).contains(&i) && !t2.contains(&i)).collect();
        assert!(diff.is_empty(), "npc/{n}: diffs {:x?}", &diff[..diff.len().min(16)]);
        let grown = o.d_span - r.d_span;
        for (rm, om) in r.models.iter().zip(&o.models) {
            assert_eq!(*om, if *rm as usize >= rfm { rm + grown } else { *rm }, "npc/{n} model offset");
        }
        assert_eq!(out[rfm + grown as usize..rfm + grown as usize + 64], retail[rfm..rfm + 64], "npc/{n} models");
        let (clean, _) = door::apply(&retail, &out, &[]).unwrap();
        assert_eq!(clean, retail, "npc/{n} back to retail");
    }
    // The two packs that fill the loader cap exactly can't grow.
    println!("refused: {refused:?}");
    assert!(refused.iter().all(|(_, c)| *c == "NPC_CAP"), "{refused:?}");
}

#[test]
fn every_pack_with_room_takes_actors_doors_and_text_together() {
    use lt_core::edit::actor::{self, AddedActor};
    use lt_core::edit::{text, trigger as trigger_edit};
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut tried = 0;
    for n in 0..50u32 {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let r = NpcPack::parse(&retail).unwrap();
        let room = actor::room(&retail).unwrap();
        let Some(src) = r.actors.iter().skip(2).find(|a| !actor::is_object(a.d) && a.script != 0) else { continue };
        if room < 2 {
            let one = vec![AddedActor { from: src.slot, home: 1330, x: 0, h: -129, z: 0, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false }; 2];
            assert_eq!(actor::apply(&retail, &retail, &one[..room + 1]).unwrap_err().code, "ACTOR_ROOM", "npc/{n}");
            continue;
        }
        tried += 1;
        let added = vec![
            AddedActor { from: src.slot, home: 1330, x: -848, h: -129, z: -560, dir: 1024, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false },
            AddedActor { from: src.slot, home: 1330, x: 100, h: -129, z: 200, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false },
        ];
        let out = match actor::apply(&retail, &retail, &added) {
            Ok((out, _)) => out,
            Err(e) if e.code == "NPC_CAP" => continue,
            Err(e) => panic!("npc/{n}: {e:?}"),
        };
        let o = NpcPack::parse(&out).unwrap();
        assert_eq!(o.actors.len(), r.actors.len() + 2, "npc/{n}");
        // The copies: the source's script and model words, home 1330, placed where asked.
        for (k, a) in added.iter().enumerate() {
            let rec = &o.actors[r.actors.len() + k];
            assert_eq!((rec.script, rec.b >> 16, rec.b as u16, rec.d), (src.script, src.b >> 16, 1330, src.d), "npc/{n}");
            let ladder = lt_core::trigger::placement_ladder(o.d_block(&out), rec.placement);
            assert_eq!(ladder.len(), 1, "npc/{n}");
            assert_eq!(ladder[0].outcome, lt_core::trigger::Outcome::Place { x: a.x, h: a.h, z: a.z, dir: a.dir }, "npc/{n}");
        }
        // Retail records and table 1 are unchanged; table 2 reads the same plus nothing.
        assert!(o.actors.iter().zip(&r.actors).all(|(a, b)| (a.script, a.b, a.placement, a.d) == (b.script, b.b, b.placement, b.d)), "npc/{n}");
        let (rt, ot) = (r.triggers(&retail), o.triggers(&out));
        let words = |g: &[lt_core::trigger::TriggerGroup], pass: u8| -> Vec<Vec<i16>> {
            g.iter().filter(|x| x.pass == pass).flat_map(|x| &x.rungs).flat_map(|x| &x.records).map(door::words).collect()
        };
        assert_eq!(words(&rt, 0), words(&ot, 0), "npc/{n} table 1");
        assert_eq!(words(&rt, 1), words(&ot, 1), "npc/{n} table 2");
        assert_eq!(actor::recover(&retail, &out).unwrap(), added, "npc/{n}");

        // A door pair and a moved table-1 or table-2 record on top.
        let home = rt.iter().find(|g| g.pass == 1).map_or(1328, |g| g.chunk);
        let doors = vec![Added::always(home, door(-64, 96, 1330, n as u8)), Added::always(1330, door(0, -640, home as u16, n as u8))];
        let out2 = match door::apply(&retail, &out, &doors) {
            Ok((b, _)) => b,
            Err(e) if e.code == "NPC_CAP" => continue,
            Err(e) => panic!("npc/{n}: {e:?}"),
        };
        let first = trigger_edit::records(&rt).next().map(|rec| trigger_edit::record_pos(rec).0);
        let out3 = match first {
            Some(at) => trigger_edit::apply(&retail, &out2, &[trigger_edit::TriggerWrite::to(at, -1000, -700)]).unwrap(),
            None => out2.clone(),
        };
        if first.is_some() {
            assert_eq!(trigger_edit::moved(&retail, &out3).len(), 1, "npc/{n}");
            let (view, _) = door::view(&retail, &out3).unwrap();
            let moved: Vec<_> = trigger_edit::records(&view).filter(|rec| trigger_edit::record_pos(rec) == (first.unwrap(), -1000, -700)).collect();
            assert_eq!(moved.len(), 1, "npc/{n} view keeps the retail id");
        }
        // A grown line laid out with all of it keeps the actors, the doors and the move.
        let line = o.dialogue(&out3).into_iter().find(|l| l.kind == "say");
        let out4 = match line {
            Some(l) => {
                let edits = std::collections::BTreeMap::from([(l.offset, "A new town line, long enough to have to move somewhere else in the pack.".to_string())]);
                match text::apply(&retail, &out3, &edits) {
                    Ok((b, _)) => b,
                    Err(e) if e.code == "NPC_CAP" => continue,
                    Err(e) => panic!("npc/{n}: {e:?}"),
                }
            }
            None => out3.clone(),
        };
        assert_eq!(actor::recover(&retail, &out4).unwrap(), added, "npc/{n}");
        assert_eq!(door::recover(&retail, &out4).unwrap().len(), 2, "npc/{n}");
        assert_eq!(trigger_edit::moved(&retail, &out4).len(), usize::from(first.is_some()), "npc/{n}");
        // Taking it all away gives retail back.
        let (no_doors, _) = door::apply(&retail, &out4, &[]).unwrap();
        let (no_actors, _) = actor::apply(&retail, &no_doors, &[]).unwrap();
        let back = match first {
            Some(at) => trigger_edit::apply(&retail, &no_actors, &[trigger_edit::TriggerWrite::retail(at)]).unwrap(),
            None => no_actors,
        };
        let clean = text::apply(&retail, &back, &Default::default()).unwrap().0;
        assert_eq!(clean, retail, "npc/{n} back to retail");
    }
    assert!(tried >= 30, "{tried} packs tried");
}

/// Character slots 0x50+ belong to the field's special object and its wanderers
/// (`edit::actor::NPC_SLOTS`): npc/22, retail's busiest pack with 77 characters,
/// takes three added actors and refuses a fourth (LT-49 hung the game with six).
#[test]
fn a_pack_keeps_its_characters_below_the_wanderers() {
    use lt_core::edit::actor::{self, AddedActor, NPC_SLOTS};
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/22").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let chars = r.actors.iter().filter(|a| !actor::is_object(a.d)).count();
    assert_eq!((chars, NPC_SLOTS), (77, 0x50));
    assert!(actor::room(&retail).unwrap() >= 4, "room isn't what stops the fourth");
    let src = r.actors.iter().skip(2).find(|a| !actor::is_object(a.d) && a.script != 0).unwrap();
    let copy = |k: i16| AddedActor { from: src.slot, home: 1328, x: 64 * k, h: -129, z: 0, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false };
    let three: Vec<AddedActor> = (0..3).map(copy).collect();
    actor::apply(&retail, &retail, &three).unwrap();
    let four: Vec<AddedActor> = (0..4).map(copy).collect();
    assert_eq!(actor::apply(&retail, &retail, &four).unwrap_err().code, "ACTOR_SLOTS");
}

#[test]
fn an_added_actor_gets_dialogue_of_its_own() {
    use lt_core::edit::actor::{self, AddedActor};
    use lt_core::edit::{script, text};
    use lt_core::tier2::{self, Operand};
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let slot = r.actors.len() as u32; // 59
    // A copy of Algo (slot 2) in area 1330 runs Algo's program.
    let copy = AddedActor { from: 2, home: 1330, x: -880, h: -129, z: -496, dir: 2048, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false };
    let (out, _) = actor::apply(&retail, &retail, &[copy]).unwrap();
    let s = script::source(&retail, &out, slot, "npc/6").unwrap();
    assert!(!s.edited && s.dsl.contains("runs actor 2's program"), "{}", s.dsl);
    assert_eq!(s.at, r.actors[2].script);

    // Its own program: one new line.
    let line = "Welcome to the new town!{nl}Mind the fresh paint.";
    let dsl = format!("face_knight\nanim 1\nspeaker @0x0450\nsay \"{line}\"\n");
    let c = script::check(&retail, &out, slot, Some(&dsl), &[]).unwrap();
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let plan = c.plan.unwrap();
    assert!(!plan.identical && plan.at.is_some() && plan.new_strings == 1, "{plan:?}");
    let out2 = c.bytes.unwrap();
    let o = NpcPack::parse(&out2).unwrap();
    let own_at = o.actors[slot as usize].script;
    assert_eq!(Some(own_at), plan.at);
    assert_eq!(o.actors[2].script, r.actors[2].script, "Algo keeps the retail program");
    let says: Vec<Vec<u8>> = tier2::disassemble(o.d_block(&out2), own_at)
        .unwrap()
        .iter()
        .filter(|i| (0x01..=0x03).contains(&i.op))
        .filter_map(|i| match i.operands.first() {
            Some(Operand::Text(t)) => Some(lt_core::text::c_string(o.d_block(&out2), *t as usize).to_vec()),
            _ => None,
        })
        .collect();
    assert_eq!(says, vec![text::encode_markup(line).unwrap()]);
    let s2 = script::source(&retail, &out2, slot, "npc/6").unwrap();
    assert!(s2.edited && s2.dsl.contains("Welcome to the new town!"), "{}", s2.dsl);
    assert!(!script::source(&retail, &out2, 2, "npc/6").unwrap().edited);

    // Algo's own edit and a grown line elsewhere keep the copy's program.
    let algo = script::source(&retail, &out2, 2, "npc/6").unwrap().dsl.replace("anim 1", "anim 2");
    let out3 = script::check(&retail, &out2, 2, Some(&algo), &[]).unwrap().bytes.unwrap();
    let o3 = NpcPack::parse(&out3).unwrap();
    assert_ne!(o3.actors[2].script, r.actors[2].script, "Algo's program moved");
    assert_ne!(o3.actors[slot as usize].script, o3.actors[2].script, "the copy keeps its own");
    let pool_line = r.dialogue(&retail).into_iter().find(|l| l.kind == "say" && !l.actors.contains(&2)).unwrap();
    let grown = std::collections::BTreeMap::from([(pool_line.offset, "A line that grows long enough to be moved out of its pool slot.".to_string())]);
    let (out4, _) = text::apply(&retail, &out3, &grown).unwrap();
    let back = actor::recover(&retail, &out4).unwrap();
    assert!(back[0].own.is_some());
    assert!(script::source(&retail, &out4, slot, "npc/6").unwrap().dsl.contains("Welcome to the new town!"));

    // Back to sharing: the copy follows Algo's edited program again.
    let out5 = script::check(&retail, &out4, slot, None, &[]).unwrap().bytes.unwrap();
    let o5 = NpcPack::parse(&out5).unwrap();
    assert_eq!(o5.actors[slot as usize].script, o5.actors[2].script);
    // And everything off gives retail.
    let out6 = script::check(&retail, &out5, 2, None, &[]).unwrap().bytes.unwrap();
    let (out7, _) = text::apply(&retail, &out6, &Default::default()).unwrap();
    let (clean, _) = actor::apply(&retail, &out7, &[]).unwrap();
    assert_eq!(clean, retail);
}

/// LT-34: chunk 395 has no pack-table entry, so a door pair 395 ↔ 372 goes
/// into 372's pack, and the editor must find 395's end there too
/// (`door::kept_pack`, as `world_triggers` reads it) — not only for new areas.
#[test]
fn a_packless_retail_chunk_reads_its_door_from_the_pack_it_was_linked_from() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let packs = lt_core::world::pack_table(&w.file("ovr/2").unwrap());
    let none = |_: u16| false;
    assert_eq!(lt_core::world::pack_for(&packs, 395, &none), None, "chunk 395 has a pack-table entry");
    let p = lt_core::world::pack_for(&packs, 372, &none).expect("chunk 372 has a pack");
    let retail = w.file(&format!("npc/{p}")).unwrap();
    let added = vec![Added::always(395, door(0, 0, 372, p as u8)), Added::always(372, door(-64, 96, 395, p as u8))];
    let (out, _) = door::apply(&retail, &retail, &added).unwrap();
    let back = door::recover(&retail, &out).unwrap();
    let same = |a: &Added, b: &Added| a.chunk == b.chunk && door::words(&a.record) == door::words(&b.record);
    assert!(back.len() == 2 && back.iter().zip(&added).all(|(a, b)| same(a, b)), "{back:?}");
    // Unrelated packs first, so the lookup has to pass them.
    let lookup = |chunk: u32| door::kept_pack([(p + 1, Vec::new()), (p, back.clone())], chunk);
    assert_eq!(lookup(395), Some(p));
    assert_eq!(lookup(394), None);
    // Each end's group holds its door, as the triggers layer filters it (group chunk = the chunk shown).
    let groups = NpcPack::parse(&out).unwrap().triggers(&out);
    for (here, there) in [(395, 372), (372, 395)] {
        let doors: Vec<u16> = groups
            .iter()
            .filter(|g| g.chunk as i32 == here)
            .flat_map(|g| g.rungs.iter().flat_map(|r| r.records.iter()))
            .filter_map(|r| match r {
                TriggerRecord::Door { to, .. } => Some(*to),
                _ => None,
            })
            .collect();
        assert!(doors.contains(&there), "chunk {here}: no door to {there} in npc/{p} ({doors:?})");
    }
}

/// @LT-41/5: every retail door of every pack can be sent elsewhere in place
/// (only its destination words change), comes back byte for byte, and keeps
/// its new destination when the editor later lays table 2 out again.
#[test]
fn every_retail_door_retargets_in_place_and_survives_added_doors() {
    use lt_core::edit::trigger::{self as trigger_edit, DoorDest, DoorWrite};
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let elsewhere = DoorDest { to: 78, tx: -96, tz: 160, facing: 3, pack: 7 };
    let mut total = 0;
    for n in 0..50u32 {
        let retail = w.file(&format!("npc/{n}")).unwrap();
        let ids = trigger_edit::door_ids(&retail, &retail).unwrap();
        if ids.is_empty() {
            continue;
        }
        total += ids.len();
        let writes: Vec<DoorWrite> = ids.iter().map(|&(at, _)| DoorWrite { at, dest: Some(elsewhere) }).collect();
        let out = trigger_edit::retarget(&retail, &retail, &writes).unwrap();
        // Only bytes 4..12 and 14 of each door change.
        let d = NpcPack::parse(&retail).unwrap().d as usize;
        let dest_bytes = |i: usize| ids.iter().any(|&(at, _)| (4..12).contains(&(i.wrapping_sub(d + at as usize))) || i == d + at as usize + 14);
        let diff: Vec<usize> = (0..retail.len()).filter(|&i| out[i] != retail[i] && !dest_bytes(i)).collect();
        assert!(diff.is_empty(), "npc/{n}: {:x?}", &diff[..diff.len().min(8)]);
        let changed = trigger_edit::retargeted(&retail, &out);
        assert!(changed.iter().all(|c| c.dest == elsewhere), "npc/{n}");
        assert_eq!(changed.len(), ids.len(), "npc/{n}: every door reads as retargeted");
        let back = trigger_edit::retarget(&retail, &out, &ids.iter().map(|&(at, _)| DoorWrite { at, dest: None }).collect::<Vec<_>>()).unwrap();
        assert_eq!(back, retail, "npc/{n} back to retail");

        // Link a door afterwards (table 2 is laid out again): the retargets stay.
        let home = NpcPack::parse(&retail).unwrap().triggers(&retail).iter().find(|g| g.pass == 1).map_or(1328, |g| g.chunk);
        let Ok((linked, _)) = door::apply(&retail, &out, &[Added::always(home, door(-64, 96, 1330, n as u8))]) else { continue };
        assert_eq!(trigger_edit::retargeted(&retail, &linked), changed, "npc/{n} after a door was added");
    }
    println!("{total} retail doors retargeted");
    assert!(total > 100, "only {total} retail doors");
}
