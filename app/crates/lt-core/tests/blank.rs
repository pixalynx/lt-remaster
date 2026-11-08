//! The blank slate (`edit::remove`, `edit::blank`, `trigger::OFF_MAP`) on
//! every retail pack: retail records and trigger records come out, read back,
//! compose with the other encoders, and go back in byte for byte; the boot
//! pack gets its starter scene. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test blank`.

use std::collections::BTreeSet;

use lt_core::edit::blank::{self, Options, BOOT_CHUNK, BOOT_SPOT, KNIGHT_HIDDEN, SCENE_MUSIC};
use lt_core::edit::door::{self, Added};
use lt_core::edit::remove::{self, How};
use lt_core::edit::trigger::{self as trigger_edit, OFF_MAP};
use lt_core::edit::{actor, pack, text as text_edit};
use lt_core::fs::Kind;
use lt_core::npc::NpcPack;
use lt_core::placement::{self, Body};
use lt_core::trigger::TriggerRecord;
use lt_core::{tier2, DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, table })
}

impl Disc {
    fn read(&self, id: &str) -> Vec<u8> {
        let e = self.table.by_id(id).unwrap();
        self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
    }

    /// Every distinct pack (npc/17 aliases npc/0).
    fn packs(&self) -> Vec<(String, Vec<u8>)> {
        self.table
            .of_kind(Kind::Npc)
            .filter(|e| e.aliases.is_empty() || e.global < e.aliases[0])
            .map(|e| (e.id.clone(), self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()))
            .collect()
    }
}

const ALL: Options = Options { actors: true, objects: true, scenes: true, flags: true, doors: true, chunk: None, free_models: false };

fn records(p: &NpcPack, b: &[u8]) -> Vec<TriggerRecord> {
    p.triggers(b).into_iter().flat_map(|g| g.rungs).flat_map(|r| r.records).collect()
}

#[test]
fn every_pack_blanks_reads_back_and_restores() {
    let Some(disc) = disc() else { return };
    for (id, retail) in disc.packs() {
        let r = NpcPack::parse(&retail).unwrap();
        let (out, rep) = blank::blank(&retail, &retail, &ALL).unwrap_or_else(|e| panic!("{id}: {e}"));
        let o = NpcPack::parse(&out).unwrap();
        assert!(o.size <= 0x31000, "{id} past the loader cap");
        assert_eq!(o.actors.len(), r.actors.len(), "{id}: records are never deleted");

        // Every record past the party is out, the right way.
        let gone = remove::recover(&retail, &out).unwrap();
        let want: BTreeSet<u32> = (2..r.actors.len() as u32).collect();
        assert_eq!(gone, want, "{id}");
        let od = o.d_block(&out);
        for a in r.actors.iter().skip(2) {
            let now = &o.actors[a.slot as usize];
            match remove::how(a.d) {
                How::Absent => {
                    let ladder = placement::decode(od, now.placement);
                    assert_eq!(ladder.len(), 1, "{id} actor {}", a.slot);
                    assert_eq!(ladder[0].body, Body::Absent, "{id} actor {}", a.slot);
                    assert_ne!((now.d >> 8) & 0xFF, 3, "{id} actor {}: a removed chest isn't a chest", a.slot);
                    assert_eq!(now.script, a.script, "{id} actor {}: its talk program stays", a.slot);
                }
                How::Silent => {
                    let code = tier2::disassemble(od, now.script).unwrap();
                    assert_eq!((code.len(), code[0].op), (1, 0x00), "{id} object {}: runs END", a.slot);
                    assert_eq!(now.placement, a.placement, "{id} object {}: stays where it is", a.slot);
                }
            }
        }
        // Every trigger record is off the map, with its type and length, so the tables parse alike.
        let (before, after) = (records(&r, &retail), records(&o, &out));
        assert_eq!(before.len(), after.len(), "{id}");
        for (b, a) in before.iter().zip(&after) {
            let (_, x, z) = trigger_edit::record_pos(a);
            assert_eq!((x, z), (OFF_MAP, OFF_MAP), "{id}");
            assert_eq!(std::mem::discriminant(b), std::mem::discriminant(a), "{id}");
        }
        assert_eq!(trigger_edit::removed(&retail, &out).len(), before.len(), "{id}");
        assert_eq!(rep.actors_left + rep.triggers_left, 0, "{id}");
        assert_eq!((rep.actors + rep.objects) as usize, r.actors.len() - 2, "{id}");
        assert_eq!((rep.scenes + rep.flags + rep.doors) as usize, before.len(), "{id}");

        // Nothing in it can start any more, and the catalogue and the story say so.
        assert!(lt_core::scene::catalog(&o, &out).iter().all(|e| e.removed), "{id}: every scene and conversation is removed");
        assert!(lt_core::scene::catalog(&r, &retail).iter().all(|e| !e.removed), "{id}: retail still has them, for the preview");
        let graph = lt_core::story::build(&[lt_core::story::PackIn { index: 0, bytes: &out, retail_actors: r.actors.len() }]);
        assert!(graph.pages.iter().all(|p| p.removed), "{id}: every page is removed");

        // The set reads back whole, and laying it out again changes nothing.
        let set = pack::Edits::recover(&retail, &out).unwrap();
        assert!(set.text.is_empty() && set.scripts.is_empty() && set.doors.is_empty() && set.actors.is_empty(), "{id}");
        assert_eq!(lt_core::edit::text::layout(&retail, &out, &set).unwrap().0, out, "{id}: layout is stable");

        // Everything goes back: the retail bytes.
        assert_eq!(blank::restore(&retail, &out, None).unwrap(), retail, "{id}: restore");
    }
}

#[test]
fn a_blank_keeps_the_projects_additions() {
    let Some(disc) = disc() else { return };
    let retail = disc.read("npc/6");
    let r = NpcPack::parse(&retail).unwrap();
    // A line, an added actor (a copy of the first copyable villager) and a door into a new area.
    let line = text_edit::edited_lines(&retail, &retail).unwrap();
    assert!(line.is_empty());
    let first = r.pool(&retail)[0].offset;
    let mut set = pack::Edits::recover(&retail, &retail).unwrap();
    set.text.insert(first, "A new line for a new game.".into());
    let from = r.actors.iter().find(|a| a.slot >= 2 && !actor::is_object(a.d) && a.script != 0).unwrap().slot;
    set.actors.push(actor::AddedActor { from, home: 78, x: 100, h: -64, z: 200, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false });
    let door_rec = TriggerRecord::Door { at: 0, x: -64, z: 96, facing: 4, to: 1330, tx: 0, tz: 64, flags: 0x8000, pack: 6, music: 0 };
    set.doors = door::canonical(&retail, &[Added::always(78, door_rec.clone())]).unwrap();
    let (edited, _, _) = lt_core::edit::text::layout(&retail, &retail, &set).unwrap();

    let (out, _) = blank::blank(&retail, &edited, &ALL).unwrap();
    let after = pack::Edits::recover(&retail, &out).unwrap();
    assert_eq!(after.text.get(&first).map(String::as_str), Some("A new line for a new game."));
    assert_eq!(after.actors.len(), 1);
    assert_eq!(after.actors[0].from, from);
    assert_eq!(after.doors.len(), 1);
    assert_eq!(door::words(&after.doors[0].record), door::words(&door_rec), "the added door keeps its spot");
    assert_eq!(after.removed.len(), r.actors.len() - 2);
    // The copy still shows: its own ladder places it, though its source is out.
    let o = NpcPack::parse(&out).unwrap();
    let copy = &o.actors[r.actors.len()];
    let ladder = placement::decode(o.d_block(&out), copy.placement);
    assert!(matches!(ladder[0].body, Body::Place { x: 100, z: 200, .. }));
    // Restoring brings retail back and keeps the additions.
    let back = blank::restore(&retail, &out, None).unwrap();
    let kept = pack::Edits::recover(&retail, &back).unwrap();
    assert!(kept.removed.is_empty());
    assert_eq!((kept.actors.len(), kept.doors.len(), kept.text.len()), (1, 1, 1));
    assert!(trigger_edit::removed(&retail, &back).is_empty());
}

#[test]
fn one_chunk_blanks_alone() {
    let Some(disc) = disc() else { return };
    let retail = disc.read("npc/6");
    let r = NpcPack::parse(&retail).unwrap();
    let o = Options { chunk: Some(78), ..Options::default() };
    let (out, _) = blank::blank(&retail, &retail, &o).unwrap();
    let gone = remove::recover(&retail, &out).unwrap();
    assert!(!gone.is_empty());
    for a in r.actors.iter().skip(2) {
        assert_eq!(gone.contains(&a.slot), a.b as u16 == 78, "actor {} (home {})", a.slot, a.b as u16);
    }
    // Doors stay by default; the chunk's other records go.
    let off: BTreeSet<u32> = trigger_edit::removed(&retail, &out).into_iter().collect();
    for g in r.triggers(&retail) {
        for rec in g.rungs.iter().flat_map(|x| &x.records) {
            let at = trigger_edit::record_pos(rec).0;
            let want = g.chunk == 78 && !matches!(rec, TriggerRecord::Door { .. });
            assert_eq!(off.contains(&at), want, "record D+0x{at:04X} of group {}", g.chunk);
        }
    }
    // Putting back that chunk only.
    assert_eq!(blank::restore(&retail, &out, Some(78)).unwrap(), retail);
}

#[test]
fn the_boot_pack_gets_a_starter_scene() {
    let Some(disc) = disc() else { return };
    let retail = disc.read("npc/7");
    let (empty, _) = blank::blank(&retail, &retail, &Options::default()).unwrap();
    assert!(blank::find_starter(&retail, &empty).unwrap().is_none());
    let (out, slot) = blank::starter(&retail, &empty).unwrap();
    let s = blank::find_starter(&retail, &out).unwrap().expect("the starter scene");
    assert_eq!(s.slot, slot);
    let (x, _, z) = BOOT_SPOT;
    assert_eq!((s.chunk, s.x, s.z, s.cue), (BOOT_CHUNK, x, z, KNIGHT_HIDDEN));
    // Its record sits in the cue rung at the top of chunk 326's ladder, first in it.
    let o = NpcPack::parse(&out).unwrap();
    let g = o.triggers(&out).into_iter().find(|g| g.pass == 1 && g.chunk == BOOT_CHUNK as i16).unwrap();
    assert_eq!(g.rungs[0].flag, KNIGHT_HIDDEN);
    assert!(matches!(g.rungs[0].records[0], TriggerRecord::Talk { x: rx, z: rz, actor, .. } if (rx, rz, actor) == (x, z, s.actor)));
    // Its program shows the Knight and releases the field music.
    let code = tier2::disassemble(o.d_block(&out), o.actors[slot as usize].script).unwrap();
    let ops: Vec<(u8, Vec<tier2::Operand>)> = code.iter().map(|i| (i.op, i.operands.clone())).collect();
    assert_eq!(ops.len(), 4);
    assert_eq!(ops[0].0, 0x05);
    assert_eq!(ops[1].0, 0x05);
    assert_eq!(ops[2].0, 0x19, "a warp onto the spawn spot, so the field loads with the Knight shown");
    assert_eq!(ops[3].0, 0x00);
    let warp = format!("{:?}", ops[2].1);
    assert!(warp.contains("326") && warp.contains(&format!("{}", BOOT_SPOT.0)) && warp.contains(&format!("{}", BOOT_SPOT.2)), "{warp}");
    let flags: Vec<String> = code[..2].iter().map(|i| format!("{:?}", i.operands)).collect();
    assert!(flags[0].contains(&format!("{}", KNIGHT_HIDDEN)) && flags[1].contains(&format!("{}", SCENE_MUSIC)), "{flags:?}");
    // Asking again doesn't add a second one, and the retail pieces stay out.
    let (again, slot2) = blank::starter(&retail, &out).unwrap();
    assert_eq!((again, slot2), (out.clone(), slot));
    assert_eq!(remove::recover(&retail, &out).unwrap().len(), blank::report(&retail, &out).unwrap().actors as usize + blank::report(&retail, &out).unwrap().objects as usize);
}

/// A moved start (a HackerGC base's boot map, `edit::boot`): the ground under retail's spawn is
/// the height retail starts the Knight at, and a starter scene can sit in any pack on a new area.
#[test]
fn a_moved_start_gets_its_starter_scene_on_a_new_area() {
    let Some(disc) = disc() else { return };
    let (x, h, z) = BOOT_SPOT;
    let exe = PsxExe::from_disc(&disc.image).unwrap();
    let wd = lt_core::worldmap::WorldDisc { image: &disc.image, table: &disc.table, exe: &exe };
    let house = wd.chunk(BOOT_CHUNK as u32).unwrap();
    assert_eq!(lt_core::edit::boot::ground(&house, x, z), Some(h));
    assert_eq!(lt_core::edit::boot::ground(&house, 0x900, 0), None, "outside the chunk");
    let retail = disc.read("npc/23");
    let (empty, _) = blank::blank(&retail, &retail, &Options::default()).unwrap();
    let spot = (-0x100, -0x30, 0x80);
    assert!(blank::starter_at(&retail, &empty, 1328, spot, 2, &[]).is_err(), "chunk 1328 is no chunk without the project's areas");
    let (out, slot) = blank::starter_at(&retail, &empty, 1328, spot, 2, &[1328]).unwrap();
    let s = blank::find_starter_at(&retail, &out, 1328, spot.0, spot.2).unwrap().expect("the starter scene");
    assert_eq!((s.slot, s.chunk, s.cue), (slot, 1328, KNIGHT_HIDDEN));
    let o = NpcPack::parse(&out).unwrap();
    let code = tier2::disassemble(o.d_block(&out), o.actors[slot as usize].script).unwrap();
    let warp = format!("{:?}", code[2].operands);
    assert!(code[2].op == 0x19 && warp.contains("1328") && warp.contains("-256") && warp.contains("128"), "{warp}");
    let (again, slot2) = blank::starter_at(&retail, &out, 1328, spot, 2, &[1328]).unwrap();
    assert_eq!((again, slot2), (out, slot));
}
