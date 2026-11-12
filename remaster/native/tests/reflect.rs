//! LT-147's criteria on the test set: the scene mirrored in the water in the lit look. They need
//! the disc, a test set built from it (with the water and first-flight scenes) and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test reflect -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::bench;
use lt_native::composite::{Compositor, Picture};
use lt_native::light::{Lights, Look};
use lt_native::play::{native_look, Session, Tick};
use lt_native::takeover::Shown;
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

/// A tick's picture with Water and reflections switched as given, and whether the mirrored pass
/// ran for it.
#[allow(clippy::too_many_arguments)]
fn draw(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, water: bool, reflections: bool, look: Look) -> (Picture, bool) {
    comp.lights = Lights { water, ..Lights::ALL };
    comp.reflections = reflections;
    let p = native_look(tick, shown, assets, cache, comp, 1, look);
    let ran = comp.reflected;
    comp.lights = Lights::ALL;
    comp.reflections = true;
    (p, ran)
}

/// The lit look's debug view `n` (`LT_LIGHT_DEBUG`).
fn debug(n: u32, tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Picture {
    std::env::set_var("LT_LIGHT_DEBUG", n.to_string());
    let p = draw(tick, shown, assets, cache, comp, true, true, Look::Lit).0;
    std::env::remove_var("LT_LIGHT_DEBUG");
    p
}

/// Each pixel: `Some(true)` water, `Some(false)` other native 3D or background, `None` where the
/// 2D layer draws over it (debug view 5).
fn mask(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Vec<Option<bool>> {
    let p = debug(5, tick, shown, assets, cache, comp);
    (0..(p.w * p.h) as usize).map(|i| (!p.layer[i]).then(|| p.native[i] && p.rgba[i * 4] == 255)).collect()
}

fn same(a: &Picture, b: &Picture, i: usize) -> bool {
    a.rgba[i * 4..i * 4 + 3] == b.rgba[i * 4..i * 4 + 3]
}

/// What the mirrored scene adds to a scene's ticks.
#[derive(Default, Debug)]
struct Added {
    ticks: u64,
    ran: u64,
    water: u64,
    /// Water pixels the mirrored scene covers (debug view 6).
    covered: u64,
    changed: u64,
    others: u64,
    /// The mean Fresnel term the reflection is blended by (debug view 8) over the water in the top
    /// and bottom thirds of its rows, and over all of it.
    top: (f64, u64),
    bottom: (f64, u64),
    all: (f64, u64),
    /// Covered pixels whose reflection (debug view 7) moved with the ripples, half a second on.
    moved: u64,
}

fn added(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, a: &mut Added) {
    let m = mask(tick, shown, assets, cache, comp);
    let cover = debug(6, tick, shown, assets, cache, comp);
    let colour = debug(7, tick, shown, assets, cache, comp);
    let fresnel = debug(8, tick, shown, assets, cache, comp);
    let (on, ran) = draw(tick, shown, assets, cache, comp, true, true, Look::Lit);
    let (off, _) = draw(tick, shown, assets, cache, comp, true, false, Look::Lit);
    let mut later = Tick { record: tick.record.clone(), frame: tick.frame.clone(), table: tick.table.clone(), crowd: tick.crowd.clone() };
    later.record.frame += 30;
    let colour_later = debug(7, &later, shown, assets, cache, comp);
    a.ticks += 1;
    a.ran += u64::from(ran);
    let rows: Vec<u32> = (0..on.h).filter(|&y| (0..on.w).any(|x| m[(y * on.w + x) as usize] == Some(true))).collect();
    let (first, last) = (rows.first().copied().unwrap_or(0), rows.last().copied().unwrap_or(0));
    let third = (last - first).max(3) / 3;
    for (i, &w) in m.iter().enumerate() {
        match w {
            None => continue,
            Some(false) => {
                a.others += u64::from(!same(&on, &off, i));
                continue;
            }
            Some(true) => {}
        }
        a.water += 1;
        a.changed += u64::from(!same(&on, &off, i));
        let f = fresnel.rgba[i * 4] as f64 / 255.0;
        let y = i as u32 / on.w;
        a.all.0 += f;
        a.all.1 += 1;
        if y < first + third {
            a.top.0 += f;
            a.top.1 += 1;
        } else if y > last - third {
            a.bottom.0 += f;
            a.bottom.1 += 1;
        }
        if cover.rgba[i * 4] < 128 {
            continue;
        }
        a.covered += 1;
        a.moved += u64::from(!same(&colour, &colour_later, i));
    }
}

/// @LT-147/1: the lit look reflects the native 3D in the water about its plane. The mirrored pass
/// runs on every tick with water in view and covers part of it (the pond's banks, the first
/// flight's islands and the Baby); it changes the water and nothing else; the ripples distort it
/// (half a second on, the reflection a water pixel shows moves); the Fresnel term blends it in,
/// more toward the horizon in the first flight's low view than near the camera, and less from the
/// field's steep camera than at the first flight's horizon (debug view 8). The mirror itself
/// is exact (`water::tests::the_mirror_projects_a_points_reflection`).
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt147_1_the_scene_is_reflected_in_the_water() {
    with(|image, assets, cache, comp| {
        let mut means = Vec::new();
        for name in ["water", "first-flight"] {
            let mut a = Added::default();
            sample(image, name, 240, assets, |tick, shown, assets| added(tick, shown, assets, cache, comp, &mut a));
            let mean = |s: (f64, u64)| s.0 / s.1.max(1) as f64;
            let (top, bottom, all) = (mean(a.top), mean(a.bottom), mean(a.all));
            means.push((top, all));
            println!("{name}: {} ticks, the mirrored pass ran on {}; {} water pixels, {} covered by the reflection, {} changed, {} others changed; {} moved with the ripples; Fresnel {top:.3} at the top, {bottom:.3} at the bottom, {all:.3} in all", a.ticks, a.ran, a.water, a.covered, a.changed, a.others, a.moved);
            assert!(a.ticks >= 5 && a.ran == a.ticks, "{name}: the mirrored pass runs with water in view");
            assert!(a.covered * 100 >= a.water * 2, "{name}: the scene is reflected ({} of {} water pixels)", a.covered, a.water);
            assert!(a.changed * 2 >= a.covered, "{name}: the reflection shows ({} changed)", a.changed);
            assert_eq!(a.others, 0, "{name}: only the water changes");
            assert!(a.moved * 100 >= a.covered * 30, "{name}: the ripples distort it ({} of {})", a.moved, a.covered);
            assert!(all >= 0.149 && top <= 0.6, "{name}: the Fresnel term's range");
            if name == "first-flight" {
                assert!(top > bottom + 0.01, "{name}: the Fresnel term, more toward the horizon");
            }
        }
        assert!(means[0].1 < means[1].0, "the field's steep camera reflects less than the first flight's horizon");
    })
}

/// @LT-147/2: the mirrored pass runs only when water is drawn. With Water off, in the original
/// look and in scenes with no water in view it doesn't run, and the picture is the same as with
/// reflections switched off (the lit look as LT-146 drew it).
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt147_2_no_water_no_reflection() {
    with(|image, assets, cache, comp| {
        let mut ticks = 0;
        for name in ["interior", "lamps", "cave", "village", "tower", "water", "first-flight"] {
            let water_scene = name == "water" || name == "first-flight";
            sample(image, name, 600, assets, |tick, shown, assets| {
                ticks += 1;
                let cases: &[(bool, Look)] = if water_scene { &[(false, Look::Lit), (true, Look::Original)] } else { &[(true, Look::Lit), (false, Look::Lit), (true, Look::Original)] };
                for &(water, look) in cases {
                    let (on, ran) = draw(tick, shown, assets, cache, comp, water, true, look);
                    let (off, _) = draw(tick, shown, assets, cache, comp, water, false, look);
                    assert!(!ran, "{name}: Water {water}, {look:?}: no mirrored pass");
                    assert!(on.rgba == off.rgba, "{name}: Water {water}, {look:?}: unchanged");
                }
            });
        }
        assert!(ticks >= 14, "{ticks} ticks");
    })
}

/// @LT-147/3: the lit view still holds 60 fps at 1440p on the water scenes, the mirrored pass
/// drawn every tick.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt147_3_reflections_hold_60_fps() {
    with(|image, assets, cache, comp| {
        let scale = 1440u32.div_ceil(224);
        for name in ["water", "first-flight"] {
            let (s, state) = Scene::load(&env("LT_TESTSET"), name).unwrap();
            let before = comp.mirrored;
            let ms = bench(image, &s, &state, 2, scale, assets, cache, comp).unwrap();
            let mean = ms.iter().sum::<f64>() / ms.len().max(1) as f64;
            let mirrored = comp.mirrored - before;
            println!("{name}: {mean:.2} ms a tick at {scale}x ({} ticks), the mirrored pass on {mirrored}", ms.len());
            assert!(mirrored as usize * 2 >= ms.len(), "{name}: the bench drew the reflection");
            assert!(ms.len() >= 100 && mean <= 1000.0 / 60.0, "{name}: {mean:.2} ms a tick");
        }
    })
}
