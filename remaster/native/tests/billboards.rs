//! LT-144's criteria on the test set. They need the disc, a test set built from it and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test billboards -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{billboards, billboards_at, field_scene, Billboarded, Field};
use lt_native::composite::Compositor;
use lt_native::smoke::REBUILT;
use lt_native::wide::Reach;
use lt_testset::{Image, Scene};

/// Every field scene in the test set.
const FIELD: &[&str] = &["village", "overworld", "interior", "dungeon", "late", "flight", "cutscene", "menu", "cave", "door", "lamps", "tower"];

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

fn count(m: &std::collections::BTreeMap<&'static str, u64>, k: &str) -> u64 {
    m.get(k).copied().unwrap_or(0)
}

const WIDE: f64 = 16.0 / 9.0;

/// @LT-144/1: sparkles, smoke and the flight shadow show in the widened margins at the native
/// resolution. At 16:9 and 2×:
/// - flight: the transform sparkles reach the margins (the game's 4:3 layer leaves them empty);
///   they, the trail and the flight shadow are billboards with sub-pixel corners;
/// - the village with the Knight east of a house (no tape goes there): the game draws the
///   chimney's smoke 60-odd pixels past the 4:3 edge, and the view reaches the margin with it;
///   further east the game drops the emitter and the rebuilt smoke shows there. The rebuild is
///   the game's own puffs, corner for corner.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt144_1_sparkles_smoke_and_the_flight_shadow_in_the_margins() {
    with(|image, assets, cache, comp| {
        let set = env("LT_TESTSET");
        let (s, state) = Scene::load(&set, "flight").unwrap();
        let f = billboards(image, &s, &state, Reach::new(WIDE, 1.0), 30, assets, cache, comp, None).unwrap();
        println!("{}", f.text("flight"));
        assert!(count(&f.in_margins, "BabyTransform_DrawSparkles") > 0, "transform sparkles in the margins");
        assert!(count(&f.drawn, "func_801FB9E0") > 0 && count(&f.drawn, "FUN_801821ac") > 0, "the flight shadow and the trail drawn natively");
        assert!(f.margin_layer_px > 10_000 && f.margin_layer_px_game == 0, "margin pixels {} (game's {})", f.margin_layer_px, f.margin_layer_px_game);
        sub_pixel(&f);

        let (s, state) = Scene::load(&set, "village").unwrap();
        let drawn = billboards_at(image, &s, &state, Some([1200, 600]), Reach::new(WIDE, 1.0), 120, assets, cache, comp, None).unwrap();
        println!("{}", drawn.text("village, Knight at 1200, 600"));
        assert!(count(&drawn.in_margins, "Particle_BuildBillboardPrim") > 0, "the game's smoke in the margins");
        assert!(drawn.margin_layer_px > 1_000 && drawn.margin_layer_px_game == 0);
        assert!(drawn.puff_corners > 1_000 && drawn.puff_corners_same == drawn.puff_corners, "rebuild {}/{}", drawn.puff_corners_same, drawn.puff_corners);
        sub_pixel(&drawn);

        let past = billboards_at(image, &s, &state, Some([1300, 600]), Reach::new(WIDE, 1.0), 120, assets, cache, comp, None).unwrap();
        println!("{}", past.text("village, Knight at 1300, 600"));
        assert_eq!(count(&past.drawn, "Particle_BuildBillboardPrim"), 0, "the game drops the emitter");
        assert!(count(&past.in_margins, REBUILT) > 0, "the rebuilt smoke in the margins");
        assert!(past.margin_layer_px > 1_000 && past.margin_layer_px_game == 0);
        sub_pixel(&past);
    })
}

fn sub_pixel(b: &Billboarded) {
    assert!(b.quads > 0);
    assert_eq!(b.sub_pixel, b.quads, "every billboard with sub-pixel corners");
    assert_eq!(b.missed, 0, "billboard packets drawn otherwise");
}

/// @LT-144/2: with shadows on, the blob shadows under characters are gone; with shadows off or in
/// the original look they are kept. Every field scene's ticks, by look.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt144_2_blob_shadows_by_look_on_every_field_scene() {
    with(|image, assets, cache, comp| {
        let mut blobs = 0;
        for name in FIELD {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            let b = billboards(image, &s, &state, Reach::new(WIDE, 1.0), 60, assets, cache, comp, None).unwrap();
            assert_eq!(b.blobs_lit, 0, "{name}: blob shadows drawn with real shadows on");
            assert_eq!((b.blobs_lit_off, b.blobs_original), (b.blobs, b.blobs), "{name}: blob shadows kept with shadows off and in the original look");
            blobs += b.blobs;
        }
        assert!(blobs > 100, "blob shadows in the field scenes ({blobs})");
    })
}

fn pct(a: u64, b: u64) -> f64 {
    100.0 * a as f64 / b.max(1) as f64
}

/// @LT-144/3: the coverage check (LT-111's) still passes on every field scene, the takeover with
/// its billboard hooks: no holes, no doubled geometry.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt144_3_the_coverage_check_passes_on_every_field_scene() {
    with(|image, assets, cache, comp| {
        for name in FIELD {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            let r: Vec<Field> = field_scene(image, &s, &state, 120, assets, cache, comp).unwrap();
            assert!(r.len() >= 3, "{name}: field ticks measured ({})", r.len());
            let sum = |f: fn(&Field) -> u64| r.iter().map(f).sum::<u64>();
            let cov = pct(sum(|f| f.covered_agree), sum(|f| f.pixels));
            let lay = pct(sum(|f| f.layer_equal), sum(|f| f.layer_pixels));
            println!("{name}: coverage {cov:.3}%, 2D layer {lay:.3}%");
            assert!(cov >= 99.0, "{name}: coverage {cov:.3}%");
            assert!(lay >= 99.0, "{name}: 2D layer {lay:.3}% of {}", sum(|f| f.layer_pixels));
        }
    })
}
