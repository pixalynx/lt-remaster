//! The script task: `Event_Tier2ScriptTask` (`ovr02.c:2846-2958`), the
//! interpreter `Event_Tier2Interpreter` (`:2215-2812`) and the child tasks
//! its opcodes spawn (walk, turns, door exit, choice menu, chest, BGM load,
//! sparkle helpers and the canned CUTSCENE tasks).
//!
//! A child signals by writing the script's `f4` during the frame; the
//! script reads it the next frame (the text task, its first child, resets it
//! to 0 every frame). An opcode that waits on a child therefore resumes the
//! frame after the signal and runs the next opcode the frame after that.

use crate::container::le16;
use crate::tier2;

use super::text::{Strings, TextEnv, TextPtr, TextTask, WinFrame};
use super::world::{ease_out, fl, Cue, World, BABY, KNIGHT};

/// How a scene stopped.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Ending {
    /// END and the manager's teardown.
    End,
    /// WARP to `chunk` (the field reloads).
    Warp { chunk: u16, x: i16, z: i16, facing: u16 },
    /// BATTLE with map id `id` (or a trap chest).
    Battle { id: u16 },
    /// INN: rest, save prompt, warp.
    Inn,
    /// CUTSCENE 10 (Holy Armor, field reload) or 13/14 (flight).
    Mode { task: u16 },
    /// The script did not finish within the frame budget.
    Timeout,
    /// GOTO back to `at` a second time with the story and every actor where
    /// they were: the game would repeat this until the player did something
    /// else (a shop purchase, another answer).
    Loop { at: u32 },
    /// An opcode the game cannot run (length 0) or a runaway loop.
    Hang { at: u32 },
}

/// A choice prompt and the answer the player gave.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceRec {
    pub frame: u32,
    pub entries: Vec<String>,
    /// 1-based.
    pub answer: u8,
}

/// The choice menu drawn this frame.
#[derive(Debug, Clone, Default)]
pub struct MenuFrame {
    pub entries: Vec<String>,
    pub cursor: u8,
}

/// Things the screen lists beside the picture.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub frame: u32,
    /// D offset of the opcode.
    pub at: u32,
    pub op: u8,
    pub name: &'static str,
    pub note: String,
}

/// The light pillar of CUTSCENE 3/9 (OVR23), for the renderer.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pillar {
    /// Centre (actor 10's feet).
    pub x: i16,
    pub z: i16,
    pub ground: i16,
    /// Bottom of the column (descends to the ground), radius, rotation (0x1000 = turn).
    pub bottom: i16,
    pub radius: i16,
    pub spin: i16,
    /// World brightness 0–256 (palette darkening).
    pub dim: u16,
    /// Falling sparkles shown, rising swirl density (0 = none).
    pub sparkles: u8,
    pub swirl: u8,
}

/// Inputs the game would read from the pad.
pub struct Pad {
    /// Frames to wait at a page end (after it is fully typed) before confirming.
    pub hold: Box<dyn Fn(usize) -> u32>,
    /// Choice answers in order (1-based); missing = 1.
    pub answers: Vec<u8>,
}

enum Child {
    Walk { side: usize, arg: usize, phase: u8, tick: u32 },
    Turn { actor: usize, about: bool, tick: u32 },
    KnightAway { actor: usize, phase: u8, tick: u32, f4: u32, turn: Option<(u32, usize)> },
    Choice { entries: Vec<String>, cursor: u8, tick: u32, answer: u8 },
    ChestFx { actor: usize, tick: u32 },
    Bgm { tick: u32 },
    Helper { actor: usize, dying: bool, tick: u32 },
    Cutscene { n: u16, tick: u32, phase: u8, a: i32, b: i32 },
}

pub struct Script {
    /// `t->f2B`: whose script runs.
    pub actor: usize,
    pub pc: usize,
    f24: u32,
    /// `t->f2`: the actor the camera tracks.
    track: i16,
    f29: u8,
    tick: u32,
    /// The children's signal (read the frame after it is posted).
    f4: u32,
    counter: i16,
    pub text: Option<TextPtr>,
    pub speaker: Option<usize>,
    shake: [i16; 3],
    pub tt: TextTask,
    children: Vec<Child>,
    /// Frames the current page end has waited for a confirm.
    page_wait: u32,
    choice_wait: u32,
    answers_used: usize,
    /// Choices already asked (by address): asked again, the default is the last entry.
    asked: Vec<usize>,
    /// Backward GOTOs: target → the story and actor positions at the last visit.
    loops: Vec<(usize, Vec<u16>, Vec<[i16; 3]>)>,
    pub ending: Option<(u32, Ending)>,
    /// Frame the tree dies (WARP: 3 frames later; END: now).
    pub dies_at: Option<u32>,
    pub events: Vec<Event>,
    pub choices: Vec<ChoiceRec>,
    pub presses: Vec<u32>,
    pub objective: Option<String>,
    pub menu: Option<MenuFrame>,
    pub pillar: Option<Pillar>,
    /// Actors with a sparkle helper.
    pub helpers: Vec<usize>,
    /// Sound requests from the text task this frame (8/9 open, 0xFFFF page blip).
    text_sfx: Vec<u16>,
    /// The opcode last written to `events` (a re-run opcode is listed once).
    logged: Option<usize>,
    /// Character slots the executed opcodes refer to, and flags they cleared
    /// (for inferring the story state the scene expects).
    pub refs: Vec<usize>,
    pub cleared: Vec<u16>,
}

fn plain_line(b: &[u8]) -> String {
    crate::text::plain(&crate::text::decode(b)).split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Script {
    /// `Event_Tier2ScriptTask` tick 0 (no opcode runs this frame).
    pub fn new(w: &mut World, actor: usize) -> Self {
        let pc = w.actors[actor].script as usize;
        let a = &mut w.actors[actor];
        a.target = a.facing;
        Self {
            actor,
            pc,
            f24: 0,
            track: 0,
            f29: 0,
            tick: 0,
            f4: 0,
            counter: 0,
            text: None,
            speaker: None,
            shake: [0; 3],
            tt: TextTask::new(),
            children: Vec::new(),
            page_wait: 0,
            choice_wait: 0,
            answers_used: 0,
            asked: Vec::new(),
            loops: Vec::new(),
            ending: None,
            dies_at: None,
            events: Vec::new(),
            choices: Vec::new(),
            presses: Vec::new(),
            objective: None,
            menu: None,
            pillar: None,
            helpers: Vec::new(),
            text_sfx: Vec::new(),
            logged: None,
            refs: Vec::new(),
            cleared: Vec::new(),
        }
    }

    fn end(&mut self, frame: u32, e: Ending) {
        if self.ending.is_none() {
            self.ending = Some((frame, e));
        }
    }

    fn log(&mut self, w: &World, op: u8, note: String) {
        let name = tier2::op_info(op).map(|o| o.name).unwrap_or("?");
        match self.events.last_mut() {
            // The generic entry written when the opcode started: add the detail.
            Some(e) if e.at == self.pc as u32 && e.op == op && e.note.is_empty() => e.note = note,
            _ => self.events.push(Event { frame: w.frame, at: self.pc as u32, op, name, note }),
        }
    }

    /// A short note for opcodes that carry an operand worth showing.
    fn note(&self, w: &World, op: u8, arg: u16) -> String {
        let b = |k: usize| self.byte_at(w, k);
        match op {
            0x10 => format!("anim {arg}"),
            0x12 => format!("sfx 0x{arg:03X}"),
            0x13 | 0x45 => format!("chunk {arg} x {} z {} h {}", self.arg_at(w, 3) as i16, self.arg_at(w, 5) as i16, self.arg_at(w, 7) as i16),
            0x14 | 0x46 => format!("pitch {} yaw {} roll {}", arg as i16, self.arg_at(w, 3) as i16, self.arg_at(w, 5) as i16),
            0x1C | 0x34 => format!("D+0x{arg:04X}"),
            0x1F => format!("\"{}\"", plain_line(crate::text::c_string(w.d, arg as usize))),
            0x24 => format!("actor {} anim {}", b(1), b(2)),
            0x25 => format!("actor {} faces actor {}", b(1), b(2)),
            0x26 | 0x48 => format!("actor {}", b(1)),
            0x27 => format!("{} frames", b(1)),
            0x29 => "to black".into(),
            0x2A => "from black".into(),
            0x2B => "to white".into(),
            0x2C => "from white".into(),
            0x0B | 0x2D => format!("flag 0x{arg:03X} {}", if w.flags.get(arg as u32) { "set" } else { "clear" }),
            0x30 => format!("{} × {} for {} frames", b(1), b(2), b(3)),
            0x36 | 0x37 => format!("actor {}", b(1)),
            0x41 => format!("distance {arg}"),
            0x42 => format!("sparkles on actor {}", b(1)),
            0x43 => format!("actor {arg}"),
            0x47 => format!("H {arg}"),
            0x49 => format!("0x{arg:03X}"),
            0x4A if b(1) == tier2::EXT_RUN_OVERLAY => format!("run overlay {} with {} (in the game only)", self.arg_at(w, 2), self.arg_at(w, 4) as i16),
            0x4A => format!("EXT {} (in the game only)", b(1)),
            _ => String::new(),
        }
    }

    /// One frame: shake jitter, the interpreter (or the child wait), the
    /// camera follow, then the children in spawn order (the text task first).
    pub fn step(&mut self, w: &mut World, strings: &mut Strings, names: &[u16; 8], item_name: &dyn Fn(u16) -> Vec<u8>, toy_name: &dyn Fn(u16) -> Vec<u8>, pad: &Pad) {
        if self.shake[0] != 0 {
            w.cam.shake[0] = (w.rand() % self.shake[0] as i32) as i16;
        }
        if self.shake[1] != 0 {
            w.cam.shake[1] = (w.rand() % self.shake[1] as i32) as i16;
        }
        let mut r = 1;
        // Tick 0 (the spawn frame) only spawns the text task: the first opcode runs next frame.
        if self.f29 < 7 && self.tick > 0 {
            if self.f24 & 0x2000 != 0 {
                match self.f4 {
                    2 | 3 => {
                        self.f24 &= !0xA000;
                        self.text = None;
                        if self.f24 & 0x20 != 0 {
                            self.f29 = 7;
                        }
                    }
                    1 => self.f24 &= !0x2000,
                    0x8000_0000 => self.f29 = 7,
                    _ => {}
                }
            } else {
                self.f24 &= !0x3800;
                let mut budget = 0;
                loop {
                    r = self.interpret(w, strings, pad);
                    budget += 1;
                    if r != 0 || self.f29 >= 7 {
                        break;
                    }
                    if budget > 4096 {
                        self.end(w.frame, Ending::Hang { at: self.pc as u32 });
                        self.f29 = 7;
                        break;
                    }
                }
            }
            if r != 0x26 && self.track != 0 && w.focus == 0 {
                let a = w.actors.get(self.track as usize).cloned().unwrap_or_default();
                w.speaker_camera(a.id as u16, a.x, a.h, a.z, false);
            }
        }
        if self.f29 == 7 {
            w.actors[KNIGHT].speed = 0;
            if self.f24 & 0x40 != 0 {
                self.f24 |= 0x2000;
                self.f29 = 8;
                self.tick = 0;
            } else {
                self.end(w.frame, Ending::End);
                self.dies_at.get_or_insert(w.frame);
            }
        } else if self.f29 == 8 && self.tick >= 17 {
            self.end(w.frame, Ending::End);
            self.dies_at.get_or_insert(w.frame);
        }
        self.tick += 1;
        if self.dies_at.is_some_and(|f| f <= w.frame) {
            return;
        }
        // Children, the text task first. Auto-confirm at page ends.
        let waiting = self.tt.state == 6;
        self.page_wait = if waiting { self.page_wait + 1 } else { 0 };
        let shown = self.tt.out.glyphs.len();
        let confirm = waiting && self.page_wait > (pad.hold)(shown);
        if confirm {
            self.presses.push(w.frame);
        }
        let mut kf = w.actors[KNIGHT].flags;
        self.text_sfx.clear();
        {
            let mut env = TextEnv {
                text: &mut self.text,
                speaker: self.speaker,
                confirm,
                mode4c: w.mode4c,
                knight_flags: &mut kf,
                signal: &mut self.f4,
                strings,
                names,
                item_name,
                toy_name,
                sfx: &mut self.text_sfx,
            };
            self.tt.step(&mut env);
        }
        w.actors[KNIGHT].flags = kf;
        for s in std::mem::take(&mut self.text_sfx) {
            if s != 0xFFFF {
                w.cue(Cue::Sfx { id: s });
            }
        }
        self.menu = None;
        let mut children = std::mem::take(&mut self.children);
        children.retain_mut(|c| self.step_child(c, w, pad));
        children.append(&mut self.children);
        self.children = children;
    }

    pub fn window(&self) -> &WinFrame {
        &self.tt.out
    }

    /// Returns false when the child dies.
    fn step_child(&mut self, c: &mut Child, w: &mut World, pad: &Pad) -> bool {
        match c {
            Child::Walk { side, arg, phase, tick } => {
                let i = *side;
                let base = *arg;
                if *tick == 0 {
                    let a = &mut w.actors[i];
                    a.wp = base + 2;
                    a.flags = (a.flags | fl::WALKING) & !fl::TALK;
                    a.pc = 0;
                    a.steps = 0;
                    w.de |= 0x8000;
                    a.anim = 0;
                    a.frame = 0;
                    a.blend.weight = 0;
                    w.vpad = 0;
                }
                *tick += 1;
                let rd = |w: &World, k: usize| le16(w.d, w.actors[i].wp + k * 2) as i16;
                match *phase {
                    0 => {
                        w.de &= 0x7FFF;
                        let (wx, wz) = (rd(w, 0) as i32, rd(w, 2) as i32);
                        let a = &w.actors[i];
                        let (ddx, ddz) = (a.x as i32 - wx, wz - a.z as i32);
                        if ddx != ddz {
                            let t = (w.t.ratan2(-ddx, a.z as i32 - wz) + 0x400) & 0xF00;
                            w.actors[i].target = t as i16;
                            if (w.actors[i].facing as i32 & 0xFFF) != t {
                                return true;
                            }
                        }
                        *phase = 1;
                    }
                    1 => {
                        let (wx, wz, dir) = (rd(w, 0), rd(w, 2), rd(w, 3));
                        let a = &mut w.actors[i];
                        a.anim = 2;
                        a.speed = 0;
                        let (dx, dz) = ((a.x as i32 - wx as i32).abs(), (a.z as i32 - wz as i32).abs());
                        let (mut sx, mut sz): (i32, i32);
                        if dz < dx {
                            sz = if dz != 0 { (dx / dz) as i16 as i32 } else { 0x7FFF };
                            sx = 1;
                        } else {
                            sx = if dx != 0 { (dz / dx) as i16 as i32 } else { 0x7FFF };
                            sz = 1;
                        }
                        sx = 6 / sx + 1;
                        sz = 6 / sz + 1;
                        let step = |p: i16, t: i16, s: i32| -> i16 {
                            if p < t {
                                ((p as i32 + s) as i16).min(t)
                            } else if t < p {
                                ((p as i32 - s) as i16).max(t)
                            } else {
                                p
                            }
                        };
                        a.x = step(a.x, wx, sx);
                        a.z = step(a.z, wz, sz);
                        let k = w.actors[KNIGHT].clone();
                        w.actors[KNIGHT].h = w.terrain(k.x as i32, k.h as i32, k.z as i32);
                        let a = &mut w.actors[i];
                        if a.x == wx && a.z == wz {
                            a.base = dir;
                            a.target = dir;
                            if a.facing as i32 & 0xFFF == dir as i32 {
                                a.flags &= !fl::WALKING;
                                a.wp += 8;
                                *phase = 2;
                            }
                        }
                    }
                    _ => {
                        w.run_script(i, i);
                        let a = &w.actors[i];
                        if w.word(a.wp + a.pc as usize * 2) == 0 {
                            w.de &= !0x400;
                            self.f4 = 2;
                            return false;
                        }
                    }
                }
                true
            }
            Child::Turn { actor, about, tick } => {
                let a = &mut w.actors[*actor];
                if *tick == 0 {
                    w.cues.push((w.frame, Cue::Sfx { id: 0x11 }));
                    if *about {
                        a.target = ((a.target as i32 + 0x800) & 0xFFF) as i16;
                    }
                }
                let t = *tick;
                *tick += 1;
                if *about {
                    a.target = a.target.wrapping_sub(0x40);
                    a.base = a.base.wrapping_sub(0x40);
                } else {
                    a.target = a.target.wrapping_add(0x40);
                    a.base = a.base.wrapping_add(0x40);
                }
                if t == 0xB {
                    self.f4 = 2;
                    return false;
                }
                true
            }
            Child::KnightAway { actor, phase, tick, f4, turn } => {
                if *tick == 0 && turn.is_none() && *phase == 0 {
                    let t = w.actors[*actor].target;
                    w.actors[KNIGHT].target = ((t as i32 + 0x800) & 0xFFF) as i16;
                    *turn = Some((0, *actor));
                }
                let t0 = *tick;
                *tick += 1;
                match *phase {
                    0 => {
                        if *f4 != 0 {
                            w.actors[KNIGHT].speed = 4;
                            w.actors[KNIGHT].flags |= fl::GHOST;
                            *tick = 1;
                            *phase = 1;
                            w.vpad = w.walk_dir_code();
                        }
                    }
                    1 => {
                        if t0 == 4 {
                            *phase = 2;
                        }
                    }
                    _ => {
                        if *f4 != 0 {
                            self.f4 = 2;
                            return false;
                        }
                    }
                }
                // Its own child: the actor (a door) turns +0x300 over 12 frames.
                if let Some((t, a)) = turn.as_mut() {
                    if *t == 0 {
                        w.cues.push((w.frame, Cue::Sfx { id: 0x11 }));
                    }
                    let c = &mut w.actors[*a];
                    c.target = c.target.wrapping_add(0x40);
                    c.base = c.base.wrapping_add(0x40);
                    if *t == 0xB {
                        *f4 = 2;
                        *turn = None;
                    } else {
                        *t += 1;
                    }
                }
                true
            }
            Child::Choice { entries, cursor, tick, answer } => {
                let t = *tick;
                *tick += 1;
                if t == 0 {
                    return true;
                }
                self.choice_wait += 1;
                *cursor = (*answer - 1).min(entries.len().saturating_sub(1) as u8);
                self.menu = Some(MenuFrame { entries: entries.clone(), cursor: *cursor });
                if self.choice_wait > (pad.hold)(entries.iter().map(|e| e.len()).sum()) {
                    self.presses.push(w.frame);
                    w.cues.push((w.frame, Cue::Sfx { id: 2 }));
                    self.f4 = *cursor as u32 + 1;
                    self.choice_wait = 0;
                    return false;
                }
                true
            }
            Child::ChestFx { actor, tick } => {
                let t = *tick;
                *tick += 1;
                if t == 0 {
                    w.cues.push((w.frame, Cue::Sfx { id: 0x13 }));
                }
                let a = &mut w.actors[*actor];
                a.frame = a.frame.saturating_add((2 * (t / 2)) as u8);
                if a.frame >= 0xC1 && a.kind != 0xF {
                    a.kind = 0xF;
                    self.f4 = 2;
                }
                true
            }
            Child::Bgm { tick } => {
                // A CD read: about a third of a second (not frame-deterministic in the game).
                *tick += 1;
                if *tick >= 10 {
                    self.f4 = 2;
                    return false;
                }
                true
            }
            Child::Helper { actor, dying, tick } => {
                *tick += 1;
                if *dying && *tick > 16 {
                    self.helpers.retain(|h| h != actor);
                    return false;
                }
                true
            }
            Child::Cutscene { n, tick, phase, a, b } => self.step_cutscene(w, *n, tick, phase, a, b),
        }
    }

    /// The canned tasks of `D_801FC40C`.
    fn step_cutscene(&mut self, w: &mut World, n: u16, tick: &mut u32, phase: &mut u8, a: &mut i32, b: &mut i32) -> bool {
        let t = *tick;
        *tick += 1;
        match n {
            // An actor hops out of the Knight to its own spot (49 frames).
            0 => {
                let slot = if w.chunk == 0x15A { 3 } else { 22 };
                if slot >= w.actors.len() {
                    self.f4 = 2;
                    return false;
                }
                if t == 0 {
                    *a = (w.actors[slot].x as i32) << 16 | (w.actors[slot].z as u16 as i32);
                    *b = w.actors[slot].h as i32;
                }
                let (dx, dz, dh) = ((*a >> 16) as i16 as i32, *a as i16 as i32, *b);
                let k = w.actors[KNIGHT].clone();
                let tt = t.min(48) as i32;
                let s = w.t.sin(tt * 42);
                let o = &mut w.actors[slot];
                o.x = (k.x as i32 + (dx - k.x as i32) / 48 * tt) as i16;
                o.z = (k.z as i32 + (dz - k.z as i32) / 48 * tt) as i16;
                o.h = (k.h as i32 + (dh - k.h as i32) / 48 * tt - ((s * 80) >> 12)) as i16;
                o.facing = ((k.facing as i32 ^ 0x800) + tt * 255) as i16;
                if t >= 0x31 {
                    self.f4 = 2;
                    return false;
                }
                true
            }
            // The Knight jumps off a ledge (52 frames).
            1 => {
                if t == 0 {
                    *a = w.actors[KNIGHT].z as i32;
                    *b = w.actors[KNIGHT].h as i32;
                    w.de |= 0x100;
                }
                let ang = (t as i32 * 0x180).min(0xC00);
                let half = t as i32 / 2;
                let s = w.t.sin(ang);
                let k = &mut w.actors[KNIGHT];
                k.z = (*a + 4 * t as i32) as i16;
                k.h = (*b - ((s * (half * half + 0x20)) >> 12)) as i16;
                if t >= 0x33 {
                    w.de &= !0x100;
                    self.f4 = 2;
                    return false;
                }
                true
            }
            // Instant (heal) and CD loads that show nothing.
            2 | 4 | 5 => {
                self.f4 = 2;
                false
            }
            // Actor 2 flickers 30 frames, then stays hidden.
            6 => {
                if t == 0 && w.actors.len() > 2 {
                    w.actors[2].kind = (w.actors[2].kind & 0xF0) | 1;
                }
                if t >= 30 {
                    self.f4 = 2;
                    return false;
                }
                true
            }
            // Name entry: the name in hand is kept.
            7 | 8 | 12 => {
                if t >= 2 {
                    self.f4 = 2;
                    return false;
                }
                true
            }
            10 | 13 | 14 => {
                if t >= if n == 10 { 1 } else { 3 } {
                    self.end(w.frame, Ending::Mode { task: n });
                    self.dies_at.get_or_insert(w.frame);
                }
                true
            }
            // The Knight slides through 3 fixed points.
            11 => {
                const PTS: [(i32, i32); 3] = [(305, -80), (149, -80), (-17, -8)];
                let mut i = *a as usize;
                while i < 3 {
                    let k = &w.actors[KNIGHT];
                    let (px, pz) = PTS[i];
                    let ang = w.t.facing((k.x as i32, k.z as i32), (px, pz));
                    let (sx, sz) = ((w.t.sin(ang) * 4) >> 12, (w.t.cos(ang) * 4) >> 12);
                    let k = &mut w.actors[KNIGHT];
                    k.x = (k.x as i32 + sx) as i16;
                    k.z = (k.z as i32 + sz) as i16;
                    if (k.x as i32 - px).abs() <= 8 && (k.z as i32 - pz).abs() <= 8 {
                        *a += 1;
                    }
                    i += 1;
                }
                if *a >= 3 {
                    self.f4 = 2;
                    return false;
                }
                true
            }
            // Dolly zoom until the scene ends; the script goes on at once.
            15 => {
                if t == 0 {
                    self.f4 = 2;
                }
                w.cam.h += 2;
                w.cam.f10 += 1;
                true
            }
            // OVR23's pillar of light around actor 10 (3: orbiting camera, 9: fixed).
            3 | 9 => self.step_pillar(w, n, t, phase, a, b),
            _ => {
                self.f4 = 2;
                false
            }
        }
    }

    /// OVR23 `func_801531B4`, its camera and phases exact, its particles as
    /// counts for the renderer. The world darkening is reported as a brightness.
    fn step_pillar(&mut self, w: &mut World, n: u16, t: u32, phase: &mut u8, a: &mut i32, b: &mut i32) -> bool {
        const LOAD: u32 = 12;
        let centre = w.actors.get(10).cloned().unwrap_or_default();
        if t == 0 {
            w.flags.set(0xFC);
            w.cue(Cue::BgmLoad { file: 47 });
        }
        if t < LOAD {
            return true;
        }
        let tk = t - LOAD;
        if tk == 0 {
            if n == 9 {
                w.actors[KNIGHT].facing = 0x400;
                w.actors[KNIGHT].target = 0x400;
            }
            w.cue(Cue::Sfx { id: 0 });
            w.cue(Cue::Sfx { id: 0x201 });
            w.actors[KNIGHT].flags |= fl::TALK;
            self.pillar = Some(Pillar { x: centre.x, z: centre.z, ground: centre.h, bottom: -0x300, radius: 0x20, dim: 256, ..Default::default() });
        }
        if tk >= 4 {
            let c = &mut w.cam;
            if n == 9 {
                super::math::step_toward(&mut c.ry, 0x400, 2);
                super::math::step_toward(&mut c.rx, 0xC8, 2);
                c.f10 = ease_out(c.f10, 0x5DC, 0x20);
                c.fe = ease_out(c.fe, 0x3C, 0x20);
            } else {
                c.ry = c.ry.wrapping_add(2);
                let goal = 0x1F4 - ((w.t.sin(c.ry as i32 & 0x7FF) * 0x148) >> 12);
                c.rx = super::world::approach(c.rx, goal as i16, 2);
                if c.fe < 0x3C {
                    c.fe += 1;
                }
            }
        }
        let p = self.pillar.get_or_insert_with(Pillar::default);
        p.x = centre.x;
        p.z = centre.z;
        p.ground = centre.h;
        p.spin = (tk as i32 * 4) as i16;
        let darken_end = if n == 9 { 15 } else { 25 };
        if (5..=darken_end).contains(&tk) {
            p.dim = (256 * 8 / (tk + 4)) as u16;
        }
        match *phase {
            0 => {
                p.bottom = (tk as i32 * 2 - 0x300).min(0) as i16;
                if tk >= 384 {
                    *phase = 1;
                    *a = 0;
                }
            }
            1 => {
                *a += 1;
                p.radius = (0x20 + *a).min(0x70) as i16;
                if *a >= 80 {
                    w.cue(Cue::Sfx { id: 0x202 });
                    *phase = 2;
                    *a = 0;
                }
            }
            2 => {
                // Falling sparkles from y −400 at 1–2 units a frame: the first lands ~200 frames on.
                *a += 1;
                p.sparkles = (*a / 10).min(20) as u8;
                if *a >= 200 {
                    if w.actors.len() > 10 {
                        let o = &mut w.actors[10];
                        o.kind &= 0xF0;
                    }
                    w.cue(Cue::Sfx { id: 0x203 });
                    *phase = 3;
                    *a = 0;
                    *b = 0x40;
                }
            }
            3 => {
                *a += 1;
                if *a % 16 == 0 && *b > 9 {
                    *b -= 1;
                }
                p.swirl = (0x48 - *b).clamp(1, 0x40) as u8;
                if *b < 0x30 {
                    self.f4 = 2;
                    *phase = 4;
                }
            }
            _ => {
                p.swirl = (0x48 - *b).clamp(1, 0x40) as u8;
                if w.fade == 0x1F {
                    p.dim = 256;
                    if n == 3 {
                        w.flags.clear(0xFC);
                    }
                    w.cue(Cue::Sfx { id: 0 });
                }
            }
        }
        true
    }

    fn arg_at(&self, w: &World, k: usize) -> u16 {
        le16(w.d, self.pc + k)
    }

    fn byte_at(&self, w: &World, k: usize) -> u8 {
        w.d.get(self.pc + k).copied().unwrap_or(0)
    }

    /// `SKIP(start, term)`: step by instruction length to `term` at depth 0.
    fn skip(w: &World, start: usize, term: u8) -> usize {
        let (mut p, mut depth) = (start, 0i32);
        let mut guard = 0;
        while p < w.d.len() && (w.d[p] != term || depth != 0) {
            match w.d[p] {
                0x0D => depth += 1,
                0x0F => depth -= 1,
                _ => {}
            }
            p += if w.d[p] < 0x80 { tier2::op_info(w.d[p]).map(|o| o.len as usize).unwrap_or(1).max(1) } else { 2 };
            guard += 1;
            if guard > 65536 {
                break;
            }
        }
        p
    }

    /// One opcode. 0 = run the next one this frame; 1 = yield; −1 / 0x26 = re-run next frame.
    fn interpret(&mut self, w: &mut World, strings: &mut Strings, pad: &Pad) -> i32 {
        let op = self.byte_at(w, 0);
        let arg = self.arg_at(w, 1);
        let len = tier2::op_info(op).map(|o| o.len as usize).unwrap_or(0);
        if len == 0 {
            self.end(w.frame, Ending::Hang { at: self.pc as u32 });
            self.f29 = 7;
            return 1;
        }
        let me = self.actor;
        if self.logged != Some(self.pc) && !matches!(op, 0x0D | 0x0E | 0x0F | 0x09 | 0x1A | 0x35) {
            self.logged = Some(self.pc);
            let b1 = self.byte_at(w, 1) as usize;
            match op {
                0x26 | 0x48 | 0x36 | 0x37 => self.refs.push(b1),
                0x24 => self.refs.push(b1),
                0x25 => {
                    self.refs.push(b1);
                    self.refs.push(self.byte_at(w, 2) as usize);
                }
                0x23 => self.refs.extend(match arg {
                    3 | 9 => &[10usize][..],
                    6 => &[2][..],
                    0 => &[22, 3][..],
                    _ => &[][..],
                }),
                // Clearing a flag that is not set: the scene expected it set.
                0x05 if !w.flags.get(arg as u32) => self.cleared.push(arg),
                _ => {}
            }
            let note = self.note(w, op, arg);
            let name = tier2::op_info(op).map(|o| o.name).unwrap_or("?");
            self.events.push(Event { frame: w.frame, at: self.pc as u32, op, name, note });
        }
        // (adv, how): how = None → pc += len; Some(pc) → jump and continue.
        let mut adv = 0;
        let mut jump: Option<usize> = None;
        match op {
            0x00 => {
                self.log(w, op, String::new());
                self.f29 = 7;
                adv = 1;
            }
            0x01..=0x03 => {
                if self.f24 & 0x8000 != 0 {
                    self.text = None;
                    self.f24 &= !0x8000;
                    return 1;
                }
                let at = arg as usize;
                self.text = Some(TextPtr { src: 0, at });
                self.tt.arg = op;
                self.f24 |= 0xA000;
                adv = 1;
                let line = plain_line(crate::text::c_string(w.d, at));
                self.log(w, op, line);
            }
            0x04 => {
                if arg == 0xFD {
                    w.flags.clear_scratch();
                } else {
                    w.flags.set(arg as u32);
                }
                self.log(w, op, format!("0x{arg:03X}"));
            }
            0x05 => {
                w.flags.clear(arg as u32);
                self.log(w, op, format!("0x{arg:03X}"));
            }
            0x06 => {
                self.log(w, op, format!("rest, then chunk {arg}"));
                self.f24 |= 0x2000;
                self.end(w.frame, Ending::Inn);
                self.dies_at.get_or_insert(w.frame + 1);
                adv = 1;
            }
            0x07 => {
                self.log(w, op, format!("shop {arg}"));
                w.actors[KNIGHT].flags &= !fl::TALK;
                adv = 1;
            }
            0x08 => {
                if w.actors[KNIGHT].flags & fl::TALK != 0 {
                    if self.counter >= 4 {
                        self.log(w, op, format!("battle {arg}"));
                        self.end(w.frame, Ending::Battle { id: arg });
                        self.dies_at.get_or_insert(w.frame + 1);
                    }
                    self.counter += 1;
                } else {
                    w.actors[KNIGHT].flags |= fl::TALK;
                }
                return -1;
            }
            0x0A | 0x3A | 0x3B => {
                if self.f24 & 0x4000 == 0 {
                    let entries: Vec<String> = if op == 0x0A {
                        vec!["Yes".into(), "No".into()]
                    } else {
                        crate::text::c_string(w.d, arg as usize).split(|&b| b == 7).take(3).map(plain_line).collect()
                    };
                    // Unanswered: the first entry, or the last ("No", "Leave") when the
                    // same prompt comes round again, so loops like a refill offer end.
                    let again = self.asked.contains(&self.pc);
                    self.asked.push(self.pc);
                    let fallback = if again { entries.len() as u8 } else { 1 };
                    let answer = pad.answers.get(self.answers_used).copied().unwrap_or(fallback).clamp(1, entries.len().max(1) as u8);
                    self.answers_used += 1;
                    self.choices.push(ChoiceRec { frame: w.frame, entries: entries.clone(), answer });
                    self.log(w, op, entries.join(" / "));
                    self.children.push(Child::Choice { entries, cursor: 0, tick: 0, answer });
                    self.f24 |= 0x4000;
                    return 1;
                }
                if self.f4 != 0 {
                    let mut pc = self.pc + if op == 0x0A { 1 } else { 3 };
                    if op != 0x3B {
                        if self.f4 == 2 {
                            pc = Self::skip(w, pc + 1, 0x0E) + 1;
                        }
                    } else {
                        pc += (self.f4 as usize - 1) * 2;
                    }
                    self.f24 &= !0x4000;
                    self.pc = pc;
                    return 0;
                }
                return 1;
            }
            0x0B | 0x1D | 0x1E | 0x0C => {
                let holds = match op {
                    0x0B => w.flags.get(arg as u32),
                    0x0C => true,
                    _ => false,
                };
                let pc = self.pc + len;
                jump = Some(if holds { pc } else { Self::skip(w, pc + 1, 0x0E) + 1 });
            }
            0x0E => {
                // End of a taken branch: on to after the matching ENDIF.
                let q = Self::skip(w, self.pc, 0x0F);
                jump = Some(q + 1);
            }
            0x2D => {
                let pc = self.pc + 3;
                jump = Some(if w.flags.get(arg as u32) { pc } else { Self::skip(w, pc + 1, 0x2E) + 1 });
            }
            0x2E | 0x2F => {
                let to = arg as usize;
                if to <= self.pc {
                    let story = w.flags.list();
                    let places: Vec<[i16; 3]> = w.actors.iter().map(|a| [a.x, a.h, a.z]).collect();
                    match self.loops.iter_mut().find(|l| l.0 == to) {
                        Some(l) if l.1 == story && l.2 == places => {
                            self.log(w, op, format!("back to D+{to:04X} again: stops here"));
                            self.end(w.frame, Ending::Loop { at: to as u32 });
                            self.dies_at.get_or_insert(w.frame + 1);
                            return 1;
                        }
                        Some(l) => (l.1, l.2) = (story, places),
                        None => self.loops.push((to, story, places)),
                    }
                }
                jump = Some(to);
            }
            0x10 => w.actors[me].anim = arg as u8,
            0x11 => {
                let (k, a) = (w.actors[KNIGHT].clone(), w.actors[me].clone());
                w.actors[me].target = ((w.t.ratan2(k.x as i32 - a.x as i32, a.z as i32 - k.z as i32) + 0x400) & 0xE00) as i16;
            }
            0x12 => {
                w.cue(Cue::Sfx { id: arg });
            }
            0x13 => {
                let (chunk, x, z, h) = (arg, self.arg_at(w, 3) as i16, self.arg_at(w, 5) as i16, self.arg_at(w, 7) as i16);
                if w.speaker_camera(chunk, x, h, z, true) {
                    return -1;
                }
                adv = 1;
            }
            0x45 => {
                let (chunk, x, z, h) = (arg, self.arg_at(w, 3) as i16, self.arg_at(w, 5) as i16, self.arg_at(w, 7) as i16);
                w.speaker_camera(chunk, x, h, z, false);
            }
            0x14 => {
                let goal = [arg as i16, self.arg_at(w, 3) as i16, self.arg_at(w, 5) as i16];
                let c = &mut w.cam;
                let mut done = true;
                for (cur, g) in [&mut c.rx, &mut c.ry, &mut c.rz].into_iter().zip(goal) {
                    *cur = (*cur as i32 & 0xFFF) as i16;
                    let mut d = (g as i32 - *cur as i32) & 0xFFF;
                    if d > 0x800 {
                        d -= 0x1000;
                    }
                    let target = (*cur as i32 + d) as i16;
                    *cur = ease_out(*cur, target, 0x20);
                    if *cur != target {
                        done = false;
                    }
                }
                if !done {
                    return -1;
                }
                adv = 1;
            }
            0x15 | 0x26 => {
                let who = if op == 0x15 {
                    self.track = 0;
                    w.actors[KNIGHT].id = w.chunk as i16;
                    0
                } else {
                    self.track = self.byte_at(w, 1) as i16;
                    self.track as usize
                };
                let a = w.actors.get(who).cloned().unwrap_or_default();
                if w.speaker_camera(a.id as u16, a.x, a.h, a.z, true) {
                    return 0x26;
                }
                adv = 1;
            }
            0x16 => self.log(w, op, format!("item {arg}")),
            0x17 => {
                self.log(w, op, format!("item {arg}"));
                adv = 1;
            }
            0x18 => {
                self.children.push(Child::KnightAway { actor: me, phase: 0, tick: 0, f4: 0, turn: None });
                self.f24 |= 0x2000;
                adv = 1;
            }
            0x19 => {
                let (facing, chunk, x, z, flags) = (arg, self.arg_at(w, 3), self.arg_at(w, 5) as i16, self.arg_at(w, 7) as i16, self.arg_at(w, 9));
                self.log(w, op, format!("chunk {chunk} ({x}, {z})"));
                w.de &= !0x80;
                if flags & 0x8000 != 0 {
                    w.cue(Cue::Sfx { id: 0xE });
                }
                self.end(w.frame, Ending::Warp { chunk, x, z, facing });
                self.dies_at.get_or_insert(w.frame + 3);
                adv = 1;
            }
            0x1B => w.actors[me].flags ^= fl::TALK,
            0x1C | 0x34 => {
                if op == 0x34 {
                    w.de |= 0x400;
                    self.f24 |= 0x10;
                }
                self.f24 |= 0x2000;
                let side = (w.de & 0x400 != 0) as usize;
                self.children.push(Child::Walk { side, arg: arg as usize, phase: 0, tick: 0 });
                adv = 1;
            }
            0x1F => {
                self.speaker = Some(arg as usize);
            }
            0x20 => self.speaker = None,
            0x21 => {
                self.log(w, op, format!("actor {}", arg & 0xFF));
                w.actors[KNIGHT].script = 0;
                adv = 1;
            }
            0x22 => {
                let id = arg & 0xFF;
                self.log(w, op, match id {
                    0x39 => "bgm/57 (slot 1)".into(),
                    0xFF => "bgm/0 (slot 1)".into(),
                    n => format!("bgm/{n}"),
                });
                w.cue(Cue::BgmLoad { file: id });
                self.f24 |= 0x2000;
                self.children.push(Child::Bgm { tick: 0 });
                adv = 1;
            }
            0x23 => {
                self.log(w, op, format!("task {arg}"));
                self.f24 |= 0x2000;
                self.children.push(Child::Cutscene { n: arg, tick: 0, phase: 0, a: 0, b: 0 });
                adv = 1;
            }
            0x24 => {
                let (a, n) = (self.byte_at(w, 1) as usize, self.byte_at(w, 2));
                if let Some(o) = w.actors.get_mut(a) {
                    o.base_anim = n;
                }
            }
            0x25 => {
                let (ai, bi) = (self.byte_at(w, 1) as usize, self.byte_at(w, 2) as usize);
                if ai < w.actors.len() && bi < w.actors.len() {
                    let (a, b) = (w.actors[ai].clone(), w.actors[bi].clone());
                    w.actors[ai].base = ((w.t.ratan2(b.x as i32 - a.x as i32, a.z as i32 - b.z as i32) + 0x400) & 0xE00) as i16;
                }
            }
            0x27 => {
                let n = self.byte_at(w, 1) as i16;
                let c = self.counter;
                self.counter += 1;
                if c < n {
                    return -1;
                }
                self.counter = 0;
            }
            0x28 => return self.chest(w, strings, arg),
            0x29 => {
                if w.fade >= 0x20 {
                    w.fade = 0;
                }
                if w.fade < 0x1F {
                    w.fade += 1;
                    return -1;
                }
            }
            0x2A => {
                if w.fade >= 0x20 {
                    w.fade = 0x1F;
                }
                if w.fade != 0 {
                    w.fade -= 1;
                    return -1;
                }
            }
            0x2B => {
                if w.fade < 0x1F {
                    w.fade = 0x20;
                }
                if w.fade < 0x3F {
                    w.fade += 1;
                    return -1;
                }
            }
            0x2C => {
                if w.fade < 0x1F {
                    w.fade = 0x3F;
                }
                if w.fade > 0x20 {
                    w.fade -= 1;
                    return -1;
                }
            }
            0x30 => {
                self.shake = [self.byte_at(w, 1) as i16, self.byte_at(w, 2) as i16, self.byte_at(w, 3) as i16];
                if self.shake[2] != 0 {
                    let c = self.counter;
                    self.counter += 1;
                    if c < self.shake[2] {
                        return -1;
                    }
                    w.cam.shake = [0, 0];
                    self.shake = [0; 3];
                    self.counter = 0;
                }
            }
            0x31 => {
                self.log(w, op, format!("movie {}", self.byte_at(w, 1)));
                adv = 1;
            }
            0x33 => self.log(w, op, format!("{}", le16(w.d, self.pc + 1) as i32 | (le16(w.d, self.pc + 3) as i32) << 16)),
            0x38 => w.actors[me].kind |= 0x40,
            0x39 => w.actors[me].x = 10000,
            0x3C => adv = 1,
            0x41 => {
                if w.cam.f10 != arg as i16 {
                    w.actors[KNIGHT].flags |= fl::TALK;
                    w.cam.f10 = ease_out(w.cam.f10, arg as i16, 0x10);
                    return -1;
                }
            }
            0x42 => {
                let a = self.byte_at(w, 1) as usize;
                self.helpers.push(a);
                self.children.push(Child::Helper { actor: a, dying: false, tick: 0 });
            }
            0x43 => {
                for c in &mut self.children {
                    if let Child::Helper { actor, dying, tick } = c {
                        if arg == 0xFFFF || *actor == arg as usize {
                            *dying = true;
                            *tick = 0;
                        }
                    }
                }
            }
            0x44 => {
                let line = plain_line(crate::text::c_string(w.d, arg as usize));
                self.log(w, op, line.clone());
                self.objective = Some(line);
            }
            0x46 => {
                w.cam.rx = arg as i16;
                w.cam.ry = self.arg_at(w, 3) as i16;
                w.cam.rz = self.arg_at(w, 5) as i16;
            }
            0x47 => w.cam.h = arg as i32,
            0x48 => {
                w.focus = self.byte_at(w, 1);
                self.track = w.focus as i16;
            }
            0x49 => {
                if self.counter < 0x100 {
                    self.counter += 8;
                    return -1;
                }
                w.cue(Cue::BgmFade { id: arg });
                w.cue(Cue::Sfx { id: 0 });
            }
            0x36 | 0x37 => {
                self.f24 |= 0x2000;
                self.children.push(Child::Turn { actor: self.byte_at(w, 1) as usize, about: op == 0x37, tick: 0 });
                adv = 1;
            }
            // 09, 0D, 0F, 1A, 32, 35, 3D–40: nothing the scene shows. 4A (HackerGC
            // EXT) runs code on the console only.
            _ => {}
        }
        match jump {
            Some(p) => self.pc = p,
            None => self.pc += len,
        }
        adv
    }

    /// CHEST (0x28): the state machine of `ovr02.c:2381-2460`, the player's
    /// inventory always having room.
    fn chest(&mut self, w: &mut World, strings: &mut Strings, flag: u16) -> i32 {
        let item = self.arg_at(w, 3) as i16;
        let msg = |s: &mut Strings, boxn: u8, body: &[u8]| {
            let mut v = vec![5, boxn, 0];
            v.extend_from_slice(body);
            s.push(v)
        };
        let len = 5;
        if w.flags.get(flag as u32) {
            self.f24 |= 0xA000;
            self.text = Some(msg(strings, 0, b"An empty treasure chest."));
            self.log(w, 0x28, "empty".into());
            self.pc += len;
            return 1;
        }
        match self.counter {
            0 => {
                if w.actors[self.actor].kind & 0xF == 3 {
                    self.f24 |= 0x2040;
                    self.children.push(Child::ChestFx { actor: self.actor, tick: 0 });
                    self.counter = 1;
                } else {
                    self.counter = 2;
                }
                -1
            }
            1 | 2 => {
                if item == 3000 {
                    self.counter = 3;
                    w.actors[KNIGHT].flags |= fl::TALK;
                    return -1;
                }
                let body = if item < 1000 { format!("You get an item:\x07#{item:03}.") } else { format!("You get a Living Toy:\x07~{:02}.", item - 1000) };
                self.text = Some(msg(strings, 5, body.as_bytes()));
                self.f24 |= 0xA000;
                w.cue(Cue::Sfx { id: 4 });
                self.counter = 0;
                if flag != 0xF8 {
                    w.flags.set(flag as u32);
                }
                self.log(w, 0x28, format!("item {item}"));
                self.pc += len;
                1
            }
            3 | 4 => {
                self.counter += 1;
                -1
            }
            _ => {
                self.end(w.frame, Ending::Battle { id: 0x125 });
                self.dies_at.get_or_insert(w.frame + 1);
                self.counter = 0;
                -1
            }
        }
    }

    pub fn baby_slot() -> usize {
        BABY
    }
}
