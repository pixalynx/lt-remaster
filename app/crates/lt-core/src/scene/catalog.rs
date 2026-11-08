//! What a pack can play: every actor a Tier-1 type-10 record wakes (a
//! scene), and every other actor whose talk script says something (a
//! conversation, started by talking to it).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::edit::remove::placement_removed;
use crate::edit::trigger::is_off;
use crate::npc::NpcPack;
use crate::text;
use crate::tier2::{self, Operand};
use crate::trigger::TriggerRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneKind {
    /// Started by walking into a type-10 record.
    Trigger,
    /// Started by talking to the actor.
    Talk,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneEntry {
    pub kind: SceneKind,
    /// Actor slot whose script runs.
    pub slot: u32,
    /// Chunk it plays on: the record's group, or the actor's home chunk.
    pub chunk: u16,
    /// Trigger: the rung's flag (0 = always) and the record's spot.
    pub flag: u16,
    pub x: i16,
    pub z: i16,
    /// D-relative script entry.
    pub script: u32,
    /// First SPEAKER name.
    pub speaker: Option<String>,
    /// The first line said, plain.
    pub line: Option<String>,
    /// SAY-family instructions over every program reached (GOTOs included).
    pub lines: u32,
    /// Camera, walk, wait, fade, BGM and warp/battle use, for the list's chips.
    pub tags: Vec<&'static str>,
    /// A scene the project added (`edit::scene`); set by the caller, which has retail.
    pub added: bool,
    /// Taken out of the game (the blank slate, or removed by hand): its record is off the map or
    /// its actor is never placed, so the game can't start it.
    pub removed: bool,
}

/// Every scene and conversation of a pack, scenes first.
pub fn catalog(pack: &NpcPack, b: &[u8]) -> Vec<SceneEntry> {
    let d = pack.d_block(b);
    let mut woken: BTreeMap<u32, (u16, u16, i16, i16)> = BTreeMap::new();
    for g in pack.triggers(b) {
        if g.chunk < 0 {
            continue;
        }
        // A record in every rung of the ladder holds in every story state (flag 0).
        let in_every = |actor: u16| g.rungs.iter().all(|r| r.records.iter().any(|rec| matches!(rec, TriggerRecord::Talk { actor: a, .. } if *a == actor)));
        for r in &g.rungs {
            for rec in &r.records {
                // The record names a character slot (objects take none), not a record.
                if let TriggerRecord::Talk { actor, x, z, .. } = rec {
                    if let Some(record) = crate::edit::scene::record_of(&pack.actors, *actor) {
                        let flag = if in_every(*actor) { 0 } else { r.flag };
                        let this = (g.chunk as u16, flag, *x, *z);
                        // One record of several still on the map keeps the scene alive.
                        let kept = woken.entry(record as u32).or_insert(this);
                        if is_off(kept.2, kept.3) && !is_off(*x, *z) {
                            *kept = this;
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for a in &pack.actors {
        let trigger = woken.get(&a.slot).copied();
        // Records of kind 4/5 (torch flames, objects) are not actors: nothing runs their scripts.
        if a.script == 0 || a.script as usize >= d.len() || matches!(crate::npc::ActorLook::of(a).kind & 0xF, 4 | 5) {
            continue;
        }
        let s = summary(d, a.script);
        let home = (a.b & 0xFFFF) as u16;
        let removed = match trigger {
            Some((_, _, x, z)) => is_off(x, z),
            None => placement_removed(d, a.placement, 0),
        };
        let (kind, chunk, flag, x, z) = match trigger {
            Some((chunk, flag, x, z)) => (SceneKind::Trigger, chunk, flag, x, z),
            // Conversations: actors homed on a chunk that say something (not the party).
            None if a.slot >= 2 && s.lines > 0 && crate::world::valid_chunk(home as u32) => (SceneKind::Talk, home, 0, 0, 0),
            None => continue,
        };
        out.push(SceneEntry { kind, slot: a.slot, chunk, flag, x, z, script: a.script, speaker: s.speaker, line: s.line, lines: s.lines, tags: s.tags, added: false, removed });
    }
    out.sort_by_key(|e| (e.kind == SceneKind::Talk, e.chunk, e.slot));
    out
}

struct Summary {
    speaker: Option<String>,
    line: Option<String>,
    lines: u32,
    tags: Vec<&'static str>,
}

fn summary(d: &[u8], entry: u32) -> Summary {
    let mut s = Summary { speaker: None, line: None, lines: 0, tags: Vec::new() };
    let mut seen = BTreeSet::new();
    let mut queue = vec![entry];
    let mut ops = BTreeSet::new();
    while let Some(e) = queue.pop() {
        if !seen.insert(e) {
            continue;
        }
        let Ok(code) = tier2::disassemble(d, e) else { continue };
        for i in &code {
            ops.insert(i.op);
            for o in &i.operands {
                match *o {
                    Operand::Code(t) => queue.push(t as u32),
                    Operand::Name(t) if s.speaker.is_none() => {
                        let n = text::plain(&text::decode(text::c_string(d, t as usize))).trim().to_string();
                        if !n.is_empty() {
                            s.speaker = Some(n);
                        }
                    }
                    Operand::Text(t) if (0x01..=0x03).contains(&i.op) => {
                        s.lines += 1;
                        if s.line.is_none() {
                            let l = text::plain(&text::decode(text::c_string(d, t as usize)));
                            s.line = Some(l.split_whitespace().collect::<Vec<_>>().join(" "));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let has = |set: &[u8]| set.iter().any(|o| ops.contains(o));
    for (tag, set) in [
        ("camera", &[0x13, 0x14, 0x15, 0x26, 0x41, 0x45, 0x46, 0x47, 0x48][..]),
        ("walk", &[0x1C, 0x34][..]),
        ("anim", &[0x10, 0x24][..]),
        ("fade", &[0x29, 0x2A, 0x2B, 0x2C][..]),
        ("music", &[0x22, 0x49][..]),
        ("choice", &[0x3A, 0x3B][..]),
        ("warp", &[0x19][..]),
        ("battle", &[0x08][..]),
    ] {
        if has(set) {
            s.tags.push(tag);
        }
    }
    s
}
