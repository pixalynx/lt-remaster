//! Streamed music on the retail disc (`LT_TEST_DISC`): slimming every area and battle bank around
//! its streamed song keeps every other cue's samples byte for byte and leaves the stream a ring.

use std::path::PathBuf;

use lt_core::audio::{stream, tsq, tvb};
use lt_core::audio::BankRef;
use lt_core::edit::{music, sound};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn tvb_of(file: &[u8]) -> tvb::Tvb<'_> {
    let at = u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize;
    tvb::Tvb::parse(&file[at..]).unwrap()
}

fn tsq_of(file: &[u8]) -> &[u8] {
    &file[8..u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize]
}

#[test]
fn slimmed_banks_keep_the_other_cues_and_leave_a_ring() {
    let Some(path) = std::env::var_os("LT_TEST_DISC").map(PathBuf::from) else { return };
    let image = DiscImage::open(&path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut slimmed = 0;
    for n in 1..music::BGM_FILES {
        if n == music::FLIGHT_BGM {
            continue;
        }
        let file = w.file(&format!("bgm/{n}")).unwrap();
        let songs = music::songs(&file);
        let Some(&cue) = songs.first() else { continue };
        assert!(music::check_song(n, cue, &file).is_ok());
        let (bank, ring, flags) = music::bank_for(n, &file, &[cue]).unwrap();
        let bank = bank.unwrap();
        assert_eq!(flags, 0);
        assert!(ring.frames as u32 >= stream::MIN_FRAMES, "bgm/{n}: {ring:?}");
        assert!(ring.offset + ring.frames as u32 * 0x800 <= stream::BANK_BYTES);
        let (old, new) = (tvb_of(&file), tvb_of(&bank));
        assert!(new.size <= ring.offset, "bgm/{n}: samples run into the ring");
        let seq = tsq_of(&bank);
        assert!(seq.len() <= stream::TSQ_BUFFER - ring.bounce as usize * 0x800, "bgm/{n}: the TSQ runs into the bounce buffer");
        // The streamed cue is gone; every other cue still finds each sample it programs, unchanged.
        assert!(tsq::cues(seq).iter().all(|c| c.index != cue as usize));
        for c in tsq::cues(seq) {
            for p in tsq::programs(seq, c.index) {
                let p = p as usize;
                if p >= tvb::SLOTS || !old.used(p) {
                    continue;
                }
                assert_eq!(new.slot_bytes(p), old.slot_bytes(p), "bgm/{n} cue {} slot {p}", c.index);
            }
        }
        slimmed += 1;
    }
    assert!(slimmed >= 55, "{slimmed}");
    // The common bank keeps its file and borrows bank 2's region.
    let zero = w.file("bgm/0").unwrap();
    let (slim0, ring0, flags0) = music::bank_for(0, &zero, &[1]).unwrap();
    assert_eq!((ring0, flags0), (stream::Ring::BANK2, music::RING2));
    let slim0 = slim0.unwrap();
    // Streaming the overworld theme frees the samples only it played (room for sounds every area has).
    assert!(tvb_of(&slim0).size < tvb_of(&zero).size, "{} vs {}", tvb_of(&slim0).size, tvb_of(&zero).size);
    assert!(music::check_song(music::FLIGHT_BGM, 1, &w.file("bgm/57").unwrap()).is_err());
}

/// A short test sound: a 440 Hz blip with a fade, as a 16-bit mono WAV.
fn blip(seconds: f64) -> Vec<u8> {
    let rate = 22_050u32;
    let n = (seconds * rate as f64) as usize;
    let pcm: Vec<i16> = (0..n).map(|i| ((i as f64 * 2.0 * std::f64::consts::PI * 440.0 / rate as f64).sin() * 20_000.0 * (1.0 - i as f64 / n as f64)) as i16).collect();
    lt_core::audio::view::wav_mono16(&pcm, rate)
}

fn bank(id: &str, kind: lt_core::audio::BankKind, index: u16) -> BankRef {
    BankRef { id: id.into(), kind, index, files: Vec::new() }
}

#[test]
fn a_sound_goes_into_a_bank_with_room_and_comes_out_again() {
    let Some(path) = std::env::var_os("LT_TEST_DISC").map(PathBuf::from) else { return };
    let image = DiscImage::open(&path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    // bgm/5 (an area bank) keeps 30 KB of its 112 KB.
    let r = bank("bgm/5", lt_core::audio::BankKind::Music, 5);
    let file = vec![w.file("bgm/5").unwrap()];
    let (out, rep) = sound::add_sound(&r, &file, &blip(0.6), Some("wav"), &sound::SoundOptions::default()).unwrap();
    assert_eq!((rep.sound_slot, rep.request), (2, 0x200 | rep.cue as u16));
    assert!(rep.cue >= 43 && rep.snr_db > 20.0, "{rep:?}");
    let after = vec![out[0].1.clone()];
    assert_eq!(sound::added_cues(&r, &after, &file), vec![rep.cue]);
    // Every retail cue still plays its own samples.
    let (old, new) = (tvb_of(&file[0]), tvb_of(&after[0]));
    for c in tsq::cues(tsq_of(&file[0])) {
        for p in tsq::programs(tsq_of(&file[0]), c.index) {
            assert_eq!(new.slot_bytes(p as usize), old.slot_bytes(p as usize));
        }
    }
    // Carried across a rebuild from the base, it lands where it was.
    let carried = sound::carry(&r, &after, rep.cue).unwrap();
    let back = sound::put_back(&r, &file, std::slice::from_ref(&carried)).unwrap();
    assert_eq!(sound::carry(&r, &[back[0].1.clone()], rep.cue), Some(carried));
    // Taken out, the bank is the retail one again, byte for byte where it matters.
    let gone = sound::remove_sound(&r, &after, rep.cue).unwrap();
    let g = &gone[0].1;
    assert_eq!(tsq_of(g), &tsq_of(&file[0])[..tsq_of(g).len()]);
    assert_eq!(tvb_of(g).size, old.size);
    // The system bank is full: a long sound is refused with the room it has.
    let sys = bank("sys/0", lt_core::audio::BankKind::SystemSe, 0);
    let se = vec![w.file("sys/0").unwrap(), w.file("sys/1").unwrap()];
    let e = sound::add_sound(&sys, &se, &blip(2.0), Some("wav"), &sound::SoundOptions::default()).unwrap_err();
    assert_eq!(e.code, "SOUND_BUDGET", "{e:?}");
    // A loop until stopped: its stream waits forever.
    let opt = sound::SoundOptions { looping: true, ..Default::default() };
    let (_, l) = sound::add_sound(&r, &file, &blip(0.4), Some("wav"), &opt).unwrap();
    assert!(l.looping);
}
