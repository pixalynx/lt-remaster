//! Survey of the actor models of every NPC pack (research for 3D actor models in the world editor).
//! `cargo run --release --example actor_model_survey -- <disc.bin>`
use std::collections::BTreeMap;

use lt_core::mesh;
use lt_core::npc::NpcPack;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let mut types: BTreeMap<(u8, u8), usize> = BTreeMap::new();
    let mut bad = 0;
    let mut objs: BTreeMap<(u8, usize), usize> = BTreeMap::new();
    let mut sizes = Vec::new();
    for p in 0..50 {
        let Ok(b) = wd.file(&format!("npc/{p}")) else { continue };
        let Some(pk) = NpcPack::parse(&b) else { println!("npc/{p}: no parse"); continue };
        let ca = pk.group_a(&b);
        let cb = pk.models.len() - ca;
        for act in &pk.actors {
            let model = (act.b >> 16) as u8;
            let pose = (act.b >> 24) as u8;
            let typ = (act.d >> 8) as u8;
            let shadow = act.d as u8;
            *types.entry((typ, shadow)).or_default() += 1;
            if act.slot < 2 || model == 0xFF || matches!(typ, 4 | 5) {
                continue;
            }
            if model as usize >= ca {
                println!("npc/{p} slot {}: model {model} >= count_a {ca}", act.slot);
                bad += 1;
                continue;
            }
            let Some(m) = pk.model(&b, model as usize).and_then(mesh::tmd_mesh) else {
                println!("npc/{p} slot {}: model {model} not a TMD", act.slot);
                bad += 1;
                continue;
            };
            *objs.entry((typ & 0xF, m.groups.len())).or_default() += 1;
            let mut bb = m.bbox();
            let first = if pose != 0xFF { pk.first_pose(&b, pose as usize) } else { None };
            if let (Some(fp), Some(t)) = (first, pk.pose_block(&b, pose as usize).filter(|_| pose != 0xFF).and_then(mesh::morph_targets)) {
                let v = mesh::blend(&m.verts, &t, fp, 256, 0, 0);
                let posed = mesh::RenderMesh { verts: v, ..m.clone() };
                bb = posed.bbox();
            }
            sizes.push((bb[4] - bb[1]) as i32);
            if pose != 0xFF && pose as usize >= cb {
                println!("npc/{p} slot {}: pose {pose} >= count_b {cb}", act.slot);
            }
            if p == 6 || p == 7 {
                println!(
                    "npc/{p} slot {:3} home {:4} model {:2} pose {:3} first {:?} type {:#x} shadow {} verts {:4} polys {:4} objs {} bbox {:?}",
                    act.slot, act.b & 0xFFFF, model, pose, first, typ, shadow, m.verts.len(), m.polys.len(), m.groups.len(), bb
                );
            }
        }
    }
    println!("(type, shadow) counts: {types:?}");
    println!("(type&0xF, objects) counts: {objs:?}");
    sizes.sort();
    println!("heights: min {:?} median {:?} max {:?}; bad {bad}", sizes.first(), sizes.get(sizes.len() / 2), sizes.last());
}
