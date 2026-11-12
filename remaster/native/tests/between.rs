//! LT-145's criteria on the test set: the field's billboards in the frames between ticks. They need
//! the disc and a test set built from it (with the door scene); the second and third a GPU too:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test between -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::billboard::FAR_FLAGS;
use lt_native::check::{field_scene, moving, Field};
use lt_native::composite::{Compositor, Picture};
use lt_native::interp::{Snap, EYE_CUT};
use lt_native::layer2d;
use lt_native::light::Look;
use lt_native::play::{pose_between, view, view_between, Session, Tick, View};
use lt_native::takeover::Shown;
use lt_native::wide::Reach;
use lt_testset::{Image, Scene};

/// Every field scene in the test set.
const FIELD: &[&str] = &["village", "overworld", "interior", "dungeon", "late", "flight", "cutscene", "menu", "cave", "door", "lamps", "tower"];

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

/// The disc and its assets, for `f`.
fn with<R>(f: impl FnOnce(&Arc<Image>, &mut Assets) -> R) -> R {
    let p = env("LT_DISC");
    let (image, disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    f(&image, &mut assets)
}

/// The disc, its assets and a GPU, for `f`.
fn with_gpu<R>(f: impl FnOnce(&Arc<Image>, &mut Assets, &mut Cache<Wgpu>, &mut Compositor) -> R) -> R {
    with(|image, assets| {
        let gpu = Wgpu::headless().expect("a GPU");
        let mut comp = Compositor::new(&gpu);
        let mut cache = Cache::new(gpu);
        f(image, assets, &mut cache, &mut comp)
    })
}

/// @LT-145/1: at 60 and 120 fps (2 and 4 frames a tick) the field's billboards move with the
/// interpolated camera between ticks: walking (the village, the overworld) and flying. Every
/// billboard moving over a tick moves every frame, evenly; none jumps at a tick; the blob shadows
/// and the flight shadow stay under their characters (drawn from tick B's layer as before, they
/// were up to a tick's motion off); and the 2D layer draws every billboard quad at the frame's
/// corners.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt145_1_billboards_move_with_the_camera_between_ticks() {
    with(|image, assets| {
        for name in ["village", "overworld", "flight"] {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            for k in [2, 4] {
                let m = moving(image, &s, &state, k, assets).unwrap();
                println!("{}", m.text(name, k));
                assert!(m.posed as f64 >= 0.98 * m.pairs as f64 && m.pairs > 200, "{name} at {k}: {} of {} pairs posed", m.posed, m.pairs);
                assert!(m.billboards > 1000 && m.paired as f64 >= 0.8 * m.billboards as f64, "{name} at {k}: {} of {} billboards blended", m.paired, m.billboards);
                assert!(m.moving > 1000, "{name} at {k}: billboards moving ({})", m.moving);
                assert_eq!(m.repeated, 0, "{name} at {k}: billboards holding still for a frame while they move");
                assert!(m.uneven <= 1.25, "{name} at {k}: a billboard's steps differ by {:.3}×", m.uneven);
                assert!(m.pop_a < 0.01 && m.pop_b < 0.01, "{name} at {k}: a jump at the tick ({:.4}, {:.4} px)", m.pop_a, m.pop_b);
                assert!(m.shadows > 300, "{name} at {k}: shadows under characters ({})", m.shadows);
                assert!(m.shadow_stray < 0.1, "{name} at {k}: a shadow strays {:.3} px from its character", m.shadow_stray);
                assert!(m.shadow_stray_before > 1.0, "{name} at {k}: tick B's layer strayed {:.3} px", m.shadow_stray_before);
                assert_eq!(m.quads, m.quads_b, "{name} at {k}: billboard quads drawn between ticks");
                assert!(m.quads_moved as f64 >= 0.5 * m.quads as f64, "{name} at {k}: {} of {} quads moved", m.quads_moved, m.quads);
            }
        }
    })
}

fn read(v: View, comp: &mut Compositor) -> Picture {
    match v {
        View::Rendered(r) => comp.read(&r),
        View::Picture(p) => p,
    }
}

/// The billboard quads `tick`'s own 2D layer draws at 2×.
fn quads(tick: &Tick) -> usize {
    layer2d::layer_with(&tick.table, 2, layer2d::Options { float: true, ..layer2d::Options::default() }).quads.len()
}

/// `tick` halfway from `prev` against `tick` as it is, at 2× and lit: the snap, and whether the
/// two pictures are the same pixel for pixel.
#[allow(clippy::too_many_arguments)]
fn snapped(prev: &Tick, tick: &Tick, shown: &Shown, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> (Option<Snap>, bool) {
    let frame = lt_native::wide::frame(&tick.record, &tick.frame, &tick.crowd, assets, Reach::GAME);
    let (_, snap) = pose_between(Some(prev), tick, &frame, 0.5, assets, Reach::GAME);
    let (half, s) = view_between(Some(prev), tick, 0.5, shown, assets, cache, comp, 2, Look::Lit, Reach::GAME);
    assert_eq!(s, snap);
    let half = read(half, comp);
    let own = read(view(tick, shown, assets, cache, comp, 2, Look::Lit, Reach::GAME), comp);
    (snap, half.rgba == own.rgba)
}

/// The first `n` field ticks of a scene, each with what was shown.
fn ticks(image: &Arc<Image>, name: &str, n: usize, assets: &mut Assets) -> Vec<(Tick, Shown)> {
    let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
    let mut session = Session::new(image, &s, &state).unwrap();
    let mut out = Vec::new();
    for &pad in &s.tape {
        for (t, shown) in session.frame(pad, assets).unwrap() {
            if t.record.camera.is_some() && !t.table.passthrough && t.table.billboards.iter().any(|p| p.flags & FAR_FLAGS == 0) {
                out.push((t, shown));
            }
        }
        if out.len() >= n {
            break;
        }
    }
    out
}

/// @LT-145/2: a snap draws the billboards at the tick's positions. The doors in the opening (out of
/// the house), a warp (the village to an interior) and a cut (the camera put elsewhere between two
/// ticks): each frame between is the tick's own view, pixel for pixel, its billboards included.
/// (A single billboard put elsewhere is drawn where its tick has it: the library's
/// `lt145_2_a_billboard_put_elsewhere_is_drawn_where_tick_b_has_it`.)
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt145_2_snaps_draw_billboards_at_the_ticks_positions() {
    with_gpu(|image, assets, cache, comp| {
        // The doors: every snapped pair.
        let (s, state) = Scene::load(&env("LT_TESTSET"), "door").unwrap();
        let mut session = Session::new(image, &s, &state).unwrap();
        let (mut snaps, mut same, mut with_billboards, mut area_changes) = (Vec::new(), 0, 0, 0);
        let mut prev: Option<Tick> = None;
        // The area changes are against the last tick that drew the field (a door fades out between).
        let mut last_field: Option<(u16, Vec<u16>)> = None;
        for &pad in &s.tape {
            for (tick, shown) in session.frame(pad, assets).unwrap() {
                let field = tick.record.camera.is_some() && !tick.table.passthrough;
                if let (Some(a), true) = (prev.as_ref(), field) {
                    let frame = lt_native::wide::frame(&tick.record, &tick.frame, &tick.crowd, assets, Reach::GAME);
                    let area_change = last_field.as_ref().is_some_and(|(cur, drawn)| tick.record.chunk != *cur && !drawn.contains(&tick.record.chunk));
                    if let (None, Some(snap)) = pose_between(Some(a), &tick, &frame, 0.5, assets, Reach::GAME) {
                        let (_, eq) = snapped(a, &tick, &shown, assets, cache, comp);
                        snaps.push(snap);
                        same += eq as u32;
                        with_billboards += (quads(&tick) > 0) as u32;
                        area_changes += area_change as u32;
                    } else {
                        assert!(!area_change, "door: an area change blended");
                    }
                }
                if field {
                    last_field = Some((tick.record.chunk, tick.frame.chunks.iter().map(|c| c.chunk).collect()));
                }
                prev = Some(tick);
            }
        }
        println!("door: {} snaps {snaps:?} ({area_changes} area changes), {same} drawn as the tick, {with_billboards} with billboards", snaps.len());
        assert!(area_changes >= 1, "door: no area change snapped");
        assert_eq!(same as usize, snaps.len(), "door: snaps not drawn as the tick");
        assert!(with_billboards >= 1, "door: no billboards at a snap");

        // A warp: the village's tick, then an interior's.
        let (v, _) = ticks(image, "village", 1, assets).remove(0);
        let (i, shown) = ticks(image, "interior", 1, assets).remove(0);
        assert!(quads(&i) > 0);
        assert_eq!(snapped(&v, &i, &shown, assets, cache, comp), (Some(Snap::NoSharedChunk), true), "a warp");

        // A cut: two village ticks, the second's camera (and all it drew) moved along its view.
        let mut v = ticks(image, "village", 2, assets);
        let (b, shown) = v.pop().unwrap();
        let (a, _) = v.pop().unwrap();
        let dz = EYE_CUT as i32 + 100;
        let mut cut = Tick { record: b.record.clone(), frame: b.frame.clone(), table: b.table.clone(), crowd: b.crowd.clone() };
        cut.record.camera.as_mut().unwrap().view.t[2] += dz;
        for c in &mut cut.frame.chunks {
            c.model_view.t[2] += dz;
        }
        for ch in &mut cut.frame.characters {
            for p in &mut ch.parts {
                p.model_view.t[2] += dz;
            }
        }
        for p in &mut cut.table.billboards {
            for v in &mut p.view {
                v[2] += dz as f64;
            }
        }
        assert!(quads(&cut) > 0);
        assert_eq!(snapped(&a, &cut, &shown, assets, cache, comp), (Some(Snap::CameraCut), true), "a cut");
    })
}

fn pct(a: u64, b: u64) -> f64 {
    100.0 * a as f64 / b.max(1) as f64
}

/// @LT-145/3: the coverage check (LT-111's) still passes on every field scene; and a frame at a
/// tick (t = 1) is that tick's view, so what it measures is what the frames between end on.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt145_3_the_coverage_check_passes_on_every_field_scene() {
    with_gpu(|image, assets, cache, comp| {
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
        let mut v = ticks(image, "village", 2, assets);
        let (b, shown) = v.pop().unwrap();
        let (a, _) = v.pop().unwrap();
        let (end, snap) = view_between(Some(&a), &b, 1.0, &shown, assets, cache, comp, 2, Look::Lit, Reach::GAME);
        assert_eq!(snap, None);
        let end = read(end, comp);
        let own = read(view(&b, &shown, assets, cache, comp, 2, Look::Lit, Reach::GAME), comp);
        assert!(end.rgba == own.rgba, "t = 1 is the tick's view");
    })
}
