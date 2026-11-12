//! LT-148's criteria on the test set: shallows, foam and caustics by the water's shore in the lit
//! look. They need the disc, a test set built from it (with the water and first-flight scenes) and
//! a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test shore -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::bench;
use lt_native::composite::{Compositor, Picture};
use lt_native::light::{Lights, Look, Normals};
use lt_native::play::{native_look, Session, Tick};
use lt_native::takeover::Shown;
use lt_native::water::SHORE_MAX;
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

/// A tick's picture with Water and the shore's effects switched as given.
#[allow(clippy::too_many_arguments)]
fn draw(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, water: bool, shallows: bool, look: Look) -> Picture {
    comp.lights = Lights { water, ..Lights::ALL };
    comp.shallows = shallows;
    let p = native_look(tick, shown, assets, cache, comp, 1, look);
    comp.lights = Lights::ALL;
    comp.shallows = true;
    p
}

/// The lit look's debug view `n` (`LT_LIGHT_DEBUG`), red channel per pixel.
fn debug(n: u32, tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> (Picture, Vec<u8>) {
    std::env::set_var("LT_LIGHT_DEBUG", n.to_string());
    let p = draw(tick, shown, assets, cache, comp, true, true, Look::Lit);
    std::env::remove_var("LT_LIGHT_DEBUG");
    let red = p.rgba.chunks(4).map(|c| c[0]).collect();
    (p, red)
}

fn later(tick: &Tick) -> Tick {
    let mut t = Tick { record: tick.record.clone(), frame: tick.frame.clone(), table: tick.table.clone(), crowd: tick.crowd.clone() };
    t.record.frame += 30;
    t
}

/// @LT-148/1: each water corner knows its distance to the shore, worked out from its mesh once.
/// In the pond, corners lie on the shore and in the stream between; in the first flight, by the
/// islands and out at sea (as far as it reaches); none past `SHORE_MAX`. Asking again gives the
/// same distances (`Normals::shore` keeps them a mesh).
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt148_1_each_water_corner_knows_its_distance_to_the_shore() {
    with(|image, assets, _, _| {
        for name in ["water", "first-flight"] {
            let mut normals = Normals::default();
            let (mut corners, mut on_shore, mut between, mut open, mut most, mut meshes) = (0usize, 0usize, 0usize, 0usize, 0.0f32, 0usize);
            sample(image, name, 300, assets, |tick, _, assets| {
                for c in &tick.frame.chunks {
                    let Some(mesh) = assets.mesh(c.mesh) else { continue };
                    let water = normals.water(c.mesh, c.texture, &mesh.corners, assets.texture(c.texture));
                    if !water.iter().any(|&w| w) {
                        continue;
                    }
                    let first = normals.shore(c.mesh, c.texture, &mesh.corners, &water);
                    let again = normals.shore(c.mesh, c.texture, &mesh.corners, &water);
                    assert!(Arc::ptr_eq(&first, &again), "{name}: once a mesh");
                    meshes += 1;
                    for (s, _) in first.iter().zip(water.iter()).filter(|(_, &w)| w) {
                        corners += 1;
                        on_shore += usize::from(s.distance < 1.0);
                        between += usize::from(s.distance >= 1.0 && s.distance < SHORE_MAX);
                        open += usize::from(s.distance >= SHORE_MAX);
                        most = most.max(s.distance);
                        assert!(s.distance <= SHORE_MAX && (s.distance < SHORE_MAX) == s.nearest.is_some());
                    }
                }
            });
            println!("{name}: {meshes} water meshes drawn, {corners} water corners: {on_shore} on the shore, {between} between, {open} open water; the farthest {most:.0}");
            assert!(on_shore > 50 && between > 50, "{name}: the shore and the water between");
            if name == "first-flight" {
                assert!(open > 1000, "{name}: open sea");
            }
        }
    })
}

/// What the shore's effects do to a scene's ticks.
#[derive(Default, Debug)]
struct Shore {
    /// Water pixels within 100 units of the shore, and those changed; the mean change of the colour
    /// (red, green, blue) of those with no foam.
    near: u64,
    near_changed: u64,
    gain: ([f64; 3], u64),
    /// Water pixels 220 or more from the shore (past the shallows), and those changed.
    deep: u64,
    deep_changed: u64,
    /// Other pixels changed.
    others: u64,
    /// Foam pixels (debug view 10), those more than 60 units from the shore, and those that moved
    /// half a second on.
    foam: u64,
    foam_far: u64,
    foam_moved: u64,
    /// Caustic pixels (debug view 11), and those that moved.
    caustic: u64,
    caustic_moved: u64,
}

fn shore(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, s: &mut Shore) {
    let (mask, water) = debug(5, tick, shown, assets, cache, comp);
    let (_, near) = debug(9, tick, shown, assets, cache, comp);
    let (_, foam) = debug(10, tick, shown, assets, cache, comp);
    let (_, caustic) = debug(11, tick, shown, assets, cache, comp);
    let later = later(tick);
    let (_, foam_later) = debug(10, &later, shown, assets, cache, comp);
    let (_, caustic_later) = debug(11, &later, shown, assets, cache, comp);
    let on = draw(tick, shown, assets, cache, comp, true, true, Look::Lit);
    let off = draw(tick, shown, assets, cache, comp, true, false, Look::Lit);
    // Debug view 9: 255 at the shore, falling to 0 at 512 units.
    let distance = |v: u8| (1.0 - v as f64 / 255.0) * 512.0;
    for i in 0..(on.w * on.h) as usize {
        if mask.layer[i] {
            continue;
        }
        let changed = on.rgba[i * 4..i * 4 + 3] != off.rgba[i * 4..i * 4 + 3];
        if !(mask.native[i] && water[i] == 255) {
            s.others += u64::from(changed);
            continue;
        }
        let d = distance(near[i]);
        if d < 100.0 {
            s.near += 1;
            s.near_changed += u64::from(changed);
            if foam[i] < 8 {
                for c in 0..3 {
                    s.gain.0[c] += on.rgba[i * 4 + c] as f64 - off.rgba[i * 4 + c] as f64;
                }
                s.gain.1 += 1;
            }
        } else if d >= 222.0 {
            s.deep += 1;
            s.deep_changed += u64::from(changed);
        }
        if foam[i] > 128 {
            s.foam += 1;
            s.foam_far += u64::from(d > 60.0);
        }
        if foam[i] > 128 || foam_later[i] > 128 {
            s.foam_moved += u64::from((foam[i] > 128) != (foam_later[i] > 128));
        }
        if caustic[i] > 40 {
            s.caustic += 1;
            s.caustic_moved += u64::from(caustic[i].abs_diff(caustic_later[i]) > 20);
        }
    }
}

/// @LT-148/2: the lit look draws the shore's effects. By the shore (within 100 units) the water
/// changes, lighter and greener where there's no foam; past the shallows (220 units on) it doesn't, nor does anything but
/// water. A foam line lies at the shore (none more than 60 units off it) and moves with the
/// ripples; a caustic web lights the shallows and moves.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt148_2_shallows_foam_and_caustics_by_the_shore() {
    with(|image, assets, cache, comp| {
        for name in ["water", "first-flight"] {
            let mut s = Shore::default();
            sample(image, name, 240, assets, |tick, shown, assets| shore(tick, shown, assets, cache, comp, &mut s));
            let gain = s.gain.0.map(|g| g / s.gain.1.max(1) as f64);
            println!(
                "{name}: {} water pixels by the shore, {} changed, gaining r {:.1} g {:.1} b {:.1}; {} past the shallows, {} changed; {} others changed; {} foam pixels ({} far off the shore), {} moved; {} caustic pixels, {} moved",
                s.near, s.near_changed, gain[0], gain[1], gain[2], s.deep, s.deep_changed, s.others, s.foam, s.foam_far, s.foam_moved, s.caustic, s.caustic_moved
            );
            assert!(s.near > 1000 && s.near_changed * 2 >= s.near, "{name}: the shallows show");
            assert!(gain[1] > 2.0 && gain[1] > gain[0], "{name}: lighter and greener");
            assert_eq!(s.deep_changed, 0, "{name}: past the shallows, the water as before");
            assert_eq!(s.others, 0, "{name}: only the water changes");
            assert!(s.foam > 200 && s.foam_far == 0, "{name}: a foam line at the shore");
            assert!(s.foam_moved * 10 >= s.foam * 2, "{name}: the foam moves ({} of {})", s.foam_moved, s.foam);
            if name == "water" {
                assert!(s.caustic > 500 && s.caustic_moved * 10 >= s.caustic * 3, "{name}: caustics in the shallows, moving");
            } else {
                assert!(s.deep > 10_000, "{name}: open sea");
            }
        }
    })
}

/// @LT-148/3: Water off and the original look are unchanged by the shore's effects (and scenes with
/// no water too); the lit view still holds 60 fps at 1440p on the water scenes.
#[test]
#[ignore = "needs LT_DISC, LT_TESTSET and a GPU"]
fn lt148_3_water_off_and_the_original_look_unchanged_at_60_fps() {
    with(|image, assets, cache, comp| {
        let mut ticks = 0;
        for name in ["water", "first-flight", "interior", "lamps"] {
            let water_scene = name == "water" || name == "first-flight";
            sample(image, name, 600, assets, |tick, shown, assets| {
                ticks += 1;
                let cases: &[(bool, Look)] = if water_scene { &[(false, Look::Lit), (true, Look::Original)] } else { &[(true, Look::Lit), (false, Look::Lit), (true, Look::Original)] };
                for &(water, look) in cases {
                    let (on, off) = (draw(tick, shown, assets, cache, comp, water, true, look), draw(tick, shown, assets, cache, comp, water, false, look));
                    assert!(on.rgba == off.rgba, "{name}: Water {water}, {look:?}: unchanged");
                }
            });
        }
        assert!(ticks >= 8, "{ticks} ticks");
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
