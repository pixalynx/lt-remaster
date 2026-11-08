//! Sound banks against the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test audio`.

use std::time::Instant;

use lt_core::audio::{self, tsq, tvb::Tvb, BankKind};
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
    fn bank(&self, id: &str) -> audio::Bank {
        let cat = audio::catalog(&self.table);
        let r = audio::bank::find(&cat, id).unwrap_or_else(|| panic!("{id} not in the catalogue"));
        let files: Vec<Vec<u8>> = r
            .files
            .iter()
            .map(|&g| {
                let e = self.table.get(g).unwrap();
                self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
            })
            .collect();
        audio::split(r, &files).unwrap_or_else(|| panic!("{id} does not split"))
    }
}

#[test]
fn catalogue_covers_every_bank() {
    let Some(d) = disc() else { return };
    let t0 = Instant::now();
    let cat = audio::catalog(&d.table);
    assert_eq!(cat.len(), 61 + 1 + 1 + 9 + 73 + 110 + 19);
    let mut rows = Vec::new();
    for r in &cat {
        let b = d.bank(&r.id);
        let row = audio::row(&b);
        assert!(row.used_slots > 0, "{} has no samples", r.id);
        assert!(Tvb::parse(&b.tvb).is_some(), "{}", r.id);
        rows.push(row);
    }
    eprintln!("catalogue: {} banks in {:?}", rows.len(), t0.elapsed());

    let get = |id: &str| rows.iter().find(|r| r.id == id).unwrap();
    let pct = |id: &str| (get(id).adpcm_bytes as f64 / get(id).spu_cap as f64 * 100.0).round() as u32;
    // bgm/0 (BGM01): 6 songs, 98% of the area region.
    let b0 = get("bgm/0");
    assert_eq!((b0.file_bytes, b0.tsq_bytes, b0.adpcm_bytes, b0.songs, b0.sfx), (124_356, 10_124, 112_176, 6, 0));
    assert_eq!(pct("bgm/0"), 98);
    assert_eq!(pct("bgm/2"), 99);
    assert_eq!(get("bgm/57").file_bytes, 200_836);
    assert_eq!(get("bgm/57").file_bytes / 1024, 196);
    assert_eq!(get("bgm/57").sound_slot, 1);
    // The mock's BGM header table (s1314/bgm_banks.json).
    assert_eq!((get("bgm/47").songs, get("bgm/47").sfx), (0, 3));
    assert_eq!((get("bgm/59").songs, get("bgm/59").sfx), (1, 2));
    // SE: 45 used slots; SE2: 28; Knoygn 4 slots over 3 samples, 12,128 B of ADPCM.
    assert_eq!(get("sys/0").used_slots, 45);
    assert_eq!(get("sys/8").used_slots, 28);
    assert_eq!(get("sys/8").adpcm_bytes, 116_384);
    let k = get("enemy/17");
    assert_eq!((k.used_slots, k.offsets, k.adpcm_bytes), (4, 3, 12_128));
    assert_eq!(k.name.as_deref(), Some("Knoygn"));
    assert_eq!(rows.iter().filter(|r| r.kind == BankKind::Toy).count(), 73);
}

#[test]
fn bgm0_matches_the_research() {
    let Some(d) = disc() else { return };
    let pitch = audio::pitch_table(&d.exe).unwrap();
    assert_eq!(pitch[48], 0x1000);
    assert_eq!(pitch[72], 0x3FFF);

    let b = d.bank("bgm/0");
    assert_eq!((b.tsq_offset, b.tvb_offset), (8, 0x2794));
    let t0 = Instant::now();
    let v = audio::bank_view(&b, Some(&pitch)).unwrap();
    eprintln!("bgm/0 view in {:?}, {} B json", t0.elapsed(), serde_json::to_string(&v).unwrap().len());
    assert_eq!(v.cues.iter().map(|c| c.cue).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 7]);
    assert_eq!(v.empty_cues, 44);
    // The common bank: loaded into slot 1 at boot (request 0xFF), not the area slot.
    assert_eq!(v.placement.sound_slot, 1);

    // Cue 1: 24 tracks, 1,034 note-ons, 533 note-offs, 40 E2 (BGM_SEQUENCE_Format.md).
    let c1 = &v.cues[0];
    assert_eq!(c1.tracks.len(), 24);
    assert_eq!((c1.note_ons, c1.note_offs, c1.programs), (1_034, 533, 40));
    assert_eq!((c1.ticks, c1.r#loop, c1.active), (2_450, true, 22));
    eprintln!("cue 1 loops to tick {:?}", c1.loop_tick);
    let lens: Vec<(u16, u32, bool, u16)> = v.cues.iter().map(|c| (c.cue, c.ticks, c.r#loop, c.active)).collect();
    assert_eq!(lens, vec![(1, 2450, true, 22), (2, 336, false, 19), (3, 212, false, 8), (4, 2522, true, 15), (5, 325, false, 16), (7, 57, false, 4)]);

    // Slot 24: 0x0D0E0, 3,024 B, looped, shared with slot 31, 215 notes in
    // cue 1 on tracks 11–15.
    let s24 = v.samples[24].as_ref().unwrap();
    assert_eq!((s24.offset, s24.bytes, s24.pcm, s24.looped), (0xD0E0, 3_024, 5_292, true));
    assert_eq!((s24.loop_start, s24.loop_end), (Some(4_788), Some(5_292)));
    assert_eq!(s24.shares, vec![31]);
    assert_eq!(s24.adsr, "CE8BA2FF");
    let mut notes = 0;
    let mut tracks = Vec::new();
    for (k, t) in c1.tracks.iter().enumerate() {
        let n = t.notes.chunks(4).filter(|q| q[3] == 24).count();
        if n > 0 {
            tracks.push(k);
        }
        notes += n;
    }
    assert_eq!(notes, 215);
    assert_eq!(tracks, vec![11, 12, 13, 14, 15]);
    // Request word for the common bank's main song (the overworld theme).
    assert_eq!(0x4040_0000u32 | (v.placement.sound_slot as u32) << 8 | 1, 0x4040_0101);

    // 51 used slots in bgm/0; slots 2–4 alias one offset.
    assert_eq!(v.row.used_slots, 51);
    let s3 = v.samples[3].as_ref().unwrap();
    assert!(!s3.first || !s3.shares.is_empty());
}

#[test]
fn every_cue_disassembles_cleanly() {
    let Some(d) = disc() else { return };
    let cat = audio::catalog(&d.table);
    let pitch = audio::pitch_table(&d.exe).unwrap();
    let t0 = Instant::now();
    let (mut cues, mut spawns, mut stalls, mut truncated, mut silent, mut shifts) = (0, 0, 0, 0, 0, 0);
    let mut slowest = (std::time::Duration::ZERO, String::new());
    for r in &cat {
        let b = d.bank(&r.id);
        let t = Instant::now();
        let v = audio::bank_view(&b, Some(&pitch)).unwrap_or_else(|| panic!("{} has no view", r.id));
        if t.elapsed() > slowest.0 {
            slowest = (t.elapsed(), r.id.clone());
        }
        silent += v.silent;
        for c in &v.cues {
            cues += 1;
            spawns += c.spawns;
            for tr in &c.tracks {
                match tr.stop {
                    "stall" => stalls += 1,
                    "truncated" => truncated += 1,
                    _ => {}
                }
            }
        }
        let tv = Tvb::parse(&b.tvb).unwrap();
        shifts += tv.adpcm.as_chunks::<16>().0.iter().filter(|blk| blk[0] & 0xF > 12).count();
        // Unknown opcodes would surface as End(b) with b outside the end set.
        for c in tsq::cues(&b.tsq) {
            assert!(c.offset as usize >= tsq::HEADER || c.offset == 0, "{} cue {} points into the header", r.id, c.index);
        }
    }
    eprintln!(
        "{} cues, {spawns} F9 spawns, {stalls} stalls, {truncated} truncated, {silent} silent slots, {shifts} blocks with shift > 12; all views in {:?}, slowest {} {:?}",
        cues,
        t0.elapsed(),
        slowest.1,
        slowest.0
    );
    assert_eq!(truncated, 0);
}

#[test]
fn slot_pcm_is_binary_with_a_header() {
    let Some(d) = disc() else { return };
    let b = d.bank("bgm/0");
    let tv = Tvb::parse(&b.tvb).unwrap();
    let bytes = audio::view::slot_pcm_bytes(&tv, 24).unwrap();
    let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let loop_start = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
    assert_eq!((len, loop_start), (5_292, 4_788));
    assert_eq!(bytes.len(), 16 + 5_292 * 2);
}
