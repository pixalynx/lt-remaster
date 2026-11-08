//! Budget of every retail NPC pack: how much a project can add before it hits
//! the u16 text reach (D+0xFFFF), the s16 table-2 reach (D+0x7FFF) and the
//! loader cap (0x31000 B), and how much of the D-span the retail text pool
//! and bytecode take (`docs/research/06-total-conversion-base.md`).
//!
//! ```text
//! cargo run --release --example pack_budget_survey -- <disc.bin>
//! ```
use lt_core::npc::NpcPack;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let path = std::env::args().nth(1).expect("disc.bin");
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    println!("pack  actors objs  d_span  reach16  reach15   used    cap_left  pool_bytes  code_bytes  t1  t2");
    for n in 0..50u32 {
        let b = w.file(&format!("npc/{n}")).unwrap();
        let p = NpcPack::parse(&b).unwrap();
        let used = b.len() - b.iter().rev().take_while(|&&x| x == 0).count();
        let objs = p.actors.iter().filter(|a| matches!((a.d >> 8) & 0xFF, 4 | 5)).count();
        let pool = p.pool(&b);
        let pool_bytes: u32 = pool.iter().map(|s| s.len).sum();
        let (cs, ce) = p.code_span(&b);
        let groups = p.triggers(&b);
        let t1: usize = groups.iter().filter(|g| g.pass == 0).flat_map(|g| &g.rungs).map(|r| r.records.len()).sum();
        let t2: usize = groups.iter().filter(|g| g.pass == 1).flat_map(|g| &g.rungs).map(|r| r.records.len()).sum();
        println!(
            "{n:>4}  {:>6} {objs:>4}  {:>6X}  {:>7}  {:>7}  {:>6X}  {:>8}  {:>10}  {:>10}  {t1:>2}  {t2:>2}",
            p.actors.len(),
            p.d_span,
            0xFFFFi64 - p.d_span as i64,
            0x7FFFi64 - p.d_span as i64,
            used,
            0x31000i64 - used as i64,
            pool_bytes,
            ce as i64 - cs as i64,
        );
    }
}
