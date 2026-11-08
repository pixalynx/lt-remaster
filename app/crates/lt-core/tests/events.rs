//! Events, text and flags against the user's own retail image (screens 11, 12, 17).
//! Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test events`.
//! The image is opened read-only. `LT_DUMP_DIR=<dir>` also writes the flag index
//! as JSON there (for oracle diffs against the Python tools).

use lt_core::fs::Kind;
use lt_core::npc::NpcPack;
use lt_core::trigger::TriggerRecord;
use lt_core::{flags, DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
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
    fn packs(&self) -> Vec<(u16, Vec<u8>, NpcPack)> {
        self.table
            .of_kind(Kind::Npc)
            .filter(|e| e.aliases.is_empty() || e.global < e.aliases[0])
            .map(|e| {
                let n: u16 = e.id.trim_start_matches("npc/").parse().unwrap();
                let b = self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
                let p = NpcPack::parse(&b).unwrap();
                (n, b, p)
            })
            .collect()
    }
}

#[test]
fn npc7_triggers_actors_and_pool() {
    let Some(disc) = disc() else { return };
    let b = disc.read("npc/7");
    let pack = NpcPack::parse(&b).unwrap();

    // Two groups: 324 (8 doors, pass 0) and the 326 ladder (pass 1, 8 rungs).
    let groups = pack.triggers(&b);
    assert_eq!(groups.len(), 2);
    let g324 = groups.iter().find(|g| g.chunk == 324).unwrap();
    assert_eq!(g324.pass, 0);
    assert_eq!(g324.rungs[0].records.len(), 8);
    assert!(g324.rungs[0].records.iter().all(|r| matches!(r, TriggerRecord::Door { to: 324, .. })));
    let g326 = groups.iter().find(|g| g.chunk == 326).unwrap();
    assert_eq!(g326.pass, 1);
    let flags: Vec<u16> = g326.rungs.iter().map(|r| r.flag).collect();
    assert_eq!(flags, vec![0x186, 0x190, 0x88, 0x21, 0x81, 0x1A, 0x0E, 0]);
    assert_eq!(g326.rungs[7].records, vec![TriggerRecord::Talk { at: g326.rungs[7].records[0].at(), x: 508, z: 76, actor: 3 }]);

    // Actors: 102 slots; actor 2 is the Mayor with an inn and a chest; 3 is a scene.
    let actors = pack.actor_views(&b);
    assert_eq!(actors.len(), 102);
    assert_eq!(actors[2].speaker.as_deref(), Some("Mayor"));
    assert_eq!(actors[2].chunk, 324);
    assert!(actors[2].tags.contains(&"inn") && actors[2].tags.contains(&"chest"));
    assert!(actors[3].scene && actors[3].tags.contains(&"warp"));
    assert_eq!((actors[3].script, actors[3].ops, actors[3].bytes), (0x2024, 30, 84));

    // Code region and gap scan (only 00 padding, if anything).
    let (start, end) = pack.code_span(&b);
    assert_eq!((start, end), (0x1F64, 0x2750));
    assert!(pack.code_gaps(&b).iter().all(|g| g.padding));

    // Pool: 95 referenced strings, the Mayor line at 07A8, no orphans.
    let pool = pack.pool(&b);
    assert_eq!(pool.iter().filter(|s| s.kind != "orphan").count(), 95);
    assert_eq!(pool.iter().filter(|s| s.kind == "orphan").count(), 0);
    let mayor = pool.iter().find(|s| s.offset == 0x07A8).unwrap();
    assert!(mayor.markup.starts_with("{box:0}Good morning, $A."), "{}", mayor.markup);
    assert!(mayor.markup.contains("Mayor Kaylan"));
    assert_eq!((mayor.len, mayor.slot), (150, 152));
    let s = pack.summary(&b);
    assert_eq!(s.lines, 92);
    assert_eq!(s.speakers[..3], ["Mayor".to_string(), "O'Neal".into(), "Blue Cat".into()]);
    assert!(s.chunks.contains(&326) && s.chunks.contains(&324));

    // Placement ladder of actor 3 tests 0x081 first.
    let ladder = pack.placement(&b, 3);
    assert!(!ladder.is_empty());
    assert_eq!(ladder.last().unwrap().cond, 0);
}

trait At {
    fn at(&self) -> u32;
}
impl At for TriggerRecord {
    fn at(&self) -> u32 {
        match *self {
            TriggerRecord::Door { at, .. }
            | TriggerRecord::Set { at, .. }
            | TriggerRecord::Clear { at, .. }
            | TriggerRecord::Talk { at, .. }
            | TriggerRecord::Other { at, .. } => at,
        }
    }
}

#[test]
fn every_pack_has_tables_and_a_pool_walk() {
    let Some(disc) = disc() else { return };
    let (mut doors, mut sets, mut talks, mut orphans, mut lines) = (0, 0, 0, 0, 0);
    let t = std::time::Instant::now();
    for (n, b, pack) in disc.packs() {
        for g in pack.triggers(&b) {
            for r in g.rungs.iter().flat_map(|r| r.records.iter()) {
                match r {
                    TriggerRecord::Door { .. } => doors += 1,
                    TriggerRecord::Set { .. } => sets += 1,
                    TriggerRecord::Talk { .. } => talks += 1,
                    r => panic!("npc/{n}: unexpected record {r:?}"),
                }
            }
        }
        let s = pack.summary(&b);
        orphans += s.orphans;
        lines += s.lines;
        // Retail pools only use 05 n 00, 06 and 07 (no bare 04, no bytes ≥ 0x80).
        for p in pack.pool(&b) {
            let odd = p.markup.match_indices('{').any(|(i, _)| {
                let rest = &p.markup[i..];
                !(rest.starts_with("{box:") || rest.starts_with("{nl}") || rest.starts_with("{wait}"))
            });
            assert!(!odd, "npc/{n}@{:04X}: {}", p.offset, p.markup);
        }
    }
    eprintln!("doors {doors} set {sets} talk {talks} orphans {orphans} lines {lines} in {:?}", t.elapsed());
    // research 03 §2.3: 1,930 doors, 141 set-flag, 0 clear-flag, 488 type-10 talks (49 distinct packs).
    assert_eq!((doors, sets, talks), (1930, 141, 488));
    assert_eq!(orphans, 79);
}

#[test]
fn flag_index_matches_the_catalogue_counts() {
    let Some(disc) = disc() else { return };
    let t = std::time::Instant::now();
    let mut builder = flags::IndexBuilder::default();
    for (n, b, pack) in disc.packs() {
        builder.add_pack(n, &b, &pack);
    }
    let index = builder.finish();
    eprintln!("flag index built in {:?}", t.elapsed());

    assert_eq!(index.packs.len(), 49);
    assert_eq!(index.cats.len(), 512);
    let in_pack = |p: u16| index.sites.iter().filter(|(_, v)| v.iter().any(|s| s.1 == p)).count();
    assert_eq!(in_pack(7), 29, "flags any npc/7 site touches");

    let e = &index.sites[&0x0E];
    let mut packs: Vec<u16> = e.iter().map(|s| s.1).collect();
    packs.dedup();
    assert_eq!(e.len(), 44, "37 script, trigger and placement sites, 7 walk reads");
    assert_eq!(packs, vec![3, 7, 21, 22, 30, 42, 44]);
    // The wake-up scene sets 0x00E in its else lane, after "I've never seen anyone get up so slowly!".
    let wake = e.iter().find(|s| s.0 == 'S' && s.1 == 7).unwrap();
    assert_eq!((wake.2, wake.3, wake.4, wake.6), (3, 0x205F, 326, 2));
    assert!(index.says[wake.5 as usize].starts_with("I've never seen"), "{}", index.says[wake.5 as usize]);

    // Chests: 8 use real flags; the rest pass 0xF8.
    let chests_f8 = index.sites[&0xF8].iter().filter(|s| s.0 == 'H').count();
    let chests_real: usize = index.sites.iter().filter(|(f, _)| **f != 0xF8).map(|(_, v)| v.iter().filter(|s| s.0 == 'H').count()).sum();
    eprintln!("chest sites: {chests_f8} with 0xF8, {chests_real} with a flag");

    let referenced = index.sites.len();
    let cats = |c: char| index.cats.chars().filter(|&x| x == c).count();
    eprintln!(
        "referenced {referenced} · S {} T {} R {} r {} U {} · objectives {} · says {}",
        cats('S'),
        cats('T'),
        cats('R'),
        cats('r'),
        cats('U'),
        index.objectives.len(),
        index.says.len()
    );
    assert_eq!((cats('S'), cats('R')), (64, 16));
    assert_eq!(index.wipes[&7], [2, 0]);

    if let Some(dir) = std::env::var_os("LT_DUMP_DIR") {
        let path = std::path::Path::new(&dir).join("flags_index.json");
        std::fs::write(&path, serde_json::to_string(&index).unwrap()).unwrap();
        eprintln!("wrote {}", path.display());
    }
}

#[test]
fn dialogue_box_from_sys14() {
    let Some(disc) = disc() else { return };
    let sys14 = lt_core::textbox::Sys14::parse(&disc.read("sys/2")).unwrap();
    let atlas = sys14.glyph_atlas();
    assert_eq!(atlas.len(), lt_core::textbox::ATLAS_W * lt_core::textbox::ATLAS_H * 4);
    // 'A' (cell 0x20) has ink; the space before '!' is not a cell.
    let ink = |cell: usize| {
        let (u, v) = ((cell % 24) * 8, (cell / 24) * 12);
        (0..12).flat_map(|y| (0..8).map(move |x| (x, y))).filter(|(x, y)| atlas[((v + y) * 192 + u + x) * 4 + 3] != 0).count()
    };
    assert!(ink(0x20) > 10, "A has ink");
    let frame = sys14.frame(0, Some("Mayor"));
    assert_eq!(frame.len(), 320 * 240 * 4);
    let plain = sys14.frame(0, None);
    // The window changes pixels inside its rectangle and nothing above it.
    let px = |f: &[u8], x: usize, y: usize| f[(y * 320 + x) * 4..][..3].to_vec();
    assert_ne!(px(&frame, 44 + 4, 0xA4 + 2), px(&lt_core::textbox::Sys14::parse(&disc.read("sys/2")).unwrap().frame(0, None), 10, 0xA4 + 2));
    assert_eq!(px(&frame, 10, 10), px(&plain, 10, 10));
    if let Some(dir) = std::env::var_os("LT_DUMP_DIR") {
        let dir = std::path::Path::new(&dir);
        std::fs::write(dir.join("atlas.rgba"), &atlas).unwrap();
        std::fs::write(dir.join("frame_mayor.rgba"), &frame).unwrap();
        std::fs::write(dir.join("frame_top.rgba"), sys14.frame(3, None)).unwrap();
    }
}

#[test]
fn pack_views_are_quick() {
    let Some(disc) = disc() else { return };
    let mut worst = (0u128, String::new());
    for (n, b, pack) in disc.packs() {
        let t = std::time::Instant::now();
        let actors = pack.actor_views(&b);
        let programs = pack.actor_programs(&b);
        let pool = pack.pool(&b);
        let triggers = pack.triggers(&b);
        let gaps = pack.code_gaps(&b);
        let us = t.elapsed().as_micros();
        let instrs: usize = programs.iter().map(|(_, r)| r.as_ref().map(|c| c.len()).unwrap_or(0)).sum();
        let json = serde_json::to_string(&programs.iter().filter_map(|(_, r)| r.as_ref().ok()).collect::<Vec<_>>()).unwrap().len();
        if us > worst.0 {
            worst = (us, format!("npc/{n}: {} actors, {instrs} instrs ({json} B json), {} strings, {} groups, {} gaps", actors.len(), pool.len(), triggers.len(), gaps.len()));
        }
        assert!(actors.iter().all(|a| a.error.is_none()), "npc/{n}");
    }
    eprintln!("slowest pack view {} µs: {}", worst.0, worst.1);
}
