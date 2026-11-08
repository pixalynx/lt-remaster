//! The blank slate (research 06 §9, phase 0): a pack, a chunk or the whole
//! game emptied of the retail story, so a new game can be built on the
//! engine. Nothing is deleted; every piece is taken out the way the editor
//! takes one out by hand, so the project can put any of it back:
//!
//! - retail actors and examine spots: [`super::remove`];
//! - retail trigger records (scene tripwires, flag records, optionally
//!   doors): moved off the map ([`super::trigger::OFF_MAP`]).
//!
//! The project's own additions (added actors, doors, scenes) stay.
//!
//! A new game starts on chunk 326 (pack 7) with flag 0xFF ("the Knight is
//! hidden") set; retail's opening scene clears it. With that scene gone the
//! Knight would stay invisible, so [`starter`] adds a small scene of the
//! project's own on the spawn spot, cued on 0xFF, that shows the Knight and
//! lets the field music play (`GameState_ResetForNewGame`, main.c:1328).
//! The field only reads the Knight's visibility (and hands over the pad) when
//! a chunk loads, so the scene ends with a warp onto the spawn spot, as
//! retail's opening ends with one (checked in Redux, 2026-10-12).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::npc::NpcPack;
use crate::trigger::TriggerRecord;

use super::remove::{self, How};
use super::scene::{self, NewScene, ProjectScene};
use super::script::{Arg, Node};
use super::trigger::{self as trigger_edit, TriggerWrite};
use super::{EditError, Result};

/// The chunk a new game starts on (`SaveState_Pack(c, 0x146, …)`, ovr01.c:547).
pub const BOOT_CHUNK: u16 = 326;
/// The pack the chunk → pack table gives chunk 326.
pub const BOOT_PACK: u32 = 7;
/// The Knight's spawn on the boot chunk: x, h, z (main.c:1333-1335).
pub const BOOT_SPOT: (i16, i16, i16) = (0x1FC, -0x21, 0x4C);
/// Set at a new game: the Knight is hidden until a scene clears it.
pub const KNIGHT_HIDDEN: u16 = 0xFF;
/// Set at a new game: scene music holds the field BGM.
pub const SCENE_MUSIC: u16 = 0xFC;
/// The Knight's facing at the spawn, in eighths (0x9FD ≈ 5 × 0x200).
pub const BOOT_FACING: i64 = 5;
/// Tier-2 CLEAR and WARP.
const OP_CLEAR: u8 = 0x05;
const OP_WARP: u8 = 0x19;

/// What a blank slate takes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    /// Retail actors (people, animals, chests, scene directors).
    pub actors: bool,
    /// Examine spots (record types 4/5: furniture text, beds, signs).
    pub objects: bool,
    /// Type-10 records that start scenes.
    pub scenes: bool,
    /// Type-8/9 records that set or clear flags as the Knight walks by.
    pub flags: bool,
    /// Doors. Off by default: a new game that reuses retail areas keeps
    /// their connections.
    pub doors: bool,
    /// Only this chunk's actors (home chunk) and trigger group; `None` = the whole pack.
    #[serde(default)]
    pub chunk: Option<u16>,
    /// Also free the models and pose blocks only taken-out actors use, so
    /// characters from other packs fit ([`super::character::freeable`]).
    #[serde(default)]
    pub free_models: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { actors: true, objects: true, scenes: true, flags: true, doors: false, chunk: None, free_models: false }
    }
}

/// What a pack holds taken out, after a blank slate (totals, not just this call).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub actors: u32,
    pub objects: u32,
    pub scenes: u32,
    pub flags: u32,
    pub doors: u32,
    /// Retail records and trigger records still in play.
    pub actors_left: u32,
    pub triggers_left: u32,
}

impl Report {
    pub fn add(&mut self, o: &Report) {
        self.actors += o.actors;
        self.objects += o.objects;
        self.scenes += o.scenes;
        self.flags += o.flags;
        self.doors += o.doors;
        self.actors_left += o.actors_left;
        self.triggers_left += o.triggers_left;
    }
}

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

/// The kind of trigger record, as the options name it.
fn wanted(rec: &TriggerRecord, o: &Options) -> bool {
    match rec {
        TriggerRecord::Door { .. } => o.doors,
        TriggerRecord::Talk { .. } => o.scenes,
        TriggerRecord::Set { .. } | TriggerRecord::Clear { .. } | TriggerRecord::Other { .. } => o.flags,
    }
}

/// `current` with the retail pieces `o` names taken out, and the totals.
pub fn blank(retail: &[u8], current: &[u8], o: &Options) -> Result<(Vec<u8>, Report)> {
    let r = header(retail, "retail")?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    for a in r.actors.iter().skip(2) {
        if o.chunk.is_some_and(|c| a.b as u16 != c) {
            continue;
        }
        let take = match remove::how(a.d) {
            How::Absent => o.actors,
            How::Silent => o.objects,
        };
        if take {
            set.removed.insert(a.slot);
        }
    }
    if o.free_models {
        let more = super::character::freeable(retail, &set.removed, &set.actors)?;
        set.freed.models.extend(more.models);
        set.freed.poses.extend(more.poses);
    }
    let laid = super::text::layout(retail, current, &set)?.0;
    let writes: Vec<TriggerWrite> = r
        .triggers(retail)
        .iter()
        .filter(|g| o.chunk.is_none_or(|c| g.chunk == c as i16))
        .flat_map(|g| &g.rungs)
        .flat_map(|x| &x.records)
        .filter(|rec| wanted(rec, o))
        .map(|rec| TriggerWrite::off(trigger_edit::record_pos(rec).0))
        .collect();
    let out = trigger_edit::apply(retail, &laid, &writes)?;
    let report = report(retail, &out)?;
    Ok((out, report))
}

/// What `current` holds taken out.
pub fn report(retail: &[u8], current: &[u8]) -> Result<Report> {
    let r = header(retail, "retail")?;
    let removed = remove::recover(retail, current)?;
    let off: BTreeSet<u32> = trigger_edit::removed(retail, current).into_iter().collect();
    let mut rep = Report::default();
    for a in r.actors.iter().skip(2) {
        match (removed.contains(&a.slot), remove::how(a.d)) {
            (true, How::Absent) => rep.actors += 1,
            (true, How::Silent) => rep.objects += 1,
            (false, _) => rep.actors_left += 1,
        }
    }
    for rec in r.triggers(retail).iter().flat_map(|g| &g.rungs).flat_map(|x| &x.records) {
        if !off.contains(&trigger_edit::record_pos(rec).0) {
            rep.triggers_left += 1;
            continue;
        }
        match rec {
            TriggerRecord::Door { .. } => rep.doors += 1,
            TriggerRecord::Talk { .. } => rep.scenes += 1,
            _ => rep.flags += 1,
        }
    }
    Ok(rep)
}

/// `current` with every retail piece put back (records and trigger records);
/// with `chunk`, only that chunk's.
pub fn restore(retail: &[u8], current: &[u8], chunk: Option<u16>) -> Result<Vec<u8>> {
    let r = header(retail, "retail")?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    set.removed.retain(|&s| chunk.is_some_and(|c| r.actors[s as usize].b as u16 != c));
    let laid = super::text::layout(retail, current, &set)?.0;
    let groups = r.triggers(retail);
    let in_scope: BTreeSet<u32> = groups
        .iter()
        .filter(|g| chunk.is_none_or(|c| g.chunk == c as i16))
        .flat_map(|g| &g.rungs)
        .flat_map(|x| &x.records)
        .map(|rec| trigger_edit::record_pos(rec).0)
        .collect();
    let writes: Vec<TriggerWrite> =
        trigger_edit::removed(retail, &laid).into_iter().filter(|at| in_scope.contains(at)).map(TriggerWrite::retail).collect();
    trigger_edit::apply(retail, &laid, &writes)
}

/// The starter scene's program: show the Knight, release the field music,
/// and warp onto the spawn spot so the field loads with him on it.
pub fn starter_program() -> Vec<Node> {
    let (x, _, z) = BOOT_SPOT;
    starter_program_at(BOOT_CHUNK, x, z, BOOT_FACING as u8)
}

/// [`starter_program`] for a start somewhere else (a HackerGC base's boot map,
/// [`super::boot`]): `facing` in eighths.
pub fn starter_program_at(chunk: u16, x: i16, z: i16, facing: u8) -> Vec<Node> {
    let num = |v: i64| Arg::Num { value: v };
    let mut out: Vec<Node> = [KNIGHT_HIDDEN, SCENE_MUSIC].into_iter().map(|f| Node::Op { op: OP_CLEAR, args: vec![num(f as i64)], at: None }).collect();
    out.push(Node::Op { op: OP_WARP, args: vec![num(facing as i64), num(chunk as i64), num(x as i64), num(z as i64), num(0)], at: None });
    out
}

/// The project's starter scene in the boot pack, if it has one: a scene of
/// its own on the spawn spot, cued on 0xFF.
pub fn find_starter(retail: &[u8], current: &[u8]) -> Result<Option<ProjectScene>> {
    let (x, _, z) = BOOT_SPOT;
    find_starter_at(retail, current, BOOT_CHUNK, x, z)
}

/// A scene of the project's own on (x, z) of `chunk`, cued on 0xFF.
pub fn find_starter_at(retail: &[u8], current: &[u8], chunk: u16, x: i16, z: i16) -> Result<Option<ProjectScene>> {
    let base = super::character::base(retail, current)?;
    let current: &[u8] = &base;
    Ok(scene::list(retail, current)?.into_iter().find(|s| s.chunk == chunk && s.x == x && s.z == z && s.cue == KNIGHT_HIDDEN))
}

/// `current` (the boot pack, npc/7) with the starter scene, unless it has
/// one; returns the pack and the scene's director slot.
pub fn starter(retail: &[u8], current: &[u8]) -> Result<(Vec<u8>, u32)> {
    let (x, h, z) = BOOT_SPOT;
    starter_at(retail, current, BOOT_CHUNK, (x, h, z), BOOT_FACING as u8, &[])
}

/// `current` with a starter scene on `spot` of `chunk` (facing in eighths),
/// unless it has one there: the pack that serves the start of a moved boot map.
/// `areas` are the project's new areas, which `chunk` may be.
pub fn starter_at(retail: &[u8], current: &[u8], chunk: u16, spot: (i16, i16, i16), facing: u8, areas: &[u32]) -> Result<(Vec<u8>, u32)> {
    let (x, h, z) = spot;
    if let Some(s) = find_starter_at(retail, current, chunk, x, z)? {
        return Ok((current.to_vec(), s.slot));
    }
    scene::create(retail, current, &NewScene { chunk, x, h, z, cue: KNIGHT_HIDDEN }, &starter_program_at(chunk, x, z, facing), areas)
}
