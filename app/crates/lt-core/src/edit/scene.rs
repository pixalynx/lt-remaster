//! Brand-new field cutscenes (screen 20, PRD 20 §5.7): the three pieces a
//! retail walk-in scene is made of, added to the NPC pack in one layout.
//!
//! - A *director*: an added actor ([`super::actor`]) that is hidden (no
//!   model, size and kind 0, like the directors of nine retail scenes), homed
//!   on the scene's chunk at the trigger spot, with a talk program of its own
//!   (the scene).
//! - A type-10 trigger record at the spot that wakes it
//!   (`ScanFieldEventTriggers`: within 0x20 on both axes, while no script
//!   runs). The record names the director by its *character* slot, which
//!   skips object records (types 4/5), not by its record index.
//! - Optionally a *cue*: the record then sits in a rung of its own at the top
//!   of the chunk's ladder ([`super::door`]), so the scene plays only while
//!   that flag is set. The scene clears it at the end, the way retail scenes
//!   clear theirs; something else (another script) sets it. Without a cue the
//!   record holds in every story state, and the scene plays every time the
//!   Knight steps there.

use serde::Serialize;

use crate::npc::{ActorRecord, NpcPack};
use crate::trigger::TriggerRecord;

use super::actor::{self, AddedActor};
use super::door::{self, Added};
use super::script::{self, Node};
use super::{EditError, Result};

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

/// Where and when a new scene starts.
#[derive(Debug, Clone, Copy)]
pub struct NewScene {
    pub chunk: u16,
    /// The trigger spot; the director stands there too (it blocks nothing).
    pub x: i16,
    pub h: i16,
    pub z: i16,
    /// The flag the scene waits for; 0 = every time the Knight steps there.
    pub cue: u16,
}

/// A scene the project added: its director and trigger.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScene {
    /// The director's record (what screen 20 and screen 11 call the actor slot).
    pub slot: u32,
    /// Its character slot (what the trigger record names).
    pub actor: u16,
    pub chunk: u16,
    pub x: i16,
    pub z: i16,
    pub cue: u16,
}

/// Character structure index of `record`: party and actors count, objects don't.
pub fn char_slot(actors: &[ActorRecord], record: usize) -> Option<u16> {
    if record >= actors.len() {
        return None;
    }
    Some(actors[..record].iter().enumerate().filter(|(i, a)| *i < 2 || !actor::is_object(a.d)).count() as u16)
}

/// The record a character slot belongs to (the inverse of [`char_slot`]).
pub fn record_of(actors: &[ActorRecord], char: u16) -> Option<usize> {
    actors.iter().enumerate().filter(|(i, a)| *i < 2 || !actor::is_object(a.d)).map(|(i, _)| i).nth(char as usize)
}

/// A retail actor a director can copy: not the party, not an object, with a
/// program (a hidden retail director first).
fn director_source(r: &NpcPack) -> Option<u32> {
    let ok = |a: &&ActorRecord| a.slot >= 2 && !actor::is_object(a.d) && a.script != 0;
    let hidden = |a: &&ActorRecord| a.b >> 16 == 0xFFFF && a.d & 0xFFFF == 0;
    r.actors.iter().filter(ok).find(hidden).or_else(|| r.actors.iter().find(ok)).map(|a| a.slot)
}

/// The talk record of a scene trigger.
fn talk(x: i16, z: i16, actor: u16) -> TriggerRecord {
    TriggerRecord::Talk { at: 0, x, z, actor }
}

/// The scenes `current` adds: hidden added actors with their wake-up records.
pub fn list(retail: &[u8], current: &[u8]) -> Result<Vec<ProjectScene>> {
    let base = super::character::base(retail, current)?;
    let current: &[u8] = &base;
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let added = actor::recover(retail, current)?;
    let doors = door::recover(retail, current)?;
    let mut out = Vec::new();
    for (k, a) in added.iter().enumerate().filter(|(_, a)| a.hidden) {
        let slot = r.actors.len() + k;
        let Some(ch) = char_slot(&c.actors, slot) else { continue };
        for d in &doors {
            if let TriggerRecord::Talk { x, z, actor, .. } = d.record {
                if actor == ch && d.chunk == a.home as i16 {
                    out.push(ProjectScene { slot: slot as u32, actor: ch, chunk: a.home, x, z, cue: d.cue });
                }
            }
        }
    }
    Ok(out)
}

/// Lays out `set`, then gives actor `slot` the program `tree` ([`script::check_tree`]).
fn with_program(retail: &[u8], current: &[u8], set: &super::pack::Edits, slot: u32, tree: &[Node], areas: &[u32]) -> Result<Vec<u8>> {
    let (laid, _, _) = super::text::layout(retail, current, set)?;
    let check = script::check_tree(retail, &laid, slot, Some(tree), areas)?;
    match check.bytes {
        Some(b) => Ok(b),
        None => {
            let first = check.diagnostics.iter().find(|d| d.severity == crate::tier2::dsl::Severity::Error);
            Err(EditError::new("SCENE_SCRIPT", first.map_or_else(|| "The scene's program doesn't assemble.".to_string(), |d| d.message.clone())))
        }
    }
}

/// `current` with a new scene: a hidden director at the spot running `tree`,
/// and the record that wakes it. `areas` are the project's new areas, which
/// the program's chunk operands may name. Returns the pack and the
/// director's slot.
pub fn create(retail: &[u8], current: &[u8], s: &NewScene, tree: &[Node], areas: &[u32]) -> Result<(Vec<u8>, u32)> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let from = director_source(&r).ok_or_else(|| EditError::new("SCENE_NO_SOURCE", "This pack has no actor a director can copy."))?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    set.actors.push(AddedActor { from, home: s.chunk, x: s.x, h: s.h, z: s.z, dir: 0, hidden: true, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false });
    let slot = (r.actors.len() + set.actors.len() - 1) as u32;
    // Its character slot: it follows every character the project's pack has.
    let ch = c.actors.iter().enumerate().filter(|(i, a)| *i < 2 || !actor::is_object(a.d)).count() as u16;
    set.doors.push(Added { chunk: s.chunk as i16, record: talk(s.x, s.z, ch), cue: s.cue });
    set.doors = door::canonical(retail, &set.doors)?;
    Ok((with_program(retail, current, &set, slot, tree, areas)?, slot))
}

/// Moves scene `slot`'s trigger (and its director) to (`x`, `z`) and sets its cue.
pub fn retrigger(retail: &[u8], current: &[u8], slot: u32, x: i16, h: i16, z: i16, cue: u16) -> Result<Vec<u8>> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    let k = (slot as usize).checked_sub(r.actors.len()).filter(|&k| k < set.actors.len()).ok_or_else(|| not_ours(slot))?;
    let ch = char_slot(&c.actors, slot as usize).ok_or_else(|| not_ours(slot))?;
    let a = &mut set.actors[k];
    if !a.hidden {
        return Err(not_ours(slot));
    }
    (a.x, a.h, a.z) = (x, h, z);
    let chunk = a.home as i16;
    set.doors.retain(|d| !matches!(d.record, TriggerRecord::Talk { actor, .. } if actor == ch && d.chunk == chunk));
    set.doors.push(Added { chunk, record: talk(x, z, ch), cue });
    set.doors = door::canonical(retail, &set.doors)?;
    Ok(super::text::layout(retail, current, &set)?.0)
}

/// `current` without scene `slot`: its director and the records that wake
/// it. Later added actors move down a slot; their wake-up records follow.
pub fn delete(retail: &[u8], current: &[u8], slot: u32) -> Result<Vec<u8>> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    let k = (slot as usize).checked_sub(r.actors.len()).filter(|&k| k < set.actors.len()).ok_or_else(|| not_ours(slot))?;
    if !set.actors[k].hidden {
        return Err(not_ours(slot));
    }
    let ch = char_slot(&c.actors, slot as usize).ok_or_else(|| not_ours(slot))?;
    set.actors.remove(k);
    set.doors.retain(|d| !matches!(d.record, TriggerRecord::Talk { actor, .. } if actor == ch));
    for d in set.doors.iter_mut() {
        if let TriggerRecord::Talk { actor, .. } = &mut d.record {
            if *actor > ch && record_of(&c.actors, *actor).is_some_and(|rec| rec >= r.actors.len()) {
                *actor -= 1;
            }
        }
    }
    Ok(super::text::layout(retail, current, &set)?.0)
}

fn not_ours(slot: u32) -> EditError {
    EditError::new("SCENE_NOT_ADDED", format!("Actor {slot} isn't a scene the project added."))
}
