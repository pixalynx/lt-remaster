//! LT-110's criteria on the test set. They need the disc, a test set built from it and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::gate::{save_png, scene, SceneResult, COVERED, EMULATED_ONLY, EXCLUDED, NATIVE_ONLY};
use lt_native::gte::Precision;
use lt_native::raster::Raster;
use lt_testset::{Image, Scene};

const FIELD: &[&str] = &["village", "overworld", "interior", "dungeon", "late", "flight", "cutscene", "menu"];

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

/// Every field scene through the gate, sampled every `every` frames.
fn gate(every: usize, why: bool) -> Vec<SceneResult> {
    let p = env("LT_DISC");
    let (image, disc): (Arc<Image>, Disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().expect("a GPU");
    let mut raster = Raster::new(&gpu);
    let mut cache = Cache::new(gpu);
    FIELD
        .iter()
        .map(|name| {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            scene(&image, name, &s, &state, every, &mut assets, &mut cache, &mut raster, Precision::Gte, why).unwrap()
        })
        .collect()
}

/// @LT-110/1: native and emulated coverage agree on at least 99 % of pixels in every field scene.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt110_1_coverage_agrees_on_every_field_scene() {
    for r in gate(60, false) {
        assert!(r.samples.len() >= 5, "{}: ticks compared ({})", r.name, r.samples.len());
        assert!(r.agreement() >= 0.99, "{}: {:.3}%", r.name, r.agreement() * 100.0);
    }
}

/// @LT-110/2: a report image per scene shows the mismatch in colour.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt110_2_a_report_image_per_scene() {
    let dir = std::env::temp_dir().join("lt110-reports");
    std::fs::create_dir_all(&dir).unwrap();
    for r in gate(240, false) {
        let w = r.worst().expect("a sample");
        let p = w.report.as_ref().expect("a report");
        let path = dir.join(format!("{}.png", r.name));
        save_png(&path, p).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
        // Four panels (emulated, native, mismatch, without the geometry); the mismatch panel uses
        // only the legend's colours, and shows covered pixels.
        let h = (p.h - 12) / 4;
        assert_eq!(p.h, h * 4 + 12, "{}", r.name);
        let legend = [COVERED, EMULATED_ONLY, NATIVE_ONLY, EXCLUDED, [0, 0, 0]];
        let mut covered = 0;
        for y in 2 * (h + 4)..2 * (h + 4) + h {
            for x in 0..p.w {
                let o = ((y * p.w + x) * 4) as usize;
                let c = [p.rgba[o], p.rgba[o + 1], p.rgba[o + 2]];
                assert!(legend.contains(&c), "{}: {c:?} at ({x}, {y})", r.name);
                covered += (c == COVERED) as u32;
            }
        }
        assert!(covered > 0, "{}", r.name);
    }
}

/// @LT-110/3: every disagreement has a cause, and only explained ones are left: the raster edge
/// rule (LT-138) and a rare texel the game has transparent. No missing draw path, no polygon the
/// native tests drop that the game draws.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt110_3_disagreements_are_explained() {
    for r in gate(120, true) {
        let mismatched: u64 = r.samples.iter().map(|s| s.emulated_only + s.native_only).sum();
        let explained: u64 = r.samples.iter().flat_map(|s| s.causes.values()).sum();
        assert_eq!(explained, mismatched, "{}: every mismatched pixel has a cause", r.name);
        let cut: u64 = r.samples.iter().flat_map(|s| s.causes.iter()).filter(|(c, _)| c.contains("cut-out")).map(|(_, n)| n).sum();
        assert!(cut * 10_000 <= r.compared(), "{}: {cut} cut-out pixels", r.name);
        for s in &r.samples {
            for (cause, n) in &s.causes {
                assert!(cause.contains("edge") || cause.contains("cut-out"), "{}: {n} pixels {cause}", r.name);
            }
        }
    }
}
