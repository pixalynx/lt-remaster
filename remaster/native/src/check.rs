//! LT-111's measurements: the native view against the emulated picture of the same tick.
//!
//! For a tick of a field scene, three runs from one saved state, each clearing to magenta (a
//! colour no geometry has):
//! - **A:** the game as it is.
//! - **B:** the tick's terrain and characters not drawn.
//! - **E:** the tick's in-world 2D packets hidden (every kept packet of bucket > 0: torch flames,
//!   the sparkle trail, the blob shadows).
//!
//! The native view of the same tick is drawn three ways to match (scale 1, GTE positions): whole,
//! its 2D layer alone, and without those packets. Then:
//! - **Coverage** (holes): A's drawn pixels against the native view's.
//! - **The 2D layer** (doubled geometry): B's picture against the 2D layer alone, colour for
//!   colour. Terrain or a character in the layer would show where B is magenta.
//! - **Effects** (sorting): where A and E differ (an effect visible in the game) against where the
//!   native view does with and without the packets.
//!
//! The pictures for menus, dialogue and movies ([`Look`]) compare the native view at N× with the
//! emulated picture at 1×.

use std::sync::Arc;

use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::Assets;
use lt_testset::{Image, Machine, Scene};

use crate::composite::{Compositor, Picture};
use crate::draw;
use crate::gte::Precision;
use crate::layer2d;
use crate::play::{Probe, Session, Tick};
use crate::takeover::Shown;

pub const MAGENTA: [u8; 3] = [0xFF, 0x00, 0xFF];
/// Pixels left out at each edge.
pub const MARGIN: u32 = 4;
const FRAMES: usize = 12;

/// The first tick that starts in a run from `state` at `frame`, and the picture the GPU made of it.
fn first(disc: &Arc<Image>, card: &[u8], state: &[u8], frame: u64, tape: &[u16], probe: Probe, assets: &mut Assets) -> Result<Option<(Tick, Shown)>, String> {
    let m = Machine::from_state(disc, card, state, frame)?;
    let mut s = Session::probing(m, probe)?;
    let mut tick: Option<Tick> = None;
    for i in 0..FRAMES {
        for (t, shown) in s.frame(tape.get(i).copied().unwrap_or(0), assets)? {
            match tick.take() {
                None => tick = Some(t),
                Some(done) => return Ok(Some((done, shown))),
            }
        }
    }
    Ok(None)
}

/// One field tick measured.
#[derive(Clone, Debug, Default)]
pub struct Field {
    pub frame: u64,
    /// Pixels compared, and those whose drawn / not drawn agree.
    pub pixels: u64,
    pub covered_agree: u64,
    /// Pixels where B or the 2D layer drew, and those of equal colour.
    pub layer_pixels: u64,
    pub layer_equal: u64,
    /// Pixels where an effect shows in the game or the native view, and those where both agree.
    pub effect_pixels: u64,
    pub effect_agree: u64,
    pub effects: u32,
    pub aside: u32,
    /// Pixels only the native 3D drew where the game drew too, and those within one 5-bit step of
    /// the game's colour (the original look's colours against the GPU's; LT-116).
    pub native_pixels: u64,
    pub native_close: u64,
    /// Over those pixels, the summed difference (native − game) per channel, and the summed absolute
    /// difference: the GPU's dither spreads a pixel by up to half a 5-bit step either way, so a
    /// colour that matches leaves the mean near 0 and the mean absolute difference small.
    pub native_diff: [i64; 3],
    pub native_abs: u64,
    /// The tick's mode (ctx+0x43: 2 in battle).
    pub mode: u8,
}

fn px(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
    let o = ((y * w + x) * 4) as usize;
    [rgba[o], rgba[o + 1], rgba[o + 2]]
}

fn close(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    (0..3).all(|i| a[i].abs_diff(b[i]) <= tol)
}

/// Measures the tick drawn from `state` at `frame` (`None` when it draws no field).
#[allow(clippy::too_many_arguments)]
pub fn field(disc: &Arc<Image>, card: &[u8], state: &[u8], frame: u64, tape: &[u16], assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Result<Option<Field>, String> {
    let probe = Probe { clear: Some(MAGENTA), ..Probe::default() };
    let Some((tick, a)) = first(disc, card, state, frame, tape, probe.clone(), assets)? else { return Ok(None) };
    if tick.record.camera.is_none() || tick.table.passthrough {
        return Ok(None);
    }
    // Effects: the 2D packets in front of bucket 0 that draw. A packet that only sets the GPU's
    // environment (GP0 E1h–E6h: draw mode, area, offset) isn't one: hiding a battle's draw area
    // would let the game draw its world over the status panel's lines (LT-116).
    let environment = |p: &crate::takeover::Linked| p.words.iter().all(|w| (0xE1..=0xE6).contains(&(w >> 24)));
    let effects: Vec<u32> = tick.table.kept().filter(|p| p.bucket > 0 && !environment(p)).map(|p| p.addr).collect();
    let Some((_, b)) = first(disc, card, state, frame, tape, Probe { skip_geometry: true, ..probe.clone() }, assets)? else { return Ok(None) };
    let Some((_, e)) = first(disc, card, state, frame, tape, Probe { hide: effects.clone(), ..probe.clone() }, assets)? else { return Ok(None) };

    let mut table = tick.table.clone();
    table.clear = Some(MAGENTA);
    cache.prepare(assets, &tick.frame);
    let scene = draw::scene(&tick.record, &tick.frame, assets, Precision::Gte, false);
    let layer = layer2d::layer(&table, 1);
    let whole = comp.draw(Some(&scene), &table, &layer, cache, 1);
    let alone = comp.draw(None, &table, &layer, cache, 1);
    let mut without = table.clone();
    for p in &mut without.packets {
        if effects.contains(&p.addr) {
            p.aside = true;
        }
    }
    let quiet = comp.draw(Some(&scene), &without, &layer2d::layer(&without, 1), cache, 1);

    let (w, h) = (a.w.min(whole.w), a.h.min(whole.h));
    let mut f = Field { frame: tick.record.frame, effects: effects.len() as u32, aside: tick.table.terrain + tick.table.characters, mode: tick.record.mode, ..Field::default() };
    for y in MARGIN..h.saturating_sub(MARGIN) {
        for x in MARGIN..w.saturating_sub(MARGIN) {
            let i = (y * whole.w + x) as usize;
            f.pixels += 1;
            let emu = px(&a.rgba, a.w, x, y) != MAGENTA;
            let nat = whole.native[i] || whole.layer[i];
            f.covered_agree += (emu == nat) as u64;
            let (pb, pl) = (px(&b.rgba, b.w, x, y), px(&alone.rgba, alone.w, x, y));
            if pb != MAGENTA || pl != MAGENTA {
                f.layer_pixels += 1;
                f.layer_equal += close(pb, pl, 8) as u64;
            }
            if emu && whole.native[i] && !whole.layer[i] {
                f.native_pixels += 1;
                let (g, n) = (px(&a.rgba, a.w, x, y), px(&whole.rgba, whole.w, x, y));
                f.native_close += close(g, n, 8) as u64;
                for c in 0..3 {
                    f.native_diff[c] += n[c] as i64 - g[c] as i64;
                    f.native_abs += n[c].abs_diff(g[c]) as u64;
                }
            }
            let ev = px(&a.rgba, a.w, x, y) != px(&e.rgba, e.w, x, y);
            let nv = px(&whole.rgba, whole.w, x, y) != px(&quiet.rgba, quiet.w, x, y);
            if ev || nv {
                f.effect_pixels += 1;
                f.effect_agree += (ev == nv) as u64;
            }
        }
    }
    Ok(Some(f))
}

/// Samples a field scene every `every` frames.
#[allow(clippy::too_many_arguments)]
pub fn field_scene(disc: &Arc<Image>, scene: &Scene, state: &[u8], every: usize, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Result<Vec<Field>, String> {
    let mut m = Machine::from_state(disc, &scene.card, state, scene.frame)?;
    let mut out = Vec::new();
    for f in 0..scene.tape.len().saturating_sub(FRAMES) {
        if f % every == every / 2 {
            let st = m.save_state()?;
            let card = m.card_now();
            if let Some(r) = field(disc, &card, &st, m.frame, &scene.tape[f..], assets, cache, comp)? {
                out.push(r);
            }
        }
        m.step(scene.tape[f])?;
    }
    Ok(out)
}

/// A scene's pictures at N× against the emulated ones (menus, dialogue, movies).
#[derive(Clone, Debug, Default)]
pub struct Look {
    pub ticks: u32,
    /// Ticks shown by passing the emulated picture through (a movie), and of those, identical.
    pub passed: u32,
    pub passed_equal: u32,
    /// Pixels only the 2D layer drew (no native 3D within one game pixel), and those within one
    /// 5-bit step of the game's. A semi-transparent panel over a polygon's edge isn't counted: the
    /// GPU and the native view put that edge a pixel apart, so the pixel under the panel holds the
    /// polygon in one and the clear in the other.
    pub layer_pixels: u64,
    pub layer_equal: u64,
}

/// Plays `scene` and compares, every `every` frames, the native view at `scale` (each native pixel's
/// top-left sample) with the picture the GPU made of the same tick.
#[allow(clippy::too_many_arguments)]
pub fn look(disc: &Arc<Image>, scene: &Scene, state: &[u8], every: u64, scale: u32, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Result<Look, String> {
    let mut s = Session::new(disc, scene, state)?;
    let mut out = Look::default();
    // A frozen frame drawn from the native picture (LT-140) is the native view's own, as its 3D is:
    // against the game's picture the game's copy is drawn.
    let frozen_frames = std::mem::replace(&mut comp.frozen_frames, false);
    let r = look_with(&mut s, scene, every, scale, assets, cache, comp, &mut out);
    comp.frozen_frames = frozen_frames;
    r.map(|()| out)
}

#[allow(clippy::too_many_arguments)]
fn look_with(s: &mut Session, scene: &Scene, every: u64, scale: u32, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, out: &mut Look) -> Result<(), String> {
    let mut last: Option<(u64, Picture)> = None;
    for &pad in &scene.tape {
        for (tick, shown) in s.frame(pad, assets)? {
            // The picture the GPU made of the last tick arrives with this one.
            if let Some((frame, native)) = last.take() {
                if frame % every < 2 {
                    compare(out, &native, &shown, scale);
                }
            }
            let native = crate::play::native(&tick, &shown, assets, cache, comp, scale);
            if tick.table.passthrough {
                // A movie: the native view is the picture on screen now, enlarged.
                if tick.record.frame % every < 2 {
                    out.ticks += 1;
                    out.passed += 1;
                    out.passed_equal += enlarged(&native, &shown, scale) as u32;
                }
            } else {
                last = Some((tick.record.frame, native));
            }
        }
    }
    Ok(())
}

/// The native 3D drew within one game pixel of (`x`, `y`).
fn near_3d(native: &Picture, x: u32, y: u32, scale: u32) -> bool {
    let (x0, y0) = (x.saturating_sub(scale), y.saturating_sub(scale));
    let (x1, y1) = ((x + 2 * scale).min(native.w), (y + 2 * scale).min(native.h));
    (y0..y1).any(|y| (x0..x1).any(|x| native.native[(y * native.w + x) as usize]))
}

/// `native` is `shown` enlarged `scale` times, pixel for pixel.
fn enlarged(native: &Picture, shown: &Shown, scale: u32) -> bool {
    native.w == shown.w * scale && (0..native.h).all(|y| (0..native.w).all(|x| px(&native.rgba, native.w, x, y) == px(&shown.rgba, shown.w, x / scale, y / scale)))
}

fn compare(out: &mut Look, native: &Picture, shown: &Shown, scale: u32) {
    out.ticks += 1;
    for y in MARGIN..(shown.h).saturating_sub(MARGIN) {
        for x in MARGIN..(shown.w).saturating_sub(MARGIN) {
            let (nx, ny) = (x * scale, y * scale);
            if nx >= native.w || ny >= native.h {
                continue;
            }
            let i = (ny * native.w + nx) as usize;
            if native.layer[i] && !near_3d(native, nx, ny, scale) {
                out.layer_pixels += 1;
                out.layer_equal += close(px(&native.rgba, native.w, nx, ny), px(&shown.rgba, shown.w, x, y), 8) as u64;
            }
        }
    }
}

/// Ordering tables the takeover read in a lockstep run, and the packets it set aside.
#[derive(Clone, Copy, Debug, Default)]
pub struct Taken {
    pub tables: u64,
    pub terrain: u64,
    pub characters: u64,
}

/// Plays `scene` with the takeover's hooks against the original code (`lt_lockstep::run` with the
/// render masks): AC4, no game state differs outside the packet buffers.
pub fn lockstep(disc: &Arc<Image>, scene: &Scene, state: &[u8]) -> Result<(lt_lockstep::Report, Taken), String> {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    let taken = Rc::new(RefCell::new(Taken::default()));
    let t2 = taken.clone();
    let setup = move |_: &Rc<Cell<u64>>| -> Option<(lt_emu_hooks::Hooks, lt_lockstep::Handler)> {
        let mut t = crate::takeover::Takeover::new();
        let counts = t2.clone();
        let h: lt_lockstep::Handler = Box::new(move |c| {
            let a = t.call(c.hook, c);
            for (table, _) in t.take() {
                let mut n = counts.borrow_mut();
                n.tables += 1;
                n.terrain += table.terrain as u64;
                n.characters += table.characters as u64;
            }
            a
        });
        Some((lt_emu_hooks::Hooks::new(crate::takeover::hooks()).expect("takeover hooks"), h))
    };
    let r = lt_lockstep::run(disc, scene, state, &lt_lockstep::original, &setup, lt_lockstep::RENDER_MASKS)?;
    let t = *taken.borrow();
    Ok((r, t))
}

/// LT-112's measurements on one field tick, at 1×.
#[derive(Clone, Debug, Default)]
pub struct Lighting {
    pub frame: u64,
    pub area: Option<crate::light::Area>,
    /// Pixels the native 3D drew; those real shadows darken (the lit look against itself with no
    /// casters), and those only the characters' shadows darken.
    pub native: u64,
    pub shadowed: u64,
    pub character_shadowed: u64,
    /// The lamps found, and the pixels they brighten (against the lit look with none).
    pub lamps: u32,
    pub lamp_lit: u64,
    /// The original look against LT-110's unlit image of the same tick: pixels either drew, and
    /// those with the same coverage and colour.
    pub original_pixels: u64,
    pub original_equal: u64,
}

fn luma(p: &[u8]) -> i32 {
    (299 * p[0] as i32 + 587 * p[1] as i32 + 114 * p[2] as i32) / 1000
}

/// Measures `tick` lit, lit with no casters, with no character casters and with no lamps, and its
/// original look against `raster` (LT-110's unlit image).
pub fn lighting(tick: &Tick, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, raster: &mut crate::raster::Raster) -> Option<Lighting> {
    tick.record.camera.as_ref()?;
    // The field's: a battle (LT-116) is lit by the game's battle light, which LT-110's unlit image
    // doesn't know.
    if tick.table.passthrough || tick.record.mode == lt_assets::BATTLE_MODE {
        return None;
    }
    cache.prepare(assets, &tick.frame);
    let mut table = tick.table.clone();
    table.clear = Some([0, 0, 0]);
    let none = layer2d::Layer::default();
    let lit = draw::lit(&tick.record, &tick.frame, assets, Precision::Gte, &mut comp.normals);
    let u = crate::light::uniforms(&tick.record, &lit)?;
    let full = comp.draw_lit(Some(&lit), &table, &none, cache, 1, Some(&u));
    let mut bare = lit.clone();
    if let Some(l) = bare.lit.as_mut() {
        l.casters.clear();
        l.chunk_casters.clear();
    }
    let unshadowed = comp.draw_lit(Some(&bare), &table, &none, cache, 1, Some(&u));
    let mut scenery = lit.clone();
    if let Some(l) = scenery.lit.as_mut() {
        l.casters.clear();
    }
    let no_characters = comp.draw_lit(Some(&scenery), &table, &none, cache, 1, Some(&u));
    let dark = crate::light::Uniforms { points: Vec::new(), ..u.clone() };
    let unlamped = comp.draw_lit(Some(&lit), &table, &none, cache, 1, Some(&dark));

    let plain = draw::scene(&tick.record, &tick.frame, assets, Precision::Gte, false);
    let original = comp.draw(Some(&plain), &table, &none, cache, 1);
    let a3 = raster.draw(&plain, cache);

    let mut m = Lighting { frame: tick.record.frame, area: Some(u.rig.area), lamps: u.points.len() as u32, ..Lighting::default() };
    let (w, h) = (full.w.min(a3.w), full.h.min(a3.h));
    for y in MARGIN..h.saturating_sub(MARGIN) {
        for x in MARGIN..w.saturating_sub(MARGIN) {
            let i = (y * full.w + x) as usize;
            fn px(p: &Picture, i: usize) -> &[u8] {
                &p.rgba[i * 4..i * 4 + 3]
            }
            if full.native[i] {
                m.native += 1;
                let l = luma(px(&full, i));
                m.shadowed += (l + 3 < luma(px(&unshadowed, i))) as u64;
                m.character_shadowed += (l + 3 < luma(px(&no_characters, i))) as u64;
                m.lamp_lit += (l > luma(px(&unlamped, i)) + 3) as u64;
            }
            let j = (y * a3.w + x) as usize;
            if original.native[i] || a3.mask[j] {
                m.original_pixels += 1;
                m.original_equal += (original.native[i] == a3.mask[j] && px(&original, i) == &a3.rgba[j * 4..j * 4 + 3]) as u64;
            }
        }
    }
    Some(m)
}

/// [`lighting`] for a scene's field ticks, every `every` frames.
#[allow(clippy::too_many_arguments)]
pub fn lighting_scene(disc: &Arc<Image>, scene: &Scene, state: &[u8], every: u64, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, raster: &mut crate::raster::Raster) -> Result<Vec<Lighting>, String> {
    let mut s = Session::new(disc, scene, state)?;
    let mut out = Vec::new();
    for &pad in &scene.tape {
        for (tick, _) in s.frame(pad, assets)? {
            if tick.record.frame % every < 2 {
                if let Some(m) = lighting(&tick, assets, cache, comp, raster) {
                    out.push(m);
                }
            }
        }
    }
    Ok(out)
}

/// Marks the calling thread as user-interactive work: on macOS a default-QoS thread can land on an
/// efficiency core and take several times as long (the app's emulator and native threads do this).
pub fn interactive() {
    #[cfg(target_os = "macos")]
    // SAFETY: sets a scheduling hint on the calling thread only; no memory is touched.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

/// The lit native view's time per tick at `scale` (from the capture resolved to the GPU done, as the
/// app runs it), in milliseconds, for a scene's field ticks every `every` frames; the first two (pipelines, the
/// texture cache) aren't counted.
#[allow(clippy::too_many_arguments)]
pub fn bench(disc: &Arc<Image>, scene: &Scene, state: &[u8], every: u64, scale: u32, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Result<Vec<f64>, String> {
    interactive();
    let mut s = Session::new(disc, scene, state)?;
    let mut out = Vec::new();
    let mut warm = 0;
    for &pad in &scene.tape {
        for (tick, shown) in s.frame(pad, assets)? {
            if tick.record.camera.is_none() || tick.table.passthrough || tick.record.frame % every >= 2 {
                continue;
            }
            // As the app runs it: composited on the GPU and left there for the window.
            let t0 = std::time::Instant::now();
            let v = crate::play::view(&tick, &shown, assets, cache, comp, scale, crate::light::Look::Lit, crate::wide::Reach::GAME);
            comp.wait();
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            std::hint::black_box(v);
            warm += 1;
            if warm > 2 {
                out.push(ms);
            }
        }
    }
    Ok(out)
}

/// LT-113's measurements on a scene, blending each pair of ticks at `k` frames a tick.
#[derive(Clone, Debug, Default)]
pub struct Motion {
    /// Field ticks with a tick before them, and of those, blended with it.
    pub pairs: u32,
    pub blended: u32,
    /// Snaps by reason: no camera, mode, no shared chunk (warps, doors), camera cut.
    pub snaps: [u32; 4],
    /// Area changes (the new current chunk wasn't drawn the tick before: a warp or a door), and
    /// of those, snapped.
    pub area_changes: u32,
    pub area_changes_snapped: u32,
    /// Shown frames while the scenery moved a pixel or more on screen between ticks (the camera
    /// follows the Knight, so walking scrolls the world: measured at the ground under him), and of
    /// those, frames where it didn't move (a frame shown twice, as at 30).
    pub moving_frames: u32,
    pub repeated: u32,
    /// The most uneven tick: its largest frame-to-frame step on screen over its smallest (1 is
    /// even), over ticks where the scenery moved two pixels or more.
    pub uneven: f64,
    /// Camera turns of half a degree or more in a tick, and the most uneven one's largest step over
    /// its smallest.
    pub turns: u32,
    pub turn_uneven: f64,
    /// Character parts in blended pairs, those with their vertices blended, and characters drawn
    /// at tick B because they jumped or their parts didn't match.
    pub parts: u32,
    pub morphed: u32,
    pub snapped_characters: u32,
    /// The camera's eye: total travel (world units) and the pairs it moved in.
    pub eye_travel: f64,
    pub eye_moves: u32,
}

/// A world point's place on screen through a pose's camera.
fn on_screen(cam: &lt_scene::Camera, view: &crate::light::Affine, p: [f64; 3]) -> Option<[f64; 2]> {
    let t = view.apply(p);
    (t[2] > 1.0).then(|| [cam.offset[0] as f64 + cam.h as f64 * t[0] / t[2], cam.offset[1] as f64 + cam.h as f64 * t[1] / t[2]])
}

fn view_rotation(v: &crate::light::Affine) -> [[f64; 3]; 3] {
    v.m.map(|r| {
        let l = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt().max(1e-9);
        r.map(|x| x / l)
    })
}

/// The angle between two rotations' third rows (the view direction), degrees.
fn view_turn(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> f64 {
    let d: f64 = (0..3).map(|k| a[2][k] * b[2][k] + a[0][k] * b[0][k]).sum::<f64>() / 2.0;
    d.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Plays `scene` and blends every pair of ticks at `k` frames a tick (no drawing: the poses).
pub fn motion(disc: &Arc<Image>, scene: &Scene, state: &[u8], k: u32, assets: &mut Assets) -> Result<Motion, String> {
    use crate::interp::{between, Between, Snap};
    let mut s = Session::new(disc, scene, state)?;
    let mut m = Motion { uneven: 1.0, turn_uneven: 1.0, ..Motion::default() };
    // As the app pairs them: each field tick with the tick before it (with or without the field);
    // an area change is against the last tick that drew the field (a door fades out between).
    let mut prev: Option<Tick> = None;
    let mut last_field: Option<(u16, Vec<u16>)> = None;
    for &pad in &scene.tape {
        for (tick, _) in s.frame(pad, assets)? {
            if let (Some(a), Some(cb)) = (prev.as_ref(), tick.record.camera.as_ref()) {
                if !tick.table.passthrough {
                    m.pairs += 1;
                    let area_change = last_field.as_ref().is_some_and(|(cur, drawn)| tick.record.chunk != *cur && !drawn.contains(&tick.record.chunk));
                    m.area_changes += area_change as u32;
                    let poses: Vec<Between> = (1..=k).map(|j| between((&a.record, &a.frame), (&tick.record, &tick.frame), j as f64 / k as f64)).collect();
                    match &poses[0] {
                        Between::Snap(r) => {
                            let i = match r {
                                Snap::NoCamera => 0,
                                Snap::Mode => 1,
                                Snap::NoSharedChunk => 2,
                                Snap::CameraCut => 3,
                            };
                            m.snaps[i] += 1;
                            m.area_changes_snapped += area_change as u32;
                        }
                        Between::Pose(p0) => {
                            m.blended += 1;
                            m.parts += p0.parts.iter().map(|v| v.len() as u32).sum::<u32>();
                            m.morphed += p0.morphed;
                            m.snapped_characters += p0.snapped_characters;
                            // A fixed world point (where tick B's current chunk origin is, moved
                            // under the Knight: the ground the camera looks at) on screen at t = 0
                            // and at each frame.
                            let start = between((&a.record, &a.frame), (&tick.record, &tick.frame), 0.0);
                            let ground = crate::light::Affine::of(&cb.view).inverse().map(|w| w.apply([0.0, 0.0, cb.view.t[2] as f64]));
                            let mut at: Vec<[f64; 2]> = Vec::new();
                            let mut rots = Vec::new();
                            for b in std::iter::once(&start).chain(poses.iter()) {
                                if let Between::Pose(p) = b {
                                    if let Some(x) = ground.and_then(|g| on_screen(cb, &p.view, g)) {
                                        at.push(x);
                                    }
                                    rots.push(view_rotation(&p.view));
                                }
                            }
                            if let (Between::Pose(p0), Between::Pose(p1)) = (&start, &poses[k as usize - 1]) {
                                let (e0, e1) = (p0.view.inverse().map(|v| v.t), p1.view.inverse().map(|v| v.t));
                                if let (Some(e0), Some(e1)) = (e0, e1) {
                                    let d = ((e1[0] - e0[0]).powi(2) + (e1[1] - e0[1]).powi(2) + (e1[2] - e0[2]).powi(2)).sqrt();
                                    m.eye_travel += d;
                                    m.eye_moves += (d > 0.5) as u32;
                                }
                            }
                            if at.len() == k as usize + 1 {
                                let steps: Vec<f64> = at.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).collect();
                                let total = ((at[k as usize][0] - at[0][0]).powi(2) + (at[k as usize][1] - at[0][1]).powi(2)).sqrt();
                                if total >= 1.0 {
                                    m.moving_frames += k;
                                    m.repeated += steps.iter().filter(|&&d| d < 1e-3).count() as u32;
                                }
                                if total >= 2.0 {
                                    let (lo, hi) = steps.iter().fold((f64::MAX, 0.0f64), |(lo, hi), &d| (lo.min(d), hi.max(d)));
                                    m.uneven = m.uneven.max(hi / lo.max(1e-9));
                                }
                            }
                            if rots.len() == k as usize + 1 && view_turn(&rots[0], &rots[k as usize]) >= 0.5 {
                                m.turns += 1;
                                let steps: Vec<f64> = rots.windows(2).map(|w| view_turn(&w[0], &w[1])).collect();
                                let (lo, hi) = steps.iter().fold((f64::MAX, 0.0f64), |(lo, hi), &d| (lo.min(d), hi.max(d)));
                                m.turn_uneven = m.turn_uneven.max(hi / lo.max(1e-9));
                            }
                        }
                    }
                }
            }
            if tick.record.camera.is_some() && !tick.table.passthrough {
                last_field = Some((tick.record.chunk, tick.frame.chunks.iter().map(|c| c.chunk).collect()));
            }
            prev = Some(tick);
        }
    }
    Ok(m)
}

/// [`lockstep`] with all the native view's hooks (`crate::play::Player`: the scene capture and the
/// takeover), what the frames between ticks are drawn from (LT-113). Returns the ticks captured.
pub fn lockstep_player(disc: &Arc<Image>, scene: &Scene, state: &[u8]) -> Result<(lt_lockstep::Report, u64), String> {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    let ticks = Rc::new(RefCell::new(0u64));
    let t2 = ticks.clone();
    let setup = move |_: &Rc<Cell<u64>>| -> Option<(lt_emu_hooks::Hooks, lt_lockstep::Handler)> {
        use lt_emu_hooks::Handler;
        let hooks = crate::play::Player::hooks();
        let otag = hooks.iter().position(|h| h.name == "takeover: Dbg_DrawOTag").expect("the DrawOTag hook");
        let mut p = crate::play::Player::default();
        let count = t2.clone();
        let h: lt_lockstep::Handler = Box::new(move |c| {
            let a = p.call(c);
            if c.hook == otag {
                // Each table drawn: its tick's capture paired (and let go).
                *count.borrow_mut() += p.end().len() as u64;
            }
            a
        });
        Some((lt_emu_hooks::Hooks::new(hooks).expect("the native view's hooks"), h))
    };
    let r = lt_lockstep::run(disc, scene, state, &lt_lockstep::original, &setup, lt_lockstep::RENDER_MASKS)?;
    let n = *ticks.borrow();
    Ok((r, n))
}

/// LT-114's measurements of a scene at one reach (`crate::wide`), summed over its field ticks.
#[derive(Clone, Debug, Default)]
pub struct Widened {
    pub ticks: u32,
    /// Captured chunk draws whose objects `wide::object_in` at the game's reach picks exactly (the
    /// same set the game drew), of all the scene's.
    pub objects_agree: u64,
    pub chunk_draws: u64,
    /// Pixels in the margins (both sides), and those with no native 3D: drawing only what the game
    /// drew (`naive`, the empty corners), and as the native view draws.
    pub margin_px: u64,
    pub margin_empty_naive: u64,
    pub margin_empty: u64,
    /// The same for the ticks on the overworld grid (ground everywhere): margin pixels, and those
    /// with no native 3D.
    pub grid_px: u64,
    pub grid_empty: u64,
    /// Pixels the reference (`wide::everything`: every object of every chunk near) covers and the
    /// view doesn't, in the margins and in the middle.
    pub margin_missing: u64,
    pub centre_px: u64,
    pub centre_missing: u64,
    /// Chunks and objects drawn (every tick): the game's, and the view's.
    pub game_chunks: u64,
    pub chunks: u64,
    pub game_objects: u64,
    pub objects: u64,
    /// Polygons drawn deeper than the game's limit, and pixels covered that the game's distance
    /// leaves empty (at the same aspect).
    pub beyond_far: u64,
    pub further_px: u64,
    /// The NPC gap, summed over ticks.
    pub npc: crate::wide::NpcGap,
}

impl Widened {
    pub fn text(&self, name: &str, reach: crate::wide::Reach) -> String {
        let pc = |a: u64, b: u64| 100.0 * a as f64 / b.max(1) as f64;
        format!(
            "{name} at {:.3}:1, distance {}: {} ticks; objects as the game's {}/{}; margins {} px: empty {:.2}% drawing the game's chunks only, {:.2}% widened ({:.3}% of {} on the grid); missing against everything: margins {:.3}%, middle {:.3}%; chunks {} (game {}), objects {} (game {}); beyond the game's far {} polygons, {} px further; characters drawn {}, by the crowd {} ({} in the margins); left out: {} in the margins, {} in view; misread {}",
            reach.aspect, reach.distance, self.ticks, self.objects_agree, self.chunk_draws, self.margin_px,
            pc(self.margin_empty_naive, self.margin_px), pc(self.margin_empty, self.margin_px), pc(self.grid_empty, self.grid_px), self.grid_px,
            pc(self.margin_missing, self.margin_px), pc(self.centre_missing, self.centre_px),
            self.chunks, self.game_chunks, self.objects, self.game_objects, self.beyond_far, self.further_px,
            self.npc.drawn, self.npc.crowd, self.npc.crowd_margins, self.npc.in_margins, self.npc.unslotted_in_view, self.npc.misread,
        )
    }
}

/// The native 3D's coverage of a scene, scale 1 (GTE positions), no 2D layer.
fn coverage(tick: &Tick, frame: &lt_assets::Frame, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, sight: draw::Sight) -> (draw::Scene, Picture) {
    cache.prepare(assets, frame);
    let scene = draw::scene_with(&tick.record, frame, assets, Precision::Gte, None, None, sight);
    let none = layer2d::Layer::default();
    let p = comp.draw_lit(Some(&scene), &tick.table, &none, cache, 1, None);
    (scene, p)
}

/// LT-114 on a scene at `reach`, every `every` frames of its field ticks: the object test against
/// the game's, the margins' coverage (naive, widened, against everything near), what a longer
/// distance adds, and the NPC gap. With `out`, writes `<out>/<scene>-<frame>.png` for the first
/// tick measured: the game's 4:3 above, the naive widening, then the native view's.
#[allow(clippy::too_many_arguments)]
pub fn widened(disc: &Arc<Image>, scene: &Scene, state: &[u8], reach: crate::wide::Reach, every: u64, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, out: Option<&std::path::Path>) -> Result<Widened, String> {
    use crate::wide::{self, object_in, Reach, OBJECT_FAR};
    let mut s = Session::new(disc, scene, state)?;
    s.reach = reach;
    let mut w = Widened::default();
    let mut written = out.is_none();
    // Field ticks seen: every `every / 2` of them is measured (a tick is two frames).
    let mut seen = 0u64;
    let mut best_crowd = 0;
    for &pad in &scene.tape {
        let ticks = s.frame(pad, assets)?;
        for (mut tick, shown) in ticks {
            // Field ticks: a battle (LT-116) has its arena, not the grid.
            let Some(cam) = tick.record.camera.clone().filter(|_| tick.record.mode != lt_assets::BATTLE_MODE) else { continue };
            // The object test, every tick.
            for c in &tick.record.chunks {
                let Some(a) = assets.chunk(c.chunk) else { continue };
                let game: std::collections::BTreeSet<u64> = c.groups.iter().map(|g| g.id & 0xFFFF_FFFF).collect();
                let ours: std::collections::BTreeSet<u64> = a
                    .groups
                    .iter()
                    .filter(|(_, g)| a.centres.iter().any(|&(i, ct)| i == g.index && object_in(&c.model_view, ct.map(|v| v as i32), cam.h, 0, OBJECT_FAR)))
                    .map(|(&ofs, _)| ofs as u64)
                    .collect();
                w.chunk_draws += 1;
                w.objects_agree += u64::from(game == ours);
            }
            if tick.table.passthrough {
                continue;
            }
            seen += 1;
            if !(seen - 1).is_multiple_of((every / 2).max(1)) {
                continue;
            }
            w.ticks += 1;
            let gap = wide::npc_gap(&tick.record, &tick.crowd, reach);
            w.npc = w.npc + gap;
            // The tick with the most characters the crowd draws in the margins: lit at 2×, the
            // game's 4:3, wide without the crowd, wide with it.
            if let (Some(dir), true) = (out, gap.crowd_margins > best_crowd) {
                best_crowd = gap.crowd_margins;
                let mut t = tick;
                let look = crate::light::Look::Lit;
                let game = crate::play::native_at(&t, &shown, assets, cache, comp, 2, look, Reach::GAME);
                let crowd = std::mem::take(&mut t.crowd);
                let without = crate::play::native_at(&t, &shown, assets, cache, comp, 2, look, reach);
                t.crowd = crowd;
                let with = crate::play::native_at(&t, &shown, assets, cache, comp, 2, look, reach);
                crate::gate::save_png(&dir.join(format!("{}-crowd.png", scene.name)), &stack_pictures(&[&game, &without, &with]))?;
                tick = t;
            }
            let width = tick.table.display[2] as u32;
            let sight = reach.sight(width);
            let left = sight.left;
            let near = Reach { distance: 1.0, ..reach };
            let widened = wide::frame(&tick.record, &tick.frame, &tick.crowd, assets, reach).into_owned();
            let nearer = wide::frame(&tick.record, &tick.frame, &tick.crowd, assets, near).into_owned();
            let all = wide::everything(&tick.record, &tick.frame, assets, 2);
            w.game_chunks += tick.frame.chunks.len() as u64;
            w.chunks += widened.chunks.len() as u64;
            w.game_objects += tick.frame.chunks.iter().map(|c| c.groups.len() as u64).sum::<u64>();
            w.objects += widened.chunks.iter().map(|c| c.groups.len() as u64).sum::<u64>();
            let (sv, view) = coverage(&tick, &widened, assets, cache, comp, sight);
            let (_, naive) = coverage(&tick, &tick.frame, assets, cache, comp, sight);
            let (_, every_pic) = coverage(&tick, &all, assets, cache, comp, near.sight(width));
            let (sn, near_pic) = coverage(&tick, &nearer, assets, cache, comp, near.sight(width));
            if reach.distance > 1.0 {
                // The same objects drawn to the game's far: what the further limit adds.
                let (sf, _) = coverage(&tick, &widened, assets, cache, comp, draw::Sight { far: crate::gte::FAR, ..sight });
                w.beyond_far += sv.polygons.saturating_sub(sf.polygons);
                w.further_px += (0..view.native.len()).filter(|&i| view.native[i] && !near_pic.native[i]).count() as u64;
            }
            let _ = sn;
            let (pw, ph) = (view.w, view.h);
            let grid = (wide::home(&tick.record) as u32) < lt_core::world::GRID_CHUNKS;
            for y in 0..ph {
                for x in 0..pw {
                    let i = (y * pw + x) as usize;
                    let margin = x < left || x >= pw - left;
                    // Where the reference has 3D and the nearer view (the same distance) doesn't.
                    let missing = every_pic.native[i] && !near_pic.native[i];
                    if margin {
                        w.margin_px += 1;
                        w.margin_empty_naive += u64::from(!naive.native[i]);
                        w.margin_empty += u64::from(!view.native[i]);
                        w.grid_px += u64::from(grid);
                        w.grid_empty += u64::from(grid && !view.native[i]);
                        w.margin_missing += u64::from(missing);
                    } else {
                        w.centre_px += 1;
                        w.centre_missing += u64::from(missing);
                    }
                }
            }
            if !written {
                let dir = out.expect("out");
                let game = coverage(&tick, &tick.frame, assets, cache, comp, draw::Sight::GAME).1;
                let pic = stack_pictures(&[&game, &naive, &view]);
                crate::gate::save_png(&dir.join(format!("{}-{}.png", scene.name, tick.record.frame)), &pic)?;
                written = true;
            }
        }
    }
    Ok(w)
}

/// Pictures one above another, centred, 4 px apart.
pub fn stack_pictures(ps: &[&Picture]) -> crate::gate::Picture {
    let w = ps.iter().map(|p| p.w).max().unwrap_or(1);
    let h = ps.iter().map(|p| p.h + 4).sum::<u32>();
    let mut rgba = [24u8, 24, 28, 255].repeat((w * h) as usize);
    let mut top = 0;
    for p in ps {
        let dx = (w - p.w) / 2;
        for y in 0..p.h {
            let s = (y * p.w * 4) as usize;
            let d = (((top + y) * w + dx) * 4) as usize;
            rgba[d..d + (p.w * 4) as usize].copy_from_slice(&p.rgba[s..s + (p.w * 4) as usize]);
        }
        top += p.h + 4;
    }
    crate::gate::Picture { w, h, rgba }
}

/// `m` turned `degrees` about the view's x axis, after it: positive turns the camera up, toward
/// the horizon.
fn turn_up(m: &lt_scene::Matrix, degrees: f64) -> lt_scene::Matrix {
    let (s, c) = (-degrees.to_radians()).sin_cos();
    let mut out = *m;
    for j in 0..3 {
        let (y, z) = (m.m[1][j] as f64, m.m[2][j] as f64);
        out.m[1][j] = (c * y - s * z).round().clamp(-32768.0, 32767.0) as i16;
        out.m[2][j] = (s * y + c * z).round().clamp(-32768.0, 32767.0) as i16;
    }
    let (y, z) = (m.t[1] as f64, m.t[2] as f64);
    out.t[1] = (c * y - s * z).round() as i32;
    out.t[2] = (s * y + c * z).round() as i32;
    out
}

/// A tick seen by a camera turned up `degrees` about its own x axis: a camera that sees further than
/// the game's (as a free camera will, LT-125), for the draw distance. Its chunks only.
pub fn tilted(tick: &Tick, degrees: f64) -> (lt_scene::Record, lt_assets::Frame) {
    let mut r = tick.record.clone();
    let mut f = tick.frame.clone();
    if let Some(cam) = r.camera.as_mut() {
        cam.view = turn_up(&cam.view, degrees);
    }
    for c in &mut r.chunks {
        c.model_view = turn_up(&c.model_view, degrees);
    }
    for c in &mut f.chunks {
        c.model_view = turn_up(&c.model_view, degrees);
    }
    r.characters.clear();
    f.characters.clear();
    (r, f)
}

/// The draw distance with a camera that looks further (LT-114/2): a scene's ticks turned up `tilt`
/// degrees, drawn at `reach` and at the game's distance (same aspect).
#[derive(Clone, Debug, Default)]
pub struct Further {
    pub ticks: u32,
    pub px: u64,
    /// Chunks drawn at the game's distance and at `reach`'s.
    pub chunks_game: u64,
    pub chunks: u64,
    /// Polygons drawn deeper than the game's far limit, and pixels covered the game's distance
    /// leaves empty.
    pub beyond_far: u64,
    pub further_px: u64,
}

/// [`Further`] for a scene, every `every` frames of field ticks; with `out`, the first tick's two
/// pictures (game distance above) as `<out>/<scene>-tilted.png`.
#[allow(clippy::too_many_arguments)]
pub fn further(disc: &Arc<Image>, scene: &Scene, state: &[u8], reach: crate::wide::Reach, tilt: f64, every: u64, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, out: Option<&std::path::Path>) -> Result<Further, String> {
    use crate::wide::{self, Reach};
    let mut s = Session::new(disc, scene, state)?;
    let near = Reach { distance: 1.0, ..reach };
    let mut f = Further::default();
    let (mut seen, mut written) = (0u64, out.is_none());
    for &pad in &scene.tape {
        for (tick, _) in s.frame(pad, assets)? {
            if tick.record.camera.is_none() || tick.table.passthrough || tick.record.mode == lt_assets::BATTLE_MODE {
                continue;
            }
            seen += 1;
            if !(seen - 1).is_multiple_of((every / 2).max(1)) {
                continue;
            }
            let (r, fr) = tilted(&tick, tilt);
            wide::load(&r, assets, reach);
            let t = Tick { record: r, frame: fr, table: tick.table.clone(), crowd: Vec::new() };
            let width = t.table.display[2] as u32;
            let (far_frame, near_frame) = (wide::frame(&t.record, &t.frame, &[], assets, reach).into_owned(), wide::frame(&t.record, &t.frame, &[], assets, near).into_owned());
            let (sf, pf) = coverage(&t, &far_frame, assets, cache, comp, reach.sight(width));
            let (sl, _) = coverage(&t, &far_frame, assets, cache, comp, draw::Sight { far: crate::gte::FAR, ..reach.sight(width) });
            let (_, pn) = coverage(&t, &near_frame, assets, cache, comp, near.sight(width));
            f.ticks += 1;
            f.px += pf.native.len() as u64;
            f.chunks_game += near_frame.chunks.len() as u64;
            f.chunks += far_frame.chunks.len() as u64;
            f.beyond_far += sf.polygons.saturating_sub(sl.polygons);
            f.further_px += (0..pf.native.len()).filter(|&i| pf.native[i] && !pn.native[i]).count() as u64;
            if !written {
                let pic = stack_pictures(&[&pn, &pf]);
                crate::gate::save_png(&out.expect("out").join(format!("{}-tilted.png", scene.name)), &pic)?;
                written = true;
            }
        }
    }
    Ok(f)
}

/// The 2D layer (HUD, dialogue, menus, effects) at a wider aspect against the game's 4:3 (LT-114/4),
/// the same ticks drawn both ways at `scale`.
#[derive(Clone, Debug, Default)]
pub struct Hud {
    pub ticks: u32,
    /// Pixels the 2D layer drew at 4:3, and those drawn the same (colour and coverage) in the
    /// middle of the wider picture.
    pub layer_px: u64,
    pub same: u64,
    /// 2D layer pixels in the wider picture's margins (none: the layer stays 4:3; the field's
    /// billboards, which LT-144 draws into the margins, aren't counted).
    pub outside: u64,
}

/// [`Hud`] for a scene at `reach`, every `every` frames of its ticks with a 2D layer; with `out`,
/// the tick with the most 2D as `<out>/<scene>-hud.png` (4:3 above, wider below).
#[allow(clippy::too_many_arguments)]
pub fn hud(disc: &Arc<Image>, scene: &Scene, state: &[u8], reach: crate::wide::Reach, every: u64, scale: u32, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, out: Option<&std::path::Path>) -> Result<Hud, String> {
    use crate::light::Look;
    use crate::play::native_at;
    use crate::wide::Reach;
    let mut s = Session::new(disc, scene, state)?;
    s.reach = reach;
    let mut h = Hud::default();
    let mut best: Option<(u64, crate::gate::Picture)> = None;
    let mut seen = 0u64;
    for &pad in &scene.tape {
        for (tick, shown) in s.frame(pad, assets)? {
            if tick.table.passthrough {
                continue;
            }
            seen += 1;
            if !(seen - 1).is_multiple_of((every / 2).max(1)) {
                continue;
            }
            let game = native_at(&tick, &shown, assets, cache, comp, scale, Look::Original, Reach::GAME);
            // The field's billboards reach the margins on purpose (LT-144): they're the world's,
            // not the HUD's, so they're kept to the 4:3 here.
            let wide = match crate::play::view_with(&tick, &shown, assets, cache, comp, scale, Look::Original, reach, &|o| layer2d::Options { wide: false, ..o }) {
                crate::play::View::Rendered(r) => comp.read(&r),
                crate::play::View::Picture(p) => p,
            };
            let left = (wide.w - game.w) / 2;
            h.ticks += 1;
            let mut n = 0;
            for y in 0..game.h {
                for x in 0..game.w {
                    let (i, j) = ((y * game.w + x) as usize, (y * wide.w + x + left) as usize);
                    if game.layer[i] {
                        n += 1;
                        h.same += u64::from(wide.layer[j] && game.rgba[i * 4..i * 4 + 3] == wide.rgba[j * 4..j * 4 + 3]);
                    }
                }
            }
            h.layer_px += n;
            for y in 0..wide.h {
                for x in (0..left).chain(wide.w - left..wide.w) {
                    h.outside += u64::from(wide.layer[(y * wide.w + x) as usize]);
                }
            }
            if out.is_some() && best.as_ref().is_none_or(|(b, _)| n > *b) {
                best = Some((n, stack_pictures(&[&game, &wide])));
            }
        }
    }
    if let (Some(dir), Some((_, p))) = (out, best) {
        crate::gate::save_png(&dir.join(format!("{}-hud.png", scene.name)), &p)?;
    }
    Ok(h)
}

/// LT-140's measurements of a scene's frozen frames (the field menu's) at one reach: every tick drawn
/// (the compositor keeps the picture as the freeze's tick is drawn), lit, at `scale`.
#[derive(Default)]
pub struct FrozenFrame {
    /// Ticks that showed a frozen frame, and of them those drawn from the native picture.
    pub ticks: u32,
    pub native: u32,
    /// Freezes seen, and those whose kept picture is, bit for bit, the native picture last drawn
    /// for the draw area the game copied (the frame on screen as the menu opened).
    pub freezes: u32,
    pub kept_exact: u32,
    /// The quads' colour (the game's dim: 0x7C − 4·level), from their packets.
    pub dim: u8,
    /// Margin pixels of the frozen ticks, those the 2D layer drew, and those within a 5-bit step
    /// (8 in each channel) of the frozen picture dimmed by the quads' colour; the worst channel.
    pub margin_px: u64,
    pub margin_drawn: u64,
    pub margin_near: u64,
    pub margin_worst: u8,
    /// The middle's outer two columns a side (PSX pixels, under no panel but the cursor), the same.
    pub edge_px: u64,
    pub edge_near: u64,
    /// How bright the margins and the edge columns are against the frozen picture dimmed (sums of
    /// their pixels' channels over the dimmed picture's), the worst difference of a tick: a fade
    /// drawn over the frozen frame darkens both alike.
    pub level_gap: f64,
    /// Middle pixels the margin copies changed: each frozen tick drawn again without them.
    pub copies_changed: u64,
    /// The frozen ticks' middles (the 4:3 part, by frame) and the frozen picture's middle, for
    /// comparing reaches (the first [`FROZEN_KEPT`] ticks).
    pub middles: Vec<(u64, Picture)>,
    pub frozen: Option<Picture>,
}

/// Frozen ticks whose middles [`frozen_frame`] keeps.
pub const FROZEN_KEPT: usize = 12;

/// The `left`..`left + w` columns of `p`.
fn crop_columns(p: &Picture, left: u32, w: u32) -> Picture {
    let mut out = Picture { w, h: p.h, rgba: Vec::with_capacity((w * p.h * 4) as usize), native: Vec::new(), layer: Vec::new() };
    for y in 0..p.h {
        let i = (y * p.w + left) as usize;
        out.rgba.extend_from_slice(&p.rgba[i * 4..(i + w as usize) * 4]);
        out.native.extend_from_slice(&p.native[i..i + w as usize]);
        out.layer.extend_from_slice(&p.layer[i..i + w as usize]);
    }
    out
}

/// [`FrozenFrame`] for a scene at `reach`.
#[allow(clippy::too_many_arguments)]
pub fn frozen_frame(disc: &Arc<Image>, scene: &Scene, state: &[u8], reach: crate::wide::Reach, scale: u32, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Result<FrozenFrame, String> {
    use crate::light::Look;
    use crate::play::{view_with, View};
    let mut s = Session::new(disc, scene, state)?;
    s.reach = reach;
    let mut f = FrozenFrame::default();
    // The picture last drawn for each draw area (x, y), and the frozen frame's.
    let mut last: std::collections::HashMap<[u16; 2], Picture> = std::collections::HashMap::new();
    let mut frozen: Option<Picture> = None;
    let read = |v: View, comp: &Compositor| match v {
        View::Rendered(r) => comp.read(&r),
        View::Picture(p) => p,
    };
    for &pad in &scene.tape {
        for (tick, shown) in s.frame(pad, assets)? {
            let t = &tick.table;
            if let Some(m) = t.frozen.first() {
                f.freezes += 1;
                frozen = last.get(&[m.from[0], m.from[1]]).cloned();
            }
            let p = read(view_with(&tick, &shown, assets, cache, comp, scale, Look::Lit, reach, &|o| o), comp);
            if !t.frozen.is_empty() {
                let kept = comp.frozen();
                f.kept_exact += u32::from(kept.is_some() && kept.as_ref().map(|k| &k.rgba) == frozen.as_ref().map(|q| &q.rgba));
            }
            if !t.passthrough {
                last.insert([t.display[0], t.display[1]], p.clone());
            }
            let quads: Vec<_> = t.packets.iter().filter(|q| !q.aside && t.frozen_quads.contains(&q.addr)).collect();
            if quads.is_empty() || t.passthrough {
                continue;
            }
            f.ticks += 1;
            let Some(fz) = frozen.as_ref().filter(|q| q.w == p.w && q.h == p.h && comp.has_frozen()) else { continue };
            f.native += 1;
            let dim = (quads[0].words[0] & 0xFF) as u8;
            f.dim = dim;
            let width = t.display[2] as u32 * scale;
            let left = (p.w - width) / 2;
            let near = |i: usize| -> u8 {
                (0..3).map(|c| {
                    let e = (fz.rgba[i * 4 + c] as u32 * dim as u32 / 128).min(255) as i32;
                    (p.rgba[i * 4 + c] as i32 - e).unsigned_abs() as u8
                }).max().unwrap_or(0)
            };
            let expected = |i: usize| (0..3).map(|c| (fz.rgba[i * 4 + c] as u64 * dim as u64 / 128).min(255)).sum::<u64>();
            let got = |i: usize| (0..3).map(|c| p.rgba[i * 4 + c] as u64).sum::<u64>();
            let (mut margin_level, mut edge_level) = ([0u64; 2], [0u64; 2]);
            for y in 0..p.h {
                for x in 0..p.w {
                    let i = (y * p.w + x) as usize;
                    if x < left || x >= left + width {
                        margin_level[0] += got(i);
                        margin_level[1] += expected(i);
                    } else if x < left + 2 * scale || x >= left + width - 2 * scale {
                        edge_level[0] += got(i);
                        edge_level[1] += expected(i);
                    }
                    if x < left || x >= left + width {
                        f.margin_px += 1;
                        f.margin_drawn += u64::from(p.layer[i]);
                        let d = near(i);
                        f.margin_near += u64::from(d <= 8);
                        f.margin_worst = f.margin_worst.max(d);
                    } else if x < left + 2 * scale || x >= left + width - 2 * scale {
                        f.edge_px += 1;
                        f.edge_near += u64::from(near(i) <= 8);
                    }
                }
            }
            if left > 0 && margin_level[1] > 0 && edge_level[1] > 0 {
                let ratio = |l: [u64; 2]| l[0] as f64 / l[1] as f64;
                f.level_gap = f.level_gap.max((ratio(margin_level) - ratio(edge_level)).abs());
            }
            if left > 0 {
                // The same tick without the margin copies: the middle doesn't change.
                let bare = read(view_with(&tick, &shown, assets, cache, comp, scale, Look::Lit, reach, &|o| layer2d::Options { wide: false, ..o }), comp);
                for y in 0..p.h {
                    for x in left..left + width {
                        let i = (y * p.w + x) as usize;
                        f.copies_changed += u64::from(p.rgba[i * 4..i * 4 + 3] != bare.rgba[i * 4..i * 4 + 3]);
                    }
                }
            }
            if f.middles.len() < FROZEN_KEPT {
                f.middles.push((tick.record.frame, crop_columns(&p, left, width)));
                if f.frozen.is_none() {
                    f.frozen = Some(crop_columns(fz, left, width));
                }
            }
        }
    }
    Ok(f)
}

/// LT-150's measurements of a scene's full-screen fades at one reach: every tick drawn (as the app
/// draws them, so a frozen frame is kept), lit, at `scale`. Each tick with a full-screen quad
/// carried into the margins is drawn three more ways: without those quads (the unfaded picture),
/// with them kept to the 4:3 (as before LT-150), and at the game's 4:3.
#[derive(Default)]
pub struct Fades {
    /// Ticks with a fade carried into the margins, and of them those it darkens (the middle's edge
    /// columns at least 0.02 darker against the unfaded picture).
    pub ticks: u32,
    pub darkened: u32,
    /// The packets carried (addresses), and the first and last such tick's frames.
    pub packets: Vec<u32>,
    pub frames: (u64, u64),
    /// Margin and edge-column pixels compared, over the darkened ticks.
    pub margin_px: u64,
    pub edge_px: u64,
    /// The worst gap of a darkened tick between how much darker than the unfaded picture the
    /// margins are and the middle's edge columns are ([`fade_gap`]); and the same with the fades
    /// kept to the 4:3 (the margins as before LT-150).
    pub worst_gap: f64,
    pub worst_gap_4_3: f64,
    /// The tick of the worst gap: its frame, and its margins' and edge columns' sums.
    pub worst: Option<(u64, Box<FadeSums>, Box<FadeSums>)>,
    /// How dark the fade gets: the most a tick's edge columns lose of the unfaded picture's
    /// brightness (a fraction), and the margins' on that tick.
    pub deepest: f64,
    pub deepest_margins: f64,
    /// Middle pixels the margin copies changed (each tick against the fades kept to the 4:3).
    pub copies_changed: u64,
    /// Middle pixels the 2D layer alone draws at 4:3 (panels, text, effects), and those the widened
    /// view draws the same; ticks showing a frozen frame aside (LT-140's).
    pub layer_px: u64,
    pub layer_same: u64,
    /// The mid-fade tick (its edge columns nearest half as bright as unfaded): its frame, and the
    /// unfaded picture above the faded.
    pub shot: Option<(u64, crate::gate::Picture)>,
}

/// Bands of the unfaded picture's channel values [`fade_gap`] compares in.
pub const FADE_BANDS: usize = 32;

/// A set of pixels' channels, faded against unfaded: `[channel][band of the unfaded value] =
/// [sum faded, sum unfaded, count]`, values 0–1.
pub type FadeSums = [[[f64; 3]; FADE_BANDS]; 3];

/// Adds a pixel's channels (`faded`, `unfaded`) to `sums`.
fn add_fade(sums: &mut FadeSums, faded: &[u8], unfaded: &[u8]) {
    for c in 0..3 {
        let (f, u) = (faded[c] as f64 / 255.0, unfaded[c] as f64 / 255.0);
        let s = &mut sums[c][((u * FADE_BANDS as f64) as usize).min(FADE_BANDS - 1)];
        s[0] += f;
        s[1] += u;
        s[2] += 1.0;
    }
}

/// How alike a fade darkens two sets of pixels (LT-150): in each channel and each band of its
/// unfaded value ([`FADE_BANDS`]), how much darker than the unfaded picture each set is (the mean
/// of unfaded − faded, 0–1), compared where both have `min` values; the worst difference.
/// Comparing like values keeps it fair whatever the fade does: a subtracting fade takes the same
/// off every channel (and stops at black), a tint more off some channels than others.
pub fn fade_gap(a: &FadeSums, b: &FadeSums, min: f64) -> f64 {
    let dark = |s: &[f64; 3]| (s[1] - s[0]) / s[2];
    (0..3).flat_map(|c| (0..FADE_BANDS).map(move |k| (c, k))).filter(|&(c, k)| a[c][k][2] >= min && b[c][k][2] >= min).map(|(c, k)| (dark(&a[c][k]) - dark(&b[c][k])).abs()).fold(0.0, f64::max)
}

/// A set's brightness against the unfaded picture: all its channels' sums, faded over unfaded.
fn fade_level(s: &FadeSums) -> f64 {
    let (f, u) = s.iter().flatten().fold((0.0, 0.0), |(f, u), b| (f + b[0], u + b[1]));
    if u > 0.0 { f / u } else { 1.0 }
}

/// [`Fades`] for a scene at `reach`.
#[allow(clippy::too_many_arguments)]
pub fn fades(disc: &Arc<Image>, scene: &Scene, state: &[u8], reach: crate::wide::Reach, scale: u32, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Result<Fades, String> {
    use crate::light::Look;
    use crate::play::{view_with, View};
    use crate::wide::Reach;
    let mut s = Session::new(disc, scene, state)?;
    s.reach = reach;
    let mut f = Fades::default();
    let read = |v: View, comp: &Compositor| match v {
        View::Rendered(r) => comp.read(&r),
        View::Picture(p) => p,
    };
    let mut mid = f64::MAX;
    for &pad in &scene.tape {
        for (tick, shown) in s.frame(pad, assets)? {
            let t = &tick.table;
            let screened = layer2d::layer_with(t, scale, layer2d::Options { wide: true, ..layer2d::Options::default() }).screened;
            let left_of = |p: &Picture| (p.w - (t.display[2] as u32 * scale).min(p.w)) / 2;
            if screened.is_empty() || t.passthrough {
                read(view_with(&tick, &shown, assets, cache, comp, scale, Look::Lit, reach, &|o| o), comp);
                continue;
            }
            // The extra pictures first, so the compositor's last picture of the tick is the app's.
            let mut bare = Tick { record: tick.record.clone(), frame: tick.frame.clone(), table: t.clone(), crowd: tick.crowd.clone() };
            for p in bare.table.packets.iter_mut().filter(|p| screened.contains(&p.addr)) {
                p.aside = true;
            }
            let game = read(view_with(&tick, &shown, assets, cache, comp, scale, Look::Lit, Reach::GAME, &|o| o), comp);
            let unfaded = read(view_with(&bare, &shown, assets, cache, comp, scale, Look::Lit, reach, &|o| o), comp);
            let kept = read(view_with(&tick, &shown, assets, cache, comp, scale, Look::Lit, reach, &|o| layer2d::Options { fades_4_3: true, ..o }), comp);
            let p = read(view_with(&tick, &shown, assets, cache, comp, scale, Look::Lit, reach, &|o| o), comp);
            let (left, width) = (left_of(&p), (t.display[2] as u32 * scale).min(p.w));
            if left == 0 || unfaded.w != p.w || kept.w != p.w {
                continue;
            }
            f.ticks += 1;
            if f.frames.0 == 0 {
                f.frames.0 = tick.record.frame;
            }
            f.frames.1 = tick.record.frame;
            for a in &screened {
                if !f.packets.contains(a) {
                    f.packets.push(*a);
                }
            }
            // Channel sums: margins, edge columns (8 PSX columns a side), and the margins with the
            // fade kept to the 4:3.
            let edge = 8 * scale;
            let (mut margins, mut edges, mut margins_4_3) = (Box::new([[[0f64; 3]; FADE_BANDS]; 3]), Box::new([[[0f64; 3]; FADE_BANDS]; 3]), Box::new([[[0f64; 3]; FADE_BANDS]; 3]));
            let (mut m_px, mut e_px) = (0u64, 0u64);
            for y in 0..p.h {
                for x in 0..p.w {
                    let i = (y * p.w + x) as usize;
                    let (px, u) = (&p.rgba[i * 4..i * 4 + 3], &unfaded.rgba[i * 4..i * 4 + 3]);
                    if x < left || x >= left + width {
                        add_fade(&mut margins, px, u);
                        add_fade(&mut margins_4_3, &kept.rgba[i * 4..i * 4 + 3], u);
                        m_px += 1;
                    } else if x < left + edge || x >= left + width - edge {
                        add_fade(&mut edges, px, u);
                        e_px += 1;
                    }
                }
            }
            let level = fade_level(&edges);
            if level <= 0.98 {
                f.darkened += 1;
                f.margin_px += m_px;
                f.edge_px += e_px;
                let min = (64 * scale * scale) as f64;
                let gap = fade_gap(&margins, &edges, min);
                f.worst_gap_4_3 = f.worst_gap_4_3.max(fade_gap(&margins_4_3, &edges, min));
                if gap > f.worst_gap || f.worst.is_none() {
                    f.worst_gap = f.worst_gap.max(gap);
                    f.worst = Some((tick.record.frame, margins.clone(), edges.clone()));
                }
                if 1.0 - level > f.deepest {
                    f.deepest = 1.0 - level;
                    f.deepest_margins = 1.0 - fade_level(&margins);
                }
                if (level - 0.5).abs() < mid {
                    mid = (level - 0.5).abs();
                    f.shot = Some((tick.record.frame, stack_pictures(&[&unfaded, &p])));
                }
            }
            // The middle: the margin copies change nothing, and the 2D layer alone is the 4:3's.
            let frozen = !t.frozen_quads.is_empty();
            for y in 0..p.h {
                for x in left..left + width {
                    let (i, g) = ((y * p.w + x) as usize, (y * game.w + x - left) as usize);
                    f.copies_changed += u64::from(p.rgba[i * 4..i * 4 + 3] != kept.rgba[i * 4..i * 4 + 3]);
                    if !frozen && g < game.layer.len() && game.layer[g] && !game.native[g] && !p.native[i] {
                        f.layer_px += 1;
                        f.layer_same += u64::from(game.rgba[g * 4..g * 4 + 3] == p.rgba[i * 4..i * 4 + 3]);
                    }
                }
            }
        }
    }
    Ok(f)
}

/// LT-119's measurements of a scene's characters: which record fields stay live without a packet
/// slot, and how exactly the records rebuild what the game draws.
#[derive(Clone, Debug, Default)]
pub struct Characters {
    /// Per field (`FIELDS`), ticks it changed and ticks compared, for NPCs with a slot both ticks
    /// and for NPCs without one.
    pub slotted: Vec<(u32, u32)>,
    pub unslotted: Vec<(u32, u32)>,
    /// Characters the game drew (NPCs and doors) whose world matrix and vertices the record
    /// rebuilds exactly (`crate::crowd`), of all.
    pub rebuilt: u32,
    pub drawn: u32,
    /// Ticks of a slotted NPC whose next state one shadow step gives exactly, of all.
    pub shadow: u32,
    pub shadow_ticks: u32,
}

/// The record fields LT-119 follows: name, offset, size.
pub const FIELDS: [(&str, usize, usize); 9] = [("x", 0, 2), ("h", 2, 2), ("z", 4, 2), ("dir", 0xC, 2), ("animation", 0x14, 1), ("frame", 0x30, 1), ("target pose", 0x1D, 1), ("current pose", 0x1C, 1), ("weight", 0x1F, 1)];

impl Characters {
    pub fn text(&self, name: &str) -> String {
        let row = |v: &[(u32, u32)]| FIELDS.iter().zip(v).map(|(f, (c, n))| format!("{} {:.1}%", f.0, 100.0 * *c as f64 / (*n).max(1) as f64)).collect::<Vec<_>>().join(", ");
        format!(
            "{name}: changed a tick, with a slot ({} ticks): {}; without ({} ticks): {}; rebuilt from the records {}/{}; shadow step {}/{}",
            self.slotted.first().map_or(0, |f| f.1),
            row(&self.slotted),
            self.unslotted.first().map_or(0, |f| f.1),
            row(&self.unslotted),
            self.rebuilt,
            self.drawn,
            self.shadow,
            self.shadow_ticks
        )
    }
}

/// [`Characters`] over a scene's tape, every tick.
pub fn characters(disc: &Arc<Image>, scene: &Scene, state: &[u8], assets: &mut Assets) -> Result<Characters, String> {
    use crate::crowd::{actors, npcs, Crowd, Shadow, FIRST_NPC, WANDERERS};
    let mut s = Session::new(disc, scene, state)?;
    let mut crowd = Crowd::new();
    let mut out = Characters { slotted: vec![(0, 0); FIELDS.len()], unslotted: vec![(0, 0); FIELDS.len()], ..Characters::default() };
    let mut prev: Option<lt_scene::Record> = None;
    for &pad in &scene.tape {
        for (tick, _) in s.frame(pad, assets)? {
            let r = &tick.record;
            let Some(cam) = r.camera.as_ref() else {
                prev = None;
                continue;
            };
            let home = crate::wide::home(r);
            let m = &s.m;
            let ram = |a: u32, l: usize| m.read(a, l);
            // What the game drew, rebuilt from the records.
            for c in &tick.frame.characters {
                let i = c.index as usize;
                let Some(a) = actors(r).nth(i).map(|(_, a)| a).filter(|a| (FIRST_NPC..WANDERERS).contains(&i) && matches!(a.kind(), 0 | 2 | 3)) else { continue };
                let Some(rebuilt) = rebuild(&mut crowd, &ram, &tick.table.vram, assets, &a, r.chunk, &cam.view) else { continue };
                out.drawn += 1;
                let ok = rebuilt == c.parts;
                out.rebuilt += u32::from(ok);

            }
            // Which fields move, and the shadow step against the game.
            if let Some(p) = prev.as_ref().filter(|p| p.actors.len() == r.actors.len()) {
                for ((_, a), (_, b)) in npcs(r).zip(npcs(p)) {
                    if !a.drawable(home) || a.kind() != 0 || !b.drawable(home) || a.slotted() != b.slotted() {
                        continue;
                    }
                    let row = if a.slotted() { &mut out.slotted } else { &mut out.unslotted };
                    for (k, &(_, o, l)) in FIELDS.iter().enumerate() {
                        row[k].1 += 1;
                        row[k].0 += u32::from(a.0[o..o + l] != b.0[o..o + l]);
                    }
                    if a.slotted() && a.anim() == b.anim() && a.set() != 0xFF && r.anim_hold == 0 {
                        let tbl = u32::from_le_bytes(m.read(b.anim_table() + b.anim() as u32 * 4, 4).try_into().unwrap_or([0; 4]));
                        let n = m.read(tbl, 1).first().copied().unwrap_or(0);
                        let mut sh = Shadow::of(&b);
                        sh.step(&m.read(tbl, 1 + n as usize * 2), false);
                        out.shadow_ticks += 1;
                        out.shadow += u32::from(sh == Shadow::of(&a));
                    }
                }
            }
            prev = Some(r.clone());
        }
    }
    Ok(out)
}

/// A chest the game draws, against the crowd's (LT-119): in `scene`, its first chest (kind 3)
/// moved 160 units in front of the Knight, turned to `dir`, its lid byte (+0x30) set to `lid`, in a
/// measuring run (the machine is written, as LT-111's probes do). Some ticks later: the parts the
/// game drew, and whether the record rebuilds them exactly. `None` when no chest was drawn.
pub fn chest(disc: &Arc<Image>, scene: &Scene, state: &[u8], lid: u8, dir: i16, assets: &mut Assets) -> Result<Option<(usize, bool)>, String> {
    use crate::crowd::{npcs, Actor, Crowd};
    let mut s = Session::new(disc, scene, state)?;
    let mut crowd = Crowd::new();
    let mut placed: Option<usize> = None;
    let mut ticks = 0;
    for &pad in &scene.tape {
        if placed.is_none() {
            let recs = s.m.read(lt_scene::CHARACTERS, lt_scene::CHARACTER_COUNT * lt_scene::CHARACTER_BYTES);
            let knight = Actor(&recs[..lt_scene::CHARACTER_BYTES]).position();
            let r = lt_scene::Record { actors: recs.clone(), actor_count: lt_scene::CHARACTER_COUNT as u32, ..Default::default() };
            let found = npcs(&r).find(|(_, a)| a.kind() == 3 && a.placed()).map(|(i, _)| i);
            if let Some(i) = found {
                let at = lt_scene::CHARACTERS + (i * lt_scene::CHARACTER_BYTES) as u32;
                for (o, v) in [(0u32, knight[0] + 160), (2, knight[1]), (4, knight[2]), (0xC, dir as i32)] {
                    s.m.write(at + o, &(v as i16).to_le_bytes());
                }
                s.m.write(at + 0x30, &[lid]);
                placed = Some(i);
            }
        }
        for (tick, _) in s.frame(pad, assets)? {
            let Some(i) = placed else { continue };
            ticks += 1;
            if ticks > 60 {
                return Ok(None);
            }
            let (Some(c), Some(cam)) = (tick.frame.characters.iter().find(|c| c.index as usize == i), tick.record.camera.as_ref()) else { continue };
            if ticks < 4 {
                continue;
            }
            let a = Actor(&tick.record.actors[i * lt_scene::CHARACTER_BYTES..(i + 1) * lt_scene::CHARACTER_BYTES]);
            let m = &s.m;
            let ram = |a: u32, l: usize| m.read(a, l);
            let rebuilt = rebuild(&mut crowd, &ram, &tick.table.vram, assets, &a, tick.record.chunk, &cam.view);
            return Ok(Some((c.parts.len(), rebuilt.as_deref() == Some(&c.parts[..]))));
        }
    }
    Ok(None)
}

/// What `crate::crowd` draws for a character, from its record: its parts (an NPC posed by its
/// blend state as the record holds it, a door's base, a chest's body and lid). `None` when its
/// model can't be read.
pub fn rebuild(crowd: &mut crate::crowd::Crowd, ram: &dyn lt_assets::tmd::Ram, vram: &[u16], assets: &mut Assets, a: &crate::crowd::Actor, chunk: u16, view: &lt_scene::Matrix) -> Option<Vec<lt_assets::PartUse>> {
    use crate::crowd::{compose, lid, world};
    let base = crowd.base(ram, vram, assets, a, chunk)?;
    let (cur, tgt, _, w) = a.blend();
    let body = crowd.posed(ram, a, base.first()?, (cur, tgt, w as u16));
    let wm = world(a);
    let entry = u8::from(matches!(a.kind(), 2 | 3));
    let mut parts = vec![lt_assets::PartUse { object: 0, world: wm, model_view: compose(view, &wm), vertices: body, entry, light: None, arena_light: None }];
    if a.kind() == 3 {
        let l = lid(a);
        parts.push(lt_assets::PartUse { object: 1, world: l, model_view: compose(view, &l), vertices: base.get(1)?.clone(), entry, light: None, arena_light: None });
    }
    Some(parts)
}

/// LT-144's measurements on a scene's field ticks, at 2× and `reach`.
#[derive(Clone, Debug, Default)]
pub struct Billboarded {
    pub ticks: u32,
    /// The billboards' projections, by caller; the quads drawn as billboards (the game's, then
    /// those put back), and of all of them those with a corner in the margins.
    pub projected: std::collections::BTreeMap<&'static str, u64>,
    pub drawn: std::collections::BTreeMap<&'static str, u64>,
    pub restored: std::collections::BTreeMap<&'static str, u64>,
    pub in_margins: std::collections::BTreeMap<&'static str, u64>,
    /// Quads drawn, and those with a corner off the whole-pixel grid (sub-pixel).
    pub quads: u64,
    pub sub_pixel: u64,
    /// The billboards' packets (a projection wrote all their corners) the 2D layer didn't draw as
    /// billboards: should be none.
    pub missed: u64,
    /// 2D-layer pixels in the margins: as the view draws them, and with the billboards the game's
    /// (4:3, whole pixels).
    pub margin_layer_px: u64,
    pub margin_layer_px_game: u64,
    /// The smoke rebuild against the game's puffs: corners compared, and those the same.
    pub puff_corners: u64,
    pub puff_corners_same: u64,
    /// Blob-shadow commands in the tables, and drawn: lit with shadows on, lit with shadows off,
    /// the original look.
    pub blobs: u64,
    pub blobs_lit: u64,
    pub blobs_lit_off: u64,
    pub blobs_original: u64,
}

impl Billboarded {
    pub fn text(&self, name: &str) -> String {
        let list = |m: &std::collections::BTreeMap<&'static str, u64>| m.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ");
        format!(
            "{name}: {} field ticks\n  projected: {}\n  drawn as billboards: {}\n  put back: {}\n  in the margins: {}\n  {} quads, {} sub-pixel; {} billboard packets missed\n  2D layer in the margins: {} px (game's: {} px)\n  smoke rebuild: {}/{} corners the game's\n  blob shadows: {} in the tables; drawn lit {} / lit, shadows off {} / original {}",
            self.ticks,
            list(&self.projected),
            list(&self.drawn),
            list(&self.restored),
            list(&self.in_margins),
            self.quads,
            self.sub_pixel,
            self.missed,
            self.margin_layer_px,
            self.margin_layer_px_game,
            self.puff_corners_same,
            self.puff_corners,
            self.blobs,
            self.blobs_lit,
            self.blobs_lit_off,
            self.blobs_original,
        )
    }
}

/// Measures `scene`'s field ticks (every `every` frames) for LT-144 at 2× and `reach`, and writes
/// the tick with the most billboards in the margins to `out` (`<name>-billboards.png`: the view,
/// then with the billboards the game's, then the 4:3 view).
#[allow(clippy::too_many_arguments)]
pub fn billboards(disc: &Arc<Image>, scene: &Scene, state: &[u8], reach: crate::wide::Reach, every: u64, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, out: Option<(&std::path::Path, &str)>) -> Result<Billboarded, String> {
    billboards_at(disc, scene, state, None, reach, every, assets, cache, comp, out)
}

/// [`billboards`] with the Knight moved to `knight` (x, z) as the run starts and the pad left
/// alone for 300 frames: a place the test set's tapes don't go (chimney smoke in view).
#[allow(clippy::too_many_arguments)]
pub fn billboards_at(disc: &Arc<Image>, scene: &Scene, state: &[u8], knight: Option<[i16; 2]>, reach: crate::wide::Reach, every: u64, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, out: Option<(&std::path::Path, &str)>) -> Result<Billboarded, String> {
    use crate::billboard::{Lookup, CALLERS};
    use crate::light::{Lights, Look};
    use crate::play::{layer_options, view_with, View};
    let mut s = Session::new(disc, scene, state)?;
    s.reach = reach;
    let still = vec![0u16; 300];
    let tape = match knight {
        Some([x, z]) => {
            s.m.write(lt_scene::CHARACTERS, &x.to_le_bytes());
            s.m.write(lt_scene::CHARACTERS + 4, &z.to_le_bytes());
            &still
        }
        None => &scene.tape,
    };
    let sine: Vec<i16> = s.m.read(crate::smoke::SINE, crate::smoke::SINE_LEN * 2).as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect();
    let mut b = Billboarded::default();
    let mut seen = 0u64;
    let mut best = 0u64;
    for &pad in tape {
        let ticks = s.frame(pad, assets)?;
        for (tick, shown) in ticks {
            if tick.record.camera.is_none() || tick.table.passthrough {
                continue;
            }
            // Every tick: the smoke rebuild against the game's puffs.
            let game_puffs: Vec<&crate::billboard::Projection> = tick.table.billboards.iter().filter(|p| p.caller == "Particle_BuildBillboardPrim").collect();
            let mut used = 0;
            for t in &tick.table.torches {
                let n = t.drawn().len() * 8;
                let (c, same) = crate::smoke::verify(t, &sine, &game_puffs[used.min(game_puffs.len())..]);
                used += n;
                b.puff_corners += c;
                b.puff_corners_same += same;
            }
            seen += 1;
            if !(seen - 1).is_multiple_of((every / 2).max(1)) {
                continue;
            }
            b.ticks += 1;
            for p in &tick.table.billboards {
                *b.projected.entry(p.caller).or_default() += 1;
            }
            // The quads: the lit look at 2× and `reach`, as the view draws them.
            let frame = crate::wide::frame(&tick.record, &tick.frame, &tick.crowd, assets, reach);
            cache.prepare(assets, &frame);
            let sight = reach.sight(tick.table.display[2] as u32);
            let lit = draw::scene_with(&tick.record, &frame, assets, Precision::Float, Some(&mut comp.normals), None, sight);
            let u = crate::light::uniforms(&tick.record, &lit);
            let opts = layer_options(u.as_ref(), comp.lights, 2, sight.left);
            let layer = layer2d::layer_with(&tick.table, 2, opts);
            let [ox, oy, dw, dh] = tick.table.display.map(f32::from);
            let left = sight.left as f32;
            let mut margins = 0;
            for q in &layer.quads {
                b.quads += 1;
                b.sub_pixel += u64::from(q.corners.iter().any(|c| c[0].fract() != 0.0 || c[1].fract() != 0.0));
                *(if q.restored { &mut b.restored } else { &mut b.drawn }).entry(q.caller).or_default() += 1;
                // In the margins: reaching into the picture's columns either side of the 4:3, on its
                // lines.
                let (x0, x1) = q.corners.iter().fold((f32::MAX, f32::MIN), |(a, b), c| (a.min(c[0]), b.max(c[0])));
                let (y0, y1) = q.corners.iter().fold((f32::MAX, f32::MIN), |(a, b), c| (a.min(c[1]), b.max(c[1])));
                let lines = y1 > oy && y0 < oy + dh;
                if lines && ((x0 < ox && x1 > ox - left) || (x1 > ox + dw && x0 < ox + dw + left)) {
                    *b.in_margins.entry(q.caller).or_default() += 1;
                    margins += 1;
                }
            }
            // Billboard packets the layer didn't draw as billboards: every polygon whose corners
            // are all a billboard caller's projection's, by address.
            let look = Lookup::new(&tick.table.billboards);
            let mut packets = 0u64;
            for p in tick.table.kept() {
                let mut at = 0;
                while at < p.words.len() {
                    let n = emulator_core::gpu::gp0_command_word_count((p.words[at] >> 24) as u8).max(1).min(p.words.len() - at);
                    packets += u64::from(look.command(p, at, &p.words[at..at + n]).is_some_and(|(c, _)| CALLERS.contains(&c)) && !(opts.no_blob_shadows && p.shadow));
                    at += n;
                }
            }
            b.missed += packets.saturating_sub(layer.quads.iter().filter(|q| !q.restored).count() as u64);
            // Blob shadows by look.
            b.blobs += tick.table.kept().filter(|p| p.shadow).count() as u64;
            b.blobs_lit += layer2d::layer_with(&tick.table, 2, layer_options(u.as_ref(), Lights::ALL, 2, 0)).blob_shadows as u64;
            b.blobs_lit_off += layer2d::layer_with(&tick.table, 2, layer_options(u.as_ref(), Lights { shadows: false, lamps: true, water: true }, 2, 0)).blob_shadows as u64;
            b.blobs_original += layer2d::layer_with(&tick.table, 2, layer_options(None, Lights::ALL, 2, 0)).blob_shadows as u64;
            // The 2D layer's pixels in the margins, and the game's.
            let read = |v: View, comp: &mut Compositor| match v {
                View::Rendered(r) => comp.read(&r),
                View::Picture(p) => p,
            };
            let a = read(view_with(&tick, &shown, assets, cache, comp, 2, Look::Lit, reach, &|o| o), comp);
            let g = read(view_with(&tick, &shown, assets, cache, comp, 2, Look::Lit, reach, &|o| layer2d::Options { float: false, wide: false, ..o }), comp);
            let edge = sight.left * 2;
            let in_margin = |p: &Picture| (0..p.h).flat_map(|y| (0..p.w).map(move |x| (x, y))).filter(|&(x, y)| (x < edge || x >= p.w - edge) && p.layer[(y * p.w + x) as usize]).count() as u64;
            b.margin_layer_px += in_margin(&a);
            b.margin_layer_px_game += in_margin(&g);
            if let (Some((dir, name)), true) = (out, margins > best) {
                best = margins;
                let n = read(view_with(&tick, &shown, assets, cache, comp, 2, Look::Lit, crate::wide::Reach::GAME, &|o| o), comp);
                let (w, h) = (a.w, a.h + g.h + n.h);
                let mut rgba = vec![0u8; (w * h * 4) as usize];
                for (row, p) in [(0, &a), (a.h, &g), (a.h + g.h, &n)] {
                    let dx = (w - p.w) / 2;
                    for y in 0..p.h {
                        let s = (y * p.w * 4) as usize;
                        let d = (((row + y) * w + dx) * 4) as usize;
                        rgba[d..d + (p.w * 4) as usize].copy_from_slice(&p.rgba[s..s + (p.w * 4) as usize]);
                    }
                }
                crate::gate::save_png(&dir.join(format!("{name}-billboards.png")), &crate::gate::Picture { w, h, rgba })?;
            }
        }
    }
    Ok(b)
}

/// LT-145's measurements on a scene: every pair of field ticks blended at `k` frames a tick, the
/// billboards as each frame places them (no GPU: the poses, and the 2D layer's quads).
#[derive(Clone, Debug, Default)]
pub struct Moving {
    /// Field ticks with a tick before them; of those posed, and snapped by reason (no camera,
    /// mode, no shared chunk, camera cut).
    pub pairs: u32,
    pub posed: u32,
    pub snaps: [u32; 4],
    /// Billboards drawn in posed pairs: blended with tick A's, held where tick B has them in the
    /// world, and paired but too far apart (drawn as B has them).
    pub billboards: u32,
    pub paired: u32,
    pub held: u32,
    pub jumped: u32,
    /// Billboards moving a pixel or more on screen over a tick, their frames, and of those frames
    /// the ones where it didn't move (shown twice, as when the layer was tick B's).
    pub moving: u32,
    pub moving_frames: u32,
    pub repeated: u32,
    /// The most uneven: a billboard's largest step from frame to frame over its smallest, over
    /// those moving two pixels or more.
    pub uneven: f64,
    /// The furthest a blended billboard is at t = 0 from where tick A drew it, and any billboard
    /// at t = 1 from where tick B draws it (pixels): no pop at a tick.
    pub pop_a: f64,
    pub pop_b: f64,
    /// Blob shadows and the flight shadow blended and under a character (its nearest, within 48
    /// units), and the most their gap on screen strays from a straight blend of the two ticks'
    /// gaps (pixels): as drawn, and as tick B's 2D layer drew them (LT-113).
    pub shadows: u32,
    pub shadow_stray: f64,
    pub shadow_stray_before: f64,
    /// The 2D layer's billboard quads in frames between ticks, and those at other corners than
    /// tick B's; and the quads tick B's own layer draws (as many frames).
    pub quads: u64,
    pub quads_moved: u64,
    pub quads_b: u64,
}

impl Moving {
    pub fn text(&self, name: &str, k: u32) -> String {
        format!(
            "{name} at {k} frames a tick: {} pairs, {} posed, snaps {:?}\n  billboards {} (paired {}, held {}, jumped {})\n  moving {} over {} frames, {} repeated, uneven {:.3}x\n  pop at tick A {:.4} px, at tick B {:.4} px\n  shadows under characters {}: gap strays {:.3} px (tick B's layer: {:.3} px)\n  layer quads between ticks {} ({} moved; tick B's layer {})",
            self.pairs, self.posed, self.snaps, self.billboards, self.paired, self.held, self.jumped, self.moving, self.moving_frames, self.repeated, self.uneven, self.pop_a, self.pop_b, self.shadows, self.shadow_stray, self.shadow_stray_before, self.quads, self.quads_moved, self.quads_b
        )
    }
}

/// Plays `scene` and blends every pair of field ticks at `k` frames a tick, measuring the
/// billboards (LT-145) at 1× and the game's reach.
pub fn moving(disc: &Arc<Image>, scene: &Scene, state: &[u8], k: u32, assets: &mut Assets) -> Result<Moving, String> {
    use crate::billboard::{between as moved, Projection, FAR_FLAGS};
    use crate::interp::{between, Between, Pose, Snap};
    use crate::light::Affine;
    let centre = |q: &Projection| -> [f64; 2] { [0, 1].map(|x| q.corners.iter().map(|c| c.float[x] as f64).sum::<f64>() / 4.0) };
    let dist = |a: [f64; 2], b: [f64; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    let corner_off = |a: &Projection, b: &Projection| a.corners.iter().zip(&b.corners).map(|(x, y)| dist(x.float.map(f64::from), y.float.map(f64::from))).fold(0.0, f64::max);
    let mut s = Session::new(disc, scene, state)?;
    let mut m = Moving { uneven: 1.0, ..Moving::default() };
    let mut prev: Option<Tick> = None;
    let opts = layer2d::Options { float: true, ..layer2d::Options::default() };
    for &pad in &scene.tape {
        for (tick, _) in s.frame(pad, assets)? {
            if let (Some(a), true) = (prev.as_ref(), tick.record.camera.is_some()) {
                if !tick.table.passthrough {
                    m.pairs += 1;
                    let poses: Vec<Between> = (0..=k).map(|j| between((&a.record, &a.frame), (&tick.record, &tick.frame), j as f64 / k as f64)).collect();
                    let ps: Vec<&Pose> = poses.iter().filter_map(|b| if let Between::Pose(p) = b { Some(p) } else { None }).collect();
                    if let Between::Snap(r) = &poses[0] {
                        m.snaps[match r {
                            Snap::NoCamera => 0,
                            Snap::Mode => 1,
                            Snap::NoSharedChunk => 2,
                            Snap::CameraCut => 3,
                        }] += 1;
                    } else if ps.len() == k as usize + 1 {
                        m.posed += 1;
                        let (pa, pb) = (&a.table.billboards, &tick.table.billboards);
                        let mv: Vec<_> = ps.iter().map(|p| moved(pa, pb, p)).collect();
                        let half = &mv[1];
                        m.paired += half.paired;
                        m.held += half.held;
                        m.jumped += half.jumped;
                        for (i, p) in pb.iter().enumerate() {
                            if p.flags & FAR_FLAGS != 0 {
                                continue;
                            }
                            m.billboards += 1;
                            let at: Vec<[f64; 2]> = mv.iter().map(|v| centre(&v.projections[i])).collect();
                            m.pop_b = m.pop_b.max(corner_off(&mv[k as usize].projections[i], p));
                            let with = half.with[i];
                            if let Some(j) = with {
                                m.pop_a = m.pop_a.max(corner_off(&mv[0].projections[i], &pa[j]));
                            }
                            let steps: Vec<f64> = at.windows(2).map(|w| dist(w[0], w[1])).collect();
                            let total = dist(at[0], at[k as usize]);
                            if total >= 1.0 {
                                m.moving += 1;
                                m.moving_frames += k;
                                m.repeated += steps.iter().filter(|&&d| d < 1e-3).count() as u32;
                            }
                            if total >= 2.0 {
                                let (lo, hi) = steps.iter().fold((f64::MAX, 0.0f64), |(lo, hi), &d| (lo.min(d), hi.max(d)));
                                m.uneven = m.uneven.max(hi / lo.max(1e-9));
                            }
                            // A blob shadow (or the flight shadow) under a character: the gap
                            // between them on screen.
                            let (Some(j), true) = (with, p.caller == "Npc_DrawIfVisible" || p.caller == crate::billboard::FLIGHT_SHADOW) else { continue };
                            let wc = ps[0].from_b.apply([0, 1, 2].map(|x| p.view.iter().map(|v| v[x]).sum::<f64>() / 4.0));
                            let under = tick.frame.characters.iter().enumerate().filter_map(|(ci, c)| {
                                let w = ps[0].from_b.apply(Affine::of(&c.parts.first()?.model_view).t);
                                let d = ((w[0] - wc[0]).powi(2) + (w[2] - wc[2]).powi(2)).sqrt();
                                (d <= 48.0).then_some((ci, d))
                            });
                            let Some((ci, _)) = under.min_by(|x, y| x.1.total_cmp(&y.1)) else { continue };
                            if ps.iter().any(|q| q.parts.get(ci).is_none_or(|v| v.is_empty())) {
                                continue;
                            }
                            m.shadows += 1;
                            let feet: Vec<[f64; 2]> = ps.iter().map(|q| p.screen(q.parts[ci][0].0.t).map(f64::from)).collect();
                            let stray = |shadow: &[[f64; 2]]| {
                                let gap: Vec<[f64; 2]> = shadow.iter().zip(&feet).map(|(s, f)| [s[0] - f[0], s[1] - f[1]]).collect();
                                (0..=k as usize).map(|f| {
                                    let t = f as f64 / k as f64;
                                    dist(gap[f], [0, 1].map(|x| gap[0][x] + (gap[k as usize][x] - gap[0][x]) * t))
                                }).fold(0.0, f64::max)
                            };
                            m.shadow_stray = m.shadow_stray.max(stray(&at));
                            // Before: tick A's layer at t = 0, tick B's after.
                            let before: Vec<[f64; 2]> = (0..=k as usize).map(|f| if f == 0 { centre(&pa[j]) } else { centre(p) }).collect();
                            m.shadow_stray_before = m.shadow_stray_before.max(stray(&before));
                        }
                        // The 2D layer draws the quads at the frame's corners.
                        let own = layer2d::layer_with(&tick.table, 1, opts);
                        m.quads_b += own.quads.len() as u64 * (k as u64 - 1);
                        for v in &mv[1..k as usize] {
                            let l = layer2d::layer_from(&tick.table, &v.projections, 1, opts);
                            m.quads += l.quads.len() as u64;
                            m.quads_moved += l.quads.iter().zip(&own.quads).filter(|(q, o)| q.corners.iter().zip(&o.corners).any(|(x, y)| (x[0] - y[0]).abs() > 1e-3 || (x[1] - y[1]).abs() > 1e-3)).count() as u64;
                        }
                    }
                }
            }
            prev = Some(tick);
        }
    }
    Ok(m)
}

/// LT-142's measurements on a scene: the actors two event tasks draw themselves
/// (`lt_scene::Drawer::ApproachKnight`, `FlickerIn`).
#[derive(Clone, Debug, Default)]
pub struct EventActors {
    /// Field ticks, and those with an event actor drawn.
    pub ticks: u32,
    pub event_ticks: u32,
    /// Ticks each (character index, drawer) was drawn.
    pub drawn: std::collections::BTreeMap<(u16, lt_scene::Drawer), u32>,
    /// The event characters' parts: with posed vertices, and placed at their record's position
    /// (the world matrix's translation).
    pub parts: u32,
    pub posed: u32,
    pub placed: u32,
    /// Event characters lt-assets resolved (every part), of all.
    pub resolved: u32,
    pub characters: u32,
    /// Per drawer: event characters their record rebuilds exactly (world matrix and vertices, as
    /// `crate::crowd` poses an NPC), of those compared. The approach's always are; Zeppetto's
    /// flicker isn't (no blend of his record's poses gives the vertices the game draws then: the
    /// capture's copy of them is what the native view draws).
    pub rebuilt: std::collections::BTreeMap<lt_scene::Drawer, (u32, u32)>,
    /// Packets inside the event parts' polygon records: set aside (drawn natively), and kept in
    /// the 2D layer.
    pub aside: u32,
    pub kept: u32,
    /// Sampled ticks: pixels the event actors change in the lit view (1×), and of those the native
    /// pixels where the lit view isn't the original look.
    pub sampled: u32,
    pub pixels: u64,
    pub lit: u64,
    /// `Model_DrawParts` calls left uncaptured.
    pub other_models: u32,
}

impl EventActors {
    pub fn text(&self, name: &str) -> String {
        let drawn = self.drawn.iter().map(|((i, by), n)| format!("{by:?} #{i} {n}")).collect::<Vec<_>>().join(", ");
        let rebuilt = self.rebuilt.iter().map(|(by, (n, of))| format!("{by:?} {n}/{of}")).collect::<Vec<_>>().join(", ");
        format!(
            "{name}: {} of {} field ticks with event actors ({drawn}); parts {} (posed {}, placed {}); resolved {}/{}; rebuilt from the record: {rebuilt}; packets aside {}, kept {}; {} ticks drawn: {} pixels, {} lit; other models {}",
            self.event_ticks, self.ticks, self.parts, self.posed, self.placed, self.resolved, self.characters, self.aside, self.kept, self.sampled, self.pixels, self.lit, self.other_models
        )
    }
}

/// [`EventActors`] over a scene's tape, every tick; the views drawn every `every`th event tick.
pub fn event_actors(disc: &Arc<Image>, scene: &Scene, state: &[u8], every: u32, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Result<EventActors, String> {
    use crate::crowd::{actors, Crowd};
    use crate::light::Look;
    use lt_scene::Drawer;
    let mut s = Session::new(disc, scene, state)?;
    let mut crowd = Crowd::new();
    let mut out = EventActors::default();
    for &pad in &scene.tape {
        for (tick, shown) in s.frame(pad, assets)? {
            let r = &tick.record;
            out.other_models += r.other_models;
            let Some(cam) = r.camera.as_ref() else { continue };
            out.ticks += 1;
            let events: Vec<&lt_scene::CharacterDraw> = r.characters.iter().filter(|c| c.by != Drawer::Npc).collect();
            if events.is_empty() {
                continue;
            }
            out.event_ticks += 1;
            let m = &s.m;
            let ram = |a: u32, l: usize| m.read(a, l);
            for c in &events {
                *out.drawn.entry((c.index, c.by)).or_default() += 1;
                out.characters += 1;
                for p in &c.parts {
                    out.parts += 1;
                    out.posed += u32::from(!p.vertices.is_empty());
                    out.placed += u32::from(p.world.t == c.position.map(i32::from));
                    // The part's polygon records, as the takeover reads them.
                    let part = m.read(p.record, 0x2C);
                    let records = (u32::from_le_bytes([part[0x28], part[0x29], part[0x2A], part[0x2B]]) & 0x1FFF_FFFF) | 0x8000_0000;
                    let bytes: u32 = (0..20).map(|k| u16::from_le_bytes([part[k * 2], part[k * 2 + 1]]) as u32 * crate::takeover::RECORD_BYTES[k]).sum();
                    for q in tick.table.packets.iter().filter(|q| (records..records + bytes).contains(&q.addr)) {
                        out.aside += u32::from(q.aside);
                        out.kept += u32::from(!q.aside);
                    }
                }
                let Some(used) = tick.frame.characters.iter().find(|u| u.index == c.index) else { continue };
                out.resolved += u32::from(used.parts.len() == c.parts.len());
                if let Some((_, a)) = actors(r).nth(c.index as usize) {
                    if let Some(rebuilt) = rebuild(&mut crowd, &ram, &tick.table.vram, assets, &a, r.chunk, &cam.view) {
                        let e = out.rebuilt.entry(c.by).or_default();
                        e.0 += u32::from(rebuilt == used.parts);
                        e.1 += 1;
                    }
                }
            }
            if (out.event_ticks - 1) % every.max(1) == 0 {
                out.sampled += 1;
                let full = crate::play::native_look(&tick, &shown, assets, cache, comp, 1, Look::Lit);
                let original = crate::play::native_look(&tick, &shown, assets, cache, comp, 1, Look::Original);
                let mut without = Tick { record: tick.record.clone(), frame: tick.frame.clone(), table: tick.table.clone(), crowd: tick.crowd.clone() };
                without.frame.characters.retain(|u| !events.iter().any(|c| c.index == u.index));
                let rest = crate::play::native_look(&without, &shown, assets, cache, comp, 1, Look::Lit);
                for i in 0..(full.w * full.h).min(rest.w * rest.h).min(original.w * original.h) as usize {
                    let at = i * 4..i * 4 + 3;
                    if full.rgba[at.clone()] != rest.rgba[at.clone()] {
                        out.pixels += 1;
                        out.lit += u64::from(full.native[i] && full.rgba[at.clone()] != original.rgba[at]);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// LT-116/4's measurement: the native view's hooks through a scene, each hook's calls in battle and
/// outside it, and how often its guard word held it back (another overlay's code at its address).
#[derive(Clone, Debug, Default)]
pub struct Guards {
    /// Per hook of [`crate::play::Player::hooks`]: its name, calls in battle, calls outside, and
    /// times its address ran with another word there than its guard in battle frames.
    pub hooks: Vec<(String, u64, u64, u64)>,
    /// `Model_DrawParts` calls in battle; of them, those taken for a field character, an event
    /// actor or the first flight's glow (none, if the guards hold); and battle draws outside battle.
    pub model_battle: u64,
    pub field_in_battle: u64,
    pub battle_outside: u64,
    /// Of the battle calls, those whose caller lies where `Npc_DrawIfVisible` does in OVR02 (OVR04's
    /// code there): what the address alone would have taken for an NPC.
    pub npc_address_in_battle: u64,
    /// Frames run in battle.
    pub battle_frames: u64,
}

impl Guards {
    /// The field's hooks (OVR02's and OVR24's draw functions) and their calls in battle.
    pub fn field_calls_in_battle(&self) -> Vec<(&str, u64)> {
        let field = ["WorldChunk_DrawObjects", "WorldChunk_DrawObjectMesh", "Npc_DrawIfVisible", "Field_DrawTorchFlames", "func_80133914"];
        self.hooks.iter().filter(|h| field.iter().any(|f| h.0.contains(f))).map(|h| (h.0.as_str(), h.1)).collect()
    }
}

/// Plays `scene` with the native view's hooks (`crate::play::Player`), counting [`Guards`].
pub fn guards(disc: &Arc<Image>, scene: &Scene, state: &[u8]) -> Result<Guards, String> {
    use lt_emu_hooks::{Action, Call, Handler, Hooks};
    struct Counting {
        player: crate::play::Player,
        calls: Vec<[u64; 2]>,
        g: Guards,
    }
    impl Handler for Counting {
        fn call(&mut self, c: &Call) -> Action {
            let battle = c.read(0x1F80_0043, 1)[0] == lt_assets::BATTLE_MODE;
            self.calls[c.hook][battle as usize] += 1;
            if c.name == "Model_DrawParts" || c.name == "Model_DrawParts (second entry)" {
                if battle {
                    self.g.model_battle += 1;
                    self.g.field_in_battle += u64::from(lt_scene::npc_call(c) || lt_scene::event_actor(c).is_some() || lt_scene::flight_part(c));
                    let ra = c.ra() & 0x1FFF_FFFF;
                    self.g.npc_address_in_battle += u64::from(ra > lt_scene::NPC_DRAW & 0x1FFF_FFFF && ra <= (lt_scene::NPC_DRAW + lt_scene::NPC_DRAW_SIZE) & 0x1FFF_FFFF);
                } else {
                    self.g.battle_outside += u64::from(lt_scene::battle_call(c).is_some());
                }
            }
            self.player.call(c)
        }
    }
    let mut m = Machine::from_state(disc, &scene.card, state, scene.frame)?;
    let table = crate::play::Player::hooks();
    let mut h = Hooks::new(table.clone())?;
    h.install(&mut m.cpu);
    let mut k = Counting { player: crate::play::Player::default(), calls: vec![[0; 2]; table.len()], g: Guards::default() };
    let mut held = vec![0u64; table.len()];
    for &pad in &scene.tape {
        let before = h.counts.guarded.clone();
        let battle_before = m.read(0x1F80_0043, 1)[0] == lt_assets::BATTLE_MODE;
        k.player.begin(m.frame);
        m.bus.set_port1_buttons(emulator_core::ButtonState::from_bits(pad));
        h.run_frame(&mut m.cpu, &mut m.bus, &mut k).map_err(|e| format!("frame {}: {e}", m.frame))?;
        m.bus.run_spu_to_current_cycle();
        m.bus.spu.discard_audio();
        m.frame += 1;
        k.player.end();
        // A frame wholly in battle (it began and ended there).
        if battle_before && m.read(0x1F80_0043, 1)[0] == lt_assets::BATTLE_MODE {
            k.g.battle_frames += 1;
            for (i, n) in held.iter_mut().enumerate() {
                *n += h.counts.guarded[i] - before[i];
            }
        }
    }
    k.g.hooks = table.iter().enumerate().map(|(i, t)| (t.name.clone(), k.calls[i][1], k.calls[i][0], held[i])).collect();
    Ok(k.g)
}

/// LT-116's measurements on one battle tick, at 1× (GTE positions), its native 3D against the
/// picture the GPU made of it.
#[derive(Clone, Debug, Default)]
pub struct BattleTick {
    pub frame: u64,
    /// Actors and Toys drawn, and whether the arena was; parts that missed their assets.
    pub actors: u32,
    pub arena: bool,
    pub misses: u32,
    /// Packets set aside (drawn natively).
    pub aside: u32,
    /// Corners of polygons the GTE lights, and of them those with a normal from the file.
    pub lit_corners: u64,
    pub with_normals: u64,
    /// Pixels only the native 3D drew where the game drew too; the mean absolute difference of the
    /// original look's colour from the game's there (per channel), and the same with the battle's
    /// lighting taken off (each lit corner left at its own colour).
    pub pixels: u64,
    pub abs: f64,
    pub abs_unlit: f64,
    /// Mean brightness (0..255) of those pixels in the game's picture, the original look and the lit
    /// look.
    pub bright_game: f64,
    pub bright_original: f64,
    pub bright_lit: f64,
    /// The arena's flash: its light over the battle's base light (`crate::draw::flash`), mean of the
    /// channels; and the actors' furthest from 1.
    pub arena_flash: f32,
    pub actor_flash: f32,
}

/// [`BattleTick`] for a battle tick (`None` for any other).
pub fn battle_tick(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Option<BattleTick> {
    use lt_scene::Drawer;
    let r = &tick.record;
    // Not while the transition still swirls the field away over the arena (its VRAM moves).
    if r.mode != lt_assets::BATTLE_MODE || r.camera.is_none() || tick.table.passthrough || !tick.table.moves.is_empty() {
        return None;
    }
    let mut b = BattleTick { frame: r.frame, aside: tick.table.characters, arena_flash: 1.0, actor_flash: 1.0, ..BattleTick::default() };
    b.actors = r.characters.iter().filter(|c| c.by == Drawer::Battle).count() as u32;
    b.arena = r.characters.iter().any(|c| c.by == Drawer::Arena);
    b.misses = tick.frame.misses.len() as u32;
    let base = r.battle_light;
    for c in &r.characters {
        for p in &c.parts {
            let (Some(l), Some(base)) = (&p.light, &base) else { continue };
            let f = crate::draw::flash(l, base);
            let mean = (f[0] + f[1] + f[2]) / 3.0;
            if c.by == Drawer::Arena {
                b.arena_flash = mean;
            } else if (mean - 1.0).abs() > (b.actor_flash - 1.0).abs() {
                b.actor_flash = mean;
            }
        }
    }
    for ch in &tick.frame.characters {
        let Some(mesh) = assets.mesh(ch.mesh) else { continue };
        for p in &ch.parts {
            let Some(&(first, count)) = mesh.ranges.get(p.object as usize) else { continue };
            for i in first as usize..(first + count) as usize {
                if mesh.corners[i].flags & lt_core::mesh::flag::LIT != 0 && p.light.is_some() {
                    b.lit_corners += 1;
                    b.with_normals += u64::from(mesh.normals.get(i).is_some_and(|n| *n != [0; 3]));
                }
            }
        }
    }
    cache.prepare(assets, &tick.frame);
    let draw = |frame: &lt_assets::Frame, comp: &mut Compositor| {
        let scene = draw::scene(r, frame, assets, Precision::Gte, false);
        comp.draw(Some(&scene), &tick.table, &layer2d::layer(&tick.table, 1), cache, 1)
    };
    let original = draw(&tick.frame, comp);
    let mut unlit = tick.frame.clone();
    for c in &mut unlit.characters {
        for p in &mut c.parts {
            p.light = None;
            p.arena_light = None;
        }
    }
    let unlit = draw(&unlit, comp);
    let lit = crate::play::native_look(tick, shown, assets, cache, comp, 1, crate::light::Look::Lit);
    let (mut abs, mut abs_unlit, mut bg, mut bo, mut bl) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (w, h) = (shown.w.min(original.w), shown.h.min(original.h));
    let sum = |p: [u8; 3]| p.iter().map(|&v| v as u64).sum::<u64>();
    for y in MARGIN..h.saturating_sub(MARGIN) {
        for x in MARGIN..w.saturating_sub(MARGIN) {
            let i = (y * original.w + x) as usize;
            if !original.native[i] || original.layer[i] {
                continue;
            }
            let (g, o, u) = (px(&shown.rgba, shown.w, x, y), px(&original.rgba, original.w, x, y), px(&unlit.rgba, unlit.w, x, y));
            b.pixels += 1;
            abs += (0..3).map(|c| o[c].abs_diff(g[c]) as u64).sum::<u64>();
            abs_unlit += (0..3).map(|c| u[c].abs_diff(g[c]) as u64).sum::<u64>();
            bg += sum(g);
            bo += sum(o);
            bl += sum(px(&lit.rgba, lit.w, x, y));
        }
    }
    let n = (3 * b.pixels.max(1)) as f64;
    (b.abs, b.abs_unlit, b.bright_game, b.bright_original, b.bright_lit) = (abs as f64 / n, abs_unlit as f64 / n, bg as f64 / n, bo as f64 / n, bl as f64 / n);
    Some(b)
}

/// [`battle_tick`] over a scene's battle ticks, every `every` frames (each tick at 1×, with the
/// probe's changes when given).
#[allow(clippy::too_many_arguments)]
pub fn battle_scene(disc: &Arc<Image>, scene: &Scene, state: &[u8], every: u64, probe: Probe, assets: &mut Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor) -> Result<Vec<BattleTick>, String> {
    let m = Machine::from_state(disc, &scene.card, state, scene.frame)?;
    let mut s = Session::probing(m, probe)?;
    let mut out = Vec::new();
    let mut last: Option<Tick> = None;
    for &pad in &scene.tape {
        for (tick, shown) in s.frame(pad, assets)? {
            // The picture the GPU made of the last tick arrives with this one.
            if let Some(t) = last.take() {
                if let Some(b) = battle_tick(&t, &shown, assets, cache, comp) {
                    out.push(b);
                }
            }
            if tick.record.frame % every < 2 {
                last = Some(tick);
            }
        }
    }
    Ok(out)
}
