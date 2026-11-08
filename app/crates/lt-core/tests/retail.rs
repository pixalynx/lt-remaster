//! Checks against the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core`.
//! The image is opened read-only.

use lt_core::{fs::Kind, iso9660, DiscImage, FileTable, PsxExe};

fn disc() -> Option<DiscImage> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    Some(DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin"))
}

#[test]
fn hidden_file_table_matches_research() {
    let Some(disc) = disc() else { return };
    assert_eq!(disc.sectors(), 185_985);

    let vol = iso9660::read_volume(&disc).unwrap();
    assert_eq!(vol.volume_space_size, 185_985);
    let root = iso9660::read_dir(&disc, vol.root_lba, vol.root_size).unwrap();
    assert_eq!(root.iter().filter(|e| e.is_dir).count(), 13, "13 decoy folders");

    let exe = PsxExe::from_disc(&disc).unwrap();
    assert_eq!(exe.lba, 24);
    assert_eq!(exe.bytes.len(), 317_440);
    assert_eq!(exe.vaddr_to_offset(lt_core::fs::TABLE_VADDR).unwrap(), 0x41B04);

    let table = FileTable::parse(&exe, disc.sectors()).unwrap();
    assert_eq!(table.entries.len(), 493);
    assert_eq!(table.unique_files, 488);
    assert_eq!(table.alias_entries, 4);

    let npc7 = table.by_id("npc/7").unwrap();
    assert_eq!(npc7.raw, [0x19, 0x08, 0x63, 0x00, 0x60, 0x00, 0x00, 0x00]);
    assert_eq!((npc7.extent.lba, npc7.extent.sectors), (86_013, 96));
    assert_eq!(npc7.exe_offset, 0x41D34);
    assert!(npc7.aliases.is_empty());

    let npc0 = table.by_id("npc/0").unwrap();
    assert_eq!(npc0.extent.lba, 85_513);
    assert_eq!(npc0.aliases, vec![table.by_id("npc/17").unwrap().global]);

    assert_eq!(table.by_id("toy/22").unwrap().extent, lt_core::fs::Extent { lba: 672, sectors: 26 });
    assert_eq!(table.by_id("enemy/17").unwrap().extent.lba, 6_780);
    assert_eq!(table.by_id("world").unwrap().extent.lba, 20_011);
    assert_eq!(table.of_kind(Kind::Npc).count(), 50);
}

#[test]
fn assets_match_research() {
    let Some(disc) = disc() else { return };
    let exe = PsxExe::from_disc(&disc).unwrap();
    let table = FileTable::parse(&exe, disc.sectors()).unwrap();
    let read = |id: &str| {
        let e = table.by_id(id).unwrap();
        disc.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
    };

    // sys/2 = SYS14: the HUD & font page.
    let sys14 = read("sys/2");
    let tim = lt_core::tim::Tim::parse(&sys14).unwrap();
    assert_eq!((tim.info.bpp, tim.info.width, tim.info.height), (4, 512, 512));
    assert_eq!((tim.info.image.x, tim.info.image.y), (896, 0));
    let clut = tim.info.clut.unwrap();
    assert_eq!((clut.x, clut.y, clut.w, clut.h), (0, 496, 160, 16));
    assert_eq!(tim.palettes_per_row(), 10);
    // Row 496, palette 0: transparent, then the white text ramp.
    let row: Vec<_> = tim.palette(0, 0).into_iter().map(lt_core::tim::bgr555_to_hex).collect();
    assert_eq!(row[0], None);
    assert_eq!(row[1].as_deref(), Some("#FFFFFF"));
    assert_eq!(tim.rgba(0, 0).len(), 512 * 512 * 4);

    // toy/22 = Draken.
    let draken = lt_core::toy::parse(&read("toy/22")).unwrap();
    let m = draken.model.unwrap();
    assert_eq!((m.vertices, m.normals, m.primitives), (219, 259, 304));
    assert_eq!(m.classes["3004"], 218);
    assert_eq!(m.classes["3804"], 74);
    assert_eq!(m.classes["3C00"], 4);
    assert_eq!(m.classes["3400"], 8);
    let t = draken.texture.unwrap();
    assert_eq!((t.width, t.height, t.clut.unwrap().h), (128, 128, 3));
    assert_eq!(draken.poses.unwrap().poses.len(), 7);

    // toy/40 = Heal Bat: 8 poses over all 145 vertices; ability sequence 3.
    let bat = lt_core::toy::parse(&read("toy/40")).unwrap();
    let poses = bat.poses.unwrap();
    let deltas: Vec<u16> = poses.poses.iter().map(|p| p.max_delta).collect();
    assert_eq!(deltas, vec![24, 34, 41, 46, 60, 55, 34, 27]);
    assert!(poses.poses.iter().all(|p| p.first_vertex == 0 && p.count == 145 && p.records == 1));
    assert_eq!(poses.len, 7_060);
    let seq3 = bat.sequences.iter().find(|s| s.index == 3).unwrap();
    assert_eq!(seq3.entries.len(), 10);
    assert_eq!(seq3.entries[4], (4, 64));
    assert_eq!(seq3.entries[9], (0xFF, 0));
}

#[test]
fn npc7_scripts_and_text() {
    let Some(disc) = disc() else { return };
    let exe = PsxExe::from_disc(&disc).unwrap();
    let table = FileTable::parse(&exe, disc.sectors()).unwrap();
    let read = |id: &str| {
        let e = table.by_id(id).unwrap();
        disc.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
    };

    // The opcode length table in OVR02 matches ours; HackerGC's EXT is 0 (the VM hangs) on retail.
    let ovr2 = read("ovr/2");
    for info in lt_core::tier2::OPS.iter().take(lt_core::tier2::RETAIL_OPS) {
        assert_eq!(ovr2[0x1D454 + info.op as usize], info.len, "length of {:02X}", info.op);
    }
    assert_eq!(ovr2[0x1D454 + lt_core::tier2::OP_EXT as usize], 0);

    let bytes = read("npc/7");
    let pack = lt_core::npc::NpcPack::parse(&bytes).unwrap();
    assert_eq!((pack.p, pack.d), (0x70, 0x204));
    assert_eq!(pack.actors.len(), 102);
    assert_eq!(pack.models.len(), 26);
    assert_eq!(pack.d_span, 0x2CB8);

    // The wake-up scene (PRD 11 §4.5): actor 3, D+0x2024, 30 ops, 84 bytes.
    let d = pack.d_block(&bytes);
    let code = lt_core::tier2::disassemble(d, 0x2024).unwrap();
    assert_eq!(code.len(), 30);
    assert_eq!(code.iter().map(|i| i.bytes.len()).sum::<usize>(), 84);
    assert_eq!(code[0].name, "IF_FLAG");
    assert_eq!(code.last().unwrap().name, "END");
    let objective = code.iter().find(|i| i.name == "OBJECTIVE").unwrap();
    assert_eq!(objective.operands[0], lt_core::tier2::Operand::Objective(0x1CC0));
    assert_eq!(objective.at, 0x2068);

    // Every actor script decodes to END.
    let programs = pack.programs(&bytes);
    assert!(programs.iter().all(|(_, _, r)| r.is_ok()));

    // Dialogue reached from the scripts; the Mayor's letter line.
    let lines = pack.dialogue(&bytes);
    let mayor = lines.iter().find(|l| l.offset == 0x07A8).unwrap();
    assert_eq!(mayor.kind, "say");
    assert_eq!(mayor.speaker.as_deref(), Some("Mayor"));
    assert!(mayor.plain.contains("Mayor Kaylan"), "{}", mayor.plain);
    let objective_line = lines.iter().find(|l| l.offset == 0x1CC0).unwrap();
    assert!(objective_line.plain.starts_with("The Mayor was looking"), "{}", objective_line.plain);
}

#[test]
fn every_pack_script_decodes() {
    let Some(disc) = disc() else { return };
    let exe = PsxExe::from_disc(&disc).unwrap();
    let table = FileTable::parse(&exe, disc.sectors()).unwrap();
    let mut actors = 0;
    let mut programs = 0;
    for e in table.of_kind(Kind::Npc) {
        let bytes = disc.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
        let pack = lt_core::npc::NpcPack::parse(&bytes).unwrap_or_else(|| panic!("{} header", e.id));
        actors += pack.actors.len();
        for (actor, entry, res) in pack.programs(&bytes) {
            assert!(res.is_ok(), "{} actor {actor} @{entry:#06x}: {res:?}", e.id);
            programs += 1;
        }
    }
    eprintln!("{actors} actor records, {programs} programs");
    assert!(programs > 0);
}

#[test]
fn world_chunk_39() {
    let Some(disc) = disc() else { return };
    let exe = PsxExe::from_disc(&disc).unwrap();
    let table = FileTable::parse(&exe, disc.sectors()).unwrap();
    let world = table.by_id("world").unwrap();
    assert_eq!(world.extent.lba, 20_011);
    let lba = world.extent.lba + 39 * lt_core::world::CHUNK_SECTORS;
    let c = disc.read_sectors(lba, lt_core::world::CHUNK_SECTORS).unwrap();
    let s = lt_core::world::stats(39, &c).unwrap();
    eprintln!(
        "chunk 39: groups {} verts {} polys {} lists {:?} mesh {:#x} heights {} {:?} zones {:?} blocked {} tim {:?}",
        s.groups, s.vertices, s.polygons, s.lists, s.mesh_bytes, s.heights.len(), s.heights, s.zones, s.blocked_cells,
        s.texture.as_ref().map(|t| (t.width, t.height, t.image.x, t.image.y))
    );
    assert_eq!(s.grid, Some((3, 3)));
    assert_eq!(lt_core::world::chunk_at(3, 3), Some(39));
    assert_eq!(s.groups, 53);
    assert!(s.mesh_bytes <= 0x2A800);
    assert!(s.texture.is_some());
    assert_eq!(s.heights.len(), 28);
    assert_eq!(format!("{:.1}", s.mesh_bytes as f64 * 100.0 / 0x2A800 as f64), "97.0");
    eprintln!("tex {:.1}%", s.texture_bytes as f64 * 100.0 / 0x10800 as f64);
}

/// Opt in with `LT_TEST_CARD=/path/to/card1.mcd` (read-only).
#[test]
fn save_card() {
    let Some(path) = std::env::var_os("LT_TEST_CARD") else { return };
    let bytes = std::fs::read(path).unwrap();
    let card = lt_core::save::parse_card(&bytes).unwrap();
    assert!(card.header_ok);
    assert_eq!(card.saves.len(), 1);
    let s = &card.saves[0];
    eprintln!(
        "{} title={:?} lvl={} loc={} rubies={} map={} pos={:?} frames={} steps={} flags={:?} hdr={} bak={} sum={:08X} fresh={:08X} party={:?}",
        s.file_name, s.title, s.level, s.location, s.rubies, s.map, s.position, s.play_frames, s.steps, s.flags_set,
        s.header_ok, s.backup_ok, s.body_sum, s.body_sum_fresh, s.party.iter().map(|p| (&p.name, p.level, p.hp, p.luck)).collect::<Vec<_>>()
    );
    assert_eq!(s.file_name, "BASLUSP00811KNIGHT00");
    assert!(s.header_ok && s.backup_ok);
    assert_eq!(s.rubies, 104);
    assert_eq!(s.play_frames, 17_892);
    assert_eq!(s.steps, 224);
    assert_eq!(s.map, 329);
    assert_eq!(s.position, (-1424, -129, 1312));
    assert_eq!(s.flags_set, vec![0x0E, 0x20, 0x23, 0xFE]);
    assert_eq!((s.party[0].level, s.party[0].hp, s.party[0].luck), (1, 18, 144));
    assert_eq!(s.party[1].luck, 184);
    assert_eq!(s.body_sum, s.body_sum_fresh);
    assert_eq!(s.icon_rgba.len(), 16 * 16 * 4);
    assert_eq!(s.icon_frames.len(), 3);
    assert!(s.backup_equal && s.header_magic == lt_core::save::HEADER_MAGIC && s.header_version == 0x100);
    assert_eq!(s.title_fw, "ＧＣ　　１　Ｋｎｉｇｈｔ　　Ｓａｎ　Ｃｌａｒｉａ　　　００：０４");

    // Writing back the fields as stored reproduces the card byte for byte
    // (title and body sum are regenerated the way OVR05 does).
    use lt_core::save::{write_save, EditItem, SaveEdit};
    let sb = &s.slot[0x2C..];
    let w = |o: usize| u32::from_le_bytes(sb[o..o + 4].try_into().unwrap());
    let edit = SaveEdit {
        knight_name: s.party[0].name.clone(),
        baby_name: s.party[1].name.clone(),
        third_name: s.third_name.clone(),
        rubies: s.rubies,
        play_frames: s.play_frames,
        steps: s.steps,
        map: s.map,
        x: s.position.0,
        h: s.position.1,
        z: s.position.2,
        save_point: sb[0xF7],
        flags: (0..16).map(|i| w(4 * i)).collect(),
        toys: (0..3).map(|i| w(0x40 + 4 * i)).collect(),
        items: s.items.iter().map(|i| EditItem { equipped: i.a != 0, id: i.id }).collect(),
    };
    let mut same = bytes.clone();
    write_save(&mut same, s.block, Some(&edit)).unwrap();
    assert!(same == bytes, "round trip changed the card");

    // The editor's example edit: 50,000 rubies → body sum 0x0DF2C7EA.
    let mut rich = bytes.clone();
    write_save(&mut rich, s.block, Some(&SaveEdit { rubies: 50_000, ..edit })).unwrap();
    let saved = &lt_core::save::parse_card(&rich).unwrap().saves[0];
    assert_eq!((saved.rubies, saved.body_sum), (50_000, 0x0DF2_C7EA));
    assert_eq!(lt_core::save::to_mcs(&rich, 1).len(), 128 + 8192);
}
