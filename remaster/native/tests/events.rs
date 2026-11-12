//! LT-142's criteria on the test set: the actors two field event tasks draw themselves
//! (`Event_ActorApproachKnightTask`, `Event_ActorFlickerInTask`). They need the disc and a test
//! set built from it (with the gift and zeppetto scenes); the second a GPU too:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test events -- --ignored --test-threads=1`

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{event_actors, field_scene, Field};
use lt_native::composite::Compositor;
use lt_scene::check::capture_scene;
use lt_scene::{Drawer, Record};
use lt_testset::{Image, Scene, SCENES};

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

/// A scene's records, captured over its tape.
fn records(image: &Arc<Image>, name: &str) -> Vec<Record> {
    let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
    capture_scene(image, &s, &state).unwrap().0
}

/// The ticks `index` was drawn by `by`, and the places it was drawn at.
fn drawn(rs: &[Record], index: u16, by: Drawer) -> (Vec<u64>, BTreeSet<[i16; 3]>) {
    let mut ticks = Vec::new();
    let mut places = BTreeSet::new();
    for r in rs {
        for c in r.characters.iter().filter(|c| c.index == index && c.by == by) {
            ticks.push(r.tick);
            places.insert(c.position);
        }
    }
    (ticks, places)
}

/// @LT-142/1: test set scenes play the events that use the two tasks. "gift": the Elder's gift box,
/// Mr. O'Neal (record 22) jumping out of it to the Knight (the approach). "zeppetto": Zeppetto
/// (record 2) flickering out, then Mr. O'Neal (record 3, the approach as it runs in Zeppetto's
/// house) hopping to the Knight. Every `Model_DrawParts` call in both is a character's.
#[test]
#[ignore = "needs LT_DISC and LT_TESTSET"]
fn lt142_1_test_set_scenes_play_the_approach_and_the_flicker() {
    assert!(SCENES.contains(&"gift") && SCENES.contains(&"zeppetto"));
    let image = Image::open(&env("LT_DISC")).unwrap();

    let gift = records(&image, "gift");
    let (ticks, places) = drawn(&gift, 22, Drawer::ApproachKnight);
    println!("gift: Mr. O'Neal drawn by the approach on {} of {} ticks, at {} places", ticks.len(), gift.len(), places.len());
    assert!(ticks.len() >= 300, "gift: the approach draws Mr. O'Neal ({} ticks)", ticks.len());
    assert!(ticks[0] > 0 && places.len() >= 20, "gift: he jumps from the Knight to his place ({} places from tick {})", places.len(), ticks[0]);
    assert_eq!(gift.iter().map(|r| r.other_models).sum::<u32>(), 0, "gift: every Model_DrawParts call is a character's");

    let zeppetto = records(&image, "zeppetto");
    let (flicker, _) = drawn(&zeppetto, 2, Drawer::FlickerIn);
    let (approach, places) = drawn(&zeppetto, 3, Drawer::ApproachKnight);
    println!("zeppetto: Zeppetto drawn by the flicker on {} ticks ({:?}…); Mr. O'Neal by the approach on {}, at {} places", flicker.len(), &flicker[..flicker.len().min(8)], approach.len(), places.len());
    let span = flicker.last().unwrap() - flicker.first().unwrap() + 1;
    assert!(flicker.len() >= 10 && (flicker.len() as u64) < span, "zeppetto: he flickers ({} of {span} ticks)", flicker.len());
    assert!(span <= 30, "zeppetto: for 30 ticks at most ({span})");
    assert!(approach.len() >= 300 && places.len() >= 20, "zeppetto: Mr. O'Neal hops to the Knight ({} ticks, {} places)", approach.len(), places.len());
    assert!(flicker.last() < approach.first(), "zeppetto: the flicker, then the approach");
    assert!(!zeppetto.iter().any(|r| r.characters.iter().any(|c| c.index == 2 && c.by == Drawer::Npc && flicker.contains(&r.tick))), "zeppetto: the actor loop doesn't draw him too");
    assert_eq!(zeppetto.iter().map(|r| r.other_models).sum::<u32>(), 0, "zeppetto: every Model_DrawParts call is a character's");
}

fn pct(a: u64, b: u64) -> f64 {
    100.0 * a as f64 / b.max(1) as f64
}

/// @LT-142/2: the capture records those actors as characters, with world matrices (at their
/// records' places) and posed parts; lt-assets resolves every one; the takeover sets their packets
/// aside (none left to the 2D layer); the native view draws them, lit; and the coverage check
/// passes on both scenes. The approach's world matrix and vertices are exactly what its record
/// rebuilds (`crate::crowd`'s construction).
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt142_2_event_actors_are_captured_and_drawn_lit() {
    let p = env("LT_DISC");
    let (image, disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().expect("a GPU");
    let mut comp = Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    for name in ["gift", "zeppetto"] {
        let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
        let e = event_actors(&image, &s, &state, 8, &mut assets, &mut cache, &mut comp).unwrap();
        println!("{}", e.text(name));
        assert!(e.characters >= 300, "{name}: event actors captured ({})", e.characters);
        assert_eq!(e.posed, e.parts, "{name}: every part has posed vertices");
        assert_eq!(e.placed, e.parts, "{name}: every world matrix is at its record's place");
        assert_eq!(e.resolved, e.characters, "{name}: lt-assets resolves every one");
        let (n, of) = e.rebuilt[&Drawer::ApproachKnight];
        assert!(n == of && of >= 300, "{name}: the approach rebuilds exactly from its record ({n}/{of})");
        assert!(e.aside > 1000 && e.kept == 0, "{name}: their packets are set aside ({}), none kept ({})", e.aside, e.kept);
        assert!(e.sampled >= 20 && e.pixels > 5000, "{name}: the native view draws them ({} pixels over {} ticks)", e.pixels, e.sampled);
        assert!(pct(e.lit, e.pixels) >= 95.0, "{name}: lit ({} of {} pixels unlike the original look)", e.lit, e.pixels);
        assert_eq!(e.other_models, 0);

        let r: Vec<Field> = field_scene(&image, &s, &state, 120, &mut assets, &mut cache, &mut comp).unwrap();
        assert!(r.len() >= 3, "{name}: field ticks measured ({})", r.len());
        let sum = |f: fn(&Field) -> u64| r.iter().map(f).sum::<u64>();
        let cov = pct(sum(|f| f.covered_agree), sum(|f| f.pixels));
        let lay = pct(sum(|f| f.layer_equal), sum(|f| f.layer_pixels));
        println!("{name}: coverage {cov:.3}%, 2D layer {lay:.3}% over {} ticks", r.len());
        assert!(cov >= 99.0, "{name}: coverage {cov:.3}%");
        assert!(lay >= 99.0, "{name}: 2D layer {lay:.3}%");
    }
}
