//! LT-109's criteria on the test set. They need the disc and a test set built from it:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-assets -- --ignored`
//! (`lt-testset build` makes the set).

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::{animation, meshes, tally, warps, Disc};
use lt_assets::gpu::{Cache, Counting, Upload, Wgpu};
use lt_assets::model::{ModelIndex, ModelSource};
use lt_assets::{Assets, MeshKey};
use lt_core::fs::Kind;
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

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

fn image() -> (Arc<Image>, Disc) {
    let p = env("LT_DISC");
    (
        Image::open(&p).expect("the disc"),
        Disc::open(&p).expect("the disc's files"),
    )
}

fn scene(name: &str) -> (Scene, Vec<u8>) {
    Scene::load(&env("LT_TESTSET"), name).expect("a test-set scene")
}

/// @LT-109/1: every chunk group drawn in the field scenes matches a group Living Tool decodes.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt109_1_every_chunk_group_drawn_in_the_field_matches() {
    let (image, disc) = image();
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    for name in FIELD {
        let (s, state) = scene(name);
        let t = tally(&image, &s, &state, &mut assets).unwrap();
        assert!(t.groups > 0, "{name} draws groups");
        assert_eq!(
            t.groups_matched + t.group_misses,
            t.groups,
            "{name}: a group that doesn't match is reported"
        );
        assert_eq!(
            t.groups_matched, t.groups,
            "{name}: every group matched ({:?})",
            t.misses
        );
    }
}

/// @LT-109/2: characters in the village, an interior and the overworld match their model; with
/// the NPC packs left out of the index, NPCs draw from their TMD in RAM and the same mesh.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt109_2_characters_match_their_model_or_draw_from_ram() {
    let (image, disc) = image();
    let files = disc.files();
    let mut full = Assets::new(&files, ModelIndex::build(&files));
    let mut seen = HashMap::new();
    for name in ["village", "interior", "overworld"] {
        let (s, state) = scene(name);
        let t = tally(&image, &s, &state, &mut full).unwrap();
        assert!(t.characters > 0, "{name} draws characters");
        assert_eq!(
            t.parts_matched, t.parts,
            "{name}: every part matched ({:?})",
            t.misses
        );
        assert_eq!(
            t.from_ram, 0,
            "{name}: every model is a file's ({:?})",
            t.sources
        );
        assert!(
            t.sources.keys().any(|k| k == "sys/6#0"),
            "{name}: the Knight is sys/6's ({:?})",
            t.sources
        );
        seen.insert(name, t.sources);
    }
    assert!(
        seen["village"].contains_key("npc/6#3"),
        "the village NPC is npc/6's model 3"
    );
    assert!(
        seen["overworld"].keys().any(|k| k.starts_with("sys/9@")),
        "overworld travellers are sys/9's"
    );

    // No NPC packs: the party still comes from sys/, NPCs from RAM, drawn the same.
    let mut bare = Assets::new(&files, ModelIndex::build_from(&files, &[Kind::Sys]));
    let (s, state) = scene("village");
    let t = tally(&image, &s, &state, &mut bare).unwrap();
    assert_eq!(
        t.parts_matched, t.parts,
        "every part still drawn ({:?})",
        t.misses
    );
    assert!(
        t.from_ram > 0 && t.sources.keys().any(|k| k.starts_with("RAM ")),
        "NPCs from RAM ({:?})",
        t.sources
    );
    let mut compared = 0;
    for sig in bare_sigs(&bare) {
        let (a, b) = (
            bare.model(sig).unwrap(),
            full.model(sig)
                .expect("the same model met with the full index"),
        );
        assert!(
            matches!(a.source, ModelSource::Ram { .. })
                && matches!(b.source, ModelSource::Disc { .. })
        );
        assert_eq!(
            a.mesh.corners, b.mesh.corners,
            "{} from RAM draws as {}",
            a.source, b.source
        );
        compared += 1;
    }
    assert!(compared > 0);
}

fn bare_sigs(a: &Assets) -> Vec<u64> {
    a.models()
        .filter(|m| matches!(m.source, ModelSource::Ram { .. }))
        .map(|m| m.sig)
        .collect()
}

/// @LT-109/3: the water and the other animated pages are decoded every tick from the emulated
/// VRAM, and equal what the GPU makes of it.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt109_3_animated_pages_update_every_tick_from_vram() {
    let (image, disc) = image();
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let (s, state) = scene("village");
    let a = animation(&image, &s, &state, &mut assets).unwrap();
    assert_eq!(a.ticks_written, a.ticks, "every tick writes animated tiles");
    assert!(
        a.writes_checked > 0 && a.writes_equal == a.writes_checked,
        "{}/{} writes equal the GPU's VRAM",
        a.writes_equal,
        a.writes_checked
    );
    assert_eq!(
        a.moved_equal, a.moved,
        "the moves land where the GPU puts them"
    );
    assert!(
        a.distinct.values().any(|&n| n >= 16),
        "the water animates ({:?})",
        a.distinct
    );
    // The field's 13 moves were learnt: 8 texture strips (2 inside the water rect) and 5 palette rows.
    assert!(a.rects.len() >= 12, "{:?}", a.rects);
}

/// @LT-109/4: each asset uploads once; a warp to a new area uploads only that area's chunks, and
/// a warp back uploads nothing.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt109_4_each_asset_uploads_once() {
    let (image, disc) = image();
    let files = disc.files();
    let (s, state) = scene("village");
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let mut cache = Cache::new(Counting::default());
    let w = warps(&image, &s, &state, 330, 240, &mut assets, &mut cache).unwrap();
    check_warps(&w);

    // The same on a GPU when there is one, and the textures there equal the assets' copies.
    let Some(gpu) = Wgpu::headless() else { return };
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let mut cache = Cache::new(gpu);
    let w = warps(&image, &s, &state, 330, 240, &mut assets, &mut cache).unwrap();
    check_warps(&w);
    let mut animated = 0;
    for key in cache.texture_keys().collect::<Vec<_>>() {
        let t = assets.texture(key).unwrap();
        let back = cache
            .gpu
            .read_texture(cache.texture(key).unwrap(), t.w, t.h);
        assert!(
            back == t.rgba,
            "texture {key:016x} on the GPU equals the assets' copy"
        );
        animated += (!assets.animated_tiles(key).is_empty()) as u32;
    }
    assert!(animated > 0, "an animated texture was read back");
}

fn check_warps(w: &lt_assets::check::Warps) {
    let chunk_ups = |i: usize| -> BTreeSet<u16> {
        meshes(&w.phases[i])
            .into_iter()
            .filter_map(|(k, _)| {
                if let MeshKey::Chunk(id, _) = k {
                    Some(id)
                } else {
                    None
                }
            })
            .collect()
    };
    assert_eq!(
        chunk_ups(0),
        BTreeSet::from([77, 78, 79]),
        "the village's chunks"
    );
    assert_eq!(
        chunk_ups(1),
        BTreeSet::from([330]),
        "the warp uploads the interior only"
    );
    assert!(
        w.phases[2].is_empty(),
        "the warp back uploads nothing: {:?}",
        w.phases[2]
    );
    let mut keys = BTreeSet::new();
    for u in w.phases.iter().flatten() {
        let k = match u {
            Upload::Mesh { key, .. } => format!("{key:?}"),
            Upload::Texture { key, .. } => format!("t{key}"),
        };
        assert!(keys.insert(k), "uploaded twice: {u:?}");
    }
    assert!(w.writes > 0, "animated tiles were written");
}
