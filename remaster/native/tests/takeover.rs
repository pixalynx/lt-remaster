//! LT-111's criteria on the test set. They need the disc, a test set built from it and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test takeover -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{field_scene, lockstep, look, Field, Look};
use lt_native::composite::Compositor;
use lt_testset::{Image, Scene};

const FIELD: &[&str] = &["village", "overworld", "interior", "dungeon", "late", "flight", "cutscene", "menu"];

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

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

/// Every field scene measured, sampled every `every` frames.
fn fields(every: usize) -> Vec<(&'static str, Vec<Field>)> {
    with(|image, assets, cache, comp| {
        FIELD
            .iter()
            .map(|name| {
                let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
                (*name, field_scene(image, &s, &state, every, assets, cache, comp).unwrap())
            })
            .collect()
    })
}

fn pct(a: u64, b: u64) -> f64 {
    100.0 * a as f64 / b.max(1) as f64
}

/// @LT-111/1: the field scenes with native terrain and characters and the emulated 2D. No holes:
/// the native view covers the pixels the game draws. No doubled geometry: the 2D layer alone is
/// the game's picture with the terrain and characters not drawn.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt111_1_native_terrain_and_characters_with_no_holes_or_doubles() {
    for (name, r) in fields(120) {
        assert!(r.len() >= 3, "{name}: field ticks measured ({})", r.len());
        assert!(r.iter().all(|f| f.aside > 0), "{name}: every tick takes terrain or characters over");
        let sum = |f: fn(&Field) -> u64| r.iter().map(f).sum::<u64>();
        let cov = pct(sum(|f| f.covered_agree), sum(|f| f.pixels));
        let lay = pct(sum(|f| f.layer_equal), sum(|f| f.layer_pixels));
        assert!(cov >= 99.0, "{name}: coverage {cov:.3}%");
        assert!(lay >= 99.0, "{name}: 2D layer {lay:.3}% of {}", sum(|f| f.layer_pixels));
    }
}

/// @LT-111/2: effects (torch flames, the sparkle trail, shadows) show where the game shows them,
/// in front of and behind the native geometry.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt111_2_effects_sort_with_native_geometry() {
    let mut effects = 0;
    for (name, r) in fields(120) {
        let sum = |f: fn(&Field) -> u64| r.iter().map(f).sum::<u64>();
        effects += sum(|f| f.effects as u64);
        if sum(|f| f.effect_pixels) == 0 {
            continue;
        }
        let eff = pct(sum(|f| f.effect_agree), sum(|f| f.effect_pixels));
        assert!(eff >= 99.0, "{name}: effects agree {eff:.3}% of {}", sum(|f| f.effect_pixels));
    }
    assert!(effects > 0, "the field scenes have effect packets");
}

/// @LT-111/3: dialogue and menus at 2× look like the game's, and movies pass through unchanged.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt111_3_dialogue_menus_and_movies_at_2x() {
    with(|image, assets, cache, comp| {
        let mut each = |name: &str| -> Look {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            look(image, &s, &state, 120, 2, assets, cache, comp).unwrap()
        };
        for name in ["menu", "cutscene"] {
            let l = each(name);
            assert!(l.layer_pixels > 1000, "{name}: 2D-only pixels compared ({})", l.layer_pixels);
            let p = pct(l.layer_equal, l.layer_pixels);
            assert!(p >= 99.0, "{name}: {p:.3}% of {} 2D-only pixels within a step", l.layer_pixels);
        }
        let boot = each("boot");
        assert!(boot.passed > 0, "the boot scene shows a movie");
        assert_eq!(boot.passed_equal, boot.passed, "movie ticks passed through exactly");
    })
}

/// @LT-111/4: the takeover's hooks change no game state outside the masked packet buffers.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt111_4_lockstep_with_the_takeover() {
    let image = Image::open(&env("LT_DISC")).unwrap();
    for name in FIELD {
        let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
        let (r, t) = lockstep(&image, &s, &state).unwrap();
        assert!(r.divergence.is_none(), "{}", r.text());
        assert!(r.frames as usize >= s.tape.len(), "{name}: the whole tape ({} frames)", r.frames);
        assert!(t.tables > 0 && t.terrain > 0 && t.characters > 0, "{name}: {t:?}");
    }
}
