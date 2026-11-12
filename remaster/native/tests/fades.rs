//! LT-150's criteria on the test set: full-screen fades over the live field carry on into a widened
//! view's margins. They need the disc, a test set built from it and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test fades -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{fades, Fades};
use lt_native::composite::Compositor;
use lt_native::wide::Reach;
use lt_testset::{Image, Scene};

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

/// A scene's fades at `aspect`, lit at 2×, every tick drawn, with a compositor of its own.
fn scene(name: &str, aspect: f64) -> Fades {
    let p = env("LT_DISC");
    let (image, disc): (Arc<Image>, Disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().expect("a GPU");
    let mut comp = Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
    fades(&image, &s, &state, Reach::new(aspect, 1.0), 2, &mut assets, &mut cache, &mut comp).unwrap()
}

const WIDE: [(&str, f64); 2] = [("16:9", 16.0 / 9.0), ("21:9", 21.0 / 9.0)];

/// The chunk load's fade, as each of the game's two buffers links it.
const CHUNK_FADE: [u32; 2] = [0x801C_CF88, 0x801C_A740];

/// @LT-150/1: at 16:9 and 21:9, while a full-screen fade covers the 4:3 picture, the margins are
/// darkened like the middle's edge columns (brightness against the unfaded picture within 0.05).
/// - The dungeon's fade-in and the door scene's area change: the chunk load's fade (a 640 × 448
///   subtracting quad) over its frozen frame and then over the live field.
/// - Every darkened tick: in each channel and band of its unfaded value, how much darker than the
///   unfaded picture (the tick drawn without the fade) the margins are is within 0.05 of how much
///   darker the middle's outer 8 columns a side are. With the fade kept to the 4:3 (as before),
///   the gap is most of the picture's brightness.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt150_1_the_margins_fade_like_the_middle() {
    for name in ["dungeon", "door"] {
        for (aspect_name, aspect) in WIDE {
            let f = scene(name, aspect);
            println!(
                "{name} at {aspect_name}: {} fade ticks (f{}..f{}), {} darkened, deepest {:.3} (margins {:.3}); packets {:08X?}; {} margin px, {} edge px; worst gap {:.4} (fade kept to the 4:3: {:.4})",
                f.ticks, f.frames.0, f.frames.1, f.darkened, f.deepest, f.deepest_margins, f.packets, f.margin_px, f.edge_px, f.worst_gap, f.worst_gap_4_3
            );
            assert!(CHUNK_FADE.iter().all(|a| f.packets.contains(a)), "{name} at {aspect_name}: the chunk load's fade carried");
            assert!(f.ticks >= 100 && f.darkened >= 100, "{name} at {aspect_name}: ticks with a fade");
            assert!(f.deepest >= 0.9 && f.deepest_margins >= 0.9, "{name} at {aspect_name}: the fade reaches black, margins too");
            assert!(f.margin_px > 1_000_000 && f.edge_px > 100_000);
            assert!(f.worst_gap <= 0.05, "{name} at {aspect_name}: the margins darkened like the edge columns ({:.4})", f.worst_gap);
            assert!(f.worst_gap_4_3 > 0.5, "{name} at {aspect_name}: kept to the 4:3, the margins don't fade ({:.4})", f.worst_gap_4_3);
        }
    }
}

/// @LT-150/2: panels, text and effects stay 4:3 (LT-114/4's HUD check and LT-149/2 pass too, in
/// their own suites).
/// - Every tick with a fade carried, in the fade scenes (and the battles: the battle loading
///   screen's tint, the fade after a battle): drawing it again with the fade kept to the 4:3
///   changes no middle pixel.
/// - The 2D layer alone in the middle (panels, text, effects) is the 4:3 view's, pixel for pixel
///   (ticks showing a frozen frame aside: LT-140's).
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt150_2_panels_text_and_effects_stay_4_3() {
    let (mut ticks, mut layer_px) = (0u32, 0u64);
    for name in ["dungeon", "door", "battle-1"] {
        for (aspect_name, aspect) in WIDE {
            let f = scene(name, aspect);
            println!("{name} at {aspect_name}: {} fade ticks, packets {:08X?}; middle pixels changed by the margin copies {}; 2D-only middle {} px, {} the 4:3 picture's", f.ticks, f.packets, f.copies_changed, f.layer_px, f.layer_same);
            assert!(f.ticks >= 50, "{name} at {aspect_name}: ticks with a fade");
            assert_eq!(f.copies_changed, 0, "{name} at {aspect_name}: the margin copies change no middle pixel");
            assert_eq!(f.layer_same, f.layer_px, "{name} at {aspect_name}: the 2D layer in the middle as at 4:3");
            ticks += f.ticks;
            layer_px += f.layer_px;
        }
    }
    assert!(ticks >= 500 && layer_px > 1_000_000);
}
