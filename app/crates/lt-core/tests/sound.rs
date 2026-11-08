//! Sample replacement (`edit::sound`) on every sound bank the disc holds:
//! each bank's first samples go out as WAV and come back into their slots
//! (loops kept, the rest of the bank byte for byte, budgets held), and a
//! 44.1 kHz WAV replaces a toy's sample as a one-shot.
//! Opt in with `LT_TEST_DISC=… cargo test --release -p lt-core --test sound`.

use lt_core::audio::{adpcm, bank, tvb::Tvb, view};
use lt_core::edit::sound::{self, Looping, ReplaceOptions};
use lt_core::{DiscImage, FileTable, PsxExe};

#[test]
fn every_bank_takes_its_samples_back_from_wav() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let (mut banks, mut samples, mut looped, mut worst) = (0, 0, 0, f32::MAX);
    for r in bank::catalog(&table) {
        let files: Vec<Vec<u8>> = r.files.iter().map(|&g| {
            let e = table.get(g).unwrap();
            image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
        }).collect();
        let Some(b) = bank::split(&r, &files) else { continue };
        let Some(t) = Tvb::parse(&b.tvb) else { continue };
        let firsts: Vec<usize> = t.spans().into_iter().flatten().filter(|s| s.first == s.slot).map(|s| s.slot).take(3).collect();
        for slot in firsts {
            let old = adpcm::decode(t.slot_bytes(slot).unwrap());
            if old.pcm.len() <= 28 || old.peak == 0 {
                continue;
            }
            // Out as the exporter writes it (without the silent lead block), back in at the same rate.
            let wav = view::wav_mono16(&old.pcm[28..], 22_050);
            let (out, rep) = match sound::replace_sample(&r, &files, slot, &wav, &ReplaceOptions { rate: Some(22_050), looping: Looping::Keep, gain: None }) {
                Ok(x) => x,
                Err(e) if e.code == "SAMPLE_BUDGET" => continue,
                Err(e) => panic!("{} slot {slot}: {e}", r.id),
            };
            let mut after = files.clone();
            for (i, f) in &out {
                after[*i] = f.clone();
            }
            let nb = bank::split(&r, &after).unwrap();
            let nt = Tvb::parse(&nb.tvb).unwrap();
            let new = adpcm::decode(nt.slot_bytes(slot).unwrap());
            assert_eq!(new.looped(), old.looped(), "{} slot {slot}: looping", r.id);
            if old.looped() {
                assert_eq!(new.loop_start, old.loop_start.map(|l| l / 28 * 28), "{} slot {slot}: loop start", r.id);
                looped += 1;
            }
            let n = old.pcm.len().min(new.pcm.len());
            let snr = adpcm::snr_db(&old.pcm[..n], &new.pcm[..n]) as f32;
            assert!(snr > 15.0, "{} slot {slot}: {snr:.1} dB", r.id);
            worst = worst.min(snr);
            assert!(rep.bytes_after <= rep.bytes_before + 32, "{} slot {slot}: {} → {}", r.id, rep.bytes_before, rep.bytes_after);
            samples += 1;
        }
        banks += 1;
    }
    eprintln!("{banks} banks, {samples} samples ({looped} looped), worst {worst:.1} dB");
    assert!(banks > 200 && samples > 500);
}

#[test]
fn a_new_wav_replaces_a_toy_sample_as_a_one_shot() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let cat = bank::catalog(&table);
    let r = bank::find(&cat, "toy/22").unwrap();
    let e = table.get(r.files[0]).unwrap();
    let files = vec![image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()];
    let b0 = bank::split(r, &files).unwrap();
    let t = Tvb::parse(&b0.tvb).unwrap();
    let slot = t.spans().into_iter().flatten().next().unwrap().slot;
    // 0.25 s of a 660 Hz tone at 44.1 kHz, stereo 16-bit.
    let n = 11_025;
    let mut wav = Vec::new();
    let data: Vec<u8> = (0..n).flat_map(|i| {
        let v = ((i as f32 * 2.0 * std::f32::consts::PI * 660.0 / 44_100.0).sin() * 12_000.0) as i16;
        [v.to_le_bytes(), v.to_le_bytes()].concat()
    }).collect();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&[1, 0, 2, 0]);
    wav.extend_from_slice(&44_100u32.to_le_bytes());
    wav.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    wav.extend_from_slice(&[4, 0, 16, 0]);
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    let (out, rep) = sound::replace_sample(r, &files, slot, &wav, &ReplaceOptions { rate: None, looping: Looping::Off, gain: None }).unwrap();
    assert_eq!(rep.rate, 22_050);
    assert!((rep.seconds - 0.25).abs() < 0.01, "{}", rep.seconds);
    assert!(rep.loop_start.is_none());
    assert!(rep.snr_db > 25.0, "{}", rep.snr_db);
    let b1 = bank::split(r, &[out[0].1.clone()]).unwrap();
    let nt = Tvb::parse(&b1.tvb).unwrap();
    let flags: Vec<u8> = nt.slot_bytes(slot).unwrap().chunks(16).map(|b| b[1]).collect();
    assert_eq!((flags[0], flags[1]), (0x00, 0x04));
    assert_eq!(&flags[flags.len() - 2..], &[0x01, 0x07]);
    // The toy still parses as a toy with its model.
    assert!(lt_core::mesh::model_parts(lt_core::mesh::Container::Toy, &out[0].1).is_some());
}
