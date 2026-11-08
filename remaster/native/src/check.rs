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
    let effects: Vec<u32> = tick.table.kept().filter(|p| p.bucket > 0).map(|p| p.addr).collect();
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
    let mut f = Field { frame: tick.record.frame, effects: effects.len() as u32, aside: tick.table.terrain + tick.table.characters, ..Field::default() };
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
    let mut last: Option<(u64, Picture)> = None;
    for &pad in &scene.tape {
        for (tick, shown) in s.frame(pad, assets)? {
            // The picture the GPU made of the last tick arrives with this one.
            if let Some((frame, native)) = last.take() {
                if frame % every < 2 {
                    compare(&mut out, &native, &shown, scale);
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
    Ok(out)
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
    if tick.table.passthrough {
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
            let Some(cam) = tick.record.camera.clone() else { continue };
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
            if tick.record.camera.is_none() || tick.table.passthrough {
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
    /// 2D layer pixels in the wider picture's margins (none: the layer stays 4:3).
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
            let wide = native_at(&tick, &shown, assets, cache, comp, scale, Look::Original, reach);
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
    let mut parts = vec![lt_assets::PartUse { object: 0, world: wm, model_view: compose(view, &wm), vertices: body }];
    if a.kind() == 3 {
        let l = lid(a);
        parts.push(lt_assets::PartUse { object: 1, world: l, model_view: compose(view, &l), vertices: base.get(1)?.clone() });
    }
    Some(parts)
}
