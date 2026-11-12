//! LT-140's criteria on the test set: the field menu's frozen frame fills a widened view's margins.
//! They need the disc, a test set built from it (with the menu scene) and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test frozen -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{frozen_frame, FrozenFrame};
use lt_native::composite::Compositor;
use lt_native::wide::Reach;
use lt_testset::{Image, Scene};

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

/// The menu scene at `aspect` (4:3 for the game's), lit at 2×, every tick drawn. Each run has a
/// compositor of its own: it keeps the frozen frame from the tick the game freezes it.
fn menu(aspect: Option<f64>) -> FrozenFrame {
    with(|image, assets, cache, comp| {
        let (s, state) = Scene::load(&env("LT_TESTSET"), "menu").unwrap();
        let reach = aspect.map_or(Reach::GAME, |a| Reach::new(a, 1.0));
        frozen_frame(image, &s, &state, reach, 2, assets, cache, comp).unwrap()
    })
}

const WIDE: [(&str, f64); 2] = [("16:9", 16.0 / 9.0), ("21:9", 21.0 / 9.0)];

/// @LT-140/1: at 16:9 and 21:9 the menu scene's margins show the field as it was when the menu
/// opened, dimmed like the middle (within a 5-bit step).
/// - The game freezes the frame on screen as the menu opens; the native view keeps the picture it
///   drew of that frame, bit for bit, margins and all.
/// - Every menu tick draws its frozen frame from that picture, and every margin pixel is drawn.
/// - Every margin pixel is within a 5-bit step (8) of that picture dimmed by the menu's quads'
///   colour, as are the middle's outer columns, which show the frozen frame (but where the cursor
///   is): the margins are dimmed like the middle.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt140_1_the_margins_show_the_frozen_field_dimmed() {
    for (name, aspect) in WIDE {
        let f = menu(Some(aspect));
        println!(
            "{name}: {} freeze(s), {} kept exactly; {} menu ticks, {} from the native picture; dim 0x{:02X}; margins {} px, {} drawn, {} within a 5-bit step (worst {}); middle's edge columns {} px, {} within",
            f.freezes, f.kept_exact, f.ticks, f.native, f.dim, f.margin_px, f.margin_drawn, f.margin_near, f.margin_worst, f.edge_px, f.edge_near
        );
        assert!(f.freezes >= 1 && f.kept_exact == f.freezes, "{name}: the frozen frame is the native picture of the frame on screen");
        assert!(f.ticks > 100 && f.native == f.ticks, "{name}: every menu tick draws from the native picture");
        assert_eq!(f.dim, 0x3C, "{name}: the menu's dim");
        assert!(f.margin_px > 1_000_000 && f.margin_drawn == f.margin_px, "{name}: every margin pixel is drawn");
        assert_eq!(f.margin_near, f.margin_px, "{name}: margins within a 5-bit step of the frozen field dimmed");
        assert!(f.edge_near * 100 >= f.edge_px * 98, "{name}: the middle's edge columns are dimmed alike");
    }
}

/// @LT-140/2: the menu panels and text stay pixel for pixel as at 4:3.
/// - Drawing each menu tick again without the margin copies changes no middle pixel.
/// - Against the 4:3 view's ticks: wherever the two frozen pictures agree (the native 3D of the
///   4:3 and the widened view differ at a few edges), every middle pixel is the 4:3 picture's.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt140_2_the_panels_stay_as_at_4_3() {
    let game = menu(None);
    let g_frozen = game.frozen.as_ref().expect("the 4:3 view's frozen frame");
    assert!(game.middles.len() >= 8);
    for (name, aspect) in WIDE {
        let f = menu(Some(aspect));
        let w_frozen = f.frozen.as_ref().expect("the frozen frame");
        assert_eq!((g_frozen.w, g_frozen.h), (w_frozen.w, w_frozen.h));
        let (mut compared, mut agree, mut same, mut ticks) = (0u64, 0u64, 0u64, 0u32);
        for (frame, g) in &game.middles {
            let Some((_, w)) = f.middles.iter().find(|(fr, _)| fr == frame) else { continue };
            ticks += 1;
            for i in 0..(g.w * g.h) as usize {
                compared += 1;
                if g_frozen.rgba[i * 4..i * 4 + 3] == w_frozen.rgba[i * 4..i * 4 + 3] {
                    agree += 1;
                    same += u64::from(g.rgba[i * 4..i * 4 + 3] == w.rgba[i * 4..i * 4 + 3]);
                }
            }
        }
        println!("{name}: {} middle pixels changed by the margin copies; {ticks} ticks against 4:3, {compared} middle pixels, frozen pictures agree on {agree}, {same} of those the 4:3 picture's", f.copies_changed);
        assert_eq!(f.copies_changed, 0, "{name}: the margin copies change no middle pixel");
        assert!(ticks >= 8 && agree * 100 >= compared * 95, "{name}: ticks compared, and the frozen pictures agree");
        assert_eq!(same, agree, "{name}: panels and text as at 4:3");
    }
}
