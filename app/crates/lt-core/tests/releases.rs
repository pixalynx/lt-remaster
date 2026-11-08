//! The EU and Japanese releases against the user's own images, read-only. Opt in with
//! `LT_TEST_DISC_EU=<Guardian's Crusade (EU) .cue/.img>` and
//! `LT_TEST_DISC_JP=<Knight & Baby (Japan) .cue/.bin>` (`cargo test -p lt-core --test releases`);
//! `LT_TEST_DISC` (USA) runs the rest of the suite. The images are opened read-only.

use std::path::Path;
use std::sync::Mutex;

use lt_core::gamedata::{modules::EffectTable, GameData, Source};
use lt_core::npc::NpcPack;
use lt_core::release::Release;
use lt_core::text::{self, Encoding};
use lt_core::worldmap::{self, WorldDisc};
use lt_core::{cue, extract, fs::Kind, identity, world, DiscImage, FileTable, PsxExe};

/// [`text::decode`] reads the open disc's encoding, a process-wide setting: tests that decode
/// dialogue take turns.
static TEXT: Mutex<()> = Mutex::new(());

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

impl Disc {
    fn world(&self) -> WorldDisc<'_> {
        WorldDisc { image: &self.image, table: &self.table, exe: &self.exe }
    }

    fn source(&self) -> Source<'_> {
        Source::new(&self.image, &self.exe, &self.table)
    }
}

fn open(release: Release) -> Option<Disc> {
    let var = match release {
        Release::Usa => "LT_TEST_DISC",
        Release::Eu => "LT_TEST_DISC_EU",
        Release::Jp => "LT_TEST_DISC_JP",
    };
    let path = std::env::var_os(var)?;
    let image = DiscImage::open(cue::resolve_image(Path::new(&path)).unwrap()).unwrap_or_else(|e| panic!("{var}: {e}"));
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, exe, table })
}

/// What differs per release, as the decomp (`gc_decomp/docs/VERSIONS.md`) and the discs have it.
struct Expect {
    sectors: u32,
    table_va: u32,
    table_offset: usize,
    entries: usize,
    unique: usize,
    world_lba: u32,
    new_area_first: u32,
    npc7_lba: u32,
    npc7_global: u16,
}

fn expect(r: Release) -> Expect {
    match r {
        Release::Usa => Expect {
            sectors: 185_985,
            table_va: 0x801B_4304,
            table_offset: 0x41B04,
            entries: 493,
            unique: 488,
            world_lba: 20_011,
            new_area_first: 1328,
            npc7_lba: 86_013,
            npc7_global: 70,
        },
        Release::Eu => Expect {
            sectors: 186_066,
            table_va: 0x801B_461C,
            table_offset: 0x41E1C,
            entries: 493,
            unique: 488,
            world_lba: 19_999,
            new_area_first: 1332,
            npc7_lba: 86_001,
            npc7_global: 70,
        },
        Release::Jp => Expect {
            sectors: 172_194,
            table_va: 0x801B_4F50,
            table_offset: 0x42750,
            entries: 491,
            unique: 490,
            world_lba: 20_103,
            new_area_first: 1220,
            npc7_lba: 86_105,
            npc7_global: 68,
        },
    }
}

fn opens_and_reads_its_file_table(release: Release) {
    let Some(d) = open(release) else { return };
    let x = expect(release);
    let info = release.info();
    assert_eq!(d.exe.release, release);
    assert_eq!(d.image.sectors(), x.sectors);
    assert_eq!(lt_core::release::find_exe(&d.image).unwrap().0, release, "SYSTEM.CNF boots {}", info.exe_name);
    // The table where the built-in map puts it, cut into the release's slices.
    assert_eq!(d.exe.va(lt_core::fs::TABLE_VADDR), x.table_va);
    assert_eq!(d.exe.vaddr_to_offset(x.table_va).unwrap(), x.table_offset);
    assert_eq!(d.table.release, release);
    assert_eq!((d.table.entries.len(), d.table.unique_files), (x.entries, x.unique));
    assert_eq!(d.table.by_id("world").unwrap().extent.lba, x.world_lba);
    assert_eq!(info.world_lba, x.world_lba);
    assert_eq!(info.new_area_first(), x.new_area_first);
    let npc7 = d.table.by_id("npc/7").unwrap();
    assert_eq!((npc7.global, npc7.extent.lba, npc7.extent.sectors), (x.npc7_global, x.npc7_lba, 96));
    for (kind, count) in [(Kind::Npc, 50), (Kind::Toy, 73), (Kind::Enemy, 112), (Kind::Ovr, 25), (Kind::Bgm, 61)] {
        assert_eq!(d.table.of_kind(kind).count(), count, "{kind:?}");
    }
    // Every file is the shape its slice says (USA's checks, unchanged).
    let bad: Vec<_> = d
        .table
        .entries
        .iter()
        .filter(|e| e.extent.sectors > 0 && e.kind != Kind::Movie)
        .filter(|e| !extract::shape_ok(e.kind, e.index, &d.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()))
        .map(|e| e.id.clone())
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

fn is_its_known_dump(release: Release) {
    let Some(d) = open(release) else { return };
    let fp = identity::fingerprint(&d.image, |_, _| true).unwrap().unwrap();
    assert_eq!(fp.crc32, format!("{:08x}", release.info().crc32));
    assert!(fp.matches_redump);
    assert_eq!(fp.release, Some(release));
}

/// Overlay tables through the per-overlay maps: the Tier-2 length table, the chunk → pack
/// table, the movie player's records, the effect table and its arena modules.
fn reads_its_overlay_tables(release: Release) {
    let Some(d) = open(release) else { return };
    let w = d.world();
    let ovr2 = w.file("ovr/2").unwrap();
    let ops = (d.exe.ovr_va(2, 0x801F_C454) - lt_core::gamedata::OVR_LOAD) as usize;
    for info in lt_core::tier2::OPS.iter().take(lt_core::tier2::RETAIL_OPS) {
        assert_eq!(ovr2[ops + info.op as usize], info.len, "length of {:02X}", info.op);
    }
    let packs = worldmap::pack_table(&w);
    assert_eq!(packs.len(), if release == Release::Jp { 237 } else { 243 });
    assert_eq!(packs[0], (324, 7));
    let movies = lt_core::movie::player_settings_in(&d.exe, &w.file("ovr/1").unwrap());
    assert_eq!(movies.len(), release.info().sub_table(Kind::Movie).count as usize);
    assert_eq!((movies[0].last_frame, movies[0].width, movies[0].height), (401, 320, 224));
    let effects = EffectTable::parse_in(&d.exe, &w.file("ovr/4").unwrap());
    assert_eq!(effects.entries.len(), 122);
    let resident: Vec<u32> = (0..122).filter_map(|i| effects.module_mask(i)).collect();
    assert_eq!(resident.len(), 85);
    assert!(resident.iter().all(|&m| m != 0), "every module-resident effect is in some module");
}

fn loads_game_data(release: Release) {
    let Some(d) = open(release) else { return };
    let g = GameData::load(d.source()).unwrap();
    assert_eq!((g.enemies.len(), g.items.items.len(), g.toys.roster.len(), g.shops.shops.len()), (110, 215, 72, 23));
    assert_eq!((g.encounters.zones.len(), g.encounters.story.len()), (244, 64));
    assert_eq!(&g.items.drop_remap[..10], &[0, 1, 2, 3, 4, 5, 6, 188, 189, 9]);
    assert_eq!(g.shops.shops[1].items, vec![1, 5, 9, 11, 13, 10, 12, 14]);
    assert_eq!((g.items.items[1].price, g.enemy(4).unwrap().level), (10, 17));
    let names = |n: [&str; 6]| n.map(String::from).to_vec();
    let got = vec![
        g.items.items[1].name.clone(),
        g.items.items[1].desc.clone(),
        g.toys.roster[0].name.clone(),
        g.toys.roster[0].file.as_ref().unwrap().phases[0].name.clone(),
        g.enemy(2).unwrap().name.clone(),
        g.growth.templates[1].name.clone(),
    ];
    if release == Release::Jp {
        assert_eq!(got, names(["オニギリ", "たいりょくが すこし かいふくする", "ガブリエル", "かれらにしゅくふくを", "ディノ", "ベイビー"]));
    } else {
        assert_eq!(got, names(["Candy Bar", "Restores your hit points a little", "Gabriel", "Holy Light", "Dino", "Baby"]));
    }
}

fn reads_scripts_text_and_world(release: Release) {
    let Some(d) = open(release) else { return };
    let w = d.world();
    let _turn = TEXT.lock().unwrap_or_else(|e| e.into_inner());
    text::set_encoding(release.info().text);
    let (mut programs, mut errors, mut raw) = (0, 0, 0);
    for p in 0..50 {
        let b = w.file(&format!("npc/{p}")).unwrap();
        let pack = NpcPack::parse(&b).unwrap_or_else(|| panic!("npc/{p}"));
        for (_, _, r) in pack.programs(&b) {
            if r.is_ok() {
                programs += 1
            } else {
                errors += 1
            }
        }
        raw += pack.dialogue(&b).iter().flat_map(|l| &l.tokens).filter(|t| matches!(t, text::Tok::Raw(v) if v[0] >= 0x80)).count();
    }
    assert!(programs > 3700 && errors == 0, "{programs} programs, {errors} errors");
    assert_eq!(raw, 0, "every two-byte character decodes");
    let b = w.file("npc/7").unwrap();
    let lines = NpcPack::parse(&b).unwrap().dialogue(&b);
    let hello = if release == Release::Jp { "おはよう $A|きょうは " } else { "Good morning, $A.|I was looking for you," };
    assert!(lines.iter().any(|l| l.plain.starts_with(hello)), "npc/7 greets the Knight");
    text::set_encoding(Encoding::Ascii);
    let chunks = (0..world::CHUNK_COUNT).filter(|&c| w.chunk(c).ok().and_then(|b| world::stats(c, &b)).is_some()).count();
    assert_eq!(chunks as u32, world::CHUNK_COUNT);
}

/// A project on another release opens, says which, and refuses edits.
fn makes_a_read_only_project(release: Release) {
    let Some(d) = open(release) else { return };
    let info = d.exe.release.info();
    let dir = std::env::temp_dir().join(format!("lt-release-{release:?}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let disc = lt_core::project::DiscRef {
        title: info.title.into(),
        serial: info.serial.into(),
        verified: "redump".into(),
        fingerprint: None,
        hint_path: String::new(),
    };
    let mut p = lt_core::project::Project::create(&dir, "On another release", disc, None).unwrap();
    assert_eq!(p.file.disc.release(), release);
    assert_eq!(p.new_area_first(), info.new_area_first());
    let npc7 = d.world().file("npc/7").unwrap();
    let mut edited = npc7.clone();
    edited[0x300] ^= 1;
    let err = p.stage("npc/7", vec!["npc/7".into()], &npc7, &edited, "editor").unwrap_err();
    assert_eq!(err.code(), "RELEASE_READ_ONLY");
    assert!(p.manifest.entries.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

macro_rules! per_release {
    ($($name:ident),* $(,)?) => {
        mod eu {
            $(#[test] fn $name() { super::$name(lt_core::release::Release::Eu) })*
        }
        mod jp {
            $(#[test] fn $name() { super::$name(lt_core::release::Release::Jp) })*
        }
    };
}

per_release!(
    opens_and_reads_its_file_table,
    is_its_known_dump,
    reads_its_overlay_tables,
    loads_game_data,
    reads_scripts_text_and_world,
    makes_a_read_only_project,
);

/// USA reads as it always did through the same paths (the rest of the suite covers it in depth).
#[test]
fn usa_is_unchanged() {
    for f in [opens_and_reads_its_file_table, reads_its_overlay_tables, loads_game_data, reads_scripts_text_and_world] {
        f(Release::Usa);
    }
    let Some(d) = open(Release::Usa) else { return };
    assert!(d.exe.layout.is_none(), "USA retail has no address map");
    assert_eq!(lt_core::fs::canonical(70).2, "npc/7");
}
