//! STR movies against the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test movies`.
//! `LT_MOVIE_OUT=<dir>` also writes sample frames as PNG and movie/8's audio as WAV.

use std::time::Instant;

use lt_core::movie::{self, Movie};
use lt_core::{DiscImage, FileTable, Kind, PsxExe};

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

/// (movie index, frames, audio sectors) for the 11 distinct extents.
const EXPECTED: [(u16, u32, u32); 11] = [
    (0, 444, 526),
    (1, 1612, 2046),
    (2, 508, 582),
    (3, 225, 215),
    (4, 645, 802),
    (5, 2176, 2720),
    (6, 1217, 1506),
    (7, 615, 812),
    (8, 215, 189),
    (9, 223, 279),
    (12, 1605, 2256),
];

fn scan(d: &Disc, index: u16) -> Movie {
    let e = d.table.of_kind(Kind::Movie).find(|e| e.index == index).unwrap();
    movie::scan(&d.image, e.extent.lba, e.extent.sectors).unwrap()
}

#[test]
fn movie_table_has_eleven_extents() {
    let Some(d) = disc() else { return };
    let movies: Vec<_> = d.table.of_kind(Kind::Movie).collect();
    assert_eq!(movies.len(), 14);
    let mut extents: Vec<_> = movies.iter().map(|e| e.extent).collect();
    extents.sort();
    extents.dedup();
    assert_eq!(extents.len(), 11);
    let alias = |a: u16, b: u16| movies[a as usize].extent == movies[b as usize].extent;
    assert!(alias(9, 10) && alias(9, 11) && alias(12, 13));
}

#[test]
fn every_movie_scans_with_the_expected_layout() {
    let Some(d) = disc() else { return };
    for (index, frames, audio_sectors) in EXPECTED {
        let t = Instant::now();
        let m = scan(&d, index);
        let i = &m.info;
        eprintln!(
            "movie/{index}: {} frames {}x{} {:.2} fps v{:?} q{}..{} chunks {}..{} audio {:?} ({:?})",
            i.frames,
            i.width,
            i.height,
            i.fps,
            i.versions,
            i.qscale_min,
            i.qscale_max,
            i.chunks_min,
            i.chunks_max,
            i.audio.as_ref().map(|a| (a.rate, a.channels, a.bits, a.stride, a.seconds, a.offset)),
            t.elapsed()
        );
        assert_eq!(i.frames, frames, "movie/{index} frame count");
        assert_eq!((i.first_frame, i.last_frame), (1, frames), "movie/{index} frame numbers");
        assert_eq!((i.width, i.height), (320, 224), "movie/{index} size");
        assert_eq!(i.versions, vec![2], "movie/{index} bitstream version");
        assert_eq!(i.incomplete_frames, 0);
        assert_eq!(i.other_sectors, 0);
        assert_eq!(i.sectors_per_frame, 10.0);
        assert_eq!((i.speed, i.speed_assumed), (2, false));
        assert_eq!(i.fps, 15.0);
        let a = i.audio.as_ref().expect("every movie has XA audio");
        assert_eq!((a.rate, a.channels, a.bits, a.stride), (37_800, 2, 4, 8));
        assert_eq!(a.sectors, audio_sectors, "movie/{index} audio sectors");
        assert_eq!(a.other_sectors, 0);
        assert_eq!(i.video_sectors + i.audio_sectors + i.empty_sectors, i.sectors);
    }
}

#[test]
fn every_frame_of_every_movie_decodes() {
    let Some(d) = disc() else { return };
    let out = std::env::var_os("LT_MOVIE_OUT").map(std::path::PathBuf::from);
    let mut total = 0u32;
    let t = Instant::now();
    for (index, frames, _) in EXPECTED {
        let m = scan(&d, index);
        let mut slowest = std::time::Duration::ZERO;
        for n in 0..m.info.frames {
            let ft = Instant::now();
            let (rgba, stats) = m.decode_frame(&d.image, n).unwrap_or_else(|e| panic!("movie/{index} frame {n}: {e}"));
            slowest = slowest.max(ft.elapsed());
            assert_eq!(rgba.len(), 320 * 224 * 4);
            assert_eq!(stats.version, 2);
            assert_eq!(stats.macroblocks, 20 * 14);
            // The bitstream fits the bytes the header declares.
            assert!(stats.bits <= (m.frames[n as usize].bytes as usize - 8) * 8, "movie/{index} frame {n} overruns");
            if let Some(dir) = &out {
                if n == frames / 2 || n == frames / 4 || (index == 8 && n == 60) {
                    std::fs::create_dir_all(dir).unwrap();
                    let png = movie::png(320, 224, &rgba);
                    std::fs::write(dir.join(format!("movie{index}_f{n}.png")), png).unwrap();
                }
            }
        }
        total += m.info.frames;
        eprintln!("movie/{index}: {} frames, slowest {slowest:?}", m.info.frames);
    }
    eprintln!("{total} frames in {:?}", t.elapsed());
    assert_eq!(total, EXPECTED.iter().map(|e| e.1).sum::<u32>());
}

/// A mid-movie frame is a real picture: varied, not noise, not flat.
#[test]
fn frames_look_like_pictures() {
    let Some(d) = disc() else { return };
    for (index, frames, _) in EXPECTED {
        let m = scan(&d, index);
        let (rgba, _) = m.decode_frame(&d.image, frames / 2).unwrap();
        let luma: Vec<i32> = rgba.chunks(4).map(|p| (p[0] as i32 * 3 + p[1] as i32 * 6 + p[2] as i32) / 10).collect();
        let mean = luma.iter().sum::<i32>() / luma.len() as i32;
        // Neighbouring pixels are close in a picture and unrelated in noise.
        let mut diff = 0i64;
        for y in 0..224 {
            for x in 1..320 {
                diff += (luma[y * 320 + x] - luma[y * 320 + x - 1]).abs() as i64;
            }
        }
        let avg_step = diff as f64 / (224.0 * 319.0);
        eprintln!("movie/{index} frame {}: mean luma {mean}, mean neighbour step {avg_step:.2}", frames / 2);
        assert!(avg_step < 12.0, "movie/{index} looks like noise ({avg_step:.1})");
    }
}

#[test]
fn audio_decodes_with_plausible_levels() {
    let Some(d) = disc() else { return };
    let out = std::env::var_os("LT_MOVIE_OUT").map(std::path::PathBuf::from);
    for (index, _, _) in EXPECTED {
        let m = scan(&d, index);
        let t = Instant::now();
        let pcm = m.decode_audio(&d.image).unwrap();
        let a = m.info.audio.as_ref().unwrap();
        assert_eq!((pcm.rate, pcm.channels), (37_800, 2));
        assert_eq!(pcm.frames() as u64, a.frames);
        let peak = pcm.samples.iter().map(|&s| (s as i32).abs()).max().unwrap();
        let rms = (pcm.samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / pcm.samples.len() as f64).sqrt();
        let clipped = pcm.samples.iter().filter(|&&s| s == i16::MAX || s == i16::MIN).count();
        eprintln!(
            "movie/{index}: {:.1} s audio, peak {peak}, rms {rms:.0}, clipped {clipped} ({:?})",
            a.seconds,
            t.elapsed()
        );
        assert!(peak > 2000, "movie/{index} audio is near-silent");
        assert!(rms > 100.0 && rms < 12_000.0, "movie/{index} rms {rms}");
        assert!((clipped as f64) < pcm.samples.len() as f64 * 0.001, "movie/{index} clips");
        if let (Some(dir), 8) = (&out, index) {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("movie8.wav"), movie::wav(&pcm)).unwrap();
        }
    }
}

#[test]
fn player_settings_from_ovr01() {
    let Some(d) = disc() else { return };
    let e = d.table.by_id("ovr/1").unwrap();
    let ovr = d.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
    let s = movie::player_settings(&ovr);
    assert_eq!(s.len(), 14);
    assert!(s.iter().all(|r| r.rgb24 && r.width == 320 && r.height == 224));
    assert_eq!(s[8].last_frame, 144);
    assert_eq!(s[12].last_frame, 1605);
}

/// Debug builds decode a frame in tens of ms; this keeps an eye on it.
#[test]
fn decode_speed() {
    let Some(d) = disc() else { return };
    let m = scan(&d, 5);
    let t = Instant::now();
    for n in 1000..1060 {
        m.decode_frame(&d.image, n).unwrap();
    }
    let per = t.elapsed() / 60;
    eprintln!("movie/5: {per:?} per frame");
    assert!(per.as_millis() < 1000);
}
