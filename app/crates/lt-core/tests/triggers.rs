//! Tier-1 trigger moves against the user's own retail image (screen 10).
//! Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test triggers`.
//! The image is opened read-only.
//!
//! The case is the one users hit first: Mary (`npc/6` slot 4, chunk 78) wakes
//! through two type-10 talk records 32 units either side of her spot, so
//! moving her placement alone leaves her scene behind.

use std::collections::BTreeMap;

use lt_core::edit::placement::{self as place_edit, Place, PlaceWrite};
use lt_core::edit::text as text_edit;
use lt_core::edit::trigger::{self as trigger_edit, record_pos, TriggerWrite};
use lt_core::fs::Kind;
use lt_core::npc::NpcPack;
use lt_core::placement::{self, Body};
use lt_core::trigger::{self, Hotspot, TriggerRecord};
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, exe, table })
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

const MARY: u32 = 4;
const MARY_TALKS: [u32; 2] = [0x43C, 0x444];

/// Offsets of the bytes that differ between two equal-length buffers.
fn diff(a: &[u8], b: &[u8]) -> Vec<usize> {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).enumerate().filter(|(_, (x, y))| x != y).map(|(i, _)| i).collect()
}

/// Mary's talk records in chunk 78's groups, as (at, x, z).
fn mary_talks(b: &[u8]) -> Vec<(u32, i16, i16)> {
    let p = NpcPack::parse(b).unwrap();
    placement::trigger_groups(&p, b)
        .iter()
        .filter(|g| g.chunk == 78)
        .flat_map(|g| &g.rungs)
        .flat_map(|r| &r.records)
        .filter_map(|r| match *r {
            TriggerRecord::Talk { at, x, z, actor } if actor as u32 == MARY => Some((at, x, z)),
            _ => None,
        })
        .collect()
}

#[test]
fn marys_talk_triggers_are_found() {
    let Some(disc) = disc() else { return };
    let b = disc.read("npc/6");
    let p = NpcPack::parse(&b).unwrap();
    let mary = placement::roster(&p, &b).into_iter().find(|a| a.slot == MARY).unwrap();
    assert_eq!(mary.speaker.as_deref(), Some("Mary"));
    assert_eq!((mary.home, mary.script, mary.program), (78, 0x1978, 0x1D50));
    let always = mary.branches.last().unwrap();
    assert_eq!(always.cond, 0);
    assert_eq!(always.body, Body::Place { x: -1170, h: -129, z: -832, dir: 1024 });

    // Pass 1's group for chunk 78: the last rung (flag 0, always) holds her two records.
    let groups = placement::trigger_groups(&p, &b);
    let g = groups.iter().find(|g| g.chunk == 78 && g.pass == 1).unwrap();
    let last = g.rungs.last().unwrap();
    assert_eq!(last.flag, 0);
    assert_eq!(
        last.records,
        vec![
            TriggerRecord::Talk { at: 0x43C, x: -1170, z: -800, actor: 4 },
            TriggerRecord::Talk { at: 0x444, x: -1170, z: -864, actor: 4 },
        ]
    );
    assert_eq!(mary_talks(&b), vec![(0x43C, -1170, -800), (0x444, -1170, -864)]);
    // Earlier rungs (flags 0x97, 0x1E, 0x1F, 0x04) wake other actors and win when set.
    assert!(g.rungs[..g.rungs.len() - 1].iter().all(|r| r.flag != 0));
}

#[test]
fn mary_and_her_triggers_move_in_one_pack() {
    let Some(disc) = disc() else { return };
    let retail = disc.read("npc/6");
    let p = NpcPack::parse(&retail).unwrap();
    let mary = placement::roster(&p, &retail).into_iter().find(|a| a.slot == MARY).unwrap();
    let branch = mary.branches.last().unwrap().at;
    let (dx, dz) = (256i16, -96i16);
    let to = Place { x: -1170 + dx, h: -129, z: -832 + dz, dir: 1024 };
    let talks = mary_talks(&retail);
    let writes: Vec<TriggerWrite> = talks.iter().map(|&(at, x, z)| TriggerWrite::to(at, x + dx, z + dz)).collect();

    // Placement then triggers, and triggers then placement: the same bytes.
    let a = place_edit::apply(&retail, &retail, &[PlaceWrite { at: branch, place: Some(to) }]).unwrap();
    let a = trigger_edit::apply(&retail, &a, &writes).unwrap();
    let b = trigger_edit::apply(&retail, &retail, &writes).unwrap();
    let b = place_edit::apply(&retail, &b, &[PlaceWrite { at: branch, place: Some(to) }]).unwrap();
    assert_eq!(a, b, "save order doesn't matter");

    // Only the placement words (x, h, z, dir) and the two records' x/z changed.
    let d = p.d as usize;
    let allowed = |i: usize| {
        let rel = i - d;
        (branch as usize + 2..branch as usize + 10).contains(&rel) || MARY_TALKS.iter().any(|&at| (at as usize..at as usize + 4).contains(&rel))
    };
    let changed = diff(&retail, &a);
    assert!(!changed.is_empty() && changed.iter().all(|&i| i >= d && allowed(i)), "{changed:?}");

    // Reads back: the roster, the tables, and both `moved` lists.
    let pa = NpcPack::parse(&a).unwrap();
    let back = placement::roster(&pa, &a).into_iter().find(|x| x.slot == MARY).unwrap();
    assert_eq!(back.branches.last().unwrap().body, Body::Place { x: to.x, h: to.h, z: to.z, dir: to.dir });
    assert_eq!(back.branches.last().unwrap().walk, mary.branches.last().unwrap().walk, "the walk script is untouched");
    assert_eq!(mary_talks(&a), vec![(0x43C, -1170 + dx, -800 + dz), (0x444, -1170 + dx, -864 + dz)]);
    let moved = trigger_edit::moved(&retail, &a);
    assert_eq!(moved.iter().map(|m| (m.at, m.x, m.z, m.retail_x, m.retail_z)).collect::<Vec<_>>(), vec![
        (0x43C, -914, -896, -1170, -800),
        (0x444, -914, -960, -1170, -864),
    ]);
    assert!(place_edit::moved(&retail, &a).iter().any(|(slot, m)| *slot == MARY && m.at == branch && m.place == to));

    // Composes with a grown (relocated) line in the same pack, in either order.
    let line = p.pool(&retail).into_iter().find(|s| s.kind == "say").unwrap();
    let edits = BTreeMap::from([(line.offset, format!("{} Grown so it has to move to the end of the D-span.", line.markup))]);
    let (with_text, report) = text_edit::apply(&retail, &a, &edits).unwrap();
    assert!(!report.relocated.is_empty(), "the line relocated");
    assert_eq!(mary_talks(&with_text), mary_talks(&a), "the text edit keeps the moved triggers");
    let (text_first, _) = text_edit::apply(&retail, &retail, &edits).unwrap();
    let text_first = trigger_edit::apply(&retail, &text_first, &writes).unwrap();
    let text_first = place_edit::apply(&retail, &text_first, &[PlaceWrite { at: branch, place: Some(to) }]).unwrap();
    assert_eq!(text_first, with_text);

    // Restoring each edit returns the retail bytes.
    let restore: Vec<TriggerWrite> = talks.iter().map(|&(at, _, _)| TriggerWrite::retail(at)).collect();
    let r = trigger_edit::apply(&retail, &with_text, &restore).unwrap();
    assert!(trigger_edit::moved(&retail, &r).is_empty());
    let r = place_edit::apply(&retail, &r, &[PlaceWrite { at: branch, place: None }]).unwrap();
    let (clean, _) = text_edit::apply(&retail, &r, &BTreeMap::new()).unwrap();
    assert_eq!(clean, retail, "all three reverted");
}

/// Every record of every pack: writing its own position changes nothing,
/// a move touches only its four bytes and reads back, a restore is exact.
#[test]
fn every_trigger_record_round_trips() {
    let Some(disc) = disc() else { return };
    let mut records = 0;
    for (id, b) in disc.packs() {
        let Some(p) = NpcPack::parse(&b) else { continue };
        let Some(groups) = trigger::parse_tables(p.d_block(&b), p.actors.len()) else { continue };
        let all: Vec<(u32, i16, i16)> = trigger_edit::records(&groups).map(record_pos).collect();
        if all.is_empty() {
            continue;
        }
        let same: Vec<TriggerWrite> = all.iter().map(|&(at, x, z)| TriggerWrite::to(at, x, z)).collect();
        assert_eq!(trigger_edit::apply(&b, &b, &same).unwrap(), b, "{id}: own positions");
        let nudged: Vec<TriggerWrite> = all.iter().map(|&(at, x, z)| TriggerWrite::to(at, x.wrapping_add(1), z.wrapping_sub(1))).collect();
        let out = trigger_edit::apply(&b, &b, &nudged).unwrap();
        let d = p.d as usize;
        assert!(diff(&b, &out).iter().all(|&i| all.iter().any(|&(at, _, _)| (d + at as usize..d + at as usize + 4).contains(&i))), "{id}");
        let back = trigger::parse_tables(NpcPack::parse(&out).unwrap().d_block(&out), p.actors.len()).unwrap();
        let moved: Vec<(u32, i16, i16)> = trigger_edit::records(&back).map(record_pos).collect();
        assert_eq!(moved, all.iter().map(|&(at, x, z)| (at, x.wrapping_add(1), z.wrapping_sub(1))).collect::<Vec<_>>(), "{id}: same structure");
        assert_eq!(trigger_edit::moved(&b, &out).len(), all.len(), "{id}");
        let restore: Vec<TriggerWrite> = all.iter().map(|&(at, _, _)| TriggerWrite::retail(at)).collect();
        assert_eq!(trigger_edit::apply(&b, &out, &restore).unwrap(), b, "{id}: restore");
        records += all.len();
    }
    assert!(records > 2000, "{records} records");
    eprintln!("{records} trigger records round-trip");
}

#[test]
fn hard_coded_hotspots() {
    let Some(disc) = disc() else { return };
    assert_eq!(trigger::hotspots(&disc.exe, 39), vec![Hotspot::Script { addr: 0x801B_7188, x: 832, z: -1856, code: 0x30001 }]);
    assert!(trigger::hotspots(&disc.exe, 78).is_empty());
    let warps = trigger::hotspots(&disc.exe, 0xBB);
    assert_eq!(warps.len(), 2);
    assert!(warps.iter().all(|w| matches!(w, Hotspot::Warp { to: 498, .. })));
}

/// Saves the linked move through a real project folder, reopens it (as after
/// a relaunch) and reads the pack back through the overlay.
#[test]
fn a_linked_move_reads_back_through_a_project() {
    use lt_core::overlay;
    use lt_core::project::{DiscRef, Project};
    use std::sync::Arc;

    let Some(disc) = disc() else { return };
    let parent = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("lt-triggers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent).unwrap();
    let disc_ref = DiscRef {
        title: "Guardian's Crusade (USA)".into(),
        serial: "SLUS-00811".into(),
        verified: "redump".into(),
        fingerprint: None,
        hint_path: String::new(),
    };
    let mut project = Project::create(&parent, "trigger test", disc_ref, None).unwrap();
    let retail = disc.read("npc/6");
    let p = NpcPack::parse(&retail).unwrap();
    let mary = placement::roster(&p, &retail).into_iter().find(|a| a.slot == MARY).unwrap();
    let always = mary.branches.last().unwrap().at;
    let to = Place { x: -914, h: -129, z: -928, dir: 1024 };
    let staged = place_edit::apply(&retail, &retail, &[PlaceWrite { at: always, place: Some(to) }]).unwrap();
    let staged = trigger_edit::apply(&retail, &staged, &[TriggerWrite::to(0x43C, -914, -896), TriggerWrite::to(0x444, -914, -960)]).unwrap();
    project.stage("npc/6", vec!["npc/6".into()], &retail, &staged, "editor").unwrap();

    let reopened = Project::open(&project.root).unwrap();
    let e = &reopened.manifest.entries["npc/6"];
    let files: overlay::Staged = [("npc/6".to_string(), Arc::new(reopened.blobs().get(&e.current.sha256).unwrap()))].into();
    let b = overlay::file(&disc.image, &disc.table, Some(&files), "npc/6").unwrap();
    assert_eq!(mary_talks(&b), vec![(0x43C, -914, -896), (0x444, -914, -960)]);
    assert!(place_edit::moved(&retail, &b).iter().any(|(slot, m)| *slot == MARY && m.place == to));

    // Restoring both through the project drops the entry.
    let mut p2 = reopened;
    let back = trigger_edit::apply(&retail, &b, &[TriggerWrite::retail(0x43C), TriggerWrite::retail(0x444)]).unwrap();
    let back = place_edit::apply(&retail, &back, &[PlaceWrite { at: always, place: None }]).unwrap();
    p2.stage("npc/6", vec!["npc/6".into()], &retail, &back, "editor").unwrap();
    assert!(p2.manifest.entries.is_empty());
    std::fs::remove_dir_all(&parent).unwrap();
}

