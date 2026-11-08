//! LT-105's acceptance tests on the player's disc and test set. They need both, so they're
//! ignored by default:
//!
//! ```text
//! LT_DISC="<Guardian's Crusade (USA).bin>" LT_TESTSET=<set dir> cargo test --release -p lt-lockstep -- --ignored
//! ```

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use lt_lockstep::gc::{self, KNIGHT, PLANT, TALK, X};
use lt_lockstep::{original, run, Mask, RENDER_MASKS};
use lt_testset::{Image, Scene, SCENES};

fn setup() -> (Arc<Image>, PathBuf) {
    let disc = std::env::var("LT_DISC").expect("LT_DISC: the retail USA disc image");
    let set = std::env::var("LT_TESTSET").expect("LT_TESTSET: a test set from `lt-testset build`");
    (Image::open(&PathBuf::from(disc)).expect("disc"), PathBuf::from(set))
}

/// @LT-105/1: two machines from one state and a tape, compared after every frame (two per tick).
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt105_1_two_machines_from_one_state_compared_every_frame() {
    let (image, set) = setup();
    let (scene, state) = Scene::load(&set, "village").unwrap();
    let r = run(&image, &scene, &state, &original, &original, &[]).unwrap();
    assert_eq!(r.frames, scene.tape.len() as u64);
    assert!(r.divergence.is_none(), "{}", r.text());
    assert_eq!(r.digest_a, scene.digest);
}

/// @LT-105/2: a planted one-byte difference is reported at the right tick, with its gc-sys field
/// name and the function (here a hook) that wrote it; and, masked, the store the game's own code
/// made of it is found by function name.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt105_2_a_planted_byte_is_found_named_and_traced() {
    let (image, set) = setup();
    let (scene, state) = Scene::load(&set, "village").unwrap();
    let fired = Rc::new(Cell::new(None));
    let r = run(&image, &scene, &state, &original, &gc::planter(KNIGHT + TALK, 0x01, 300, fired.clone()), &[]).unwrap();
    let d = r.divergence.as_ref().expect("the plant is found");
    assert_eq!(Some(d.frame), fired.get(), "{}", r.text());
    assert_eq!((d.tick, d.addr, d.a ^ d.b), (d.frame / 2, KNIGHT + TALK, 0x01));
    assert_eq!(d.field.as_deref(), Some("CHARACTERS[0].talk"));
    assert!(d.cause.what.starts_with(&format!("B: hook {PLANT} wrote it")), "{}", r.text());
    assert!(!d.cause.trace.is_empty());

    let fired = Rc::new(Cell::new(None));
    let masks = [Mask { name: "planted", addr: KNIGHT + X, len: 2, why: "the planted difference itself" }];
    let r = run(&image, &scene, &state, &original, &gc::planter(KNIGHT + X, 0x10, 300, fired.clone()), &masks).unwrap();
    let d = r.divergence.as_ref().expect("the game carries it on");
    assert!(d.frame >= fired.get().unwrap());
    let writer = d.cause.what.split(" stored it in ").nth(1).expect("a store");
    assert!(!writer.starts_with("0x"), "the writer has a decomp name: {}", r.text());
    assert!(r.masks[0].1 > 0);
}

/// @LT-105/3: observe-only hooks change nothing in any scene of the test set.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt105_3_observe_hooks_pass_the_whole_test_set() {
    let (image, set) = setup();
    let observe = gc::observer(gc::ovr02_words(&image, &set).unwrap());
    for name in SCENES {
        let (scene, state) = Scene::load(&set, name).unwrap();
        let r = run(&image, &scene, &state, &original, &observe, &[]).unwrap();
        assert!(r.divergence.is_none(), "{}", r.text());
        assert_eq!(r.frames, scene.tape.len() as u64);
        assert_eq!(r.digest_a, scene.digest, "{name}");
        assert!(r.hook_calls.iter().any(|(_, n)| *n > 0), "{name}: no hook fired");
    }
}

/// @LT-105/4: a masked run lists every mask with its reason and what differed in it.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt105_4_a_masked_run_lists_its_masks() {
    let (image, set) = setup();
    let (scene, state) = Scene::load(&set, "village").unwrap();
    let r = run(&image, &scene, &state, &original, &gc::terrain_skipper, RENDER_MASKS).unwrap();
    assert_eq!(r.masks.len(), RENDER_MASKS.len());
    for (m, (name, _, why)) in RENDER_MASKS.iter().zip(&r.masks) {
        assert_eq!((m.name, m.why), (*name, *why));
        assert!(r.text().contains(m.why));
    }
    assert!(r.masks.iter().any(|m| m.1 > 0), "{}", r.text());
}
