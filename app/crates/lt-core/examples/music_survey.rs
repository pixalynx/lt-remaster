//! What streaming each retail song would leave its bank (`lt_core::audio::stream`,
//! `docs/screens/14-audio.md` §14): for every BGM file, its song cues, and the ring and bounce
//! buffer a stream of one song (and of all of them) would get once the bank is slimmed.
//!
//! ```text
//! cargo run --release --example music_survey -- <disc.bin> [encode <audio file> <bgm n> <cue> <out>]
//! ```
//!
//! `encode` writes a stream file for that song and the slimmed bank beside it (`<out>.stream`,
//! `<out>.bgm`), with the encoder's report.
use lt_core::audio::{decode, stream, tsq, tvb};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&args[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    if args.get(2).map(String::as_str) == Some("encode") {
        let audio = std::fs::read(&args[3]).unwrap();
        let n: usize = args[4].parse().unwrap();
        let cue: usize = args[5].parse().unwrap();
        let file = w.file(&format!("bgm/{n}")).unwrap();
        // The common bank's songs borrow bank 2's whole region; its own file stays.
        let (bank, ring) = if n == 0 { (file.clone(), stream::Ring::BANK2) } else { stream::slim_bank(&file, &[cue]).unwrap() };
        let ext = std::path::Path::new(&args[3]).extension().and_then(|e| e.to_str());
        let pcm = decode::decode(&audio, ext).unwrap();
        let t0 = std::time::Instant::now();
        let e = stream::encode(&pcm, &stream::StreamOptions::default(), ring, pcm.title.as_deref()).unwrap();
        eprintln!("encoded in {:?}", t0.elapsed());
        println!("{}", serde_json::to_string_pretty(&e.info).unwrap());
        std::fs::write(format!("{}.stream", args[6]), &e.bytes).unwrap();
        std::fs::write(format!("{}.bgm", args[6]), &bank).unwrap();
        std::fs::write(format!("{}.wav", args[6]), stream::preview_wav(&e.bytes, 5.0).unwrap()).unwrap();
        return;
    }
    if args.get(2).map(String::as_str) == Some("se") {
        // The sound-effect banks: the system bank (sys/0 SE.TSQ + sys/1 SE.TVB, resident from boot,
        // slot 0) and the field bank (sys/8 SE2.DAT, slot 4).
        let seq = w.file("sys/0").unwrap();
        let voices = w.file("sys/1").unwrap();
        let t = tvb::Tvb::parse(&voices).unwrap();
        let used = |s: &[u8]| s.len() - s.iter().rev().take_while(|&&b| b == 0).count();
        let cues = tsq::cues(&seq);
        let free = (1..tsq::CUES - 1).filter(|&i| !cues.iter().any(|c| c.index == i)).count();
        println!("sys/0 SE.TSQ: file {} B ({} used), cap 0x1000; cues {} ({} sfx, {} songs), {} free", seq.len(), used(&seq), cues.len(),
            cues.iter().filter(|c| c.kind == tsq::CueKind::Sfx).count(), cues.iter().filter(|c| c.kind == tsq::CueKind::Song).count(), free);
        println!("sys/1 SE.TVB: file {} B, adpcm {} B of 0x20000 ({} free), slots used {}", voices.len(), t.size, 0x20000 - t.size as i64, t.used_count());
        let f = w.file("sys/8").unwrap();
        let b = u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize;
        let t8 = tvb::Tvb::parse(&f[b..]).unwrap();
        let c8 = tsq::cues(&f[8..b]);
        println!("sys/8 SE2: file {} B, tsq {} B, adpcm {} B, slots {}, cues {}", f.len(), b - 8, t8.size, t8.used_count(), c8.len());
        for c in cues.iter().take(60) { print!("{}{} ", c.index, if c.kind == tsq::CueKind::Song { "s" } else { "" }); }
        println!();
        return;
    }
    if args.get(2).map(String::as_str) == Some("ops") {
        // The opcode streams of a bank's sound-effect cues: `ops <file id> [cue…]`.
        let f = w.file(&args[3]).unwrap();
        let seq: &[u8] = if args[3] == "sys/0" { &f } else { &f[8..u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize] };
        for c in tsq::cues(seq).into_iter().filter(|c| c.kind == tsq::CueKind::Sfx).take(12) {
            let wk = tsq::walk(seq, c.offset as usize, 100_000);
            let ops: Vec<String> = wk.events.iter().map(|e| format!("{}:{:?}", e.tick, e.op)).collect();
            println!("cue {:>2} flag {:#06x} ticks {} {:?}: {}", c.index, c.flag, wk.ticks, wk.stop, ops.join(" "));
        }
        return;
    }
    if args.get(2).map(String::as_str) == Some("why") {
        let n: usize = args[3].parse().unwrap();
        let cues: Vec<usize> = args[4].split(',').map(|c| c.parse().unwrap()).collect();
        let f = w.file(&format!("bgm/{n}")).unwrap();
        let b = u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize;
        let seq = &f[8..b];
        for c in tsq::cues(seq) {
            let p = tsq::programs(seq, c.index);
            println!("cue {:>2} {:?} programs {:?}", c.index, c.kind, p);
        }
        match stream::slim_bank(&f, &cues) {
            Ok((bank, ring)) => println!("ok {} bytes, {ring:?}", bank.len()),
            Err(e) => println!("error: {e}"),
        }
        return;
    }
    println!("bgm   tsq   adpcm  songs            one song: adpcm ring  bounce     all songs: adpcm ring bounce");
    for n in 0..61 {
        let Ok(f) = w.file(&format!("bgm/{n}")) else { continue };
        let (a, b) = (u32::from_le_bytes(f[0..4].try_into().unwrap()) as usize, u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize);
        let seq = &f[a..b];
        let Some(t) = tvb::Tvb::parse(&f[b..]) else { continue };
        let songs: Vec<usize> = tsq::cues(seq).iter().filter(|c| c.kind == tsq::CueKind::Song).map(|c| c.index).collect();
        let one = songs.first().map(|&c| stream::slim_bank(&f, &[c]));
        let all = stream::slim_bank(&f, &songs);
        let show = |r: &Result<(Vec<u8>, stream::Ring), String>| match r {
            Ok((bank, ring)) => {
                let t = tvb::Tvb::parse(&bank[u32::from_le_bytes(bank[4..8].try_into().unwrap()) as usize..]).unwrap();
                format!("{:>6} {:>4} {:>2}", t.size, ring.frames, ring.bounce)
            }
            Err(_) => "    —     —  —".into(),
        };
        println!(
            "{n:>3} {:>6} {:>7}  {:<16} {:>22}     {:>22}",
            seq.len(),
            t.size,
            format!("{songs:?}"),
            one.as_ref().map(show).unwrap_or_default(),
            show(&all)
        );
    }
}
