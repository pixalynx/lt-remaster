//! Movies written (screen 14 §13, research 06 phase 1 "FMV Form 2") against
//! the user's own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test movie_write`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::cdrom::{self, PAYLOAD};
use lt_core::edit::movie::{self as movie_edit, MovieOptions};
use lt_core::movie::{self, encode, mdec, Payload};
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, exe, table })
}

fn scan(d: &Disc, id: &str) -> movie::Movie {
    let e = d.table.by_id(id).unwrap();
    movie::scan(&d.image, e.extent.lba, e.extent.sectors).unwrap()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as f64 - *y as f64).powi(2)).sum::<f64>() / (a.len() * 3 / 4) as f64;
    10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
}

fn out_dir(name: &str) -> std::path::PathBuf {
    let base = std::env::var_os("LT_TEST_OUT").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("lt-movie-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Plans, writes and verifies `files`; returns the image path and the plan.
fn build(d: &Disc, files: &[Staged], dir: &std::path::Path) -> (std::path::PathBuf, build::Plan) {
    let src = Source { image: &d.image, exe: &d.exe, table: &d.table };
    let plan = build::plan(&src, files, |_| true, PlanOptions { postgap: false, ..Default::default() });
    assert!(!plan.blocked_included, "{:?}", plan.items.iter().map(|i| (&i.id, &i.refusal)).collect::<Vec<_>>());
    let out = dir.join("out.bin");
    let cancel = AtomicBool::new(false);
    let rep = build::write_image(&src, &plan, files, &out, &cancel, &mut |_| {}).unwrap();
    build::verify_image(&out, &src, &plan, files, &rep, &cancel, &mut |_| {}).unwrap();
    (out, plan)
}

fn staged(id: &str, bytes: Vec<u8>) -> Staged {
    Staged { id: id.into(), bytes: Arc::new(bytes), base_sha256: None }
}

#[test]
fn retail_frames_reencode_in_their_own_sectors() {
    let Some(d) = disc() else { return };
    let (mut worst, mut frames, mut most) = (f64::MAX, 0, 0u16);
    for id in ["movie/8", "movie/0"] {
        let m = scan(&d, id);
        for n in (0..m.frames.len() as u32).step_by(23) {
            let data = m.demux(&d.image, n).unwrap();
            let f = &m.frames[n as usize];
            let mut rgba = vec![0u8; f.width as usize * f.height as usize * 4];
            let st = mdec::decode(&data, f.width, f.height, &mut rgba).unwrap();
            // The header's code count is the decoder's count rounded up to 64, halved.
            assert_eq!(u16::from_le_bytes([data[0], data[1]]) as usize, st.codes.div_ceil(64) * 64 / 2, "{id} frame {n}");
            most = most.max(u16::from_le_bytes([data[0], data[1]]));
            let e = mdec::encode_fit(&rgba, f.width, f.height, f.chunks.len() * 2016).unwrap();
            assert!(e.codes_word <= mdec::MAX_CODES_WORD);
            let mut back = vec![0u8; rgba.len()];
            mdec::decode(&e.data, f.width, f.height, &mut back).unwrap();
            let p = psnr(&rgba, &back);
            worst = worst.min(p);
            frames += 1;
        }
    }
    eprintln!("{frames} retail frames re-encoded in their own sector budgets: worst {worst:.1} dB; retail's largest code count {most:#x} (player limit {:#x})", mdec::MAX_CODES_WORD);
    assert!(worst > 28.0);
}

#[test]
fn a_retail_str_builds_back_byte_exact() {
    let Some(d) = disc() else { return };
    let e = d.table.by_id("movie/8").unwrap();
    let raw: Vec<u8> = (0..e.extent.sectors).flat_map(|k| d.image.read_raw(e.extent.lba + k).unwrap().to_vec()).collect();
    let old = scan(&d, "movie/8");
    let (payload, rep) = movie_edit::from_str(&old, &raw).unwrap();
    assert_eq!((rep.frames, rep.sectors, rep.source), (215, 2150, "str"));
    let dir = out_dir("str");
    let (out, plan) = build(&d, &[staged("movie/8", payload)], &dir);
    assert!(plan.items[0].raw && plan.items[0].new_lba.is_none());
    let img = DiscImage::open(&out).unwrap();
    for k in 0..e.extent.sectors {
        assert_eq!(img.read_raw(e.extent.lba + k).unwrap(), d.image.read_raw(e.extent.lba + k).unwrap(), "sector {k}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_movie_remade_from_frames_and_a_wav() {
    let Some(d) = disc() else { return };
    let old = scan(&d, "movie/8");
    let n = old.frames.len() as u32;
    let pngs: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let (rgba, _) = old.decode_frame(&d.image, i).unwrap();
            movie::png(old.info.width as u32, old.info.height as u32, &rgba)
        })
        .collect();
    let wav = movie::wav(&old.decode_audio(&d.image).unwrap());
    let (payload, rep) = movie_edit::from_frames(&old, &d.image, &pngs, Some(&wav), &MovieOptions::default()).unwrap();
    assert_eq!((rep.frames, rep.sectors, rep.moves), (n, n * 10, false));
    assert!(rep.psnr.unwrap() > 28.0, "{:?}", rep.psnr);
    assert!(rep.audio_seconds.unwrap() > 4.0);
    // The player stops at the new last frame (movie/8's record, entry 8).
    let ovr = { let o = d.table.by_id("ovr/1").unwrap(); d.image.read_sectors(o.extent.lba, o.extent.sectors).unwrap() };
    let ovr2 = movie_edit::set_last_frame(&ovr, &[8], n).unwrap();
    assert_eq!(movie::player_settings(&ovr2)[8].last_frame, n);
    let dir = out_dir("frames");
    let (out, _) = build(&d, &[staged("movie/8", payload.clone()), staged("ovr/1", ovr2)], &dir);
    let img = DiscImage::open(&out).unwrap();
    let e = d.table.by_id("movie/8").unwrap();
    let m = movie::scan(&img, e.extent.lba, n * 10).unwrap();
    assert_eq!((m.frames.len() as u32, m.info.audio.as_ref().map(|a| a.stride)), (n, Some(8)));
    let (a, _) = m.decode_frame(&img, n / 3).unwrap();
    let (b, _) = old.decode_frame(&d.image, n / 3).unwrap();
    assert!(psnr(&a, &b) > 28.0);
    let pcm = m.decode_audio(&img).unwrap();
    assert_eq!((pcm.rate, pcm.channels), (37_800, 2));
    // Every written sector is a valid Form 1 or Form 2 sector.
    for k in 0..n * 10 {
        cdrom::check_mode2(&img.read_raw(e.extent.lba + k).unwrap(), e.extent.lba + k).unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
    eprintln!("movie/8 re-made: {} frames, q {}–{}, {:.1} dB, {} B a frame", rep.frames, rep.qscale_min, rep.qscale_max, rep.psnr.unwrap(), rep.frame_bytes);
}

#[test]
fn a_longer_movie_moves_to_the_end() {
    let Some(d) = disc() else { return };
    let old = scan(&d, "movie/8");
    let (w, h) = (old.info.width, old.info.height);
    // 230 frames (2,300 sectors) where movie/8 has 2,150: it no longer fits its extent.
    let m = encode::mux(encode::Layout::retail(w, h), 230, |f| Ok((0..w as usize * h as usize).flat_map(|i| [(i % 251) as u8, f as u8, 90, 255]).collect()), None).unwrap();
    let p = Payload(&m.payload);
    assert_eq!(p.sectors(), 2300);
    assert_eq!(m.payload.len(), 2300 * PAYLOAD);
    let dir = out_dir("grow");
    let (out, plan) = build(&d, &[staged("movie/8", m.payload)], &dir);
    let item = &plan.items[0];
    let lba = item.new_lba.expect("relocated");
    let img = DiscImage::open(&out).unwrap();
    let exe = PsxExe::from_disc(&img).unwrap();
    let t = FileTable::parse(&exe, img.sectors()).unwrap();
    assert_eq!((t.by_id("movie/8").unwrap().extent.lba, t.by_id("movie/8").unwrap().extent.sectors), (lba, 2300));
    assert_eq!(movie::scan(&img, lba, 2300).unwrap().frames.len(), 230);
    let _ = std::fs::remove_dir_all(dir);
}
