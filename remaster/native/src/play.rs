//! Playing a scene with the native view (LT-111): the capture (`lt-scene`) and the takeover hooks
//! on one machine, each tick resolved to assets and paired with its ordering table.

use std::collections::VecDeque;
use std::sync::Arc;

use emulator_core::ButtonState;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::tmd::Ram;
use lt_assets::{Assets, Frame};
use lt_emu_hooks::{Action, Call, Handler, Hook, Hooks};
use lt_scene::{hooks, Capture, Record, DRAW_OBJECTS, DRAW_OBJECTS_WORD, MODEL_DRAW_PARTS, MODEL_DRAW_PARTS_2, NPC_DRAW, NPC_DRAW_SIZE};
use lt_testset::{Image, Machine, Scene};

use crate::composite::{Compositor, Picture, Rendered};
use crate::crowd::Crowd;
use crate::draw;
use crate::gte::Precision;
use crate::interp::{between, Between, Snap};
use crate::layer2d;
use crate::light::{self, Look};
use crate::takeover::{self, Shown, Table, Takeover};
use crate::wide::{self, Reach};

/// One tick: its capture resolved to assets, and its table.
pub struct Tick {
    pub record: Record,
    pub frame: Frame,
    pub table: Table,
    /// The NPCs without a packet slot a wider view draws (LT-119, `crate::crowd`): none at the
    /// game's reach.
    pub crowd: Vec<lt_assets::CharacterUse>,
}

/// What a measuring run does to the machine besides watching (`check`): the native view itself
/// never writes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// Clear to this colour (the draw environment's background, at `Dbg_PutDrawEnv`).
    pub clear: Option<[u8; 3]>,
    /// Draw no terrain and no characters in the first tick that starts.
    pub skip_geometry: bool,
    /// Zero the length of these packets in the first table (the GPU walks past them).
    pub hide: Vec<u32>,
}

struct Both {
    cap: Capture,
    takeover: Takeover,
    base: usize,
    probe: Probe,
    starts: u32,
    tables: u32,
    /// Ticks captured, and whether the last one's table has come yet.
    ended: u64,
    claimed: bool,
}

fn within(ra: u32, start: u32, size: u32) -> bool {
    let (ra, start) = (ra & 0x1FFF_FFFF, start & 0x1FFF_FFFF);
    ra > start && ra <= start + size
}

impl Handler for Both {
    fn call(&mut self, c: &Call) -> Action {
        if c.hook < self.base {
            if c.hook == 0 {
                self.starts += 1;
            }
            let before = self.cap.records.len();
            let a = self.cap.call(c);
            if c.hook == 1 && self.cap.records.len() > before {
                self.ended += 1;
                self.claimed = false;
            }
            return a;
        }
        let t = c.hook - self.base;
        let n = takeover::hooks().len();
        if t < n {
            if t == 4 {
                // Dbg_DrawOTag: the probe's packets hidden in the first table.
                if self.tables == 0 && self.starts >= 1 {
                    for &a in &self.probe.hide {
                        c.write(a + 3, &[0]);
                    }
                }
                self.tables += self.starts.min(1);
                let a = self.takeover.call(t, c);
                // The table is the tick's that last ended, if it was captured and not yet drawn.
                if let Some((table, _)) = self.takeover.tables.last_mut() {
                    table.tick = (!self.claimed && self.ended > 0).then(|| self.ended - 1);
                    self.claimed = true;
                }
                return a;
            }
            return self.takeover.call(t, c);
        }
        match t - n {
            0 => {
                if let Some(rgb) = self.probe.clear {
                    c.write(c.arg(0) + 0x19, &rgb);
                }
                Action::Continue
            }
            1 if self.probe.skip_geometry && self.starts == 1 && self.tables == 0 => Action::Return(Some(c.arg(1))),
            2 | 3 if self.probe.skip_geometry && self.starts == 1 && self.tables == 0 && within(c.ra(), NPC_DRAW, NPC_DRAW_SIZE) => Action::Return(None),
            _ => Action::Continue,
        }
    }
}

/// The native view's hooks on any machine (a test set's, the app's): the capture and the takeover,
/// each table paired with the capture of its tick. Its hooks come first in the machine's table.
pub struct Player {
    both: Both,
    records: VecDeque<Record>,
}

/// A tick drawn, not yet resolved to assets: its capture, its table, and the picture the GPU made
/// of the tick before.
pub struct Drawn {
    pub record: Record,
    pub table: Table,
    pub shown: Shown,
}

impl Default for Player {
    fn default() -> Self {
        Self::probing(Probe::default())
    }
}

impl Player {
    /// The hooks, at the start of the machine's table: the capture's, then the takeover's.
    pub fn hooks() -> Vec<Hook> {
        let mut all = hooks().hooks().to_vec();
        all.extend(takeover::hooks());
        all
    }

    fn probing(probe: Probe) -> Self {
        let base = hooks().hooks().len();
        Player { both: Both { cap: Capture::new(), takeover: Takeover::new(), base, probe, starts: 0, tables: 0, ended: 0, claimed: true }, records: VecDeque::new() }
    }

    /// Before a frame runs: its number, for the capture's records.
    pub fn begin(&mut self, frame: u64) {
        self.both.cap.frame = frame;
    }

    /// After a frame: the tables drawn, each with its tick's capture.
    pub fn end(&mut self) -> Vec<Drawn> {
        self.records.extend(self.both.cap.take());
        let mut out = Vec::new();
        for (table, shown) in self.both.takeover.take() {
            // Each table is drawn for the tick that ended last; one drawn before any tick ended (the
            // session started mid-tick) has no capture to go with.
            let Some(tick) = table.tick else { continue };
            while self.records.front().is_some_and(|r| r.tick < tick) {
                self.records.pop_front();
            }
            let Some(record) = self.records.pop_front().filter(|r| r.tick == tick) else { continue };
            out.push(Drawn { record, table, shown });
        }
        out
    }
}

impl Handler for Player {
    fn call(&mut self, c: &Call) -> Action {
        self.both.call(c)
    }
}

/// A drawn tick's capture resolved to assets: models and textures from RAM (`ram`, as it was at the
/// end of the frame) and from VRAM as the table starts.
pub fn resolve(d: Drawn, ram: &dyn Ram, assets: &mut Assets) -> (Tick, Shown) {
    let frame = assets.resolve_at(&d.record, ram, &d.table.vram);
    (Tick { record: d.record, frame, table: d.table, crowd: Vec::new() }, d.shown)
}

/// [`resolve`], with what a view at `reach` needs past the game's: the neighbour chunks read
/// (LT-114), and the NPCs without a slot posed by `crowd` (LT-119), which keeps their animation
/// from tick to tick.
pub fn resolve_with(d: Drawn, ram: &dyn Ram, assets: &mut Assets, reach: Reach, crowd: &mut Crowd) -> (Tick, Shown) {
    let (mut tick, shown) = resolve(d, ram, assets);
    wide::load(&tick.record, assets, reach);
    tick.crowd = crowd.update(&tick.record, ram, &tick.table.vram, assets, reach).into_iter().map(|d| d.character).collect();
    (tick, shown)
}

/// A test set's machine playing with the native view's hooks.
pub struct Session {
    pub m: Machine,
    h: Hooks,
    player: Player,
    /// The reach ticks are resolved for (the chunks it needs read): the game's unless set.
    pub reach: Reach,
    /// The NPCs without a slot, posed (drawn at a reach past the game's).
    pub crowd: Crowd,
}

impl Session {
    pub fn new(image: &Arc<Image>, scene: &Scene, state: &[u8]) -> Result<Session, String> {
        let m = Machine::from_state(image, &scene.card, state, scene.frame)?;
        Self::on(m)
    }

    pub fn on(m: Machine) -> Result<Session, String> {
        Self::probing(m, Probe::default())
    }

    /// A session for measuring (`check`): the native view's hooks and `probe`'s changes.
    pub fn probing(mut m: Machine, probe: Probe) -> Result<Session, String> {
        let mut all = Player::hooks();
        if probe != Probe::default() {
            all.push(Hook::observe("probe: clear", takeover::PUT_DRAW_ENV));
            all.push(Hook::replace("probe: skip WorldChunk_DrawObjects", DRAW_OBJECTS).guarded(DRAW_OBJECTS_WORD));
            all.push(Hook::replace("probe: skip Model_DrawParts", MODEL_DRAW_PARTS));
            all.push(Hook::replace("probe: skip Model_DrawParts (second entry)", MODEL_DRAW_PARTS_2));
        }
        let h = Hooks::new(all)?;
        h.install(&mut m.cpu);
        Ok(Session { m, h, player: Player::probing(probe), reach: Reach::GAME, crowd: Crowd::new() })
    }

    /// One frame. Its ticks resolved (each with the picture the GPU made of the tick before).
    pub fn frame(&mut self, pad: u16, assets: &mut Assets) -> Result<Vec<(Tick, Shown)>, String> {
        self.player.begin(self.m.frame);
        self.m.bus.set_port1_buttons(ButtonState::from_bits(pad));
        self.h.run_frame(&mut self.m.cpu, &mut self.m.bus, &mut self.player).map_err(|e| format!("frame {}: {e}", self.m.frame))?;
        self.m.bus.run_spu_to_current_cycle();
        self.m.bus.spu.discard_audio();
        self.m.frame += 1;
        let (m, reach, crowd) = (&self.m, self.reach, &mut self.crowd);
        Ok(self.player.end().into_iter().map(|d| resolve_with(d, &|a: u32, l: usize| m.read(a, l), assets, reach, crowd)).collect())
    }
}

/// The native view of `tick` at `scale` × the display area: its terrain and characters (when the
/// tick drew the field) and its 2D layer; or, while the game shows something its tables don't draw,
/// `shown` (the displayed picture) enlarged. The original look is the game's unlit 3D (LT-111);
/// the lit look lights it and leaves the blob shadows out (LT-112).
pub fn native(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, scale: u32) -> Picture {
    native_look(tick, shown, assets, cache, comp, scale, Look::Original)
}

pub fn native_look(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, scale: u32, look: Look) -> Picture {
    native_at(tick, shown, assets, cache, comp, scale, look, Reach::GAME)
}

/// [`native_look`] at `reach` (LT-114): wider than 4:3, or further.
#[allow(clippy::too_many_arguments)]
pub fn native_at(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, scale: u32, look: Look, reach: Reach) -> Picture {
    match view(tick, shown, assets, cache, comp, scale, look, reach) {
        View::Picture(p) => p,
        View::Rendered(r) => comp.read(&r),
    }
}

/// The native view of a tick as the app shows it: left on the GPU, or (a movie) the shown picture.
pub enum View {
    Rendered(Rendered),
    Picture(Picture),
}

/// The bounds a tick's view at `reach` draws to: margins from the display area's width.
fn sight(tick: &Tick, reach: Reach) -> draw::Sight {
    reach.sight(tick.table.display[2] as u32)
}

/// [`native_at`] without reading the composited picture back. A movie (the shown picture) stays
/// 4:3 whatever `reach` is.
#[allow(clippy::too_many_arguments)]
pub fn view(tick: &Tick, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, scale: u32, look: Look, reach: Reach) -> View {
    if tick.table.passthrough {
        return View::Picture(comp.present(shown, scale));
    }
    let frame = wide::frame(&tick.record, &tick.frame, &tick.crowd, assets, reach);
    cache.prepare(assets, &frame);
    let precision = if scale > 1 { Precision::Float } else { Precision::Gte };
    let field = tick.record.camera.is_some();
    let sight = sight(tick, reach);
    let scene = match look {
        Look::Lit if field => Some(draw::scene_with(&tick.record, &frame, assets, precision, Some(&mut comp.normals), None, sight)),
        _ if field => Some(draw::scene_with(&tick.record, &frame, assets, precision, None, None, sight)),
        _ => None,
    };
    let light = scene.as_ref().and_then(|s| light::uniforms(&tick.record, s));
    let layer = layer2d::layer_with(&tick.table, scale, light.is_some());
    View::Rendered(comp.render(scene.as_ref(), &tick.table, &layer, cache, scale, light.as_ref(), sight.left))
}

/// The native view `t` of the way from `prev` to `tick` (LT-113): `tick` as it is when there's no
/// `prev`, at `t` = 1, or when the two snap (a warp, a door, a cut: the reason is returned). The
/// 2D layer is `tick`'s whatever `t` is: it stays at the game's 30.
///
/// At a `reach` past the game's, both ticks are widened alike first (`crate::wide::frame`).
#[allow(clippy::too_many_arguments)]
pub fn view_between(prev: Option<&Tick>, tick: &Tick, t: f64, shown: &Shown, assets: &Assets, cache: &mut Cache<Wgpu>, comp: &mut Compositor, scale: u32, look: Look, reach: Reach) -> (View, Option<Snap>) {
    let frame = wide::frame(&tick.record, &tick.frame, &tick.crowd, assets, reach);
    let (pose, snap) = match prev {
        Some(p) if t < 1.0 && !tick.table.passthrough => {
            let before = wide::frame(&p.record, &p.frame, &p.crowd, assets, reach);
            match between((&p.record, &before), (&tick.record, &frame), t) {
                Between::Pose(pose) => (Some(pose), None),
                Between::Snap(s) => (None, Some(s)),
            }
        }
        _ => (None, None),
    };
    let Some(pose) = pose else { return (view(tick, shown, assets, cache, comp, scale, look, reach), snap) };
    cache.prepare(assets, &frame);
    let sight = sight(tick, reach);
    let normals = match look {
        Look::Lit => Some(&mut comp.normals),
        Look::Original => None,
    };
    let scene = draw::scene_with(&tick.record, &frame, assets, Precision::Float, normals, Some(&pose), sight);
    let light = light::uniforms(&tick.record, &scene);
    let layer = layer2d::layer_with(&tick.table, scale, light.is_some());
    (View::Rendered(comp.render(Some(&scene), &tick.table, &layer, cache, scale, light.as_ref(), sight.left)), None)
}

/// `p` (the native view of `table` at `scale`) placed as the display shows it, the size of the
/// emulated picture (`shown`, its lines) × `scale`: shifted by the screen position, black around.
/// The emulated picture beside it then lines up, line for line.
pub fn on_screen(p: &Picture, table: &Table, shown: &Shown, scale: u32) -> (Vec<u8>, u32, u32) {
    let (w, h) = (shown.w * scale, shown.h * scale);
    let mut out = [0u8, 0, 0, 255].repeat((w * h) as usize);
    let [dx, dy] = table.screen.map(|o| o * scale as i32);
    for y in 0..p.h as i32 {
        let ty = y + dy;
        if ty < 0 || ty >= h as i32 {
            continue;
        }
        let (x0, x1) = ((-dx).max(0), (p.w as i32).min(w as i32 - dx));
        if x0 >= x1 {
            continue;
        }
        let s = ((y * p.w as i32 + x0) * 4) as usize;
        let d = ((ty * w as i32 + x0 + dx) * 4) as usize;
        let n = ((x1 - x0) * 4) as usize;
        out[d..d + n].copy_from_slice(&p.rgba[s..s + n]);
    }
    (out, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_screen_shifts_by_the_screen_position() {
        let p = Picture { w: 4, h: 2, rgba: [9u8, 9, 9, 255].repeat(8), native: vec![false; 8], layer: vec![false; 8] };
        let table = Table { screen: [1, 1], ..Table::default() };
        let shown = Shown { w: 4, h: 4, rgba: vec![0; 64] };
        let (rgba, w, h) = on_screen(&p, &table, &shown, 1);
        assert_eq!((w, h), (4, 4));
        let lit: Vec<(u32, u32)> = (0..16).filter(|i| rgba[i * 4] == 9).map(|i| (i as u32 % 4, i as u32 / 4)).collect();
        assert_eq!(lit, vec![(1, 1), (2, 1), (3, 1), (1, 2), (2, 2), (3, 2)]);
    }
}
