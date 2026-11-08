//! Walk paths for the World editor (LT-79): where an actor's walk program
//! takes it from a placing branch, simulated frame by frame with this
//! module's sibling port of `WalkActor_RunScript` ([`World::run_script`]),
//! the other actors of the chunk standing or walking as the story flags place
//! them. NPCs are blocked only by the Knight and the Baby, who are parked off
//! the chunk, so the path is the walk as written.
//!
//! The path is the actor's spot, then each point where it bends, with marks
//! where the program does something you can see on the map: heads off in a
//! new direction (`1nnn`), walks (`2nnn`), waits on a flag (`0nnn`), leaves the
//! map (`7nnn`), re-places (`Bnnn`), stops for good (`0000`) or goes back to
//! the start of the walk (`4000`). The simulation ends at the first of those
//! that ends the walk, after one full loop, or after `max_frames`.

use serde::Serialize;

use crate::npc::NpcPack;

use super::math::Tables;
use super::world::{fl, Actor, World};

/// What the walk does at a waypoint.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Mark {
    /// `1nnn`: heads off toward `dir` (0–4095).
    Turn { dir: u16 },
    /// `2nnn`: walks `steps` steps.
    Walk { steps: u16 },
    /// `0nnn` with the flag clear: waits here until it is set (the path ends).
    Wait { flag: u16 },
    /// `7nnn` with the flag set: leaves the map here (the path ends).
    Hide { flag: u16 },
    /// `Bnnn`: once the flag is set it is placed again; either way the walk restarts.
    Replace { flag: u16 },
    /// `0000`: stops here for good.
    Stop,
    /// `4000`: back to the start of the walk.
    Loop,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Waypoint {
    pub x: i16,
    pub h: i16,
    pub z: i16,
    /// Frame (30 per second) it happens on.
    pub frame: u32,
    #[serde(flatten)]
    pub mark: Mark,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalkPath {
    /// The spot, then each point where the path bends, and where it ends (x, h, z).
    pub points: Vec<[i16; 3]>,
    pub waypoints: Vec<Waypoint>,
    /// The walk came back to its start: it repeats from the spot.
    pub loops: bool,
    /// Frames simulated.
    pub frames: u32,
    /// Farthest it gets from its spot (units).
    pub reach: i32,
}

/// A placing branch to walk from: the spot and the D offset of its walk program
/// (the word after `x h z dir`).
#[derive(Debug, Clone, Copy)]
pub struct Start {
    pub x: i16,
    pub h: i16,
    pub z: i16,
    pub dir: u16,
    pub walk: u32,
}

/// Kind-4/5 records are torch flames and "objects" the field keeps apart.
fn is_object(kind: u8) -> bool {
    kind == 4 || kind == 5
}

/// The walk of record `slot` of `pack` from `start` on `chunk` (its heightmap
/// `heights`), with `flags` set, for up to `max_frames` frames.
pub fn walk_path(tables: &Tables, pack: &NpcPack, pack_bytes: &[u8], chunk: u16, heights: Vec<i16>, flags: &[u16], slot: u32, start: Start, max_frames: u32) -> Option<WalkPath> {
    let d = pack.d_block(pack_bytes);
    let mut w = World::new(tables, d, heights, chunk);
    for &f in flags {
        w.flags.set(f as u32);
    }
    // Spawn as the field does (`func_801E8E3C`): actor slots skip kind-4/5 records.
    let mut me = None;
    for (ri, r) in pack.actors.iter().enumerate() {
        let kind = (r.d >> 8) as u8;
        if ri >= 2 && is_object(kind) {
            continue;
        }
        if ri as u32 == slot {
            me = Some(w.actors.len());
        }
        w.actors.push(Actor {
            id: (r.b & 0xFFFF) as u16 as i16,
            model: (r.b >> 16) as u8,
            set: (r.b >> 24) as u8,
            kind,
            size: r.d as u8,
            script: r.script,
            ladder: r.placement,
            flags: fl::VISIBLE,
            ..Default::default()
        });
    }
    let me = me.filter(|&i| i >= 2)?;
    // An NPC is blocked only by the Knight and the Baby (`CheckNpcAtPosition`), who are wherever the
    // player is: park them far off the chunk, so the path is the walk itself.
    for i in 0..2.min(w.actors.len()) {
        let a = &mut w.actors[i];
        (a.x, a.z) = (0x7000, 0x7000);
    }
    for i in 2..w.actors.len() {
        if i == me {
            continue;
        }
        w.select_script(i, -1);
        w.actors[i].state = 0;
    }
    // This actor stands on the branch asked for, whatever the flags pick.
    {
        let a = &mut w.actors[me];
        (a.x, a.h, a.z) = (start.x, start.h, start.z);
        let dir = start.dir as i16;
        (a.facing, a.target, a.base) = (dir, dir, dir);
        a.wp = start.walk as usize;
        a.pc = 0;
        a.steps = 0;
        a.state = 0;
        a.flags = fl::VISIBLE;
        a.id = chunk as i16;
    }
    let word = |w: &World, pc: usize| w.word(start.walk as usize + pc * 2);
    let here = |w: &World| {
        let a = &w.actors[me];
        [a.x, a.h, a.z]
    };
    let mut path = WalkPath { points: vec![here(&w)], waypoints: Vec::new(), loops: false, frames: 0, reach: 0 };
    let mark = |path: &mut WalkPath, p: [i16; 3], frame: u32, mark: Mark| path.waypoints.push(Waypoint { x: p[0], h: p[1], z: p[2], frame, mark });
    // What the walk does on entering word `pc` at `p` (marks for words that do something visible).
    let enter = |w: &World, path: &mut WalkPath, pc: usize, frame: u32, p: [i16; 3]| -> bool {
        let op = word(w, pc);
        let arg = op & 0x0FFF;
        match op & 0xF000 {
            0x1000 => mark(path, p, frame, Mark::Turn { dir: arg << 9 }),
            0x2000 => mark(path, p, frame, Mark::Walk { steps: arg }),
            0x0000 if arg == 0 => {
                mark(path, p, frame, Mark::Stop);
                return true;
            }
            0x0000 if !w.flags.get(arg as u32) => {
                mark(path, p, frame, Mark::Wait { flag: arg });
                return true;
            }
            0x7000 if w.flags.get(arg as u32) || arg == 0 => {
                mark(path, p, frame, Mark::Hide { flag: arg });
                return true;
            }
            0xB000 => {
                mark(path, p, frame, Mark::Replace { flag: arg });
                path.loops = true;
                return true;
            }
            0x4000 => {
                mark(path, p, frame, Mark::Loop);
                path.loops = true;
                return true;
            }
            _ => {}
        }
        false
    };
    if enter(&w, &mut path, 0, 0, here(&w)) {
        return Some(path);
    }
    let mut last_step: (i32, i32) = (0, 0);
    let mut still = 0u32;
    let mut end = None;
    for frame in 1..=max_frames {
        path.frames = frame;
        let before = (w.actors[me].pc as usize, here(&w));
        for i in 2..w.actors.len() {
            if w.actors[i].state == 0 && w.actors[i].id as u16 == chunk && w.actors[i].flags & fl::VISIBLE != 0 {
                w.run_script(i, i);
            }
        }
        let now = here(&w);
        let step = ((now[0] - before.1[0]) as i32, (now[2] - before.1[2]) as i32);
        if step != (0, 0) {
            // A bend: the step changed direction (the start of a new straight run).
            if last_step != (0, 0) && step != last_step && path.points.last() != Some(&before.1) {
                path.points.push(before.1);
            }
            last_step = step;
            still = 0;
            let (dx, dz) = ((now[0] - path.points[0][0]) as i32, (now[2] - path.points[0][2]) as i32);
            path.reach = path.reach.max(((dx * dx + dz * dz) as f64).sqrt() as i32);
        } else {
            still += 1;
        }
        // Words entered this frame: the ones after `before` up to the current one, all before the
        // frame's one step (a `2nnn` moves once and ends the frame), so at `before`'s spot. A walk
        // that wrapped to word 0 did so at a loop (`4000`) or a re-place (`Bnnn`), which ends the
        // path there (the step after it is the next lap's).
        let after = w.actors[me].pc as usize;
        if after != before.0 {
            let ended = if after < before.0 {
                let wraps = |pc: usize| matches!(word(&w, pc) & 0xF000, 0x4000 | 0xB000);
                let at = (before.0..before.0 + 64).find(|&pc| wraps(pc)).unwrap_or(before.0);
                enter(&w, &mut path, at, frame, before.1);
                true
            } else {
                (before.0 + 1..=after).any(|pc| enter(&w, &mut path, pc, frame, before.1))
            };
            if ended {
                end = Some(before.1);
                break;
            }
        }
        // Turning in place or blocked for a long while: nothing more to see.
        if still > 600 {
            break;
        }
    }
    let end = end.unwrap_or_else(|| here(&w));
    if path.points.last() != Some(&end) {
        path.points.push(end);
    }
    Some(path)
}
