//! Characters the game doesn't draw (LT-119): NPCs and doors without a packet slot, drawn by the
//! native view from their records.
//!
//! `Npc_DrawIfVisible` gives an NPC one of five packet slots only while its position projects
//! within the 4:3 screen (`-64..=576`, or `-60..=380` at 320) and a slot is free; the slot's part
//! record holds the posed vertices `Model_DrawParts` draws. An NPC without one has no posed mesh.
//! But its record (gc-sys `CHARACTERS`, 0x38 bytes) stays live, and holds all the game poses it
//! from (`lt_scene::Record::actors`, read at the tick's end):
//! - **Place:** x, h, z at +0, +2, +4 (the walk scripts run without a slot; `h` is only refreshed
//!   from the terrain while slotted). The world matrix is a turn about Y of `0x800 − dir` (+0xC),
//!   moved there: exactly the matrix the game builds for a slotted NPC.
//! - **Pose:** the model's TMD (`D_801FD688[+0x16]`), its pose block (+0x18) and the blend state
//!   (+0x1C current pose, +0x1D target, +0x1E speed, +0x1F weight; `lt_core::mesh::blend` is
//!   `BlendTwoPoseAnimation`). Exactly the vertices the game draws for a slotted NPC.
//!
//! **The shadow animation.** The game's animation runs in two steps: the actor loop steps the
//! sequence (+0x14 animation, +0x30 frame, the table at +0x20) when the weight is 0, setting the
//! target pose and speed; `UpdateNpcPoseBlendState`, called only while the NPC is drawn, moves the
//! weight toward the target. Without a slot the second never runs: the weight stays where it was,
//! so the frame races on every tick and the current pose is frozen. A [`Crowd`] runs both steps
//! itself for each NPC without a slot, from its record the tick it lost its slot (or was first
//! seen), as the game would if the NPC were drawn. The renderer only reads; nothing is written to
//! the machine.
//!
//! **Doors and chests** (kinds 2 and 3) take one of four object slots the same way, and are drawn
//! in their model's base pose. A chest is two parts: its body (object 0) and its lid (object 1),
//! lifted `lid × 2 + 0x28` and turned about Y by `lid << 5` (alone: not with the body's facing,
//! as the game draws it), where `lid` is its +0x30.

use std::collections::HashMap;
use std::sync::Arc;

use lt_assets::tmd::{self, Ram};
use lt_assets::{Assets, CharacterUse, MeshKey, PartUse, TextureKey};
use lt_core::mesh::{blend, morph_targets, MorphTarget};
use lt_scene::{Matrix, Record, CHARACTER_BYTES, CHARACTER_COUNT};

use crate::wide::{self, Reach};

/// `D_801FD688`: the area's models (TMD addresses), indexed by a record's +0x16.
pub const MODELS: u32 = 0x801F_D688;
/// The NPCs' indices: 0 is the Knight, 1 the Baby; from 0x50 on the records are the overworld's
/// wanderers (enemies, their own layout and draw loop), which the crowd leaves alone.
pub const FIRST_NPC: usize = 2;
pub const WANDERERS: usize = 0x50;

/// A character record's fields the native view reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor<'a>(pub &'a [u8]);

impl Actor<'_> {
    fn i16(&self, o: usize) -> i16 {
        i16::from_le_bytes([self.0[o], self.0[o + 1]])
    }
    fn u32(&self, o: usize) -> u32 {
        u32::from_le_bytes([self.0[o], self.0[o + 1], self.0[o + 2], self.0[o + 3]])
    }
    pub fn position(&self) -> [i32; 3] {
        [self.i16(0) as i32, self.i16(2) as i32, self.i16(4) as i32]
    }
    /// Its room (an interior's chunk).
    pub fn room(&self) -> u16 {
        u16::from_le_bytes([self.0[6], self.0[7]])
    }
    pub fn dir(&self) -> i16 {
        self.i16(0xC)
    }
    pub fn flags(&self) -> u8 {
        self.0[0x12]
    }
    /// Placed (0x40) and not in an area that isn't loaded (0x04).
    pub fn placed(&self) -> bool {
        self.flags() & 0x44 == 0x40
    }
    /// Has a packet slot (the game draws it).
    pub fn slotted(&self) -> bool {
        self.flags() & 0x80 != 0
    }
    pub fn anim(&self) -> u8 {
        self.0[0x14]
    }
    pub fn model(&self) -> u8 {
        self.0[0x16]
    }
    pub fn set(&self) -> u8 {
        self.0[0x17]
    }
    pub fn pose_block(&self) -> u32 {
        self.u32(0x18)
    }
    /// Current pose, target, speed, weight.
    pub fn blend(&self) -> (u8, u8, u8, u8) {
        (self.0[0x1C], self.0[0x1D], self.0[0x1E], self.0[0x1F])
    }
    pub fn anim_table(&self) -> u32 {
        self.u32(0x20)
    }
    pub fn frame(&self) -> u8 {
        self.0[0x30]
    }
    /// The low nibble of +0x32: 0 an NPC, 1 shown only by a scene, 2 and 3 doors and lids, 4 and up
    /// spots with no model.
    pub fn kind(&self) -> u8 {
        self.0[0x32] & 0xF
    }
    pub fn state(&self) -> u8 {
        self.0[0x34]
    }

    /// Whether the field's actor loop draws it when it's on screen and has a slot: placed with a
    /// model, an NPC or a door (kinds 0, 2, 3), not in state 2, and in an interior the room's own.
    pub fn drawable(&self, home: u16) -> bool {
        self.placed() && self.model() != 0xFF && matches!(self.kind(), 0 | 2 | 3) && self.state() != 2 && (home < 0x145 || self.room() == home)
    }
}

/// The records of a tick, by character index.
pub fn actors(r: &Record) -> impl Iterator<Item = (usize, Actor<'_>)> {
    r.actors.as_chunks::<CHARACTER_BYTES>().0.iter().take(CHARACTER_COUNT).map(|c| Actor(c.as_slice())).enumerate()
}

/// The NPC records of a tick the actor loop runs (`Record::actor_count`; the party and the
/// wanderers left out).
pub fn npcs(r: &Record) -> impl Iterator<Item = (usize, Actor<'_>)> {
    actors(r).take(WANDERERS.min(r.actor_count as usize)).skip(FIRST_NPC)
}

/// A turn about Y by `angle` (4096 a turn), as the game's matrices hold it: its sine table
/// truncates (2275, not 2276, at 0x180).
fn turn_y(angle: i32) -> [[i16; 3]; 3] {
    let (s, c) = (angle as f64 * std::f64::consts::TAU / 4096.0).sin_cos();
    let q = |v: f64| (v * 4096.0 + 1e-9 * v.signum()).trunc() as i16;
    [[q(c), 0, q(-s)], [0, 4096, 0], [q(s), 0, q(c)]]
}

/// The world matrix the game builds for a character (`FUN_801a0efc(0, 0x800 − dir, 0)`, moved to
/// its position): a turn about Y.
pub fn world(a: &Actor) -> Matrix {
    Matrix { m: turn_y(0x800 - a.dir() as i32), t: a.position() }
}

/// A chest's lid: lifted `lid × 2 + 0x28` from the body, and turned about Y by `lid << 5` alone.
/// `FUN_801a0ec0` writes the turn over the matrix rather than adding it, so the lid doesn't follow
/// the body's facing (the game's own quirk, kept).
pub fn lid(a: &Actor) -> Matrix {
    let lid = a.0[0x30] as i32;
    let mut t = a.position();
    t[1] -= lid * 2 + 0x28;
    Matrix { m: turn_y(lid << 5), t }
}

/// `view · world`, as the GTE composes them (`CompMatrix`).
pub fn compose(view: &Matrix, world: &Matrix) -> Matrix {
    let mut m = [[0i16; 3]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (k, v) in row.iter_mut().enumerate() {
            *v = ((0..3).map(|j| view.m[r][j] as i64 * world.m[j][k] as i64).sum::<i64>() >> 12) as i16;
        }
    }
    Matrix { m, t: view.place(world.t) }
}

/// The game's two animation steps for a character drawn: the sequence step when the weight is 0,
/// then the blend toward the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shadow {
    /// The model and animation set it was started for (another means another character).
    pub model: u8,
    pub set: u8,
    pub anim: u8,
    pub frame: u8,
    pub cur: u8,
    pub tgt: u8,
    pub speed: u8,
    pub weight: u16,
}

impl Shadow {
    pub fn of(a: &Actor) -> Shadow {
        let (cur, tgt, speed, weight) = a.blend();
        Shadow { model: a.model(), set: a.set(), anim: a.anim(), frame: a.frame(), cur, tgt, speed, weight: weight as u16 }
    }

    /// One tick: `seq` is the animation's sequence (`u8 count; (pose, speed) × count`); `hold` the
    /// game's hold on every NPC's animation.
    pub fn step(&mut self, seq: &[u8], hold: bool) {
        if !hold && self.weight == 0 && !seq.is_empty() {
            self.frame = self.frame.wrapping_add(1);
            if self.frame >= seq[0] {
                self.frame = 0;
            }
            let at = self.frame as usize * 2;
            self.tgt = seq.get(at + 1).copied().unwrap_or(self.tgt);
            self.speed = seq.get(at + 2).copied().unwrap_or(self.speed);
        }
        if self.cur != self.tgt {
            let v = self.weight + self.speed as u16;
            if v >= 0x100 {
                self.cur = self.tgt;
                self.weight = 0;
            } else {
                self.weight = v;
            }
        }
    }
}

/// A model as the crowd poses it: its objects' base vertices and its assets.
struct Model {
    base: Vec<Vec<[i16; 3]>>,
    mesh: MeshKey,
    texture: TextureKey,
    source: lt_assets::model::ModelSource,
}

/// Bytes of a model or pose block compared before a read one is used again: every area loads its
/// NPC pack at the same address.
const HEAD: usize = 64;

/// Something read from RAM once, with the first bytes it was read from.
type Cached<T> = (Vec<u8>, Option<Arc<T>>);

/// The characters the game doesn't draw, kept tick to tick: their shadow animations, and the
/// models and pose blocks read (by address, each with its first bytes).
#[derive(Default)]
pub struct Crowd {
    shadows: HashMap<usize, Shadow>,
    models: HashMap<u32, Cached<Model>>,
    poses: HashMap<u32, Cached<Vec<MorphTarget>>>,
}

/// One character the crowd draws.
#[derive(Clone, Debug)]
pub struct Drawn {
    pub character: CharacterUse,
    /// Its position on the game's screen (x, or `None` behind the camera).
    pub screen_x: Option<i64>,
}

impl Crowd {
    pub fn new() -> Crowd {
        Crowd::default()
    }

    /// The tick's characters without a slot, posed, in view at `reach` (none at the game's reach):
    /// `ram` reads the area's models and pose blocks, `vram` (as the tick's table starts) their
    /// texture the first time. Every one's shadow animation steps, in view or not.
    pub fn update(&mut self, r: &Record, ram: &dyn Ram, vram: &[u16], assets: &mut Assets, reach: Reach) -> Vec<Drawn> {
        let Some(cam) = r.camera.as_ref() else { return Vec::new() };
        if r.actors.len() < CHARACTER_COUNT * CHARACTER_BYTES {
            return Vec::new();
        }
        let home = wide::home(r);
        let hold = r.anim_hold != 0;
        let mut out = Vec::new();
        let mut seen = Vec::new();
        for (i, a) in npcs(r) {
            if !a.drawable(home) || a.slotted() {
                continue;
            }
            seen.push(i);
            // The sequence step reads the animation table in RAM.
            let seq = if a.set() == 0xFF || a.kind() != 0 {
                Vec::new()
            } else {
                let tbl = u32::from_le_bytes(ram.read(a.anim_table() + a.anim() as u32 * 4, 4).try_into().unwrap_or([0; 4]));
                let head = ram.read(tbl, 1);
                head.first().map_or(Vec::new(), |&n| ram.read(tbl, 1 + n as usize * 2))
            };
            let shadow = match self.shadows.get_mut(&i).filter(|s| s.model == a.model() && s.set == a.set()) {
                Some(s) => {
                    if s.anim != a.anim() {
                        // A script changed the animation (and set its frame to 0).
                        s.anim = a.anim();
                        s.frame = 0;
                    }
                    s.step(&seq, hold);
                    *s
                }
                None => {
                    let s = Shadow::of(&a);
                    self.shadows.insert(i, s);
                    s
                }
            };
            if reach.is_game() || r.npc_flags & 4 == 0 {
                continue;
            }
            // On screen as Npc_DrawIfVisible sees it, the screen widened by the margins.
            let world = world(&a);
            let p = cam.view.place(world.t);
            let screen_x = (p[2] > 0).then(|| cam.offset[0] as i64 + cam.h as i64 * p[0] as i64 / p[2] as i64);
            let sy = (p[2] > 0).then(|| cam.offset[1] as i64 + cam.h as i64 * p[1] as i64 / p[2] as i64);
            let left = reach.margin(if cam.wide { 512 } else { 320 }) as i64;
            let (x0, x1) = if cam.wide { (-64, 576) } else { (-60, 380) };
            let on = matches!((screen_x, sy), (Some(x), Some(y)) if (x0 - left..=x1 + left).contains(&x) && (-40..=300).contains(&y));
            if !on {
                continue;
            }
            let Some(model) = self.model(ram, vram, assets, a.model(), r.chunk) else { continue };
            let Some(body) = model.base.first() else { continue };
            let vertices = self.posed(ram, &a, body, (shadow.cur, shadow.tgt, shadow.weight));
            let mut parts = vec![PartUse { object: 0, world, model_view: compose(&cam.view, &world), vertices }];
            if a.kind() == 3 {
                let Some(top) = model.base.get(1) else { continue };
                let l = lid(&a);
                parts.push(PartUse { object: 1, world: l, model_view: compose(&cam.view, &l), vertices: top.clone() });
            }
            out.push(Drawn { character: CharacterUse { index: i as u16, mesh: model.mesh, texture: model.texture, source: model.source.clone(), parts }, screen_x });
        }
        // A character drawn by the game again, or gone, starts from its record next time.
        self.shadows.retain(|i, _| seen.contains(i));
        out
    }

    /// A character's vertices for a blend state (current, target, weight): an NPC's pose, a door's
    /// base. `BlendTwoPoseAnimation` exactly.
    pub fn posed(&mut self, ram: &dyn Ram, a: &Actor, base: &[[i16; 3]], (cur, tgt, weight): (u8, u8, u16)) -> Vec<[i16; 3]> {
        match self.pose_block(ram, a.pose_block()) {
            Some(t) if a.set() != 0xFF && a.kind() == 0 => blend(base, &t, cur, 0x100 - weight, tgt, weight),
            _ => base.to_vec(),
        }
    }

    /// A character's base vertices (its model's objects), read from RAM.
    pub fn base(&mut self, ram: &dyn Ram, vram: &[u16], assets: &mut Assets, a: &Actor, chunk: u16) -> Option<Vec<Vec<[i16; 3]>>> {
        self.model(ram, vram, assets, a.model(), chunk).map(|m| m.base.clone())
    }

    fn model(&mut self, ram: &dyn Ram, vram: &[u16], assets: &mut Assets, index: u8, chunk: u16) -> Option<Arc<Model>> {
        let at = u32::from_le_bytes(ram.read(MODELS + index as u32 * 4, 4).try_into().ok()?);
        let head = ram.read(at, HEAD);
        if let Some((_, m)) = self.models.get(&at).filter(|(h, _)| *h == head) {
            return m.clone();
        }
        let built = (|| {
            let file = tmd::from_ram(ram, at)?;
            let base = tmd::objects(&file)?
                .iter()
                .map(|o| {
                    (0..o.n_verts as usize)
                        .map(|k| {
                            let v = o.verts + k * 8;
                            [0, 2, 4].map(|j| i16::from_le_bytes([file[v + j], file[v + j + 1]]))
                        })
                        .collect()
                })
                .collect();
            let (mesh, texture, source) = assets.character_model(ram, at, chunk, vram)?;
            Some(Arc::new(Model { base, mesh, texture, source }))
        })();
        self.models.insert(at, (head, built.clone()));
        built
    }

    fn pose_block(&mut self, ram: &dyn Ram, at: u32) -> Option<Arc<Vec<MorphTarget>>> {
        if at == 0 {
            return None;
        }
        let head = ram.read(at, HEAD);
        if let Some((_, p)) = self.poses.get(&at).filter(|(h, _)| *h == head) {
            return p.clone();
        }
        // A block is a count, then per pose its records; 64 KB covers the largest.
        let parsed = morph_targets(&ram.read(at, 0x1_0000)).map(Arc::new);
        self.poses.insert(at, (head, parsed.clone()));
        parsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shadow_steps_as_the_game() {
        // A sequence of three poses at speed 0x80: each takes two ticks to reach.
        let seq = [3u8, 1, 0x80, 2, 0x80, 3, 0x80];
        let mut s = Shadow { model: 0, set: 0, anim: 0, frame: 0, cur: 1, tgt: 1, speed: 0x80, weight: 0 };
        s.step(&seq, false);
        assert_eq!((s.frame, s.cur, s.tgt, s.weight), (1, 1, 2, 0x80));
        s.step(&seq, false);
        assert_eq!((s.frame, s.cur, s.tgt, s.weight), (1, 2, 2, 0));
        s.step(&seq, false);
        assert_eq!((s.frame, s.cur, s.tgt, s.weight), (2, 2, 3, 0x80));
        // Held: no sequence step, the blend still runs.
        s.step(&seq, true);
        assert_eq!((s.frame, s.cur, s.weight), (2, 3, 0));
    }

    #[test]
    fn the_world_turns_as_the_game_builds_it() {
        let rec = {
            let mut b = vec![0u8; CHARACTER_BYTES];
            b[0..2].copy_from_slice(&(-608i16).to_le_bytes());
            b[2..4].copy_from_slice(&(-129i16).to_le_bytes());
            b[4..6].copy_from_slice(&624i16.to_le_bytes());
            b[0xC..0xE].copy_from_slice(&0x400i16.to_le_bytes());
            b
        };
        // Captured from the game (Trisken, character 4, dir 0x400).
        assert_eq!(world(&Actor(&rec)), Matrix { m: [[0, 0, -4096], [0, 4096, 0], [4096, 0, 0]], t: [-608, -129, 624] });
    }
}
