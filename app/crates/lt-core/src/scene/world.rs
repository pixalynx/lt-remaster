//! The field state a scene runs in, and the per-frame actor and camera
//! updates, ported from the decomp: `Overworld_KnightUpdate` (the scene
//! subset: scripted pad, move, collision, turn), the Baby's follow trail
//! (`FUN_80181648`), `NPC_UpdateActors` (walk programs, the sequencer, the
//! on-screen blend), `WalkActor_RunScript`/`SelectScript`, and the field
//! camera (`FUN_8019f88c` with `Event_UpdateSpeakerCamera`).
//!
//! Only the scene's chunk is loaded: terrain outside it reads as "not
//! resident" (height unchanged), and actors homed on other chunks run their
//! programs but are not drawn, as the game does for chunks it has not
//! streamed in. The Knight never re-centres the world.

use crate::container::le16;

use super::anim::{self, Blend, Sequence};
use super::math::{step_toward, Tables};

pub const KNIGHT: usize = 0;
pub const BABY: usize = 1;
/// `+0x12` bits.
pub mod fl {
    pub const TALK: u8 = 0x01;
    pub const OVERLAP: u8 = 0x02;
    pub const NOT_RESIDENT: u8 = 0x04;
    pub const GHOST: u8 = 0x08;
    pub const WALKING: u8 = 0x10;
    pub const VISIBLE: u8 = 0x40;
    pub const SLOT: u8 = 0x80;
}

/// `Character_Struct` (0x38 bytes), the fields scenes use.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub x: i16,
    pub h: i16,
    pub z: i16,
    /// Home chunk (`+6`).
    pub id: i16,
    pub rot_x: i16,
    pub rot_z: i16,
    /// `+0x0C` shown facing, `+0x0E` target, `+0x10` base heading.
    pub facing: i16,
    pub target: i16,
    pub base: i16,
    pub flags: u8,
    pub speed: i8,
    pub anim: u8,
    pub base_anim: u8,
    /// Group A model (0xFF = none) and pose set (0xFF = still).
    pub model: u8,
    pub set: u8,
    pub blend: Blend,
    pub frame: u8,
    pub anim_tbl: Vec<Sequence>,
    /// D-relative placement ladder and script (0 = none).
    pub ladder: u32,
    pub script: u32,
    /// D-relative byte offset of the walk program and its word index.
    pub wp: usize,
    pub pc: u8,
    pub steps: u8,
    pub kind: u8,
    pub size: u8,
    pub state: u8,
    /// Drawn by the game this frame (on screen, has a model).
    pub drawn: bool,
}

/// `D_801CDBD8` plus the focus actor.
#[derive(Debug, Clone)]
pub struct Camera {
    /// Projection distance (SetGeomScreen).
    pub h: i32,
    pub rx: i16,
    pub ry: i16,
    pub rz: i16,
    pub fc: i16,
    pub fe: i16,
    pub f10: i16,
    /// World-space target offset from the focus actor.
    pub off: [i16; 3],
    pub shake: [i16; 2],
    /// `D_801D7808`: target distance, step out, step in, blend.
    pub profile: [i16; 4],
}

/// One frame's camera: world → view `v = r·p + t` (rows of `r` are the camera's
/// right / down / forward axes, Y down), the eye, H and the screen shake.
#[derive(Debug, Clone, Copy, Default)]
pub struct View {
    pub r: [[f32; 3]; 3],
    pub t: [f32; 3],
    pub eye: [f32; 3],
    pub h: f32,
    pub shake: [i16; 2],
}

/// The event flags: retail's 512, or as many as the open base has
/// ([`crate::engine::Profile::flag_count`], at most [`crate::flags::MAX_COUNT`]).
#[derive(Debug, Clone)]
pub struct Flags {
    words: [u32; (crate::flags::MAX_COUNT / 32) as usize],
    count: u32,
}

impl Default for Flags {
    fn default() -> Self {
        Flags { words: [0; (crate::flags::MAX_COUNT / 32) as usize], count: crate::engine::active().flag_count.min(crate::flags::MAX_COUNT as u32) }
    }
}

impl Flags {
    /// `CheckEventFlag`: flag 0 is always set, flags past the count never.
    pub fn get(&self, f: u32) -> bool {
        if f == 0 {
            return true;
        }
        f < self.count && self.words[(f >> 5) as usize] & (1 << (f & 31)) != 0
    }
    pub fn set(&mut self, f: u32) {
        if f < self.count && f != 0xF8 {
            self.words[(f >> 5) as usize] |= 1 << (f & 31);
        }
    }
    pub fn clear(&mut self, f: u32) {
        if f < self.count {
            self.words[(f >> 5) as usize] &= !(1 << (f & 31));
        }
    }
    /// SET 0xFD: flags 0–63.
    pub fn clear_scratch(&mut self) {
        self.words[0] = 0;
        self.words[1] = 0;
    }
    pub fn list(&self) -> Vec<u16> {
        (0..self.count).filter(|&f| self.get(f) && f != 0).map(|f| f as u16).collect()
    }
}

/// A sound request (`Sfx_QueueCue`) or BGM change, by frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Cue {
    Sfx { id: u16 },
    /// `Load_BGM_StateMachine` id: 0x39 → `bgm/57` and 0xFF → `bgm/0`, both
    /// into slot 1; any other n → `bgm/n` into slot 2 (0 loads nothing).
    BgmLoad { file: u16 },
    BgmFade { id: u16 },
}

pub struct World<'a> {
    pub t: &'a Tables,
    /// The pack's D-block (Tier-2 operands, ladders and walk programs index it).
    pub d: &'a [u8],
    pub heights: Vec<i16>,
    pub chunk: u16,
    pub actors: Vec<Actor>,
    pub flags: Flags,
    /// `ctx+0xDE`.
    pub de: u16,
    /// `D_801B734C`, the scripted joypad word.
    pub vpad: u16,
    /// `ctx+0x4C`: 1 = free-roam 512 mode (dialogue windows are not drawn).
    pub mode4c: u8,
    pub cam: Camera,
    /// `D_801CD4B4`.
    pub focus: u8,
    /// `D_801FD640`: 0 clear, 1–31 to black, 32–63 to white.
    pub fade: u8,
    pub trail: [[i16; 3]; 21],
    pub trail_face: [i16; 21],
    rng: u32,
    pub cues: Vec<(u32, Cue)>,
    pub frame: u32,
    /// This frame's camera (built by [`World::camera_task`]).
    pub view: View,
}

fn wrap12(d: i32) -> i32 {
    let d = d & 0xFFF;
    if d >= 0x801 {
        d - 0x1000
    } else {
        d
    }
}

impl<'a> World<'a> {
    pub fn new(t: &'a Tables, d: &'a [u8], heights: Vec<i16>, chunk: u16) -> Self {
        Self {
            t,
            d,
            heights,
            chunk,
            actors: Vec::new(),
            flags: Flags::default(),
            de: 0,
            vpad: 0,
            mode4c: 1,
            cam: Camera {
                h: 0x37C,
                rx: 0x200,
                ry: -0x200,
                rz: 0,
                fc: 0,
                fe: 0x14,
                f10: 0x7D0,
                off: [0; 3],
                shake: [0; 2],
                profile: [0x7D0, 0x28, 0x64, 0],
            },
            focus: 0,
            fade: 0,
            trail: [[0; 3]; 21],
            trail_face: [0; 21],
            rng: 1,
            cues: Vec::new(),
            frame: 0,
            view: View::default(),
        }
    }

    /// PSX libc `rand()`.
    pub fn rand(&mut self) -> i32 {
        self.rng = self.rng.wrapping_mul(1_103_515_245).wrapping_add(12345);
        ((self.rng >> 16) & 0x7FFF) as i32
    }

    pub fn cue(&mut self, c: Cue) {
        self.cues.push((self.frame, c));
    }

    pub fn word(&self, at: usize) -> u16 {
        if at + 2 <= self.d.len() {
            le16(self.d, at)
        } else {
            0
        }
    }

    /// `GetTerrainHeightAt` on the loaded chunk; elsewhere the height is kept.
    pub fn terrain(&self, x: i32, h: i32, z: i32) -> i16 {
        let (x, z) = (x as i16 as i32, z as i16 as i32);
        if !(-0x800..0x800).contains(&x) || !(-0x800..0x800).contains(&z) || self.heights.len() < 128 * 128 {
            return h as i16;
        }
        let col = ((x + 0x800) / 32) as usize;
        let row = ((z + 0x800) >> 5) as usize;
        self.heights[row * 128 + col]
    }

    /// `CheckNpcAtPosition`: `who` ≠ 0 tests the Knight's (and, for NPCs, the
    /// Baby's) box; `who` = 0 tests the Knight against scripted NPCs (slot index or 0).
    pub fn check_npc_at(&self, x: i32, z: i32, who: usize) -> usize {
        let a = &self.actors;
        if who != 0 {
            let r = self.t.boxes[(a[who].size & 3) as usize] as i32;
            let inside = |o: &Actor| {
                let (dx, dz) = (x - o.x as i32, z - o.z as i32);
                -r < dx && dx < r && -r < dz && dz < r
            };
            if inside(&a[KNIGHT]) {
                return 1;
            }
            if who == 1 {
                return 0;
            }
            return inside(&a[BABY]) as usize;
        }
        for (i, c) in a.iter().enumerate().skip(2) {
            if c.flags & 0xC0 != 0xC0 || c.size == 0 {
                continue;
            }
            if c.kind == 2 && a[KNIGHT].flags & fl::GHOST == 0 {
                continue;
            }
            let (dx, dz) = (x - c.x as i32, z - c.z as i32);
            if dx * dx + dz * dz < self.t.radius2[(c.size & 3) as usize] as i32 && c.script != 0 {
                return i;
            }
        }
        0
    }

    /// `Camera_GetYawOffset`.
    pub fn yaw_offset(&self) -> i32 {
        (-(self.cam.ry as i32)) & 0xFFF
    }

    /// `Knight_ComputeWalkDirectionCode(0)`.
    pub fn walk_dir_code(&self) -> u16 {
        let k = &self.actors[KNIGHT];
        let a = (k.target as i32 + self.yaw_offset()) & 0xE00;
        let mut d = match a {
            0x000 => 0x1000,
            0x200 => 0x3000,
            0x400 => 0x2000,
            0x600 => 0x6000,
            0x800 => 0x4000,
            0xA00 => 0xC000,
            0xC00 => 0x8000,
            _ => 0x9000,
        };
        if k.speed >= 12 {
            d |= 0x10;
        }
        d
    }

    /* ───────────────────────── Knight ───────────────────────── */

    /// `Overworld_KnightUpdate` with no player input: the scripted pad word
    /// during events, move with wall/terrain/NPC collision, then turn.
    pub fn knight_update(&mut self) {
        if self.de & 0x8000 != 0 {
            return;
        }
        let kf = self.actors[KNIGHT].flags;
        let pad = if kf & 0x29 != 0 {
            self.mode4c = 0;
            self.vpad
        } else if kf & fl::VISIBLE == 0 {
            return;
        } else if self.de & 0x80 != 0 {
            self.vpad
        } else if kf & fl::WALKING != 0 {
            0
        } else {
            self.mode4c = 1;
            0
        };
        let ang = match pad & 0xF000 {
            0x1000 => Some(0x000),
            0x2000 => Some(0x400),
            0x3000 => Some(0x200),
            0x4000 => Some(0x800),
            0x6000 => Some(0x600),
            0x8000 => Some(0xC00),
            0x9000 => Some(0xE00),
            0xC000 => Some(0xA00),
            _ => None,
        };
        let yaw = self.yaw_offset();
        {
            let k = &mut self.actors[KNIGHT];
            match ang {
                None => {
                    if k.flags & fl::WALKING == 0 {
                        k.anim = 0;
                    }
                }
                Some(a) => {
                    k.target = (a - yaw) as i16;
                    if !matches!(k.anim, 2 | 3 | 5) {
                        k.frame = 0;
                        k.blend.weight = 0;
                    }
                    k.anim = if pad & 0x10 != 0 {
                        3
                    } else if k.speed >= 0 {
                        2
                    } else {
                        5
                    };
                }
            }
        }
        if self.de & 0x100 != 0 {
            return;
        }
        if matches!(self.actors[KNIGHT].anim, 2 | 3 | 5) {
            self.knight_move();
        }
        let k = &mut self.actors[KNIGHT];
        step_toward(&mut k.facing, k.target as i32, 0x100);
    }

    fn knight_move(&mut self) {
        let (kx, kh, kz, kflags, anim) = {
            let k = &self.actors[KNIGHT];
            (k.x as i32, k.h as i32, k.z as i32, k.flags, k.anim)
        };
        let spd: i32 = if kflags & 0x18 == 0 {
            match anim {
                2 => 6,
                3 => 12,
                _ => -6,
            }
        } else {
            self.actors[KNIGHT].speed as i32
        };
        self.actors[KNIGHT].speed = spd as i8;
        let target = self.actors[KNIGHT].target as i32;
        let (mut ddx, mut ddz) = self.t.polar(target, spd);
        let (m, mask) = (0x20i32, -0x20i32);
        let (dx, mut dz) = (ddx, ddz);
        let blocked = |w: &World, px: i32, pz: i32| -> bool {
            let st = w.terrain(px, kh, pz) as i32;
            let tt = if st == -1 || st == 0 { 100 } else { st - kh };
            (tt + 0x30) as u32 >= 0x61
        };
        if kflags & fl::GHOST == 0 {
            if dx < 0 {
                if blocked(self, kx + dx - m, kz + dz) {
                    ddx = ((kx + dx - m) & -0x20) + (m + 0x20) - kx;
                }
            } else if dx > 0 && blocked(self, m + kx + dx, kz + dz) {
                ddx = ((m + kx + dx) & -0x20) - m - kx;
            }
            if dz < 0 {
                if blocked(self, kx + dx, kz + dz - m) {
                    ddz = ((kz + dz - m) & -0x20) + (m + 0x20) - kz;
                }
            } else if dz > 0 && blocked(self, kx + dx, m + kz + dz) {
                ddz = ((m + kz + dz) & -0x20) - m - kz;
            }
        }
        if dz == 0 && ddx != 0 {
            if blocked(self, kx + ddx, kz - m) {
                ddz = (m + (m + (mask & (kz - m)))) - kz;
            }
            if blocked(self, kx + ddx, kz + m) {
                ddz = ((mask & (kz + m)) - m) - kz;
            }
        }
        if dx == 0 && ddz != 0 {
            if blocked(self, kx - m, kz + ddz) {
                ddx = (m + (m + (mask & (kx - m)))) - kx;
            }
            if blocked(self, kx + m, kz + ddz) {
                ddx = ((mask & (kx + m)) - m) - kx;
            }
        }
        if anim != 5 {
            if ddx == 0 {
                if ddz != 0 {
                    let t = if ddz < 0 { 0x800 } else { 0 };
                    self.actors[KNIGHT].target = t;
                    if dz == ddz {
                        (ddx, ddz) = self.t.polar(t as i32, spd);
                    }
                }
            } else if ddz == 0 {
                let t = if ddx < 0 { 0xC00 } else { 0x400 };
                self.actors[KNIGHT].target = t;
                if dx == ddx {
                    (ddx, ddz) = self.t.polar(t as i32, spd);
                }
            }
        }
        if kflags & fl::GHOST == 0 {
            dz = self.terrain(kx + ddx, kh, kz + ddz) as i32 - kh;
        }
        if ddx == 0 && ddz == 0 {
            return;
        }
        if (dz + 0x20) as u32 >= 0x41 {
            return;
        }
        if self.check_npc_at(kx + ddx, kz + ddz, 0) != 0 && kflags & fl::OVERLAP == 0 {
            return;
        }
        if kflags & fl::OVERLAP != 0 && self.check_npc_at(kx + ddx, kz + ddz, 0) == 0 {
            self.actors[KNIGHT].flags &= !fl::OVERLAP;
        }
        let k = &mut self.actors[KNIGHT];
        if kflags & fl::GHOST == 0 {
            if dz < -spd {
                dz = -spd;
            } else if dz > spd {
                dz = spd;
            }
            k.h = (k.h as i32 + dz) as i16;
        }
        k.x = (k.x as i32 + ddx) as i16;
        k.z = (k.z as i32 + ddz) as i16;
    }

    /* ───────────────────────── Baby ───────────────────────── */

    /// Fills the follow trail as at map entry: 12 steps from the Baby to the Knight.
    pub fn rebuild_trail(&mut self) {
        let (b, k) = (self.actors[BABY].clone(), self.actors[KNIGHT].clone());
        for i in 0..21 {
            let s = (i as i32 - 8).clamp(0, 12);
            let lerp = |a: i16, c: i16| (a as i32 + (c as i32 - a as i32) * s / 12) as i16;
            self.trail[i] = [lerp(b.x, k.x), lerp(b.h, k.h), lerp(b.z, k.z)];
            self.trail_face[i] = if i < 20 { b.facing } else { k.facing };
        }
    }

    /// `FUN_80181648`: scripted turn under WALK_BABY, else the trail.
    pub fn baby_follow(&mut self) {
        if self.actors.len() < 2 {
            return;
        }
        if self.de & 0x400 != 0 {
            let b = &mut self.actors[BABY];
            let d = wrap12(b.target as i32 - b.facing as i32).clamp(-0x100, 0x100);
            b.facing = ((b.facing as i32 + d) & 0xFFF) as i16;
            self.trail[8] = [b.x, b.h, b.z];
            return;
        }
        let [x, h, z] = self.trail[8];
        let k = self.actors[KNIGHT].clone();
        {
            let b = &mut self.actors[BABY];
            b.x = x;
            b.h = h;
            b.z = z;
        }
        if [k.x, k.h, k.z] != self.trail[20] {
            let face = self.trail_face[8];
            let b = &mut self.actors[BABY];
            b.anim = k.anim;
            b.facing = face;
            self.trail.copy_within(1..21, 0);
            self.trail_face.copy_within(1..21, 0);
            self.trail_face[20] = k.facing;
            self.trail[20] = [k.x, k.h, k.z];
        } else {
            let b = &mut self.actors[BABY];
            if b.flags & fl::WALKING != 0 {
                return;
            }
            b.anim = 0;
        }
    }

    /* ───────────────────────── NPCs ───────────────────────── */

    /// `WalkActor_SelectScript`: the first rung whose condition holds places
    /// the actor (or hides it); its walk program follows. `arrival` is
    /// `ctx+0x50`. Returns 1 (arrival), 0 (placed) or −1 (absent).
    pub fn select_script(&mut self, i: usize, arrival: i32) -> i32 {
        let mut cur = self.actors[i].ladder as usize;
        if cur == 0 {
            return -1;
        }
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 4096 || cur + 2 > self.d.len() {
                self.actors[i].state = 3;
                self.actors[i].flags &= !fl::VISIBLE;
                return -1;
            }
            let ev = (self.word(cur) & 0xFFF) as u32;
            cur += 2;
            if self.flags.get(ev) {
                break;
            }
            // Inventory_HasItem(ev − 1000): the player has no items in scenes.
            if self.word(cur) & 0xF000 == 0xA000 {
                continue;
            }
            loop {
                cur += 2;
                if cur + 2 > self.d.len() || self.word(cur) & 0xF000 == 0xA000 {
                    break;
                }
            }
        }
        let w = self.word(cur);
        let mut result = 0;
        let a = &mut self.actors[i];
        if w == 10000 {
            if arrival == a.id as i32 {
                result = 1;
                cur += 2;
            }
        } else if w == 0x7FFF {
            result = -1;
            cur += 2;
            if a.kind == 3 {
                a.kind = 15;
                a.flags |= fl::VISIBLE;
                let wd = |k: usize| le16(self.d, cur + k * 2) as i16;
                a.x = wd(0);
                a.h = wd(1);
                a.z = wd(2);
                let f = wd(3);
                a.facing = f;
                a.target = f;
                a.base = f;
                cur += 8;
            } else {
                a.state = 3;
                a.flags &= !fl::VISIBLE;
            }
        } else {
            a.flags = if a.model == 0xFF { 0xC0 } else { fl::VISIBLE };
            let wd = |k: usize| if cur + k * 2 + 2 <= self.d.len() { le16(self.d, cur + k * 2) as i16 } else { 0 };
            a.x = wd(0);
            a.h = wd(1);
            a.z = wd(2);
            let f = wd(3);
            cur += 8;
            a.rot_x = 0;
            a.rot_z = 0;
            a.facing = f;
            a.target = f;
            a.base = f;
        }
        a.wp = cur;
        a.pc = 0;
        result
    }

    /// `WalkActor_RunScript`: the walk program of actor `i` (`side` = slot;
    /// 0 is the Knight, whose `2nnn` writes the scripted pad word).
    pub fn run_script(&mut self, i: usize, side: usize) {
        if self.actors[i].flags & fl::TALK == 0 {
            let mut guard = 0;
            loop {
                guard += 1;
                if guard > 256 {
                    break;
                }
                let (wp, idx) = (self.actors[i].wp, self.actors[i].pc as usize);
                let op = self.word(wp + idx * 2);
                let arg = (op & 0xFFF) as u32;
                match op & 0xF000 {
                    0xB000 => {
                        let a = &mut self.actors[i];
                        if self.flags.get(arg) {
                            a.state = 1;
                        }
                        a.pc = 0;
                        a.steps = 0;
                        break;
                    }
                    0x0000 => {
                        if arg != 0 && self.flags.get(arg) {
                            self.actors[i].pc += 1;
                            continue;
                        }
                        break;
                    }
                    0x3000 => {
                        let a = &mut self.actors[i];
                        a.anim = arg as u8;
                        a.base_anim = arg as u8;
                        a.frame = 0;
                        a.pc += 1;
                        continue;
                    }
                    0x1000 => {
                        let a = &mut self.actors[i];
                        a.base = (arg << 9) as i16;
                        a.steps = 0;
                        a.pc += 1;
                        break;
                    }
                    0xC000 | 0xD000 | 0xE000 => {
                        let a = &mut self.actors[i];
                        let target = (arg << 9) as i32;
                        let step = a.speed as i32;
                        let p = match op & 0xF000 {
                            0xC000 => &mut a.rot_x,
                            0xD000 => &mut a.facing,
                            _ => &mut a.rot_z,
                        };
                        if target == (*p as i32 & 0xFFF) {
                            a.pc += 1;
                        } else {
                            step_toward(p, target, step);
                        }
                        return;
                    }
                    0x8000 => {
                        let a = &mut self.actors[i];
                        a.speed = arg as u8 as i8;
                        a.pc += 1;
                        continue;
                    }
                    0x6000 => {
                        let a = &mut self.actors[i];
                        a.speed = (arg as i32).wrapping_neg() as u8 as i8;
                        a.pc += 1;
                        continue;
                    }
                    0x2000 => {
                        if self.actors[i].steps as u32 >= arg {
                            let a = &mut self.actors[i];
                            a.pc = idx as u8 + 1;
                            a.steps = 0;
                            self.vpad = 0;
                            continue;
                        }
                        let a = &self.actors[i];
                        if (a.target as i32 - a.facing as i32) & 0xFFF == 0 {
                            if side == 0 {
                                self.vpad = if a.speed == 0 { 0 } else { self.walk_dir_code() };
                                self.actors[i].steps += 1;
                                if self.actors[i].speed >= 0 {
                                    break;
                                }
                            } else {
                                let speed = a.speed as i32;
                                let ang = a.target as i32 + if speed < 1 { 0x800 } else { 0 };
                                let (dx, dz) = self.t.polar(ang, speed.abs());
                                let (nx, nz) = (a.x as i32 + dx, a.z as i32 + dz);
                                let overlap = a.flags & fl::OVERLAP != 0;
                                if self.check_npc_at(nx, nz, side) == 0 || overlap {
                                    {
                                        let a = &mut self.actors[i];
                                        a.x = nx as i16;
                                        a.z = nz as i16;
                                        a.steps = a.steps.wrapping_add(1);
                                    }
                                    if overlap && self.check_npc_at(nx, nz, side) == 0 {
                                        self.actors[i].flags &= !fl::OVERLAP;
                                    }
                                }
                                let a = &self.actors[i];
                                if a.flags & fl::SLOT == 0 || a.kind & 0x40 != 0 {
                                    break;
                                }
                            }
                            let a = &self.actors[i];
                            let h = self.terrain(a.x as i32, a.h as i32, a.z as i32);
                            self.actors[i].h = h;
                        }
                        break;
                    }
                    0x4000 => {
                        let a = &mut self.actors[i];
                        a.pc = 0;
                        a.steps = 0;
                        continue;
                    }
                    0x7000 => {
                        let a = &mut self.actors[i];
                        if self.flags.get(arg) {
                            a.flags &= !(fl::SLOT | fl::VISIBLE);
                            break;
                        }
                        a.flags |= fl::VISIBLE;
                        a.pc += 1;
                        continue;
                    }
                    0x9000 => {
                        if self.actors[i].flags & fl::SLOT != 0 {
                            self.actors[i].flags |= fl::TALK;
                            self.actors[KNIGHT].anim = 0;
                            self.actors[KNIGHT].flags |= fl::TALK;
                            self.actors[i].pc += 1;
                            continue;
                        }
                        break;
                    }
                    _ => break,
                }
            }
            let a = &mut self.actors[i];
            a.target = a.base;
            if side == 0 {
                return;
            }
            if a.anim != a.base_anim {
                a.anim = a.base_anim;
                a.frame = 0;
            }
        }
        let a = &mut self.actors[i];
        step_toward(&mut a.facing, a.target as i32, 0x80);
    }

    /// Screen position of a world point through this frame's camera, or
    /// None when the GTE would flag it (behind, or nearer than H/2).
    pub fn project(&self, p: [i32; 3]) -> Option<(i32, i32)> {
        let v = &self.view;
        let q = [p[0] as f32, p[1] as f32, p[2] as f32];
        let c = |k: usize| v.r[k][0] * q[0] + v.r[k][1] * q[1] + v.r[k][2] * q[2] + v.t[k];
        let (x, y, z) = (c(0), c(1), c(2));
        if z * 2.0 <= v.h {
            return None;
        }
        Some(((160.0 + v.h * x / z) as i32, (112.0 + v.h * y / z) as i32))
    }

    /// `Npc_DrawIfVisible`'s on-screen test.
    fn on_screen(&self, a: &Actor) -> bool {
        let Some((sx, sy)) = self.project([a.x as i32, a.h as i32, a.z as i32]) else { return false };
        let (lo, hi) = if self.mode4c != 0 { (-40, 360) } else { (-60, 380) };
        (lo..=hi).contains(&sx) && (-40..=300).contains(&sy)
    }

    /// `NPC_UpdateActors` for every actor: programs and placement states for
    /// NPCs, then the sequencer, and the blend for actors drawn on screen.
    pub fn npc_update(&mut self) {
        for i in 0..self.actors.len() {
            self.actors[i].drawn = false;
            if i >= 2 {
                if self.chunk >= 0x145 && self.actors[i].id as u16 != self.chunk {
                    continue;
                }
                match self.actors[i].state {
                    0 => self.run_script(i, i),
                    1 | 2 => {
                        if self.actors[i].state == 1 {
                            if self.actors[i].flags & fl::SLOT != 0 {
                                // Still on screen: stays where it is until the camera looks away.
                            } else {
                                self.select_script(i, -1);
                                self.actors[i].state = 2;
                            }
                        }
                        if self.actors[i].state == 2 {
                            let on = {
                                let a = &self.actors[i];
                                self.project([a.x as i32, a.h as i32, a.z as i32]).is_some_and(|(sx, sy)| (sy + 0x30) as u32 <= 0x140 && sx >= -0x30 && sx < if self.mode4c != 0 { 0x231 } else { 0x171 })
                            };
                            if !on || self.fade == 0x1F || self.fade == 0x3F {
                                self.actors[i].state = 0;
                            }
                        }
                    }
                    _ => {
                        self.run_script(i, i);
                        continue;
                    }
                }
                // Residency: only the scene's chunk is streamed in.
                if self.actors[i].id as u16 != self.chunk {
                    self.actors[i].flags |= fl::NOT_RESIDENT;
                    self.actors[i].flags &= !fl::SLOT;
                    continue;
                }
                self.actors[i].flags &= !fl::NOT_RESIDENT;
            }
            if self.actors[i].flags & fl::VISIBLE == 0 {
                continue;
            }
            let a = &self.actors[i];
            if a.set != 0xFF && a.kind & 0xF != 1 {
                let a = &mut self.actors[i];
                let (anim_i, mut frame, mut blend) = (a.anim, a.frame, a.blend);
                let stepped = blend.weight == 0;
                anim::step(&a.anim_tbl, anim_i, &mut frame, &mut blend);
                a.frame = frame;
                a.blend = blend;
                if stepped && i < 2 && a.anim == 3 {
                    if i == KNIGHT && (frame == 0 || frame == 4) && self.de & 6 == 0 {
                        self.cues.push((self.frame, Cue::Sfx { id: 5 + frame as u16 / 4 }));
                    } else if i == BABY && frame == 1 {
                        self.cues.push((self.frame, Cue::Sfx { id: 10 }));
                    }
                }
            }
            let a = &self.actors[i];
            if a.state != 2 && a.model != 0xFF {
                if self.on_screen(a) {
                    let a = &mut self.actors[i];
                    a.flags |= fl::SLOT;
                    if a.kind & 0xF == 1 {
                        continue;
                    }
                    if a.set != 0xFF {
                        a.blend.update();
                    }
                    a.drawn = true;
                } else if i >= 2 {
                    self.actors[i].flags &= !fl::SLOT;
                }
            }
        }
    }

    /* ───────────────────────── Camera ───────────────────────── */

    /// `FUN_8019f88c`: the distance profile while the Knight is the focus,
    /// then this frame's view from the focus actor plus the offset.
    pub fn camera_task(&mut self) {
        if self.focus == 0 {
            let lock = self.actors[KNIGHT].flags & fl::TALK != 0;
            let p = self.cam.profile;
            let c = &mut self.cam;
            if lock {
                if c.f10 > 0x708 {
                    c.f10 -= p[1];
                }
            } else if p[3] & 0x1F != 0 {
                c.f10 = p[0];
            } else {
                let d = p[0] as i32 - c.f10 as i32;
                if d <= 0 || d < p[1] as i32 {
                    c.f10 = p[0];
                } else {
                    c.f10 += p[1];
                }
            }
        } else if self.focus & 0x80 != 0 {
            let (ch, k) = (self.actors[(self.focus & 0x7F) as usize].clone(), self.actors[KNIGHT].clone());
            self.cam.ry = (self.t.ratan2(ch.z as i32 - k.z as i32, k.x as i32 - ch.x as i32) + 0x400) as i16;
            let (dx, dz) = (ch.x as f64 - k.x as f64, ch.z as f64 - k.z as f64);
            self.cam.rx = (-self.t.ratan2(ch.h as i32 - k.h as i32, (dx * dx + dz * dz).sqrt() as i32)) as i16;
        }
        let ch = self.actors.get((self.focus & 0x7F) as usize).cloned().unwrap_or_default();
        let c = &self.cam;
        let (s, co) = (|a: i16| self.t.sin(a as i32), |a: i16| self.t.cos(a as i32));
        let (cx, sx, cy, sy, cz, sz) = (co(c.rx), s(c.rx), co(c.ry), s(c.ry), co(c.rz), s(c.rz));
        let m = |a: i32, b: i32| (a * b) >> 12;
        // FUN_801a108c: R = Rz(rz)·Rx(rx)·Ry(−ry), each product >> 12.
        let r = [
            [m(cy, cz) + m(m(sy, sx), sz), -m(cx, sz), -m(sy, cz) + m(m(cy, sx), sz)],
            [m(cy, sz) - m(m(sy, sx), cz), m(cx, cz), -m(sy, sz) - m(m(cy, sx), cz)],
            [m(sy, cx), sx, m(cy, cx)],
        ];
        let pos = [-(ch.x as i32 + c.off[0] as i32), -(ch.h as i32 + c.off[1] as i32), -(ch.z as i32 + c.off[2] as i32)];
        let tr: [i32; 3] = std::array::from_fn(|k| ((r[k][0] as i64 * pos[0] as i64 + r[k][1] as i64 * pos[1] as i64 + r[k][2] as i64 * pos[2] as i64) >> 12) as i32);
        let t = [tr[0] + c.fc as i32, tr[1] + c.fe as i32, tr[2] + c.f10 as i32];
        let rf: [[f32; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| r[i][j] as f32 / 4096.0));
        let tf = [t[0] as f32, t[1] as f32, t[2] as f32];
        let eye = std::array::from_fn(|k| -(rf[0][k] * tf[0] + rf[1][k] * tf[1] + rf[2][k] * tf[2]));
        self.view = View { r: rf, t: tf, eye, h: c.h as f32, shake: c.shake };
    }

    /// `Event_UpdateSpeakerCamera`: the target offset so that Knight + offset
    /// = (x, h, z) on `chunk`, moved at 18/frame (major axis) or set at once.
    /// True while still moving.
    pub fn speaker_camera(&mut self, chunk: u16, x: i16, h: i16, z: i16, mut smooth: bool) -> bool {
        let (mut x, mut z) = (x as i32, z as i32);
        let cur = self.chunk as i32;
        if cur != chunk as i32 {
            if chunk.is_multiple_of(2) {
                x += if cur < chunk as i32 { 0x1000 } else { -0x1000 };
            } else {
                z -= 0x1000;
                x += if cur < chunk as i32 { 0x800 } else { -0x800 };
            }
        }
        let k = &self.actors[KNIGHT];
        let d = [(x - k.x as i32) as i16, (h as i32 - k.h as i32) as i16, (z - k.z as i32) as i16];
        if self.fade == 0x1F || self.fade == 0x3F {
            smooth = false;
        }
        let off = &mut self.cam.off;
        if !smooth {
            *off = d;
        } else {
            let (ax, az) = ((d[0] as i32).abs(), (d[2] as i32).abs());
            let (rx, rz) = if az < ax { (1, if az == 0 { 0x7FFF } else { ax / az }) } else { (if ax == 0 { 0x7FFF } else { az / ax }, 1) };
            let (stx, stz) = (18 / rx, 18 / rz);
            off[0] = if stx != 0 { approach(off[0], d[0], stx as i16) } else { d[0] };
            off[1] = approach(off[1], d[1], 16);
            off[2] = if stz != 0 { approach(off[2], d[2], stz as i16) } else { d[2] };
        }
        *off != d
    }
}

/// `ApproachS16_func_801E3A98`.
pub fn approach(p: i16, target: i16, s: i16) -> i16 {
    if s == 0 {
        return target;
    }
    let (p, t, s) = (p as i32, target as i32, s as i32);
    let v = if p < t {
        (p + s).min(t)
    } else if p > t {
        (p - s).max(t)
    } else {
        p
    };
    v as i16
}

/// `Math_ApproachS16EaseOut`: linear at `step` until within 2·step, then r → r − (r/4 + 1).
pub fn ease_out(p: i16, target: i16, step: i16) -> i16 {
    let step = step.unsigned_abs() as i16;
    let mut st = step;
    if approach(p, target, step.saturating_mul(2)) == target {
        st = ((p as i32 - target as i32).abs() / 4 + 1) as i16;
    }
    approach(p, target, st)
}
