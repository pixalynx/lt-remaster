//! `lt-assets`: LT-109's checks on the test set.
//!
//! ```text
//! lt-assets match <disc.bin> <set> [scene…]        chunk groups and characters matched to assets (AC1, AC2)
//! lt-assets vram <disc.bin> <set> [scene]          animated tiles against the emulated VRAM (AC3)
//! lt-assets cache <disc.bin> <set> [to] [--wgpu]   uploads over the village, a warp to `to` (330) and back (AC4)
//! ```
//!
//! Exits 1 when a check fails.

use std::collections::BTreeSet;
use std::path::PathBuf;

use lt_assets::check::{animation, meshes, tally, warps, Disc};
use lt_assets::gpu::{Cache, Counting, Gpu, Upload, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::{Assets, MeshKey};
use lt_testset::{Image, Scene};

const FIELD: &[&str] = &[
    "village",
    "overworld",
    "interior",
    "dungeon",
    "late",
    "flight",
    "cutscene",
    "menu",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("lt-assets match|vram|cache <disc.bin> <set> …");
        std::process::exit(2);
    }
    let result = (|| -> Result<bool, String> {
        let path = PathBuf::from(&args[1]);
        let image = Image::open(&path)?;
        let disc = Disc::open(&path)?;
        let set = PathBuf::from(&args[2]);
        match args[0].as_str() {
            "match" => matched(&image, &disc, &set, &args[3..]),
            "vram" => vram(
                &image,
                &disc,
                &set,
                args.get(3).map_or("village", String::as_str),
            ),
            "cache" => {
                let to: u16 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(330);
                if args.iter().any(|a| a == "--wgpu") {
                    let gpu = Wgpu::headless().ok_or("no GPU adapter")?;
                    cache(&image, &disc, &set, to, gpu)
                } else {
                    cache(&image, &disc, &set, to, Counting::default())
                }
            }
            other => Err(format!("unknown command {other}")),
        }
    })();
    match result {
        Ok(true) => println!("PASS"),
        Ok(false) => {
            println!("FAIL");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("lt-assets: {e}");
            std::process::exit(2);
        }
    }
}

fn matched(
    image: &std::sync::Arc<Image>,
    disc: &Disc,
    set: &std::path::Path,
    scenes: &[String],
) -> Result<bool, String> {
    let files = disc.files();
    let index = ModelIndex::build(&files);
    println!(
        "model index: {} models ({} TMDs) on the disc",
        index.len(),
        index.found
    );
    let names: Vec<&str> = if scenes.is_empty() {
        FIELD.to_vec()
    } else {
        scenes.iter().map(String::as_str).collect()
    };
    let mut ok = true;
    let mut assets = Assets::new(&files, index);
    for name in names {
        let (scene, state) = Scene::load(set, name)?;
        let t = tally(image, &scene, &state, &mut assets)?;
        println!(
            "{name}: {} ticks, chunks {:?}; groups {}/{} matched; characters {}, parts {}/{} matched, {} from RAM; models {:?}; resolving {:.3} ms a tick",
            t.ticks, t.chunks, t.groups_matched, t.groups, t.characters, t.parts_matched, t.parts, t.from_ram, t.sources, t.resolve_ms
        );
        for m in t.misses.iter().take(8) {
            println!("  miss: {m}");
        }
        ok &= t.missed() == 0 && t.from_ram == 0;
    }
    println!(
        "parts whose shape several models share (a field model taken): {}",
        assets.shared_shapes
    );
    Ok(ok)
}

fn vram(
    image: &std::sync::Arc<Image>,
    disc: &Disc,
    set: &std::path::Path,
    name: &str,
) -> Result<bool, String> {
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let (scene, state) = Scene::load(set, name)?;
    let a = animation(image, &scene, &state, &mut assets)?;
    let changing = a.distinct.values().filter(|&&n| n > 1).count();
    println!(
        "{name}: {} ticks; animated rects {:?}",
        a.ticks,
        a.rects
            .iter()
            .map(|r| (r.x, r.y, r.w, r.h))
            .collect::<Vec<_>>()
    );
    println!(
        "  ticks writing animated tiles: {} of {}; {} tile writes ({:.1} a tick)",
        a.ticks_written,
        a.ticks,
        a.writes,
        a.writes as f64 / a.ticks.max(1) as f64
    );
    println!(
        "  writes equal to the tile the GPU's VRAM gives a tick later: {}/{}",
        a.writes_equal, a.writes_checked
    );
    println!(
        "  moved halfwords predicted: {}/{} equal to the VRAM a tick later",
        a.moved_equal, a.moved
    );
    println!(
        "  animated tiles: {}, {} of them changing ({:?} distinct frames)",
        a.distinct.len(),
        changing,
        a.distinct.values().collect::<Vec<_>>()
    );
    Ok(a.ticks_written == a.ticks
        && a.writes_checked > 0
        && a.writes_equal == a.writes_checked
        && a.moved_equal == a.moved
        && changing > 0)
}

fn cache<G: Gpu>(
    image: &std::sync::Arc<Image>,
    disc: &Disc,
    set: &std::path::Path,
    to: u16,
    gpu: G,
) -> Result<bool, String> {
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let mut cache = Cache::new(gpu);
    let (scene, state) = Scene::load(set, "village")?;
    let w = warps(image, &scene, &state, to, 240, &mut assets, &mut cache)?;
    let mut ok = true;
    let names = [
        "village",
        &format!("warp to {to}"),
        &format!("warp back to {}", scene.chunk),
    ];
    let mut all: Vec<&Upload> = Vec::new();
    for (i, ups) in w.phases.iter().enumerate() {
        let m = meshes(ups);
        let textures = ups
            .iter()
            .filter(|u| matches!(u, Upload::Texture { .. }))
            .count();
        let chunks: BTreeSet<u16> = m
            .iter()
            .filter_map(|(k, _)| {
                if let MeshKey::Chunk(id, _) = k {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        let models: Vec<&String> = m
            .iter()
            .filter(|(k, _)| matches!(k, MeshKey::Model(_)))
            .map(|(_, l)| l)
            .collect();
        println!("{}: {} ticks drawing chunks {:?}; uploaded {} meshes (chunks {:?}, models {:?}) and {} textures", names[i], w.ticks[i], w.drawn[i], m.len(), chunks, models, textures);
        all.extend(ups.iter());
        // Only chunks drawn in this phase, and none drawn in an earlier one.
        let earlier: BTreeSet<u16> = w.drawn[..i].iter().flatten().copied().collect();
        let fresh: BTreeSet<u16> = w.drawn[i].difference(&earlier).copied().collect();
        if chunks != fresh {
            println!("  chunk uploads {chunks:?} aren't the chunks first drawn here {fresh:?}");
            ok = false;
        }
    }
    let mut keys = BTreeSet::new();
    let twice = all
        .iter()
        .filter(|u| {
            !keys.insert(match u {
                Upload::Mesh { key, .. } => format!("{key:?}"),
                Upload::Texture { key, .. } => format!("t{key}"),
            })
        })
        .count();
    println!(
        "{} uploads ({} bytes), {} uploaded twice; {} animated tile writes",
        all.len(),
        cache.stats.upload_bytes(),
        twice,
        w.writes
    );
    Ok(ok
        && twice == 0
        && w.phases[2].iter().all(|u| {
            !matches!(
                u,
                Upload::Mesh {
                    key: MeshKey::Chunk(..),
                    ..
                }
            )
        }))
}
