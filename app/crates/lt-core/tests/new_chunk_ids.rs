//! New chunk ids past the end of the disc (spike, 2026-10-05). Writes a copy
//! of the disc with chunks 1328–1331 (copies of the Knight's house set
//! 328–331, 1330 tinted red) at LBA 20011 + id·125, past the retail end.
//! Opt in: `SPIKE_OUT=<dir> LT_TEST_DISC=… cargo test -p lt-core --release
//! --test new_chunk_ids` (writes a 440 MB image and a cue).
//!
//! Result in PCSX-Redux: a warp to 1330 (`ctx+0x50 = 0x8532`) arrived in
//! 3.6 s, the RAM slot held the tinted bytes (not retail 330), and the red
//! house rendered with the Knight standing in it. `Load_WorldChunk` reads
//! `entry LBA + id·0x3E800 / 2048` with no bound, and ids ≥ 324 behave as
//! interiors (no neighbour streaming). See docs/research/03 §4.

use std::io::Write;

use lt_core::cdrom;
use lt_core::edit::chunk::Chunk;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

#[test]
fn write_spike_image() {
    let (Some(path), Some(out)) = (std::env::var_os("LT_TEST_DISC"), std::env::var_os("SPIKE_OUT")) else { return };
    let out = std::path::PathBuf::from(out);
    let image = DiscImage::open(&path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let world = table.by_id("world").unwrap().extent.lba;
    let end = image.sectors();
    // New ids 1328..1331 = copies of the Knight's house set 328..331; 1330 (house) tinted red.
    let mut chunks = Vec::new();
    for (k, id) in (328..332).enumerate() {
        let mut c = Chunk::parse(&w.chunk(id).unwrap()).unwrap();
        if id == 330 {
            for g in &mut c.groups {
                for p in &mut g.prims {
                    for rgb in &mut p.rgb {
                        *rgb = [rgb[0].saturating_add(90), rgb[1] / 2, rgb[2] / 2];
                    }
                }
            }
        }
        chunks.push((1328 + k as u32, c.serialize().unwrap()));
    }
    let first = world + 1328 * 125;
    assert!(first >= end, "new ids must start past the retail end");
    std::fs::copy(&path, out.join("spike.bin")).unwrap();
    let mut f = std::fs::OpenOptions::new().append(true).open(out.join("spike.bin")).unwrap();
    let last = world + 1332 * 125;
    for lba in end..last {
        let mut raw = cdrom::blank(lba, [0, 0, if lba + 1 == last { 0x89 } else { 0x08 }, 0]);
        let data: Vec<u8> = match chunks.iter().find(|(id, _)| (world + id * 125..world + id * 125 + 125).contains(&lba)) {
            Some((id, bytes)) => {
                let o = (lba - (world + id * 125)) as usize * 2048;
                bytes[o..o + 2048].to_vec()
            }
            None => vec![0; 2048],
        };
        cdrom::encode_form1(&mut raw, &data);
        f.write_all(&raw).unwrap();
    }
    std::fs::write(out.join("spike.cue"), "FILE \"spike.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n").unwrap();
    println!("spike image: {} sectors (retail {end}); chunk 1328 at LBA {first}", last);
}
