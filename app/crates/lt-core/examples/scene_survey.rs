//! Survey of field scenes (research for the cutscene player): type-10 talk
//! triggers, the actors they wake, and the opcodes those programs use.
//! `cargo run --release --example scene_survey -- <disc.bin> [pack]`
use std::collections::{BTreeMap, BTreeSet};

use lt_core::npc::NpcPack;
use lt_core::tier2::{self, ast, dsl, Operand};
use lt_core::trigger::TriggerRecord;
use lt_core::worldmap::WorldDisc;
use lt_core::{text, DiscImage, FileTable, PsxExe};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let show: Option<u32> = a.get(2).and_then(|s| s.parse().ok());
    let mut freq: BTreeMap<String, (usize, BTreeSet<u32>)> = BTreeMap::new();
    let mut scenes = 0;
    for p in 0..50u32 {
        let Ok(b) = wd.file(&format!("npc/{p}")) else { continue };
        let Some(pk) = NpcPack::parse(&b) else { continue };
        let d = pk.d_block(&b);
        let mut woken = BTreeMap::new();
        for g in pk.triggers(&b) {
            for r in &g.rungs {
                for rec in &r.records {
                    if let TriggerRecord::Talk { actor, x, z, .. } = rec {
                        woken.entry(*actor as u32).or_insert((g.chunk, r.flag, *x, *z));
                    }
                }
            }
        }
        for (&slot, &(chunk, flag, x, z)) in &woken {
            let Some(act) = pk.actors.get(slot as usize) else { continue };
            scenes += 1;
            // Every program reachable from the entry (GOTOs).
            let mut seen = BTreeSet::new();
            let mut queue = vec![act.script];
            while let Some(e) = queue.pop() {
                if !seen.insert(e) { continue; }
                let Ok(code) = tier2::disassemble(d, e) else { continue };
                for i in &code {
                    let name = tier2::op_info(i.op).map(|o| o.name).unwrap_or("?");
                    let f = freq.entry(format!("{:02X} {name}", i.op)).or_default();
                    f.0 += 1;
                    f.1.insert(p);
                    for o in &i.operands { if let Operand::Code(t) = o { queue.push(*t as u32); } }
                }
            }
            if show == Some(p) {
                println!("=== npc/{p} actor {slot} woken on chunk {chunk} rung flag {flag:#x} at ({x},{z}) script D+{:#x}", act.script);
                let code = tier2::disassemble(d, act.script).unwrap();
                let stmts = ast::lift(&code, &|t| ast::TextRef::Pool(t));
                let preview = |t: u16| Some(text::plain(&text::decode(text::c_string(d, t as usize))));
                println!("{}", dsl::print(&stmts, None, &preview));
            }
        }
    }
    println!("{scenes} scenes (distinct woken actors). Opcodes in scene programs:");
    let mut v: Vec<_> = freq.into_iter().collect();
    v.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (k, (n, packs)) in v { println!("  {k:18} {n:5}  in {} packs", packs.len()); }
}
