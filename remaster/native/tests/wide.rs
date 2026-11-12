//! LT-114's criteria on the test set. They need the disc, a test set built from it (15 scenes, with
//! the door) and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test wide -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{further, hud, widened, Further, Hud, Widened};
use lt_native::composite::Compositor;
use lt_native::wide::Reach;
use lt_testset::{Image, Scene};

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

/// Every scene of the test set with a field camera.
const FIELD: [&str; 12] = ["village", "overworld", "interior", "dungeon", "late", "flight", "cutscene", "menu", "cave", "tower", "lamps", "door"];

/// The disc, its assets and a GPU, for `f`.
fn with<R>(f: impl FnOnce(&Arc<Image>, &mut Assets, &mut Cache<Wgpu>, &mut Compositor) -> R) -> R {
    let p = env("LT_DISC");
    let (image, disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().expect("a GPU");
    let mut comp = Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    f(&image, &mut assets, &mut cache, &mut comp)
}

fn wide(names: &[&str], reach: Reach, every: u64) -> Vec<(String, Widened)> {
    with(|image, assets, cache, comp| {
        names
            .iter()
            .map(|name| {
                let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
                (name.to_string(), widened(image, &s, &state, reach, every, assets, cache, comp, None).unwrap())
            })
            .collect()
    })
}

/// @LT-114/1: at 16:9 and 21:9 every field scene draws what there is to the window's edges. The
/// objects are the game's own test (it picks exactly what the game drew, every tick) widened; the
/// margins miss nothing every object of every chunk near would cover; on the overworld grid (ground
/// everywhere) they have no empty corners, where the game's own chunks leave some (flight at 21:9).
/// A room's margins past its walls are its void.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt114_1_terrain_reaches_the_edges_at_16_9_and_21_9() {
    for aspect in [16.0 / 9.0, 21.0 / 9.0] {
        for (name, w) in wide(&FIELD, Reach::new(aspect, 1.0), 60) {
            let at = format!("{name} at {aspect:.3}");
            assert!(w.ticks > 5 && w.margin_px > 0, "{at}: measured {} ticks", w.ticks);
            assert_eq!(w.objects_agree, w.chunk_draws, "{at}: the object test differs from the game's");
            assert!(w.margin_missing as f64 <= 0.001 * w.margin_px as f64, "{at}: {} of {} margin pixels left empty that hold something", w.margin_missing, w.margin_px);
            assert!(w.grid_empty as f64 <= 0.001 * w.grid_px as f64, "{at}: {} of {} margin pixels on the grid empty", w.grid_empty, w.grid_px);
            if name == "flight" && aspect > 2.0 {
                assert!(w.margin_empty_naive > 100 * w.margin_empty.max(1), "flight at 21:9: the game's chunks alone leave {} empty, the view {}", w.margin_empty_naive, w.margin_empty);
            }
        }
    }
}

/// @LT-114/2: the draw distance goes past the game's neighbour chunks: more chunks and objects at
/// distance 3; and with a camera that looks further than the game's (turned up 30°, as a free
/// camera will), ground past the game's distance fills the picture.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt114_2_draw_distance_goes_past_the_games_chunks() {
    let reach = Reach::new(16.0 / 9.0, 3.0);
    for (name, w) in wide(&["village", "overworld", "flight"], reach, 60) {
        // The game's camera looks down: what's past its distance is mostly above the top edge, but
        // the chunks and objects in view past its four are drawn.
        assert!(w.chunks > w.game_chunks && w.objects > w.game_objects, "{name}: {} chunks (game {}), {} objects (game {})", w.chunks, w.game_chunks, w.objects, w.game_objects);
    }
    let fs: Vec<(String, Further)> = with(|image, assets, cache, comp| {
        ["village", "overworld"]
            .iter()
            .map(|name| {
                let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
                (name.to_string(), further(image, &s, &state, reach, 30.0, 120, assets, cache, comp, None).unwrap())
            })
            .collect()
    });
    for (name, f) in fs {
        assert!(f.ticks > 2 && f.chunks > f.chunks_game, "{name}: {} chunks at distance 3, {} at the game's", f.chunks, f.chunks_game);
        assert!(f.further_px as f64 > 0.2 * f.px as f64, "{name}: {} of {} pixels past the game's distance", f.further_px, f.px);
    }
}

/// @LT-114/3: characters in the widened view show (LT-119 chose option 2: `crate::crowd` poses the
/// NPCs, doors and chests without a packet slot from their records). At 21:9 every placed
/// character in the margins is drawn, the game's or the crowd's, and the records are read right.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt114_3_characters_in_the_margins_show() {
    let mut crowd = 0;
    for (name, w) in wide(&["village", "door", "dungeon", "cutscene", "cave"], Reach::new(21.0 / 9.0, 1.0), 60) {
        assert_eq!(w.npc.misread, 0, "{name}: drawn characters the records' reading calls undrawable");
        assert_eq!((w.npc.in_margins, w.npc.unslotted_in_view), (0, 0), "{name}: characters in view nothing draws");
        crowd += w.npc.crowd_margins;
    }
    assert!(crowd > 0, "no character stood in the margins to draw");
}

/// @LT-114/4: the HUD, dialogue and menus stay the game's 4:3 picture in the middle at 16:10, 16:9
/// and 21:9: the same pixels, at the same scale, and none in the margins. (The field's billboards
/// reach the margins since LT-144: the check keeps them to the 4:3.)
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt114_4_hud_and_dialogue_stay_put_at_each_aspect() {
    for aspect in [1.6, 16.0 / 9.0, 21.0 / 9.0] {
        let hs: Vec<(String, Hud)> = with(|image, assets, cache, comp| {
            ["cutscene", "menu", "village"]
                .iter()
                .map(|name| {
                    let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
                    (name.to_string(), hud(image, &s, &state, Reach::new(aspect, 1.0), 90, 2, assets, cache, comp, None).unwrap())
                })
                .collect()
        });
        for (name, h) in hs {
            assert!(h.layer_px > 10_000, "{name} at {aspect:.3}: {} 2D pixels", h.layer_px);
            assert_eq!(h.outside, 0, "{name} at {aspect:.3}: 2D layer pixels in the margins");
            assert!(h.same as f64 >= 0.999 * h.layer_px as f64, "{name} at {aspect:.3}: {} of {} 2D pixels the same", h.same, h.layer_px);
        }
    }
}
