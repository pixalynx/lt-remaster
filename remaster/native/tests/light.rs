//! LT-112's criteria on the test set. They need the disc, a test set built from it (with the cave,
//! tower and lamps scenes) and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test light -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{bench, lighting_scene, Lighting};
use lt_native::composite::Compositor;
use lt_native::light::Area;
use lt_native::raster::Raster;
use lt_testset::{Image, Scene};

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

/// The disc, its assets and a GPU, for `f`.
fn with<R>(f: impl FnOnce(&Arc<Image>, &mut Assets, &mut Cache<Wgpu>, &mut Compositor, &mut Raster) -> R) -> R {
    let p = env("LT_DISC");
    let (image, disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().expect("a GPU");
    let mut comp = Compositor::new(&gpu);
    let mut raster = Raster::new(&gpu);
    let mut cache = Cache::new(gpu);
    f(&image, &mut assets, &mut cache, &mut comp, &mut raster)
}

fn measure(names: &[&str], every: u64) -> Vec<(String, Vec<Lighting>)> {
    with(|image, assets, cache, comp, raster| {
        names
            .iter()
            .map(|name| {
                let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
                (name.to_string(), lighting_scene(image, &s, &state, every, assets, cache, comp, raster).unwrap())
            })
            .collect()
    })
}

fn pct(r: &[Lighting], f: fn(&Lighting) -> u64) -> f64 {
    100.0 * r.iter().map(f).sum::<u64>() as f64 / r.iter().map(|m| m.native).sum::<u64>().max(1) as f64
}

/// @LT-112/1: the village (sun), an interior (room light) and two dungeons are lit, with real
/// shadows from the houses and props, and from the characters.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt112_1_village_interior_and_dungeon_lit_with_real_shadows() {
    let want = [("village", Area::Outdoor), ("interior", Area::Interior), ("cave", Area::Dungeon), ("tower", Area::Dungeon)];
    for (name, r) in measure(&want.map(|w| w.0), 240) {
        let area = want.iter().find(|w| w.0 == name).unwrap().1;
        assert!(r.len() >= 3, "{name}: ticks measured ({})", r.len());
        assert!(r.iter().all(|m| m.area == Some(area)), "{name}: lit as {area:?}");
        let (all, chars) = (pct(&r, |m| m.shadowed), pct(&r, |m| m.character_shadowed));
        assert!(all >= 5.0, "{name}: real shadows on {all:.2}% of the native pixels");
        assert!(chars > 0.02, "{name}: the characters' shadows on {chars:.3}%");
    }
}

/// @LT-112/2: lamps light their surroundings, found from the art with no data added by hand (the
/// inn's bar: its table lamps' painted light pools).
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt112_2_lamps_light_their_surroundings() {
    for (name, r) in measure(&["lamps"], 240) {
        assert!(r.len() >= 3, "{name}: ticks measured ({})", r.len());
        assert!(r.iter().all(|m| m.lamps >= 2), "{name}: lamps found ({:?})", r.iter().map(|m| m.lamps).collect::<Vec<_>>());
        let lit = pct(&r, |m| m.lamp_lit);
        assert!(lit >= 5.0, "{name}: the lamps brighten {lit:.2}% of the native pixels");
    }
}

/// @LT-112/3: the original look is LT-110's unlit image, pixel for pixel.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt112_3_the_original_look_is_the_unlit_image() {
    for (name, r) in measure(&["village", "interior", "cave", "tower", "lamps", "overworld"], 240) {
        let (n, eq): (u64, u64) = r.iter().fold((0, 0), |a, m| (a.0 + m.original_pixels, a.1 + m.original_equal));
        assert!(n > 100_000, "{name}: pixels compared ({n})");
        assert_eq!(eq, n, "{name}: {} pixels differ", n - eq);
    }
}

/// @LT-112/4: 60 fps at 1440p: the lit native view of every field tick, at the first scale with
/// at least 1,440 lines (7×: 1,568), from the capture resolved to the GPU done (as the app runs it),
/// takes a 60 Hz frame (16.7 ms) or less on average in each scene.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU (timing: the M1 Pro, nothing else busy)"]
fn lt112_4_sixty_fps_at_1440p() {
    let scale = 1440u32.div_ceil(224);
    with(|image, assets, cache, comp, _| {
        for name in ["village", "overworld", "interior", "dungeon", "cave", "tower", "lamps"] {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            let ms = bench(image, &s, &state, 2, scale, assets, cache, comp).unwrap();
            assert!(ms.len() >= 100, "{name}: ticks timed ({})", ms.len());
            let mean = ms.iter().sum::<f64>() / ms.len() as f64;
            assert!(mean <= 1000.0 / 60.0, "{name}: {mean:.2} ms a tick on average");
        }
    });
}
