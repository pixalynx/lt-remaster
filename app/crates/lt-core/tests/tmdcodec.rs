//! TMD codec (`edit::tmdcodec`) on every model the disc holds: toys, enemies,
//! knights, Baby forms, arenas, the field Knight and Baby, and every NPC
//! pack's Group-A models. Each writes back byte for byte, and every
//! primitive's word counts match the class table the importer uses.
//! Opt in with `LT_TEST_DISC=… cargo test --release -p lt-core --test tmdcodec`.

use std::collections::BTreeMap;

use lt_core::edit::tmdcodec::{class_words, Tmd};
use lt_core::mesh::{model_parts, Container};
use lt_core::npc::NpcPack;
use lt_core::{DiscImage, FileTable, PsxExe};

fn models(image: &DiscImage, table: &FileTable) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let read = |id: &str| table.by_id(id).and_then(|e| image.read_sectors(e.extent.lba, e.extent.sectors).ok());
    let mut push = |name: String, b: &[u8]| out.push((name, b.to_vec()));
    for (prefix, n, kind) in [("toy", 73, Container::Toy), ("enemy", 112, Container::Enemy), ("pb", 19, Container::Pb), ("bf", 48, Container::Bf)] {
        for i in 0..n {
            let id = format!("{prefix}/{i}");
            let Some(b) = read(&id) else { continue };
            let kind = if prefix == "enemy" && i < 2 { Container::Knight } else { kind };
            let Some(parts) = model_parts(kind, &b) else { continue };
            push(format!("{id}#main"), parts.main);
            if let Some(e) = parts.effect {
                if e.len() > 12 && u32::from_le_bytes(e[0..4].try_into().unwrap()) == 0x41 {
                    push(format!("{id}#effect"), e);
                }
            }
        }
    }
    if let Some(b) = read("knight/0") {
        if let Some(p) = model_parts(Container::Knight, &b) {
            push("knight/0#main".into(), p.main);
        }
    }
    if let Some(b) = read("sys/6") {
        for (k, kind) in [("knight", Container::Sys6Knight), ("baby", Container::Sys6Baby)] {
            if let Some(p) = model_parts(kind, &b) {
                push(format!("sys/6#{k}"), p.main);
            }
        }
    }
    for n in 0..50 {
        let id = format!("npc/{n}");
        let Some(b) = read(&id) else { continue };
        let Some(p) = NpcPack::parse(&b) else { continue };
        for i in 0..p.group_a(&b) {
            if let Some(m) = p.model(&b, i) {
                push(format!("{id}#model{i}"), m);
            }
        }
    }
    out
}

#[test]
fn every_retail_tmd_writes_back_byte_for_byte() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let all = models(&image, &table);
    let mut classes: BTreeMap<(u8, u8), (u8, u8, u32)> = BTreeMap::new();
    let mut parsed = 0;
    for (name, b) in &all {
        let t = match Tmd::parse(b) {
            Ok(t) => t,
            Err(e) => panic!("{name}: {e}"),
        };
        let out = t.write();
        if out[..] != b[..out.len().min(b.len())] {
            let first = out.iter().zip(b.iter()).position(|(x, y)| x != y);
            panic!("{name}: bytes differ at {first:?} (out {} B, retail {} B, objects {})", out.len(), b.len(), t.objects.len());
        }
        assert!(out.len() <= b.len(), "{name}: grew from {} to {}", b.len(), out.len());
        for o in &t.objects {
            for p in &o.prims {
                let e = classes.entry((p.mode, p.flag)).or_insert((p.olen, p.words.len() as u8, 0));
                assert_eq!((e.0, e.1), (p.olen, p.words.len() as u8), "{name}: class {:02X}{:02X} varies", p.mode, p.flag);
                e.2 += 1;
            }
        }
        parsed += 1;
    }
    for ((mode, flag), (olen, ilen, count)) in &classes {
        let key = ((*mode as u16) << 8 | *flag as u16) & !0x200;
        if lt_core::mesh::TMD_CLASSES.contains(&key) {
            assert_eq!(class_words(*mode, *flag), (*ilen, *olen), "class {mode:02X}{flag:02X} ({count} prims)");
        }
    }
    eprintln!("{parsed} TMDs, {} classes", classes.len());
    assert!(parsed > 800, "{parsed}");
}
