//! Writes a content pack's BPS patches (`crate::pack`) from retail and modded files, and prints the
//! manifest's `files` entries. The patches carry only what the mod changes: bytes equal to the source
//! at the same offset become SourceRead, runs found elsewhere in the source (data a build moved)
//! become SourceCopy, and only the rest is stored.
//!
//! `cargo run -p lt-core --example make_pack -- <out-dir> <id>=<retail>[+<other>=...]:<modded>[:<patch name>] ...`
//!
//! A source may be several files joined with `+`: the file's own retail bytes, then other retail
//! files it is made from, as `<id>@<path>` (`sys/6=a.bin+npc/0@b.bin:...`). The patch reads them
//! all, so it copies their bytes from the player's disc instead of carrying them, and the
//! manifest lists the others as `extraSources`.
//!
//! When `<out-dir>/pack.json` exists, its `files` are rewritten to match. The patches stay out of
//! git (`.gitignore`): `scripts/upload-pack.sh` publishes them on R2, where the app downloads them.
//!
//! The JP prologue pack (data/packs/jp-prologue, docs/research/10-jp-prologue.md) is rebuilt by the
//! decomp's `tools/jp_prologue_pack.sh <LivingTool>/app`, which runs:
//! `make_pack crates/lt-core/data/packs/jp-prologue ovr/1=<gc_decomp>/disk/OVR/OVR01.BIN:<mod ovr01>:ovr01.bps
//!  ovr/25=<gc_decomp>/disk/OVR/OVR25.BIN:<mod ovr25>:ovr25.bps`
use std::collections::HashMap;

use lt_core::bps::{self, Encoder};
use lt_core::pack::{sha256_hex, Manifest, PackFile};

const GRAM: usize = 8;

fn patch(source: &[u8], target: &[u8], metadata: &str) -> Vec<u8> {
    let mut index: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for o in 0..source.len().saturating_sub(GRAM - 1) {
        index.entry(&source[o..o + GRAM]).or_default().push(o);
    }
    let mut enc = Encoder::new(source.len() as u64, target.len() as u64, metadata);
    let mut i = 0;
    while i < target.len() {
        let same = (0..target.len() - i).take_while(|&k| source.get(i + k) == Some(&target[i + k])).count();
        let (mut at, mut best) = (0, 0);
        if i + GRAM <= target.len() {
            for &o in index.get(&target[i..i + GRAM]).map(Vec::as_slice).unwrap_or(&[]) {
                let n = (0..(target.len() - i).min(source.len() - o)).take_while(|&k| source[o + k] == target[i + k]).count();
                if n > best {
                    (at, best) = (o, n);
                }
            }
        }
        if same >= GRAM && same >= best {
            enc.feed(&source[i..i + same], &target[i..i + same]);
            i += same;
        } else if best >= GRAM {
            enc.feed_from(&source[at..at + best], at as u64, &target[i..i + best]);
            i += best;
        } else {
            enc.feed(&[], &target[i..i + 1]);
            i += 1;
        }
    }
    let crc = |b: &[u8]| u32::from_str_radix(&bps::crc32_hex(b), 16).unwrap();
    enc.finish(crc(source), crc(target)).expect("patch")
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&a[1]);
    std::fs::create_dir_all(out).unwrap();
    let mut entries = Vec::new();
    for spec in &a[2..] {
        let (id, rest) = spec.split_once('=').expect("id=retail:modded[:name]");
        let parts: Vec<&str> = rest.split(':').collect();
        let mut source = Vec::new();
        let mut extra_sources = Vec::new();
        for (k, s) in parts[0].split('+').enumerate() {
            let path = match s.split_once('@') {
                Some((other, path)) if k > 0 => {
                    extra_sources.push(other.to_string());
                    path
                }
                _ => s,
            };
            source.extend(std::fs::read(path).unwrap());
        }
        let target = std::fs::read(parts[1]).unwrap();
        let name = parts.get(2).map(|s| s.to_string()).unwrap_or_else(|| format!("{}.bps", id.replace('/', "")));
        let p = patch(&source, &target, &format!("Living Tool content pack: {id}"));
        assert_eq!(bps::apply(&p, &source).unwrap(), target, "{id}: the patch must reproduce the modded file");
        std::fs::write(out.join(&name), &p).unwrap();
        println!("{id}: {} -> {} bytes, patch {} bytes ({name})", source.len(), target.len(), p.len());
        entries.push(PackFile {
            id: id.into(),
            patch: name,
            patch_size: p.len() as u64,
            patch_sha256: sha256_hex(&p),
            source_size: source.len() as u64,
            source_crc32: bps::crc32_hex(&source),
            target_size: target.len() as u64,
            target_crc32: bps::crc32_hex(&target),
            extra_sources,
        });
    }
    let manifest = out.join("pack.json");
    if let Ok(text) = std::fs::read_to_string(&manifest) {
        let mut m: Manifest = serde_json::from_str(&text).expect("pack.json");
        m.files = entries.clone();
        std::fs::write(&manifest, serde_json::to_string_pretty(&m).unwrap() + "\n").unwrap();
        println!("updated {}", manifest.display());
    }
    println!("\"files\": {}", serde_json::to_string_pretty(&entries).unwrap());
}
