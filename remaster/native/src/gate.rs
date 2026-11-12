//! The gate (LT-110): a tick drawn natively against the frame the emulator made of it.
//!
//! **The emulated coverage** comes from two runs from one saved state:
//! - **A:** the game as it is.
//! - **B:** the same input, with the first tick's terrain (`WorldChunk_DrawObjects`, and the first
//!   flight's cells, `func_80133914`) and characters (`Model_DrawParts` for a character,
//!   `lt_scene::character_call`) not drawn.
//!
//! The GPU draws a tick during the next one and the display flips after that, so the first picture
//! where A and B differ is that tick's. A pixel is covered where A ≠ B.
//!
//! **The clear colour:** both runs clear to magenta. A hook on `Dbg_PutDrawEnv` (main 0x801A4D18)
//! sets the environment's background colour (+0x19, R G B) before the GPU gets it; the game copies
//! it from `ctx+0x3D` every frame, and flight paints it grass green. So geometry that draws black,
//! or the same green as the ground, still differs from the background.
//!
//! **Excluded pixels:**
//! - where B isn't the clear colour: the HUD, effects, fades;
//! - a margin at the screen's edges.
//!
//! Semi-transparent overlays (the compass, torch flames) still differ between A and B where geometry
//! is under them, so they don't hide it. The first flight paints its sky as a gradient over the
//! whole screen behind everything (`D_80135188`, LT-143): both runs leave it out, or B would show
//! no clear colour at all.
//!
//! **Colour:** where both cover a pixel, how far the native colour is from the emulated one (PS1
//! dithering and 15-bit colour put a few steps between equal colours).
//!
//! **The native coverage** is the same tick from A's capture: resolved by lt-assets, projected by
//! [`crate::gte`], rasterized by [`crate::raster`].

use std::sync::Arc;

use emulator_core::ButtonState;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::{Assets, Frame};
use lt_emu_hooks::{Action, Call, Handler, Hook, Hooks};
use lt_scene::{hooks, Capture, Record, DRAW_OBJECTS, DRAW_OBJECTS_WORD, MODEL_DRAW_PARTS, MODEL_DRAW_PARTS_2, SKY_CELL, SKY_CELL_WORD};
use lt_testset::{Image as Disc, Machine, Scene};

use crate::draw;
use crate::takeover::SKY_GRADIENT;
use crate::gte::Precision;
use crate::raster::{Image, Raster};

/// Pixels left out at each screen edge.
pub const MARGIN: u32 = 4;
/// What both branches clear to: a colour no geometry has.
pub const CLEAR: [u8; 3] = [0xFF, 0x00, 0xFF];
/// Main: `Dbg_PutDrawEnv(DRAWENV *env)`; the environment's isbg is at +0x18, R G B after it.
pub const PUT_DRAW_ENV: u32 = 0x801A_4D18;
/// Frames a branch runs: the tick (up to 3 frames), the next while the GPU draws it, the flip.
const BRANCH_FRAMES: usize = 10;
/// A colour within this many steps (of 255, any channel) is the same colour.
pub const COLOUR_CLOSE: u8 = 16;

/// The capture, and a skip of the first tick's geometry draws (branch B).
struct Brancher {
    cap: Capture,
    base: usize,
    skip: bool,
    starts: u32,
    in_first: bool,
    skipped: u32,
}

impl Handler for Brancher {
    fn call(&mut self, c: &Call) -> Action {
        if c.hook < self.base {
            match c.hook {
                0 => {
                    self.starts += 1;
                    self.in_first = self.starts == 1;
                }
                1 => self.in_first = false,
                _ => {}
            }
            return self.cap.call(c);
        }
        if c.hook - self.base == 3 {
            c.write(c.arg(0) + 0x19, &CLEAR);
            // The first flight's gradient sky: its packets emptied (the GPU walks past them).
            if c.read_u32(SKY_CELL) == SKY_CELL_WORD {
                for g in SKY_GRADIENT {
                    c.write(g + 3, &[0]);
                }
            }
            return Action::Continue;
        }
        if !self.skip || !self.in_first {
            return Action::Continue;
        }
        match c.hook - self.base {
            0 => {
                self.skipped += 1;
                // `WorldChunk_DrawObjects` returns the packet pointer it was given, unmoved.
                Action::Return(Some(c.arg(1)))
            }
            1 | 2 if lt_scene::character_call(c) => {
                self.skipped += 1;
                Action::Return(None)
            }
            // `func_80133914` returns the packet cursor it was given, unmoved.
            4 => {
                self.skipped += 1;
                Action::Return(Some(c.arg(2)))
            }
            _ => Action::Continue,
        }
    }
}

fn table() -> (Hooks, usize) {
    let base = hooks();
    let n = base.hooks().len();
    let mut v = base.hooks().to_vec();
    v.push(Hook::replace("gate: skip WorldChunk_DrawObjects", DRAW_OBJECTS).guarded(DRAW_OBJECTS_WORD));
    v.push(Hook::replace("gate: skip Model_DrawParts", MODEL_DRAW_PARTS));
    v.push(Hook::replace("gate: skip Model_DrawParts (second entry)", MODEL_DRAW_PARTS_2));
    v.push(Hook::observe("gate: clear to magenta", PUT_DRAW_ENV));
    v.push(Hook::replace("gate: skip func_80133914", SKY_CELL).guarded(SKY_CELL_WORD));
    (Hooks::new(v).expect("the gate's hook table"), n)
}

/// A displayed picture.
#[derive(Clone)]
pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

struct Branch {
    pictures: Vec<Picture>,
    /// The first tick that started in the branch, and its frame resolved at its end (A only).
    first: Option<(Record, Frame)>,
    skipped: u32,
}

fn branch(disc: &Arc<Disc>, card: &[u8], state: &[u8], frame: u64, tape: &[u16], skip: bool, assets: Option<&mut Assets>) -> Result<Branch, String> {
    let mut m = Machine::from_state(disc, card, state, frame)?;
    let (mut h, base) = table();
    h.install(&mut m.cpu);
    let mut b = Brancher { cap: Capture::new(), base, skip, starts: 0, in_first: false, skipped: 0 };
    let mut out = Branch { pictures: Vec::new(), first: None, skipped: 0 };
    let mut assets = assets;
    for i in 0..BRANCH_FRAMES {
        b.cap.frame = m.frame;
        m.bus.set_port1_buttons(ButtonState::from_bits(tape.get(i).copied().unwrap_or(0)));
        h.run_frame(&mut m.cpu, &mut m.bus, &mut b).map_err(|e| format!("frame {}: {e}", m.frame))?;
        m.bus.run_spu_to_current_cycle();
        m.bus.spu.discard_audio();
        m.frame += 1;
        let records = b.cap.take();
        if out.first.is_none() {
            if let (Some(r), Some(a)) = (records.into_iter().next(), assets.as_deref_mut()) {
                let mm = &m;
                let f = a.resolve(&r, &|addr: u32, len: usize| mm.read(addr, len), m.bus.gpu.vram.words());
                out.first = Some((r, f));
            }
        }
        // The framebuffer the display shows, from VRAM (not `display_rgba8`, which slides the
        // picture by the screen position the game sets, 8 lines down and a few pixels right).
        let d = m.bus.gpu.display_area();
        let rgba = m.bus.gpu.vram.to_rgba8(d.x, d.y, d.width, d.height);
        out.pictures.push(Picture { w: d.width as u32, h: d.height as u32, rgba });
    }
    out.skipped = b.skipped;
    Ok(out)
}

/// One tick compared.
#[derive(Clone, Default)]
pub struct Sample {
    pub tick_frame: u64,
    pub compared: u64,
    pub agree: u64,
    /// Covered in the emulated frame only (native missing), in the native only (extra).
    pub emulated_only: u64,
    pub native_only: u64,
    pub excluded: u64,
    /// The clear colour compared against.
    pub clear: [u8; 3],
    pub polygons: u64,
    pub culled: u64,
    /// Pixels both cover, those within [`COLOUR_CLOSE`] of the emulated colour, and the sum of the
    /// largest channel difference over them.
    pub colour_pixels: u64,
    pub colour_close: u64,
    pub colour_error: u64,
    /// The report image: emulated, native, mismatch and the emulated frame without the tick's
    /// geometry, stacked.
    pub report: Option<Picture>,
    /// Mismatched pixels by cause ([`explain`]), when asked for, and a few pixels of each in full.
    pub causes: std::collections::BTreeMap<String, u64>,
    pub details: Vec<String>,
}

impl Sample {
    pub fn agreement(&self) -> f64 {
        self.agree as f64 / self.compared.max(1) as f64
    }
}

/// The mismatch panel's colours.
pub const COVERED: [u8; 3] = [90, 90, 90];
pub const EMULATED_ONLY: [u8; 3] = [0, 220, 90];
pub const NATIVE_ONLY: [u8; 3] = [235, 40, 200];
pub const EXCLUDED: [u8; 3] = [20, 30, 90];

/// Compares the tick drawn at `frame` (from the saved `state`) natively and in the emulator.
/// `None` when no picture showed the tick's geometry (nothing drawn).
#[allow(clippy::too_many_arguments)]
pub fn sample(disc: &Arc<Disc>, card: &[u8], state: &[u8], frame: u64, tape: &[u16], assets: &mut Assets, cache: &mut Cache<Wgpu>, raster: &mut Raster, precision: Precision, why: bool) -> Result<Option<Sample>, String> {
    let a = branch(disc, card, state, frame, tape, false, Some(assets))?;
    let b = branch(disc, card, state, frame, tape, true, None)?;
    let Some((record, f)) = a.first else { return Ok(None) };
    // The field's gate: a battle (LT-116) is lit and clipped as LT-110's unlit raster isn't
    // (`tests/battle.rs` measures it).
    if b.skipped == 0 || record.mode == lt_assets::BATTLE_MODE {
        return Ok(None);
    }
    // The tick's picture: of those where A and B differ, the one where B shows the most clear colour
    // (in flight an earlier picture already differs a little, with the ground still drawn in B).
    let clear_in = |p: &Picture| p.rgba.chunks(4).filter(|c| c[..3] == CLEAR).count();
    let Some(k) = (0..a.pictures.len()).filter(|&i| a.pictures[i].rgba != b.pictures[i].rgba).max_by_key(|&i| (clear_in(&b.pictures[i]), std::cmp::Reverse(i))) else { return Ok(None) };
    if clear_in(&b.pictures[k]) == 0 {
        return Ok(None);
    }
    let (pa, pb) = (&a.pictures[k], &b.pictures[k]);
    cache.prepare(assets, &f);
    let scene = draw::scene(&record, &f, assets, precision, why);
    let native = raster.draw(&scene, cache);
    let mut s = compare(pa, pb, &native);
    if why {
        let (causes, details) = explain(pa, pb, &native, &s, &scene);
        s.causes = causes;
        s.details = details;
    }
    s.tick_frame = record.frame;
    s.polygons = scene.polygons;
    s.culled = scene.culled;
    Ok(Some(s))
}

/// Coverage masks compared, and the report image.
pub fn compare(a: &Picture, b: &Picture, native: &Image) -> Sample {
    let (w, h) = (a.w.min(native.w), a.h.min(native.h));
    let px = |p: &Picture, x: u32, y: u32| -> [u8; 3] {
        let o = ((y * p.w + x) * 4) as usize;
        [p.rgba[o], p.rgba[o + 1], p.rgba[o + 2]]
    };
    // Magenta survives the GPU's 15-bit colour exactly (31 widens back to 255).
    let clear = CLEAR;
    let mut s = Sample { clear, ..Sample::default() };
    let mut map = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let o = ((y * w + x) * 4) as usize;
            let edge = x < MARGIN || y < MARGIN || x + MARGIN >= w || y + MARGIN >= h;
            let overlay = px(b, x, y) != clear;
            let c = if edge || overlay {
                s.excluded += 1;
                EXCLUDED
            } else {
                let emu = px(a, x, y) != px(b, x, y);
                let nat = native.mask[(y * native.w + x) as usize];
                s.compared += 1;
                match (emu, nat) {
                    (true, true) => {
                        s.agree += 1;
                        let (e, no) = (px(a, x, y), ((y * native.w + x) * 4) as usize);
                        let d = (0..3).map(|i| e[i].abs_diff(native.rgba[no + i])).max().unwrap_or(0);
                        s.colour_pixels += 1;
                        s.colour_close += u64::from(d <= COLOUR_CLOSE);
                        s.colour_error += d as u64;
                        COVERED
                    }
                    (false, false) => {
                        s.agree += 1;
                        [0, 0, 0]
                    }
                    (true, false) => {
                        s.emulated_only += 1;
                        EMULATED_ONLY
                    }
                    (false, true) => {
                        s.native_only += 1;
                        NATIVE_ONLY
                    }
                }
            };
            map[o..o + 3].copy_from_slice(&c);
            map[o + 3] = 255;
        }
    }
    // Emulated, native, mismatch, and the emulated frame without the tick's geometry (what's
    // excluded), stacked with a 4-pixel gap.
    let gap = 4;
    let rh = h * 4 + gap * 3;
    let mut rgba = vec![0u8; (w * rh * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let dst = |row: u32| ((row * w + x) * 4) as usize;
            let e = px(a, x, y);
            rgba[dst(y)..dst(y) + 4].copy_from_slice(&[e[0], e[1], e[2], 255]);
            let no = ((y * native.w + x) * 4) as usize;
            rgba[dst(y + h + gap)..dst(y + h + gap) + 4].copy_from_slice(&[native.rgba[no], native.rgba[no + 1], native.rgba[no + 2], 255]);
            let mo = ((y * w + x) * 4) as usize;
            rgba[dst(y + 2 * (h + gap))..dst(y + 2 * (h + gap)) + 4].copy_from_slice(&map[mo..mo + 4]);
            let k = px(b, x, y);
            rgba[dst(y + 3 * (h + gap))..dst(y + 3 * (h + gap)) + 4].copy_from_slice(&[k[0], k[1], k[2], 255]);
        }
    }
    s.report = Some(Picture { w, h: rh, rgba });
    s
}

/// A scene's samples.
#[derive(Default)]
pub struct SceneResult {
    pub name: String,
    pub samples: Vec<Sample>,
    /// Ticks tried that drew nothing (a battle, a fade).
    pub empty: u32,
}

impl SceneResult {
    pub fn compared(&self) -> u64 {
        self.samples.iter().map(|s| s.compared).sum()
    }

    pub fn agreement(&self) -> f64 {
        self.samples.iter().map(|s| s.agree).sum::<u64>() as f64 / self.compared().max(1) as f64
    }

    pub fn worst(&self) -> Option<&Sample> {
        self.samples.iter().min_by(|a, b| a.agreement().total_cmp(&b.agreement()))
    }
}

/// Samples a scene every `every` frames along its tape.
#[allow(clippy::too_many_arguments)]
pub fn scene(disc: &Arc<Disc>, name: &str, scene: &Scene, state: &[u8], every: usize, assets: &mut Assets, cache: &mut Cache<Wgpu>, raster: &mut Raster, precision: Precision, why: bool) -> Result<SceneResult, String> {
    let mut m = Machine::from_state(disc, &scene.card, state, scene.frame)?;
    let mut out = SceneResult { name: name.into(), ..SceneResult::default() };
    let last = scene.tape.len().saturating_sub(BRANCH_FRAMES);
    for f in 0..last {
        if f % every == every / 2 {
            let st = m.save_state()?;
            let card = m.card_now();
            match sample(disc, &card, &st, m.frame, &scene.tape[f..], assets, cache, raster, precision, why)? {
                Some(s) => out.samples.push(s),
                None => out.empty += 1,
            }
        }
        m.step(scene.tape[f])?;
    }
    Ok(out)
}

/// Writes an RGBA picture as a PNG.
pub fn save_png(path: &std::path::Path, p: &Picture) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), p.w, p.h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().and_then(|mut w| w.write_image_data(&p.rgba)).map_err(|e| e.to_string())
}

fn inside(p: (f64, f64), t: [(f64, f64); 3]) -> bool {
    let area = (t[1].0 - t[0].0) * (t[2].1 - t[0].1) - (t[1].1 - t[0].1) * (t[2].0 - t[0].0);
    if area == 0.0 {
        return false;
    }
    let e = |a: (f64, f64), b: (f64, f64)| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
    let (d0, d1, d2) = (e(t[0], t[1]), e(t[1], t[2]), e(t[2], t[0]));
    (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
}

/// How far (x, y) is from the nearest edge of `poly`'s triangles, in pixels.
fn edge_distance(poly: &draw::Polygon, x: f64, y: f64) -> f64 {
    let s = |i: usize| (poly.screen[i].x as f64, poly.screen[i].y as f64);
    let tris: &[[usize; 3]] = if poly.screen.len() == 4 { &[[0, 1, 2], [1, 3, 2]] } else { &[[0, 1, 2]] };
    let seg = |a: (f64, f64), b: (f64, f64)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = dx * dx + dy * dy;
        let t = if l == 0.0 { 0.0 } else { (((x - a.0) * dx + (y - a.1) * dy) / l).clamp(0.0, 1.0) };
        ((a.0 + t * dx - x).powi(2) + (a.1 + t * dy - y).powi(2)).sqrt()
    };
    tris.iter().flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])]).map(|(i, j)| seg(s(i), s(j))).fold(f64::MAX, f64::min)
}

fn covers(poly: &draw::Polygon, x: f64, y: f64) -> bool {
    let s = |i: usize| (poly.screen[i].x as f64, poly.screen[i].y as f64);
    let tris: &[[usize; 3]] = if poly.screen.len() == 4 { &[[0, 1, 2], [1, 3, 2]] } else { &[[0, 1, 2]] };
    tris.iter().any(|t| inside((x, y), [s(t[0]), s(t[1]), s(t[2])]))
}

/// Why each mismatched pixel is one:
/// - **Emulated only:**
///   - a polygon there was dropped by the native tests (the reason and what it is);
///   - a drawn one covers it: `edge` within a pixel of its edge (the two rasterizers' tie rules),
///     else `cut-out` (a texel transparent natively, not in the game);
///   - no captured polygon reaches it (`no captured polygon`: a draw path the capture doesn't see).
/// - **Native only:** `edge` within a pixel of a drawn polygon's edge, else `cut-out` (a texel the
///   game's VRAM has transparent where the disc's atlas doesn't).
pub fn explain(a: &Picture, b: &Picture, native: &Image, s: &Sample, scene: &draw::Scene) -> (std::collections::BTreeMap<String, u64>, Vec<String>) {
    let mut out = std::collections::BTreeMap::new();
    let mut details = Vec::new();
    let (w, h) = (a.w.min(native.w), a.h.min(native.h));
    let px = |p: &Picture, x: u32, y: u32| -> [u8; 3] {
        let o = ((y * p.w + x) * 4) as usize;
        [p.rgba[o], p.rgba[o + 1], p.rgba[o + 2]]
    };
    let kind = |p: &draw::Polygon| format!("{} (flags {:#04x}, rgb {:?})", p.what.split(" group").next().unwrap_or(&p.what), p.flags, p.rgb);
    for y in MARGIN..h.saturating_sub(MARGIN) {
        for x in MARGIN..w.saturating_sub(MARGIN) {
            if px(b, x, y) != s.clear {
                continue;
            }
            let emu = px(a, x, y) != px(b, x, y);
            let nat = native.mask[(y * native.w + x) as usize];
            if emu == nat {
                continue;
            }
            let (fx, fy) = (x as f64, y as f64);
            let here: Vec<&draw::Polygon> = scene.all.iter().filter(|p| covers(p, fx, fy)).collect();
            let near = scene.all.iter().any(|p| p.cull.is_none() && (-1..=1).any(|dy| (-1..=1).any(|dx| covers(p, fx + dx as f64, fy + dy as f64))));
            let cause = if emu {
                let drawn: Vec<&&draw::Polygon> = here.iter().filter(|p| p.cull.is_none()).collect();
                match (here.iter().find(|p| p.cull.is_some()), !drawn.is_empty()) {
                    (_, true) if drawn.iter().all(|p| edge_distance(p, fx, fy) <= 1.0) => "emulated only: edge (a drawn polygon's edge is within a pixel)".to_string(),
                    (_, true) => "emulated only: cut-out (a texel transparent natively, not in the game)".to_string(),
                    (Some(p), false) => format!("emulated only: dropped natively ({:?}): {}", p.cull.unwrap(), kind(p)),
                    (None, false) if near => "emulated only: edge (a drawn polygon is within a pixel)".to_string(),
                    (None, false) => "emulated only: no captured polygon".to_string(),
                }
            } else {
                let drawn: Vec<&&draw::Polygon> = here.iter().filter(|p| p.cull.is_none()).collect();
                match drawn.iter().find(|p| edge_distance(p, fx, fy) > 1.0) {
                    // Deep inside a polygon both draw: the game's texel there is transparent.
                    Some(p) => format!("native only: cut-out (a texel transparent in the game, not natively): {}", kind(p)),
                    None if !drawn.is_empty() => "native only: edge (the drawn polygon's edge is within a pixel)".to_string(),
                    None => "native only: edge".to_string(),
                }
            };
            let n = out.entry(cause.clone()).or_insert(0);
            *n += 1;
            if *n <= 2 {
                let mut d = format!("({x}, {y}) {cause}: A {:?} B {:?}", px(a, x, y), px(b, x, y));
                for p in &here {
                    let sc: Vec<(i32, i32, i32)> = p.screen.iter().map(|v| (v.x as i32, v.y as i32, v.z as i32)).collect();
                    d += &format!("\n      {} {:?} flags {:#04x} rgb {:?} at {:?}", p.what, p.cull, p.flags, p.rgb, sc);
                }
                details.push(d);
            }
        }
    }
    (out, details)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Picture {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&f(x, y));
                rgba.push(255);
            }
        }
        Picture { w, h, rgba }
    }

    #[test]
    fn masks_compare_and_the_map_shows_why() {
        let (w, h) = (32, 16);
        // A: a block of geometry at x 8..16; B: the clear colour, with an opaque HUD square at 24..28.
        let a = picture(w, h, |x, y| if (8..16).contains(&x) { [10, 20, 30] } else if (24..28).contains(&x) && (6..10).contains(&y) { [1, 1, 1] } else { CLEAR });
        let b = picture(w, h, |x, y| if (24..28).contains(&x) && (6..10).contains(&y) { [1, 1, 1] } else { CLEAR });
        // Native covers 9..17: one column missing on the left, one extra on the right.
        let native = Image { w, h, rgba: vec![0; (w * h * 4) as usize], mask: (0..w * h).map(|i| (9..17).contains(&(i % w))).collect() };
        let s = compare(&a, &b, &native);
        let rows = (h - 2 * MARGIN) as u64;
        assert_eq!(s.emulated_only, rows, "column 8");
        assert_eq!(s.native_only, rows, "column 16");
        assert_eq!(s.excluded, (w * h) as u64 - (w - 2 * MARGIN) as u64 * rows + 16, "edges and the HUD");
        let r = s.report.unwrap();
        assert_eq!((r.w, r.h), (w, h * 4 + 12));
        let at = |x: u32, y: u32| {
            let o = (((2 * (h + 4) + y) * w + x) * 4) as usize;
            [r.rgba[o], r.rgba[o + 1], r.rgba[o + 2]]
        };
        assert_eq!((at(8, 8), at(16, 8), at(12, 8), at(20, 8), at(25, 8), at(0, 0)), (EMULATED_ONLY, NATIVE_ONLY, COVERED, [0, 0, 0], EXCLUDED, EXCLUDED));
    }
}
