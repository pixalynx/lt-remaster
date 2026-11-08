//! LT-119's criteria on the test set. They need the disc, a test set built from it (with the cave
//! and the door scenes) and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test crowd -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{characters, chest, widened, Characters, FIELDS};
use lt_native::composite::Compositor;
use lt_native::wide::Reach;
use lt_testset::{Image, Scene};

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

fn with<R>(f: impl FnOnce(&Arc<Image>, &mut Assets) -> R) -> R {
    let p = env("LT_DISC");
    let (image, disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    f(&image, &mut assets)
}

fn measure(names: &[&str]) -> Vec<(String, Characters)> {
    with(|image, assets| {
        names
            .iter()
            .map(|name| {
                let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
                (name.to_string(), characters(image, &s, &state, assets).unwrap())
            })
            .collect()
    })
}

fn share(v: &[(u32, u32)], field: &str) -> f64 {
    let k = FIELDS.iter().position(|f| f.0 == field).unwrap();
    v[k].0 as f64 / v[k].1.max(1) as f64
}

/// @LT-119/1: in the village and the towns (the opening's village, the walk out of the house,
/// Trisken), an NPC without a packet slot keeps its position and facing live (its walk script
/// runs) and its animation sequence racing (the frame and target pose step every tick), but its
/// current pose and weight frozen: the blend only runs while it's drawn.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt119_1_which_fields_of_an_unslotted_npc_stay_live() {
    for (name, c) in measure(&["village", "door", "dungeon"]) {
        let u = &c.unslotted;
        assert!(u[0].1 > 1000, "{name}: {} unslotted NPC ticks", u[0].1);
        assert!(share(u, "x") + share(u, "z") > 0.0, "{name}: no unslotted NPC moved");
        assert!(share(u, "dir") > 0.0, "{name}: no unslotted NPC turned");
        assert!(share(u, "frame") > 0.5 && share(u, "target pose") > 0.5, "{name}: the sequence didn't race ({:.2})", share(u, "frame"));
        assert_eq!((share(u, "current pose"), share(u, "weight")), (0.0, 0.0), "{name}: the blend ran without a slot");
        // With a slot the blend runs every tick.
        assert!(share(&c.slotted, "weight") > 0.9, "{name}: slotted weight changed {:.2}", share(&c.slotted, "weight"));
    }
}

/// @LT-119/2: option 2 (pose from the records). Everything the game draws for an NPC, a door or a
/// chest the record rebuilds exactly (world matrix, model-view, vertices), and the shadow
/// animation steps as the game does; so at 21:9 the crowd draws every character at the edge, in
/// the towns and the cave.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt119_2_option_2_draws_npcs_at_the_21_9_edge() {
    for (name, c) in measure(&["village", "door", "dungeon", "interior"]) {
        assert!(c.drawn > 500, "{name}: {} characters drawn", c.drawn);
        assert_eq!(c.rebuilt, c.drawn, "{name}: characters the records don't rebuild exactly");
        assert!(c.shadow as f64 >= 0.99 * c.shadow_ticks as f64, "{name}: shadow step {}/{}", c.shadow, c.shadow_ticks);
    }
    with(|image, assets| {
        let (s, state) = Scene::load(&env("LT_TESTSET"), "cave").unwrap();
        for (lid, dir) in [(0u8, 0x800i16), (0x10, 0x800), (0x20, 0x800), (0x10, 0x5A0)] {
            assert_eq!(chest(image, &s, &state, lid, dir, assets).unwrap(), Some((2, true)), "the chest at lid {lid:#x}, dir {dir:#x}");
        }
    });
    let gpu = Wgpu::headless().expect("a GPU");
    let mut comp = Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    with(|image, assets| {
        let mut drawn = 0;
        for name in ["village", "door", "dungeon", "cave"] {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            let w = widened(image, &s, &state, Reach::new(21.0 / 9.0, 1.0), 30, assets, &mut cache, &mut comp, None).unwrap();
            assert_eq!(w.npc.in_margins, 0, "{name}: characters in the margins left out");
            drawn += w.npc.crowd_margins;
        }
        assert!(drawn > 50, "the crowd drew {drawn} characters at the edge");
    });
}

/// @LT-119/3: LT-114's NPC criterion takes option 2: the README says so, and in every field scene
/// at 16:9 and 21:9 nothing placed in view is left out.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt119_3_lt114_takes_option_2() {
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    assert!(doc.contains("## Characters without a slot (LT-119)") && doc.contains("option 2"), "native/README.md records the choice");
    let gpu = Wgpu::headless().expect("a GPU");
    let mut comp = Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    with(|image, assets| {
        for name in ["village", "overworld", "interior", "dungeon", "late", "flight", "cutscene", "menu", "cave", "tower", "lamps", "door"] {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            for aspect in [16.0 / 9.0, 21.0 / 9.0] {
                let w = widened(image, &s, &state, Reach::new(aspect, 1.0), 60, assets, &mut cache, &mut comp, None).unwrap();
                assert_eq!((w.npc.in_margins, w.npc.unslotted_in_view, w.npc.misread), (0, 0, 0), "{name} at {aspect:.3}");
            }
        }
    });
}
