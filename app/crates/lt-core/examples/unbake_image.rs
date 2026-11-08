//! A disc image with piece moves (`edit::unbake`, the Pieces tool), for checks
//! in PCSX-Redux. It takes the path a project save takes (`commands/kit.rs`
//! `bake_pieces`): split, prints, `resolve`, `rebake`, then `bake_pack` in
//! every pack serving the chunk in any story state: doors, examine spots and
//! smoke carried, or taken out of the game with a deleted piece.
//!
//! ```text
//! cargo run --release --example unbake_image -- <disc.bin> <out.bin> <move>…
//!   <move> = <chunk>:<x>,<z>:<dc>,<dr>[,<turns>]   or   <chunk>:<x>,<z>:del
//! ```
//!
//! Each move names the smallest piece whose bounds hold chunk-local (x, z).
//! It prints each piece, its lift, every carried branch (pack, slot, D offset,
//! before → after with facing), every record taken out and what stays behind,
//! then writes and verifies the image.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::build::{self, PlanOptions, Source, Staged};
use lt_core::edit::chunk::Chunk;
use lt_core::edit::character;
use lt_core::edit::unbake::{self, PieceMove};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&args[1]).unwrap();
    let out = std::path::PathBuf::from(&args[2]);
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let packs_table = lt_core::world::pack_table_in(&exe, &wd.file("ovr/2").unwrap());

    // Moves by chunk.
    let mut by_chunk: BTreeMap<u32, Vec<(i32, i32, Option<(i16, i16, u8)>)>> = BTreeMap::new();
    for spec in &args[3..] {
        let parts: Vec<&str> = spec.split(':').collect();
        let chunk: u32 = parts[0].parse().unwrap();
        let xz: Vec<i32> = parts[1].split(',').map(|v| v.parse().unwrap()).collect();
        let step = if parts[2] == "del" {
            None
        } else {
            let v: Vec<i16> = parts[2].split(',').map(|v| v.parse().unwrap()).collect();
            Some((v[0], v[1], v.get(2).copied().unwrap_or(0) as u8))
        };
        by_chunk.entry(chunk).or_default().push((xz[0], xz[1], step));
    }

    let mut staged: Vec<Staged> = Vec::new();
    let mut pack_bytes: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    for (&chunk, specs) in &by_chunk {
        let raw = wd.chunk(chunk).unwrap();
        let c = Chunk::parse(&raw).unwrap();
        let split = unbake::split(&c);
        let prints = unbake::fingerprints(&c, &split);
        let mut stored = Vec::new();
        for &(x, z, step) in specs {
            let n = (0..split.pieces.len())
                .filter(|&n| {
                    let p = &split.pieces[n];
                    x >= p.min[0] as i32 && x <= p.max[0] as i32 && z >= p.min[2] as i32 && z <= p.max[2] as i32
                })
                .min_by_key(|&n| {
                    let p = &split.pieces[n];
                    (p.max[0] as i32 - p.min[0] as i32) * (p.max[2] as i32 - p.min[2] as i32)
                })
                .unwrap_or_else(|| panic!("chunk/{chunk}: no piece holds ({x}, {z})"));
            let p = &split.pieces[n];
            println!(
                "chunk/{chunk} piece {n}: {} polygons, x {}..{} z {}..{} y {}..{}{} · print {}",
                p.prims.len(),
                p.min[0],
                p.max[0],
                p.min[2],
                p.max[2],
                p.min[1],
                p.max[1],
                if p.fixed { " (fixed)" } else { "" },
                prints[n].hash
            );
            let (dc, dr, turns, delete) = match step {
                Some((dc, dr, t)) => (dc, dr, t, false),
                None => (0, 0, 0, true),
            };
            stored.push(PieceMove { piece: prints[n].clone(), dc, dr, turns, delete, copy: false, hold: false });
        }
        let (moves, _) = unbake::resolve(&prints, &stored);
        let (baked, rep) = unbake::rebake(&c, &split, &moves).unwrap();
        println!("  rebake: lifts {:?}, filled {} cells, {} fill polygons, mesh {} → {}", rep.lifts, rep.filled_cells, rep.fill_polygons, c.sizes().unwrap().mesh, baked.sizes().unwrap().mesh);
        staged.push(Staged { id: format!("chunk/{chunk}"), bytes: Arc::new(baked.serialize().unwrap()), base_sha256: None });

        // Every pack serving the chunk in some story state (the flag cases of `world::pack_for`).
        let mut packs = BTreeSet::new();
        for bits in 0..8u8 {
            let flags = [0xE6u16, 0xBA, 0xF7];
            if let Some(p) = lt_core::world::pack_for(&packs_table, chunk, &|f| flags.iter().enumerate().any(|(k, &v)| v == f && bits >> k & 1 == 1)) {
                packs.insert(p);
            }
        }
        for pack in packs {
            let retail = wd.file(&format!("npc/{pack}")).unwrap();
            let current = pack_bytes.get(&pack).cloned().unwrap_or_else(|| retail.clone());
            let plain = character::base(&retail, &current).unwrap().into_owned();
            let out = unbake::bake_pack(pack, &retail, &current, chunk, &split, &moves, &rep.lifts, &[], &[]).unwrap();
            for k in &out.carried {
                let slot = unbake::spots(&plain, chunk).into_iter().find(|s| s.at == k.at).map(|s| s.slot).unwrap_or(0);
                println!(
                    "  npc/{pack} slot {slot} D+{:#06x}: ({}, {}, {}) dir {} → ({}, {}, {}) dir {}",
                    k.at, k.from.x, k.from.h, k.from.z, k.from.dir, k.to.x, k.to.h, k.to.z, k.to.dir
                );
            }
            for t in &out.taken {
                println!("  npc/{pack} slot {} taken out of the game", t.slot);
            }
            let ours: BTreeSet<usize> = moves.iter().map(|m| m.piece).collect();
            for s in unbake::spots(&plain, chunk) {
                let owner = unbake::owner_of(&split, s.place.x as i32, s.place.z as i32);
                let gone = out.carried.iter().any(|k| k.at == s.at) || out.taken.iter().any(|t| t.slot == s.slot);
                if owner.is_some_and(|o| ours.contains(&o)) && !gone {
                    println!("  npc/{pack} slot {} (type {}) at ({}, {}) stays behind", s.slot, s.kind, s.place.x, s.place.z);
                }
            }
            if let Some(b) = out.bytes {
                pack_bytes.insert(pack, b);
            }
        }
    }
    for (pack, bytes) in pack_bytes {
        staged.push(Staged { id: format!("npc/{pack}"), bytes: Arc::new(bytes), base_sha256: None });
    }
    let src = Source { image: &image, exe: &exe, table: &table };
    let plan = build::plan(&src, &staged, |_| true, PlanOptions { postgap: false, new_area_first: 1328 });
    let cancel = AtomicBool::new(false);
    let rep = build::write_image(&src, &plan, &staged, &out, &cancel, &mut |_| {}).unwrap();
    build::verify_image(&out, &src, &plan, &staged, &rep, &cancel, &mut |_| {}).unwrap();
    println!("wrote {} ({} files staged)", out.display(), staged.len());
}
