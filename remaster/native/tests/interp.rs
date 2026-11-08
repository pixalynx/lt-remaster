//! LT-113's criteria on the test set. They need the disc and a test set built from it (with the
//! door scene); no GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test interp -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{lockstep_player, motion, Motion};
use lt_native::interp::{between, Between, Snap, EYE_CUT};
use lt_native::play::{Session, Tick};
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

fn measure(names: &[&str], k: u32) -> Vec<(String, Motion)> {
    with(|image, assets| {
        names
            .iter()
            .map(|name| {
                let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
                (name.to_string(), motion(image, &s, &state, k, assets).unwrap())
            })
            .collect()
    })
}

/// The first `n` ticks of a scene with the field drawn.
fn ticks(name: &str, n: usize, assets: &mut Assets, image: &Arc<Image>) -> Vec<Tick> {
    let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
    let mut session = Session::new(image, &s, &state).unwrap();
    let mut out = Vec::new();
    for &pad in &s.tape {
        for (t, _) in session.frame(pad, assets).unwrap() {
            if t.record.camera.is_some() && !t.table.passthrough {
                out.push(t);
            }
        }
        if out.len() >= n {
            break;
        }
    }
    out
}

/// @LT-113/1: walking (the village, the overworld) and camera turns (flight) move every frame at 60
/// and 120 Hz, evenly within each tick; the game's timing is unchanged with the native view's hooks
/// (lockstep).
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt113_1_smooth_at_60_and_120_with_game_timing_unchanged() {
    for k in [2, 4] {
        for (name, m) in measure(&["village", "overworld", "flight"], k) {
            assert!(m.blended as f64 >= 0.98 * m.pairs as f64, "{name} at {k}: {} of {} pairs blended", m.blended, m.pairs);
            assert!(m.moving_frames > 0 || m.turns > 0, "{name} at {k}: nothing moved");
            assert_eq!(m.repeated, 0, "{name} at {k}: frames shown twice while moving");
            assert!(m.uneven <= 1.25, "{name} at {k}: a tick's steps differ by {:.3}×", m.uneven);
            if name == "flight" {
                assert!(m.turns > 100, "flight at {k}: camera turns ({})", m.turns);
                assert!(m.turn_uneven <= 1.25, "flight at {k}: a turn's steps differ by {:.3}×", m.turn_uneven);
            }
        }
    }
    let image = Image::open(&env("LT_DISC")).unwrap();
    for name in ["village", "flight"] {
        let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
        let (r, ticks) = lockstep_player(&image, &s, &state).unwrap();
        assert!(r.divergence.is_none(), "{}", r.text());
        assert!(ticks > 500, "{name}: ticks captured ({ticks})");
    }
}

/// @LT-113/2: warps, doors and cutscene cuts snap with no frame in between; walking doesn't.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt113_2_warps_doors_and_cuts_snap() {
    // The way out of the house in the opening: every area change snaps.
    for (name, m) in measure(&["door"], 2) {
        assert!(m.area_changes >= 1, "{name}: area changes ({})", m.area_changes);
        assert_eq!(m.area_changes_snapped, m.area_changes, "{name}: {m:?}");
    }
    // Walking about snaps only for a change of mode (the field menu, a battle's start).
    for (name, m) in measure(&["village", "overworld", "interior", "dungeon", "cave", "tower", "late"], 2) {
        assert_eq!(m.snaps[2] + m.snaps[3], 0, "{name}: snapped while walking: {:?}", m.snaps);
    }
    with(|image, assets| {
        // A warp: the village's last tick, then an interior.
        let a = ticks("village", 1, assets, image).remove(0);
        let b = ticks("interior", 1, assets, image).remove(0);
        assert!(matches!(between((&a.record, &a.frame), (&b.record, &b.frame), 0.5), Between::Snap(Snap::NoSharedChunk)));
        // A cut: the same place, the camera put elsewhere between two ticks (moved along its view, Z:
        // the 512 mode stretches X).
        let v = ticks("village", 2, assets, image);
        let mut cut = v[1].record.clone();
        let cam = cut.camera.as_mut().unwrap();
        cam.view.t[2] += EYE_CUT as i32 + 100;
        // Every draw's model-view moves with the camera.
        let mut frame = v[1].frame.clone();
        for c in &mut frame.chunks {
            c.model_view.t[2] += EYE_CUT as i32 + 100;
        }
        for ch in &mut frame.characters {
            for p in &mut ch.parts {
                p.model_view.t[2] += EYE_CUT as i32 + 100;
            }
        }
        assert!(matches!(between((&v[0].record, &v[0].frame), (&cut, &frame), 0.5), Between::Snap(Snap::CameraCut)));
        assert!(matches!(between((&v[0].record, &v[0].frame), (&v[1].record, &v[1].frame), 0.5), Between::Pose(_)));
    });
}

/// @LT-113/3: the characters' morph animation blends vertex by vertex, and every tick's frame is
/// that tick's pose (nothing pops at a tick).
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt113_3_morph_animation_blends_with_no_popping() {
    for (name, m) in measure(&["village", "interior", "dungeon", "late", "cutscene"], 2) {
        assert!(m.parts > 0, "{name}: no parts");
        assert!(m.morphed as f64 >= 0.98 * m.parts as f64, "{name}: {} of {} parts blended", m.morphed, m.parts);
        assert_eq!(m.snapped_characters, 0, "{name}: characters drawn without blending");
    }
    with(|image, assets| {
        let v = ticks("village", 40, assets, image);
        let mut checked = 0;
        for w in v.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            let (Between::Pose(half), Between::Pose(end)) = (between((&a.record, &a.frame), (&b.record, &b.frame), 0.5), between((&a.record, &a.frame), (&b.record, &b.frame), 1.0)) else { continue };
            for (ci, cb) in b.frame.characters.iter().enumerate() {
                let Some(ca) = a.frame.characters.iter().find(|c| c.index == cb.index) else { continue };
                for (pi, (pa, pb)) in ca.parts.iter().zip(&cb.parts).enumerate() {
                    if pa.vertices.len() != pb.vertices.len() {
                        continue;
                    }
                    // Halfway: each vertex the average of the two ticks'.
                    for ((x, y), h) in pa.vertices.iter().zip(&pb.vertices).zip(&half.parts[ci][pi].1) {
                        for i in 0..3 {
                            assert!((h[i] - (x[i] as f32 + y[i] as f32) / 2.0).abs() < 1e-3);
                        }
                    }
                    // At the tick: tick B's pose exactly.
                    for (y, e) in pb.vertices.iter().zip(&end.parts[ci][pi].1) {
                        assert_eq!(y.map(|v| v as f32), *e);
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 50, "parts checked ({checked})");
    });
}
