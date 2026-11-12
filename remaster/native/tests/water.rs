//! LT-146's criteria on the test set: the game's water drawn as water in the lit look. They need
//! the disc, a test set built from it (with the water and first-flight scenes) and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test water -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::bench;
use lt_native::composite::{Compositor, Picture};
use lt_native::draw;
use lt_native::gte::Precision;
use lt_native::light::{Lights, Look, Normals};
use lt_native::play::{native_look, view, view_between, Session, Tick, View};
use lt_native::takeover::Shown;
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

/// A view as a picture.
fn read_view(v: View, comp: &mut Compositor) -> Picture {
    match v {
        View::Picture(p) => p,
        View::Rendered(r) => comp.read(&r),
    }
}

/// Every `every`th frame's tick of a scene (its field ticks), handed to `f`.
fn sample(image: &Arc<Image>, name: &str, every: usize, assets: &mut Assets, mut f: impl FnMut(&Tick, &Shown, &mut Assets)) {
    let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
    let mut session = Session::new(image, &s, &state).unwrap();
    // A tick finishes every other frame or so: take the first field tick at or after each mark.
    let mut want = false;
    for (i, &pad) in s.tape.iter().enumerate() {
        want |= i % every == every / 2;
        for (tick, shown) in session.frame(pad, assets).unwrap() {
            if want && tick.record.camera.is_some() && !tick.table.passthrough {
                want = false;
                f(&tick, &shown, assets);
            }
        }
    }
}

/// The water corners the lit look draws in a tick.
fn water_corners(tick: &Tick, assets: &Assets, normals: &mut Normals) -> usize {
    let scene = draw::lit(&tick.record, &tick.frame, assets, Precision::Gte, normals);
    scene.batches.iter().flat_map(|b| b.corners.iter()).filter(|v| v.water).count()
}

fn lit(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, water: bool, look: Look) -> Picture {
    comp.lights = Lights { water, ..Lights::ALL };
    let p = native_look(tick, shown, assets, cache, comp, 1, look);
    comp.lights = Lights::ALL;
    p
}

/// Each pixel: `Some(true)` water, `Some(false)` other native 3D or background, `None` where the
/// 2D layer draws (it blends over whatever is under it, water too). The lit look's debug view 5.
fn mask(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Vec<Option<bool>> {
    std::env::set_var("LT_LIGHT_DEBUG", "5");
    let p = lit(tick, shown, assets, cache, comp, true, Look::Lit);
    std::env::remove_var("LT_LIGHT_DEBUG");
    (0..(p.w * p.h) as usize).map(|i| (!p.layer[i]).then(|| p.native[i] && p.rgba[i * 4] == 255)).collect()
}

fn luma(p: &Picture, i: usize) -> f32 {
    0.299 * p.rgba[i * 4] as f32 + 0.587 * p.rgba[i * 4 + 1] as f32 + 0.114 * p.rgba[i * 4 + 2] as f32
}

fn same(a: &Picture, b: &Picture, i: usize) -> bool {
    a.rgba[i * 4..i * 4 + 3] == b.rgba[i * 4..i * 4 + 3]
}

/// What turning Water on does to a tick: water pixels, those changed, other pixels changed; the
/// mean lightening of water pixels in the top and bottom thirds of the water's rows; and the water
/// pixels lightened by 60 or more (glints).
#[derive(Default, Debug)]
struct Change {
    water: u64,
    changed: u64,
    others: u64,
    top: (f64, u64),
    bottom: (f64, u64),
    glints: u64,
}

fn change(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, c: &mut Change) {
    let m = mask(tick, shown, assets, cache, comp);
    let (on, off) = (lit(tick, shown, assets, cache, comp, true, Look::Lit), lit(tick, shown, assets, cache, comp, false, Look::Lit));
    let rows: Vec<u32> = (0..on.h).filter(|&y| (0..on.w).any(|x| m[(y * on.w + x) as usize] == Some(true))).collect();
    let (first, last) = (rows.first().copied().unwrap_or(0), rows.last().copied().unwrap_or(0));
    let third = (last - first).max(3) / 3;
    for (i, &w) in m.iter().enumerate().take((on.w * on.h) as usize) {
        match w {
            None => continue,
            Some(false) => {
                c.others += u64::from(!same(&on, &off, i));
                continue;
            }
            Some(true) => {}
        }
        c.water += 1;
        c.changed += u64::from(!same(&on, &off, i));
        let gain = (luma(&on, i) - luma(&off, i)) as f64;
        c.glints += u64::from(gain >= 60.0);
        let y = i as u32 / on.w;
        if y < first + third {
            c.top.0 += gain;
            c.top.1 += 1;
        } else if y > last - third {
            c.bottom.0 += gain;
            c.bottom.1 += 1;
        }
    }
}

/// @LT-146/1: the lit look draws the game's water as water, found from its textures: the
/// village's pond (the field effect page's scrolling columns) and the first flight's sea (its blue
/// chunk texture), and none in the scenes with no water in view. Turning Water on changes the
/// water and nothing else (where the 2D layer, which blends over it, doesn't draw): waves shade it, the sky's reflection lightens it more toward the
/// horizon (the first flight), and the sun glints off it where it isn't shadowed (the pond).
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt146_1_the_games_water_is_drawn_as_water() {
    with(|image, assets, cache, comp| {
        let mut normals = Normals::default();
        for name in ["interior", "lamps", "cave", "tower", "gift", "zeppetto", "late", "cutscene"] {
            let mut n = 0;
            sample(image, name, 300, assets, |tick, _, assets| n += water_corners(tick, assets, &mut normals));
            println!("{name}: {n} water corners");
            assert_eq!(n, 0, "{name}: no water in view, none found");
        }
        for name in ["water", "first-flight"] {
            let (mut corners, mut c) = (0, Change::default());
            sample(image, name, 240, assets, |tick, shown, assets| {
                corners += water_corners(tick, assets, &mut normals);
                change(tick, shown, assets, cache, comp, &mut c);
            });
            let (top, bottom) = (c.top.0 / c.top.1.max(1) as f64, c.bottom.0 / c.bottom.1.max(1) as f64);
            println!("{name}: {corners} water corners; {} water pixels, {} changed, {} others changed; lightened {top:.1} at the top, {bottom:.1} at the bottom; {} glinting", c.water, c.changed, c.others, c.glints);
            assert!(corners > 500 && c.water > 5000, "{name}: water found");
            assert!(c.changed * 100 >= c.water * 75, "{name}: drawn as water ({} of {} pixels)", c.changed, c.water);
            assert_eq!(c.others, 0, "{name}: nothing but the water changes (where the 2D layer doesn't draw over it)");
            if name == "first-flight" {
                assert!(top > bottom + 2.0, "the sky reflected more toward the horizon");
            } else {
                assert!(c.glints > 20, "the sun glints off the pond");
            }
        }
    })
}

/// @LT-146/2: only the water changes. The original look is the same with Water on or off; a
/// scene with no water draws the same lit look either way (and, with water, LT-146/1 finds only
/// the water changed). Water is a Lights switch like Shadows and Lamps (the window's settings
/// keep it: `lt-remaster`'s `lt146_2_water_is_a_lights_switch`).
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt146_2_only_the_water_changes() {
    with(|image, assets, cache, comp| {
        let mut ticks = 0;
        for name in ["water", "first-flight", "interior", "lamps"] {
            sample(image, name, 600, assets, |tick, shown, assets| {
                ticks += 1;
                let (on, off) = (lit(tick, shown, assets, cache, comp, true, Look::Original), lit(tick, shown, assets, cache, comp, false, Look::Original));
                assert!(on.rgba == off.rgba, "{name}: the original look doesn't change");
                if name == "interior" || name == "lamps" {
                    let (on, off) = (lit(tick, shown, assets, cache, comp, true, Look::Lit), lit(tick, shown, assets, cache, comp, false, Look::Lit));
                    assert!(on.rgba == off.rgba, "{name}: no water, the lit look as before");
                }
            });
        }
        assert!(ticks >= 8, "{ticks} ticks");
    })
}

/// @LT-146/3: the waves move with time: the same tick half a second later changes the water and
/// nothing else, and a frame between ticks runs on the game's frames (`water::clock`; at t = 1 it
/// is the tick's own view). The lit view still holds 60 fps at 1440p on the water scenes.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt146_3_the_waves_move_smoothly_and_hold_60_fps() {
    with(|image, assets, cache, comp| {
        let (mut water, mut moved, mut others, mut pairs) = (0u64, 0u64, 0u64, 0);
        let mut prev: Option<Tick> = None;
        let (s, state) = Scene::load(&env("LT_TESTSET"), "water").unwrap();
        let mut session = Session::new(image, &s, &state).unwrap();
        let mut want = false;
        for (i, &pad) in s.tape.iter().enumerate() {
            want |= i % 240 == 120;
            for (tick, shown) in session.frame(pad, assets).unwrap() {
                if want && tick.record.camera.is_some() && !tick.table.passthrough {
                    want = false;
                    let m = mask(&tick, &shown, assets, cache, comp);
                    let now = lit(&tick, &shown, assets, cache, comp, true, Look::Lit);
                    let mut later = Tick { record: tick.record.clone(), frame: tick.frame.clone(), table: tick.table.clone(), crowd: tick.crowd.clone() };
                    later.record.frame += 30;
                    let then = lit(&later, &shown, assets, cache, comp, true, Look::Lit);
                    for (k, &w) in m.iter().enumerate() {
                        let d = !same(&now, &then, k);
                        match w {
                            Some(true) => {
                                water += 1;
                                moved += u64::from(d);
                            }
                            Some(false) => others += u64::from(d),
                            None => {}
                        }
                    }
                    if let Some(p) = prev.as_ref() {
                        let end = read_view(view_between(Some(p), &tick, 1.0, &shown, assets, cache, comp, 1, Look::Lit, Reach::GAME).0, comp);
                        let own = read_view(view(&tick, &shown, assets, cache, comp, 1, Look::Lit, Reach::GAME), comp);
                        assert!(end.rgba == own.rgba, "t = 1 is the tick's view, its waves too");
                        pairs += 1;
                    }
                }
                prev = Some(tick);
            }
        }
        println!("water: half a second on, {moved} of {water} water pixels moved, {others} other pixels; {pairs} t = 1 frames");
        assert!(water > 5000 && moved * 100 >= water * 50 && others == 0, "the waves move, nothing else");
        assert!(pairs >= 3);
        let clocks: Vec<f32> = [0.0, 0.25, 0.5, 0.75, 1.0].map(|t| lt_native::water::clock(1000, 1002, t)).to_vec();
        assert!(clocks.windows(2).all(|w| w[1] > w[0]) && clocks[4] == lt_native::water::clock(1002, 1004, 0.0), "the clock runs on through ticks");

        let scale = 1440u32.div_ceil(224);
        for name in ["water", "first-flight"] {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            let ms = bench(image, &s, &state, 2, scale, assets, cache, comp).unwrap();
            let mean = ms.iter().sum::<f64>() / ms.len().max(1) as f64;
            println!("{name}: {mean:.2} ms a tick at {scale}x ({} ticks)", ms.len());
            assert!(ms.len() >= 100 && mean <= 1000.0 / 60.0, "{name}: {mean:.2} ms a tick");
        }
    })
}
