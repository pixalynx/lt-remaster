//! What a new game starts with (LT-56, `edit::start`) on the user's own images, which are only
//! read: the starter scene's program gives the Baby, rubies, items and flags before its warp, and
//! the party templates hold the levels.
//!
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test start`.
//! With `LT_TEST_BASE=<package dir>` and `LT_TEST_BASE_IMAGE=<its materialised .bin>`, the same runs
//! on a blank HackerGC base (its shifted EXE and its starter scene in npc/7).

use std::path::PathBuf;

use lt_core::base::BasePackage;
use lt_core::edit::blank;
use lt_core::edit::start::{self, Start};
use lt_core::edit::tables::{self, TemplateFields};
use lt_core::gamedata::growth::Growth;
use lt_core::gamedata::Source;
use lt_core::npc::NpcPack;
use lt_core::{tier2, DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

impl Disc {
    fn open(path: &std::path::Path) -> Self {
        let image = DiscImage::open(path).unwrap();
        let exe = PsxExe::from_disc(&image).unwrap();
        let table = FileTable::parse(&exe, image.sectors()).unwrap();
        Self { image, exe, table }
    }
    fn read(&self, id: &str) -> Vec<u8> {
        let e = self.table.by_id(id).unwrap();
        self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
    }
    fn growth(&self) -> Growth {
        Growth::load(Source::new(&self.image, &self.exe, &self.table)).unwrap()
    }
}

fn full() -> Start {
    // A potion-class item, a key item (record 198), a Living Toy (row 3); two story flags.
    Start { baby: true, rubies: Some(1234), items: vec![5, 500, 1003], flags: vec![0x81, 0x1A0] }
}

/// The director's program as ops and their first operand.
fn ops(pack: &[u8], slot: u32) -> Vec<(u8, i64)> {
    let p = NpcPack::parse(pack).unwrap();
    let code = tier2::disassemble(p.d_block(pack), p.actors[slot as usize].script).unwrap();
    code.iter()
        .map(|i| {
            let v = match i.operands.first() {
                Some(tier2::Operand::Flag(v) | tier2::Operand::Item(v) | tier2::Operand::U16(v)) => *v as i64,
                Some(tier2::Operand::I32(v)) => *v as i64,
                Some(tier2::Operand::S16(v)) => *v as i64,
                _ => 0,
            };
            (i.op, v)
        })
        .collect()
}

/// `current` (a pack with a starter scene on `spot`) takes `s` and reads it back, and the
/// director's bytes are the head then the warp.
fn round_trip(reference: &[u8], current: &[u8], chunk: u16, spot: (i16, i16)) {
    let found = blank::find_starter_at(reference, current, chunk, spot.0, spot.1).unwrap().expect("a starter scene");
    let (none, extra) = start::of_scene(reference, current, found.slot).unwrap();
    assert_eq!((none, extra), (Start::default(), 0), "the plain starter gives nothing");
    let with = start::set_scene(reference, current, found.slot, &full(), 0x200, &[]).unwrap();
    assert_eq!(start::of_scene(reference, &with, found.slot).unwrap(), (full(), 0));
    let got = ops(&with, found.slot);
    let want = vec![
        (0x05, 0xFF),
        (0x05, 0xFC),
        (0x05, 0xFE),
        (0x04, 0xF9),
        (0x33, -999_999),
        (0x33, 1234),
        (0x16, 5),
        (0x16, 500),
        (0x16, 1003),
        (0x04, 0x81),
        (0x04, 0x1A0),
    ];
    assert_eq!(&got[..want.len()], want.as_slice());
    assert_eq!(got[want.len()].0, 0x19, "then the warp");
    assert_eq!(got.last().unwrap().0, 0x00);
    // The scene still lists, on its spot and cue, and taking the start out restores the program.
    let s = blank::find_starter_at(reference, &with, chunk, spot.0, spot.1).unwrap().unwrap();
    assert_eq!(s.slot, found.slot);
    let back = start::set_scene(reference, &with, found.slot, &Start::default(), 0x200, &[]).unwrap();
    assert_eq!(start::of_scene(reference, &back, found.slot).unwrap().0, Start::default());
    assert_eq!(ops(&back, found.slot), ops(current, found.slot));
    // A start the bag can't hold is refused before anything is written.
    let too_many = Start { items: vec![5; start::ITEM_ROOM + 1], ..Default::default() };
    assert_eq!(start::set_scene(reference, current, found.slot, &too_many, 0x200, &[]).unwrap_err().code, "START_ITEMS_FULL");
}

/// The Knight at `level` in `exe`, read back through `Growth` (the gamedata reader screen 13 uses).
fn knight_at(d: &Disc, exe: &[u8], level: u16) -> (u16, u16, u16) {
    let g = d.growth();
    let base = tables::template_at(exe, 0).unwrap();
    let t = start::at_level(&base, &base, level, &g.level_up, g.exp_step).unwrap().unwrap();
    let out = tables::set_template(exe, 0, &TemplateFields::of(&t)).unwrap();
    let back = tables::template_at(&out, 0).unwrap();
    assert_eq!(tables::template_at(&out, 1).unwrap().level, tables::template_at(exe, 1).unwrap().level, "the Baby's stays");
    assert_eq!((back.hp, back.pp), (back.max_hp, back.max_pp), "full HP and PP");
    (back.level, back.max_hp, back.max_pp)
}

/// @LT-56/2: on retail's npc/7 and EXE, the start goes into the starter scene's program and the
/// Knight's template, where a new game reads them.
#[test]
fn a_start_is_written_where_a_new_game_reads_it() {
    let Some(path) = std::env::var_os("LT_TEST_DISC").map(PathBuf::from) else { return };
    let d = Disc::open(&path);
    let retail7 = d.read("npc/7");
    let (with_starter, _) = blank::starter(&retail7, &retail7).unwrap();
    let (x, _, z) = blank::BOOT_SPOT;
    round_trip(&retail7, &with_starter, blank::BOOT_CHUNK, (x, z));
    let rule = d.growth();
    assert!(rule.level_up.verified, "the level-up rule reads from the EXE's code");
    assert_eq!(knight_at(&d, &d.exe.bytes, 5), (5, 49, 22));
}

/// @LT-56/2: the same on a blank HackerGC base: its npc/7 (read against retail's, LT-42) and its
/// shifted EXE, whose templates sit where the layout puts them.
#[test]
fn a_start_is_written_on_a_blank_base() {
    let (Some(retail), Some(pkg), Some(img)) = (
        std::env::var_os("LT_TEST_DISC").map(PathBuf::from),
        std::env::var_os("LT_TEST_BASE").map(PathBuf::from),
        std::env::var_os("LT_TEST_BASE_IMAGE").map(PathBuf::from),
    ) else {
        return;
    };
    let pkg = BasePackage::open(&pkg).unwrap();
    let r = Disc::open(&retail);
    let b = Disc::open(&img);
    assert!(lt_core::edit::boot::supported(&b.exe.bytes), "{} has a boot map", pkg.manifest.tag());
    let (retail7, base7) = (r.read("npc/7"), b.read("npc/7"));
    let boot = lt_core::edit::boot::read(&b.exe.bytes).unwrap();
    round_trip(&retail7, &base7, boot.chunk, (boot.x, boot.z));
    // The base's templates are retail's, moved.
    let (rk, bk) = (tables::template_at(&r.exe.bytes, 0).unwrap(), tables::template_at(&b.exe.bytes, 0).unwrap());
    assert_eq!((bk.name.as_str(), bk.level, bk.max_hp), (rk.name.as_str(), rk.level, rk.max_hp));
    assert_eq!(knight_at(&b, &b.exe.bytes, 5), (5, 49, 22));
    eprintln!("{}: start on chunk/{} carries the Baby, rubies, items and flags; the Knight's template at level 5 has 49 HP, 22 PP", pkg.manifest.tag(), boot.chunk);
}
