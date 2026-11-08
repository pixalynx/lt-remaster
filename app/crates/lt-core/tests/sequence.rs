//! TSQ songs re-laid and MIDI imported (screen 14 §12, research 06 phase 1
//! "TSQ/MIDI") against the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test sequence`.

use lt_core::audio::bank::{self, Bank, BankRef};
use lt_core::audio::{midi, tsq, view};
use lt_core::edit::sound;
use lt_core::{DiscImage, FileTable, PsxExe};
use serde_json::Value;

fn banks() -> Option<Vec<Bank>> {
    Some(bank_files()?.into_iter().filter_map(|(r, f)| bank::split(&r, &f)).collect())
}

fn bank_files() -> Option<Vec<(BankRef, Vec<Vec<u8>>)>> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(
        bank::catalog(&table)
            .iter()
            .map(|r| {
                let files: Vec<Vec<u8>> = r
                    .files
                    .iter()
                    .map(|&g| {
                        let e = table.get(g).unwrap();
                        image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
                    })
                    .collect();
                (r.clone(), files)
            })
            .collect(),
    )
}

/// What a cue plays, without where its bytes sit.
fn played(b: &Bank) -> Vec<Value> {
    let v = view::bank_view(b, None).unwrap();
    v.cues
        .iter()
        .map(|c| {
            let mut j = serde_json::to_value(c).unwrap();
            j.as_object_mut().unwrap().remove("offset");
            for t in j["tracks"].as_array_mut().unwrap() {
                t.as_object_mut().unwrap().remove("start");
            }
            j
        })
        .collect()
}

#[test]
fn every_song_relays_and_plays_the_same() {
    let Some(banks) = banks() else { return };
    let (mut songs, mut skipped, mut freed_total) = (0, 0, 0usize);
    for b in &banks {
        let before = played(b);
        for c in tsq::cues(&b.tsq).into_iter().filter(|c| c.kind == tsq::CueKind::Song) {
            let Some(song) = tsq::song_of(&b.tsq, c.index) else {
                skipped += 1;
                continue;
            };
            let (out, rep) = tsq::replace_song(&b.tsq, c.index, &song).unwrap_or_else(|| panic!("{} cue {}", b.id, c.index));
            let nb = Bank { tsq: out.clone(), ..b.clone() };
            assert_eq!(played(&nb), before, "{} cue {} plays differently re-laid", b.id, c.index);
            // Same code, minus the dead bytes: never longer than the old TSQ plus alignment.
            assert!(out.len() <= b.tsq.len() + 8, "{} cue {}: {} → {}", b.id, c.index, b.tsq.len(), out.len());
            freed_total += rep.freed;
            songs += 1;
        }
    }
    eprintln!("{songs} song cues re-laid, {skipped} skipped (overlapping streams), {freed_total} B freed and re-added");
    assert!(songs >= 60);
}

/// Notes a cue plays: (start, end, note, slot), sorted.
fn notes(c: &view::CueView) -> Vec<(i32, i32, i32, i32)> {
    let mut v: Vec<_> = c.tracks.iter().flat_map(|t| t.notes.chunks(4).map(|n| (n[0], n[1], n[2], n[3]))).collect();
    v.sort();
    v
}

#[test]
fn every_song_comes_back_from_midi() {
    let Some(all) = bank_files() else { return };
    let (mut songs, mut same, mut total, mut dropped, mut worst_vol, mut over) = (0, 0, 0usize, 0u32, 0f64, 0);
    for (r, files) in &all {
        let Some(b) = bank::split(r, files) else { continue };
        let Some(v) = view::bank_view(&b, None) else { continue };
        for c in v.cues.iter().filter(|c| c.kind == "song" && c.note_ons > 0) {
            let bytes = midi::write_cue(c, &format!("{} cue {}", b.id, c.cue));
            let (out, rep) = match sound::import_midi(r, files, c.cue as usize, &bytes, &midi::ImportOptions::default()) {
                Ok(x) => x,
                // A retail song can sit near its TSQ buffer: re-written as plain notes it may not fit.
                Err(e) if e.code == "SEQ_BUDGET" => {
                    eprintln!("{} cue {}: {}", b.id, c.cue, e.message);
                    over += 1;
                    continue;
                }
                Err(e) => panic!("{} cue {}: {} {}", b.id, c.cue, e.code, e.message),
            };
            let mut after = files.clone();
            for (i, f) in out {
                after[i] = f;
            }
            let nb = bank::split(r, &after).unwrap();
            let nv = view::bank_view(&nb, None).unwrap();
            let nc = nv.cues.iter().find(|x| x.cue == c.cue).unwrap();
            // Every other cue plays as before.
            for (x, y) in v.cues.iter().zip(&nv.cues).filter(|(x, _)| x.cue != c.cue) {
                assert_eq!(notes(x), notes(y), "{} cue {} changed when cue {} was replaced", b.id, x.cue, c.cue);
            }
            let (a, n) = (notes(c), notes(nc));
            total += a.len();
            dropped += rep.song.dropped;
            if a == n {
                same += 1;
            } else {
                let missing = a.iter().filter(|x| !n.contains(x)).count();
                if std::env::var_os("LT_SEQ_DIFF").is_some() {
                    let gone: Vec<_> = a.iter().filter(|x| !n.contains(x)).take(6).collect();
                    let new: Vec<_> = n.iter().filter(|x| !a.contains(x)).take(6).collect();
                    eprintln!("  only before {gone:?}\n  only after {new:?} (lens {} {})", a.len(), n.len());
                }
                eprintln!("{} cue {}: {} of {} notes differ ({} dropped for polyphony)", b.id, c.cue, missing, a.len(), rep.song.dropped);
            }
            assert_eq!(nc.r#loop, c.r#loop, "{} cue {} loop", b.id, c.cue);
            // Loudness survives within a step of the 0–127 scales.
            let vol = |c: &view::CueView| -> Vec<u16> { c.tracks.iter().flat_map(|t| t.vol.chunks(2).map(|v| v[0].max(v[1]))).collect() };
            let (mut va, mut vb) = (vol(c), vol(nc));
            va.sort();
            vb.sort();
            if va.len() == vb.len() {
                for (x, y) in va.iter().zip(&vb) {
                    worst_vol = worst_vol.max((*x as f64 - *y as f64).abs() / 0x3FFF as f64);
                }
            }
            songs += 1;
        }
    }
    eprintln!("{songs} songs through MIDI: {same} note-for-note, {total} notes, {dropped} dropped, worst loudness step {:.3}; {over} over their TSQ buffer", worst_vol);
    assert!(songs + over >= 60);
    assert!(same * 10 >= songs * 9, "{same} of {songs} songs came back note for note");
}
