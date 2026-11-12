//! LT-143's criteria on the test set: the first flight's sky scene (OVR24). They need the disc and a
//! test set built from it (with the first-flight scene); the second a GPU too:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test sky -- --ignored --test-threads=1`

use std::path::PathBuf;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::{Assets, MeshKey};
use lt_native::check::{field_scene, lighting_scene, Field};
use lt_native::composite::Compositor;
use lt_native::draw;
use lt_native::gate::{self, COLOUR_CLOSE};
use lt_native::gte::Precision;
use lt_native::light::{self, Area, Look, Normals};
use lt_native::play::{native_at, native_look, Session};
use lt_native::wide::Reach;
use lt_native::raster::Raster;
use lt_scene::check::capture_scene;
use lt_scene::{Drawer, Matrix, SKY_CHUNKS, SKY_MODEL_INDEX};
use lt_testset::{Image, Scene, SCENES};

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

fn pct(a: u64, b: u64) -> f64 {
    100.0 * a as f64 / b.max(1) as f64
}

/// @LT-143/1: the capture records OVR24's terrain cells and its model each tick. Every tick has the
/// camera (the view at ctx+0x98, H, the screen offset and mode: the scene switches between 512
/// and 320 wide), the cells drawn, each placed in the map
/// (its type's record holds the lists it was drawn with, and its offset under the view is the
/// GTE's translation), and the GTE's lighting (the one normal, the far colour). The task's model
/// (the Baby's glow) is recorded with its world matrix and posed vertices. lt-assets resolves
/// every cell to a mesh decoded from RAM.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt143_1_the_capture_records_the_cells_and_the_model_each_tick() {
    let p = env("LT_DISC");
    let image = Image::open(&p).unwrap();
    let (s, state) = Scene::load(&env("LT_TESTSET"), "first-flight").unwrap();
    let records = capture_scene(&image, &s, &state).unwrap().0;
    let sky: Vec<_> = records.iter().filter(|r| !r.sky.is_empty()).collect();
    let cells: usize = sky.iter().map(|r| r.sky.len()).sum();
    let placed = sky
        .iter()
        .flat_map(|r| {
            let view = r.camera.as_ref().unwrap().view;
            r.sky.iter().map(move |c| (view, c))
        })
        .filter(|(view, c)| c.kind != 0xFFFF && c.model_view.m == view.m && c.model_view.t == if c.offset == [0; 3] { view.t } else { view.place(c.offset) })
        .count();
    let glow: Vec<_> = records.iter().flat_map(|r| r.characters.iter().filter(|c| c.by == Drawer::Flight)).collect();
    println!(
        "first-flight: {} of {} ticks with sky cells; {cells} cells, {placed} placed; {} glow draws; other models {}",
        sky.len(),
        records.len(),
        glow.len(),
        records.iter().map(|r| r.other_models).sum::<u32>()
    );
    assert!(sky.len() * 100 >= records.len() * 95 && sky.len() > 1000, "cells on {} of {} ticks", sky.len(), records.len());
    assert_eq!(placed, cells, "every cell placed in the map");
    let wide = sky.iter().filter(|r| r.camera.as_ref().is_some_and(|c| c.wide)).count();
    println!("first-flight: {wide} ticks at 512 wide, {} at 320", sky.len() - wide);
    assert!(wide > 0 && wide < sky.len(), "both screen modes met");
    for r in &sky {
        let cam = r.camera.as_ref().expect("a camera");
        assert!(cam.h > 0 && cam.offset[0] == if cam.wide { 257 } else { 160 }, "the camera: {cam:?}");
        let g = r.sky_gte.as_ref().expect("the GTE's lighting");
        assert_eq!(g.normal, [0, -6000, 0], "the one normal");
        assert_eq!([g.control[21], g.control[22], g.control[23]], [0xF00; 3], "the far colour, 0xF0 grey");
    }
    assert!(glow.len() > 300, "the glow drawn ({} ticks)", glow.len());
    for c in &glow {
        assert_eq!((c.index, c.parts.len()), (SKY_MODEL_INDEX, 1), "one part (the second is empty)");
        let part = &c.parts[0];
        assert_eq!(part.vertices.len(), 125, "posed vertices");
        assert_eq!(part.world.t.map(|v| v as i16), c.position, "placed at its world matrix");
        assert_ne!(part.world, Matrix::default());
    }
    assert_eq!(records.iter().map(|r| r.other_models).sum::<u32>(), 0, "every Model_DrawParts call recorded");

    // lt-assets resolves every cell: a chunk use with the cell's id and a mesh read from RAM.
    let disc = Disc::open(&p).unwrap();
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let mut s2 = Session::new(&image, &s, &state).unwrap();
    let (mut ticks, mut uses, mut drawn) = (0, 0, 0);
    for &pad in s.tape.iter().take(600) {
        for (tick, _) in s2.frame(pad, &mut assets).unwrap() {
            ticks += 1;
            drawn += tick.record.sky.len();
            uses += tick.frame.chunks.iter().filter(|c| c.chunk & 0xF000 == SKY_CHUNKS && matches!(c.mesh, MeshKey::Sky(_))).count();
            assert!(tick.frame.misses.is_empty(), "misses: {:?}", tick.frame.misses);
            assert!(!tick.frame.characters.iter().any(|c| c.index == SKY_MODEL_INDEX), "the glow is the 2D layer's");
        }
    }
    println!("first-flight: {ticks} ticks resolved, {uses} of {drawn} cells");
    assert!(drawn > 2000 && uses == drawn);
}

/// @LT-143/2: the native view draws the scene lit, and the coverage checks pass. LT-110's gate
/// (the tick's geometry against the native raster) agrees on the cells; where both draw, the
/// original look has the game's colours (its NCDS lighting and depth cue, run on PSoXide's GTE)
/// as closely as the field's do; LT-111's check passes (coverage and the 2D layer). The lit look
/// lights the cells with the outdoor rig and their geometry's normals (the game's NCDS lights
/// every polygon with one normal, `D_80135058`), and fogs them by the game's depth cue toward
/// what it fogs to; at 16:9 the sky reaches the margins.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt143_2_the_native_view_draws_the_sky_lit_and_the_coverage_checks_pass() {
    let p = env("LT_DISC");
    let (image, disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().expect("a GPU");
    let mut comp = Compositor::new(&gpu);
    let mut raster = Raster::new(&gpu);
    let mut cache = Cache::new(gpu);
    let (s, state) = Scene::load(&env("LT_TESTSET"), "first-flight").unwrap();

    // LT-110's gate, and the colours, against the village's.
    let colour = |r: &gate::SceneResult| {
        let (p, c) = r.samples.iter().fold((0, 0), |a, s| (a.0 + s.colour_pixels, a.1 + s.colour_close));
        pct(c, p)
    };
    let g = gate::scene(&image, "first-flight", &s, &state, 120, &mut assets, &mut cache, &mut raster, Precision::Gte, false).unwrap();
    let (vs, vstate) = Scene::load(&env("LT_TESTSET"), "village").unwrap();
    let v = gate::scene(&image, "village", &vs, &vstate, 120, &mut assets, &mut cache, &mut raster, Precision::Gte, false).unwrap();
    println!(
        "first-flight: gate {} ticks, {:.3}% agree over {} pixels; colour within {COLOUR_CLOSE} on {:.3}% (the village {:.3}%)",
        g.samples.len(),
        g.agreement() * 100.0,
        g.compared(),
        colour(&g),
        colour(&v)
    );
    assert!(g.samples.len() >= 15, "ticks compared: {}", g.samples.len());
    assert!(g.agreement() >= 0.99, "coverage {:.3}%", g.agreement() * 100.0);
    assert!(colour(&g) >= 90.0 && colour(&g) >= colour(&v), "colours as close as the field's");

    // LT-111's check.
    let r: Vec<Field> = field_scene(&image, &s, &state, 120, &mut assets, &mut cache, &mut comp).unwrap();
    let sum = |f: fn(&Field) -> u64| r.iter().map(f).sum::<u64>();
    let (cov, lay) = (pct(sum(|f| f.covered_agree), sum(|f| f.pixels)), pct(sum(|f| f.layer_equal), sum(|f| f.layer_pixels)));
    println!("first-flight: coverage {cov:.3}%, 2D layer {lay:.3}% over {} ticks", r.len());
    assert!(r.len() >= 15 && cov >= 99.0 && lay >= 99.0);

    // Lit: the outdoor rig, the lit look unlike the original on the native pixels, the fog.
    let lit = lighting_scene(&image, &s, &state, 240, &mut assets, &mut cache, &mut comp, &mut raster).unwrap();
    assert!(lit.len() >= 8 && lit.iter().all(|m| m.area == Some(Area::Outdoor)), "lit as outdoors");
    let mut session = Session::new(&image, &s, &state).unwrap();
    let (mut native, mut changed, mut fogged, mut polys, mut samples) = (0u64, 0u64, 0u64, 0u64, 0);
    let (mut top, mut unpainted, mut width) = (0u64, 0u64, 0u32);
    let mut normals = Normals::default();
    for (i, &pad) in s.tape.iter().enumerate() {
        for (tick, shown) in session.frame(pad, &mut assets).unwrap() {
            if i % 240 != 120 || tick.record.sky.is_empty() {
                continue;
            }
            samples += 1;
            let a = native_look(&tick, &shown, &assets, &mut cache, &mut comp, 1, Look::Lit);
            let o = native_look(&tick, &shown, &assets, &mut cache, &mut comp, 1, Look::Original);
            for k in 0..(a.w * a.h).min(o.w * o.h) as usize {
                if a.native[k] {
                    native += 1;
                    changed += u64::from(a.rgba[k * 4..k * 4 + 3] != o.rgba[k * 4..k * 4 + 3]);
                }
            }
            // At 16:9 the sky (the 2D layer's gradient) reaches the margins: the top rows are sky
            // from edge to edge.
            let w = native_at(&tick, &shown, &assets, &mut cache, &mut comp, 1, Look::Lit, Reach::new(16.0 / 9.0, 1.0));
            width = w.w;
            for y in 2..6 {
                for x in 0..w.w {
                    let k = ((y * w.w + x) * 4) as usize;
                    top += 1;
                    unpainted += u64::from(w.rgba[k..k + 3] == [0, 0, 0]);
                }
            }
            let scene = draw::lit(&tick.record, &tick.frame, &assets, Precision::Gte, &mut normals);
            let sky: Vec<&draw::Vertex> = scene.batches.iter().filter(|b| !b.model).flat_map(|b| b.corners.iter()).collect();
            polys += sky.len() as u64;
            fogged += sky.iter().filter(|v| v.fog > 0.0).count() as u64;
            assert_eq!(light::sky_fog(&tick.record), [240.0 / 255.0; 3], "the fog's colour");
        }
    }
    println!("first-flight: lit on {:.2}% of {native} native pixels over {samples} ticks; fog on {:.2}% of {polys} corners", pct(changed, native), pct(fogged, polys));
    assert!(samples >= 8 && pct(changed, native) >= 90.0, "the lit look lights the scene");
    assert!(fogged > 0 && fogged < polys, "near cells clear, far ones fogged");
    println!("first-flight: at 16:9 ({width} wide), {unpainted} of {top} top-row pixels unpainted");
    assert!(width > 320 && top > 0 && unpainted == 0, "the sky reaches the margins");
}

/// @LT-143/3: a test set scene covers it: "first-flight", the overworld with story flag 0xBA
/// cleared and root mode 28, plays OVR24 from its first tick to its last.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt143_3_a_test_set_scene_plays_the_first_flight() {
    assert!(SCENES.contains(&"first-flight"));
    let image = Image::open(&env("LT_DISC")).unwrap();
    let (s, state) = Scene::load(&env("LT_TESTSET"), "first-flight").unwrap();
    assert!(s.tape.len() >= 2400, "{} frames", s.tape.len());
    let records = capture_scene(&image, &s, &state).unwrap().0;
    let with = records.iter().filter(|r| !r.sky.is_empty()).count();
    let kinds: std::collections::BTreeSet<u16> = records.iter().flat_map(|r| r.sky.iter().map(|c| c.kind)).collect();
    println!("first-flight: {with} of {} ticks draw the sky, {} cell types", records.len(), kinds.len());
    assert!(with * 100 >= records.len() * 95);
    assert_eq!(kinds.len(), 16, "every cell type of the map");
}
