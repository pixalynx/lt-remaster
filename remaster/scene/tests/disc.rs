//! LT-108's acceptance tests on the player's disc and test set. They need both (and, for AC2, a
//! `draw_hooks.rs` log), so they're ignored by default:
//!
//! ```text
//! cargo run --release --example draw_hooks -- <disc> <dir> 4300 120 $(lt-scene args)   # in emu-host/
//! LT_DISC=<disc> LT_TESTSET=<set> LT_DRAW_HOOKS=<dir>/hooks.jsonl cargo test --release -p lt-scene -- --ignored
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use lt_scene::check::{capture_from_boot, capture_scene, compare, completeness, plain_scene, stability};
use lt_testset::{Image, Scene};

fn disc() -> Arc<Image> {
    Image::open(&PathBuf::from(std::env::var("LT_DISC").expect("LT_DISC: the retail USA disc image"))).expect("disc")
}

fn village() -> (Scene, Vec<u8>) {
    let set = PathBuf::from(std::env::var("LT_TESTSET").expect("LT_TESTSET: a test set from `lt-testset build`"));
    Scene::load(&set, "village").expect("village scene")
}

/// @LT-108/1: each tick of the village scene yields one record with the camera, 3 chunk slots,
/// the groups drawn and every visible character with its matrix and vertices.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt108_1_one_complete_record_per_village_tick() {
    let image = disc();
    let (scene, state) = village();
    let (records, _) = capture_scene(&image, &scene, &state).unwrap();
    assert_eq!(records.len(), scene.tape.len() / 2);
    assert_eq!(completeness(&records, Some(3)), Vec::<String>::new());
    let r = &records[0];
    let cam = r.camera.as_ref().unwrap();
    assert_eq!((cam.h, cam.offset, cam.wide), (892, [257, 112], true));
    assert!(r.characters.iter().any(|c| c.index == 0), "the Knight is drawn");
}

/// @LT-108/2: the records match draw_hooks.rs's log of the same frames, tick for tick.
#[test]
#[ignore = "needs LT_DISC and LT_DRAW_HOOKS (draw_hooks.rs 4300 120 with lt-scene's args)"]
fn lt108_2_records_match_the_draw_hooks_log() {
    let log = std::fs::read_to_string(std::env::var("LT_DRAW_HOOKS").expect("LT_DRAW_HOOKS")).unwrap();
    let records = capture_from_boot(&disc(), 4300, 120).unwrap();
    let (ticks, diffs) = compare(&records, &log).unwrap();
    assert_eq!(ticks, 60);
    assert_eq!(diffs, Vec::<String>::new());
}

/// @LT-108/3: ids stay the same across ticks for the same chunk group and character.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt108_3_ids_are_stable_across_ticks() {
    let image = disc();
    let (scene, state) = village();
    let (records, _) = capture_scene(&image, &scene, &state).unwrap();
    let (problems, (ga, gn), (ca, cn)) = stability(&records);
    assert_eq!(problems, Vec::<String>::new());
    assert!(ga * 100 > gn * 95 && ca * 100 > cn * 95, "groups {ga}/{gn}, characters {ca}/{cn} carry over");
}

/// @LT-108/4: the capture adds under 1 ms per tick (release build).
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET; run in release"]
fn lt108_4_capture_costs_under_a_millisecond_per_tick() {
    let image = disc();
    let (scene, state) = village();
    let (mut plain, mut with, mut ticks) = (f64::MAX, f64::MAX, 1);
    for _ in 0..3 {
        plain = plain.min(plain_scene(&image, &scene, &state).unwrap().as_secs_f64());
        let (records, t) = capture_scene(&image, &scene, &state).unwrap();
        with = with.min(t.as_secs_f64());
        ticks = records.len();
    }
    let added_ms = (with - plain) / ticks as f64 * 1000.0;
    assert!(added_ms < 1.0, "{added_ms:.3} ms per tick");
}
