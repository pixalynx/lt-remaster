//! NPC packs (`npc/n`): actors, triggers, dialogue and event scripts for one
//! area. Layout: `docs/screens/12-text-dialogue-editor.md` §4.5.
//!
//! ```text
//! +0x00 u32 P   secondary header (file offset)
//! +0x04 u32 D   dialogue/script block (file offset); Tier-2 operands are u16 D-relative
//! +0x08 u32[]   model / pose section offsets
//! D+0   u32 n   actor slots (0 Knight, 1 Baby)
//! D+4+16i       {u32 script, u32 b, u32 placement, u32 d}   (script/placement D-relative)
//! D+4+16n s16   pass1 (2nd Tier-1 trigger table)
//! …             Tier-1 tables, name table + text pool, bytecode, walk/placement programs
//! ```

use serde::Serialize;

use crate::container::{le16, le32};

/// NPC loader cap: 0x31000 bytes = 98 sectors (`LoadFile` compares sectors).
pub const CAP_SECTORS: u32 = 98;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorRecord {
    pub slot: u32,
    /// D-relative entry point of the actor's talk/scene script.
    pub script: u32,
    pub b: u32,
    /// D-relative placement program.
    pub placement: u32,
    pub d: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NpcPack {
    pub size: usize,
    /// File offset of the P header.
    pub p: u32,
    /// File offset of the D-block.
    pub d: u32,
    /// Model/pose section offsets (file-relative).
    pub models: Vec<u32>,
    pub actors: Vec<ActorRecord>,
    pub pass1: i16,
    /// Bytes from D to the first model section after it.
    pub d_span: u32,
}

impl NpcPack {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 16 {
            return None;
        }
        let (p, d) = (le32(b, 0), le32(b, 4));
        if p as usize + 4 > b.len() || d as usize + 4 > b.len() || d <= p {
            return None;
        }
        let count_a = le16(b, p as usize) as usize;
        let count_b = le16(b, p as usize + 2) as usize;
        let models: Vec<u32> = (0..count_a + count_b)
            .map(|i| 8 + i * 4)
            .take_while(|&o| o + 4 <= p as usize)
            .map(|o| le32(b, o))
            .collect();
        let db = &b[d as usize..];
        let n = le32(db, 0) as usize;
        if n > 2048 || 4 + n * 16 + 2 > db.len() {
            return None;
        }
        let actors = (0..n)
            .map(|i| {
                let r = 4 + i * 16;
                ActorRecord {
                    slot: i as u32,
                    script: le32(db, r),
                    b: le32(db, r + 4),
                    placement: le32(db, r + 8),
                    d: le32(db, r + 12),
                }
            })
            .collect();
        let pass1 = le16(db, 4 + n * 16) as i16;
        let d_span = models.iter().copied().filter(|&m| m > d).min().unwrap_or(b.len() as u32) - d;
        Some(Self { size: b.len(), p, d, models, actors, pass1, d_span })
    }

    /// The D-block (from D to the end of the file; operands index into it).
    pub fn d_block<'a>(&self, b: &'a [u8]) -> &'a [u8] {
        &b[self.d as usize..]
    }

    /// Group A section count (`g_NpcModelPtrTable`): the TMDs actors draw.
    pub fn group_a(&self, b: &[u8]) -> usize {
        (le16(b, self.p as usize) as usize).min(self.models.len())
    }

    /// Bytes of model section `i` (to the next section start, or the end of the file).
    fn section<'a>(&self, b: &'a [u8], i: usize) -> Option<&'a [u8]> {
        let at = *self.models.get(i)?;
        let end = self.models.iter().copied().chain([self.p, self.d, b.len() as u32]).filter(|&o| o > at).min().unwrap_or(b.len() as u32);
        b.get(at as usize..end as usize)
    }

    /// Group A model `i` (an actor's record `b` byte 2, `Character_Struct+0x16`).
    pub fn model<'a>(&self, b: &'a [u8], i: usize) -> Option<&'a [u8]> {
        (i < self.group_a(b)).then(|| self.section(b, i)).flatten()
    }

    /// Group B pose block `i` (record `b` byte 3, `Character_Struct+0x17`).
    pub fn pose_block<'a>(&self, b: &'a [u8], i: usize) -> Option<&'a [u8]> {
        self.section(b, self.group_a(b) + i)
    }

    /// The pose an actor with animation set `set` shows first: the first step of
    /// its animation 0. The P-chain word `set` (P-relative, like every word the
    /// loader relocates from P+4 up to a 0) points at the set's list of
    /// sequences; each is `u8 count; (pose, speed) × count`, poses 1-based.
    pub fn first_pose(&self, b: &[u8], set: usize) -> Option<u8> {
        let p = self.p as usize;
        let word = |at: usize| b.get(at..at + 4).map(|_| le32(b, at) as usize);
        let list = p + word(p + 4 + set * 4).filter(|&o| o != 0)?;
        let seq = p + word(list)?;
        (*b.get(seq)? > 0).then(|| b.get(seq + 1).copied()).flatten()
    }

    /// Every animation of set `set` (`Character_Struct+0x20`, indexed by the
    /// actor's animation `+0x14`): `(pose, speed)` steps. The P-chain is one
    /// run of P-relative words up to a 0: the first word per pose block points
    /// at that set's list, and the lists (inside the run) point at sequences.
    /// A list runs to the next list or the end of the run.
    pub fn anim_set(&self, b: &[u8], set: usize) -> Vec<Vec<(u8, u8)>> {
        let p = self.p as usize;
        let mut words = Vec::new();
        let mut at = p + 4;
        while at + 4 <= b.len() && words.len() < 4096 {
            let w = le32(b, at);
            if w == 0 {
                break;
            }
            words.push(w as usize);
            at += 4;
        }
        let sets = self.models.len() - self.group_a(b);
        // A word that points into the run is a list start (as a word index).
        let index = |w: usize| (w >= 4 && (w - 4) % 4 == 0 && (w - 4) / 4 < words.len()).then(|| (w - 4) / 4);
        let Some(start) = words.get(set).copied().and_then(index) else { return Vec::new() };
        let end = (0..sets.min(words.len()))
            .filter_map(|s| index(words[s]))
            .filter(|&i| i > start)
            .min()
            .unwrap_or(words.len());
        words[start..end]
            .iter()
            .map(|&w| {
                let seq = p + w;
                let n = b.get(seq).copied().unwrap_or(0) as usize;
                (0..n).filter_map(|k| Some((*b.get(seq + 1 + k * 2)?, *b.get(seq + 2 + k * 2)?))).collect()
            })
            .collect()
    }
}

/// One Group A model and the actor records drawn with it (the model browser, screen 06).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUse {
    /// Group A index (record `b` byte 2).
    pub index: u32,
    /// File offset of the TMD (tools like PSXPrev list models by it).
    pub offset: u32,
    pub bytes: u32,
    /// Records from slot 2 on drawn with it: the spawn loop reads no model for the Knight
    /// and Baby slots (`i >= 2`, `ovr02_801e8958.c`). Empty = no actor uses it.
    pub actors: Vec<u32>,
    /// Pose set (record `b` byte 3) of the first of them that has one.
    pub set: Option<u8>,
    /// The pose that set shows first ([`NpcPack::first_pose`]).
    pub pose: Option<u8>,
    /// The first of them is a two-part object (kind 3: a chest, its lid 40 units up).
    pub lid: bool,
    /// Home chunk of the first of them that has one (its field VRAM holds the
    /// model's pages); else [`NpcPack::home_chunk`].
    pub chunk: Option<u32>,
}

/// A record's home chunk (`b` low half) names a chunk; 0 = none.
fn is_home(h: u32) -> bool {
    h != 0 && crate::world::valid_chunk(h)
}

/// `npc/7#12` → (7, 12): Group A model 12 of `npc/7`.
pub fn model_id(id: &str) -> Option<(u32, usize)> {
    let (pack, index) = id.strip_prefix("npc/")?.split_once('#')?;
    Some((pack.parse().ok()?, index.parse().ok()?))
}

impl NpcPack {
    /// Every Group A model with the actor records drawn with it.
    pub fn model_uses(&self, b: &[u8]) -> Vec<ModelUse> {
        (0..self.group_a(b))
            .map(|i| {
                let users: Vec<(&ActorRecord, ActorLook)> =
                    self.actors.iter().skip(2).map(|a| (a, ActorLook::of(a))).filter(|(_, l)| l.model == Some(i as u8)).collect();
                let set = users.iter().find_map(|(_, l)| l.poses);
                let bytes = self.model(b, i).map_or(0, |m| m.len() as u32);
                ModelUse {
                    index: i as u32,
                    offset: self.models[i],
                    bytes,
                    actors: users.iter().map(|(a, _)| a.slot).collect(),
                    set,
                    pose: set.and_then(|s| self.first_pose(b, s as usize)),
                    lid: users.first().is_some_and(|(_, l)| l.kind & 0xF == 3),
                    chunk: users.iter().map(|(a, _)| a.b & 0xFFFF).find(|&h| is_home(h)).or_else(|| self.home_chunk()),
                }
            })
            .collect()
    }

    /// The chunk whose field VRAM the pack's models sample (they ship no TIM): the
    /// first home chunk (record `b` low half, 0 = none) of an actor from slot 2 on.
    pub fn home_chunk(&self) -> Option<u32> {
        self.actors.iter().skip(2).map(|a| a.b & 0xFFFF).find(|&h| is_home(h))
    }
}

/// How the field draws an actor record (`NPC_UpdateActors`, `Npc_DrawIfVisible`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorLook {
    /// Group A model (record `b` byte 2); `None` = 0xFF, never drawn.
    pub model: Option<u8>,
    /// Group B pose block and P-chain animation set (record `b` byte 3); `None` = 0xFF, still.
    pub poses: Option<u8>,
    /// Record `d` byte 1 (`Character_Struct+0x32`): low nibble 1 = not drawn, 2 = small
    /// object, 3 = two-part object (a chest: object 1 is the lid, 40 units up); 4/5 = a
    /// torch flame, not an actor; bit 0x20 = big model (spans packet slots).
    pub kind: u8,
    /// Record `d` byte 0 (`+0x33`): 2/3 = blob shadow sizes.
    pub shadow: u8,
}

impl ActorLook {
    pub fn of(a: &ActorRecord) -> Self {
        let byte = |v: u32, k: u32| (v >> (8 * k)) as u8;
        let some = |v: u8| (v != 0xFF).then_some(v);
        Self { model: some(byte(a.b, 2)), poses: some(byte(a.b, 3)), kind: byte(a.d, 1), shadow: byte(a.d, 0) }
    }

    /// Whether `Npc_DrawIfVisible` draws a model for it.
    pub fn drawn(&self) -> bool {
        self.model.is_some() && !matches!(self.kind & 0xF, 1 | 4 | 5)
    }

    /// The Place actor bar previews it (LT-78): an actor with a model, drawn or hidden (kind 1)
    /// until a scene reveals it.
    pub fn previewed(&self) -> bool {
        self.model.is_some() && !matches!(self.kind & 0xF, 4 | 5)
    }
}

/* ───────────────────────── Dialogue ───────────────────────── */

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::text::{self, Tok};
use crate::tier2::{self, Operand};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogueLine {
    /// D-relative offset of the string (its stable id: `npc/7@07A8`).
    pub offset: u32,
    /// say | name | objective | choice
    pub kind: &'static str,
    pub speaker: Option<String>,
    /// Actor slots whose scripts reach this string.
    pub actors: Vec<u32>,
    /// D-relative instruction offsets that reference it.
    pub sites: Vec<u32>,
    pub tokens: Vec<Tok>,
    pub plain: String,
    /// Bytes without the NUL.
    pub len: u32,
    /// Bytes available in place (to the next 4-byte boundary after the NUL).
    pub slot: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptRef {
    pub actor: u32,
    pub entry: u32,
    pub ops: usize,
    pub ok: bool,
}

impl NpcPack {
    /// Every Tier-2 program reachable from the actor records (and GOTOs).
    pub fn programs(&self, b: &[u8]) -> Vec<(u32, u32, Result<Vec<tier2::Instr>, tier2::DisasmError>)> {
        let d = self.d_block(b);
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<(u32, u32)> = self
            .actors
            .iter()
            .filter(|a| a.script != 0 && (a.script as usize) < d.len())
            .map(|a| (a.slot, a.script))
            .collect();
        let mut out = Vec::new();
        while let Some((actor, entry)) = queue.pop_front() {
            if !seen.insert(entry) {
                continue;
            }
            let res = tier2::disassemble(d, entry);
            if let Ok(code) = &res {
                for i in code {
                    for o in &i.operands {
                        if let Operand::Code(t) = o {
                            queue.push_back((actor, *t as u32));
                        }
                    }
                }
            }
            out.push((actor, entry, res));
        }
        out
    }

    /// Text reachable from the scripts, in pool order.
    pub fn dialogue(&self, b: &[u8]) -> Vec<DialogueLine> {
        let d = self.d_block(b);
        let mut lines: BTreeMap<u32, DialogueLine> = BTreeMap::new();
        for (actor, _, res) in self.programs(b) {
            let Ok(code) = res else { continue };
            let mut speaker: Option<String> = None;
            for i in &code {
                for o in &i.operands {
                    let (kind, at) = match *o {
                        Operand::Text(t) => ("say", t),
                        Operand::Name(t) => ("name", t),
                        Operand::Objective(t) => ("objective", t),
                        Operand::Choice(t) => ("choice", t),
                        _ => continue,
                    };
                    let at = at as u32;
                    let raw = text::c_string(d, at as usize);
                    let tokens = text::decode(raw);
                    if kind == "name" {
                        speaker = Some(text::plain(&tokens));
                    }
                    let entry = lines.entry(at).or_insert_with(|| DialogueLine {
                        offset: at,
                        kind,
                        speaker: if kind == "name" { None } else { speaker.clone() },
                        actors: Vec::new(),
                        sites: Vec::new(),
                        plain: text::plain(&tokens),
                        len: raw.len() as u32,
                        slot: ((raw.len() as u32 + 1).div_ceil(4)) * 4,
                        tokens,
                    });
                    if !entry.actors.contains(&actor) {
                        entry.actors.push(actor);
                    }
                    entry.sites.push(i.at);
                }
                if i.op == 0x20 {
                    speaker = None;
                }
            }
        }
        lines.into_values().collect()
    }
}

/* ───────────────────────── Views for the event, text and flag screens ───────────────────────── */

use crate::trigger::{self, PlacementBranch, TriggerGroup, TriggerRecord};

/// One actor record with what its script does (screen 11 tree, screen 12 scopes).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorView {
    pub slot: u32,
    /// D-relative script entry.
    pub script: u32,
    /// D-relative placement program.
    pub placement: u32,
    /// Home chunk (low half of record `b`).
    pub chunk: u16,
    /// First speaker name (opcode 0x1F) in the script.
    pub speaker: Option<String>,
    /// From the opcodes used: inn (06), shop (07), chest (28), warp (19), objective (44), battle (08).
    pub tags: Vec<&'static str>,
    /// Actor ids a type-10 trigger makes talk are scenes.
    pub scene: bool,
    pub ops: usize,
    pub bytes: usize,
    pub error: Option<String>,
}

/// A string in the text pool. Pool strings are 4-byte aligned C strings that
/// run from the first referenced one up to the bytecode.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolString {
    /// D-relative offset (the stable text id `npc/7@07A8`).
    pub offset: u32,
    /// say | name | objective | choice | orphan (no operand points at it).
    pub kind: &'static str,
    pub markup: String,
    /// Bytes including the NUL.
    pub len: u32,
    /// Bytes to the next string (in-place edit room).
    pub slot: u32,
}

/// A byte run inside the bytecode region that no actor program reaches.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeGap {
    pub at: u32,
    pub len: u32,
    /// True when the run is all 00 (alignment padding).
    pub padding: bool,
}

/// Per-pack totals for the pack pickers (screens 11, 12).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSummary {
    pub actors: usize,
    /// Distinct strings the scripts show (say, objective, choice; not speaker names).
    pub lines: usize,
    pub orphans: usize,
    /// Distinct speaker names in actor order.
    pub speakers: Vec<String>,
    /// Chunks the pack serves: trigger groups and actor home chunks.
    pub chunks: Vec<u16>,
    pub triggers: usize,
    pub doors: usize,
    pub scenes: usize,
}

fn text_kind(o: &Operand) -> Option<(&'static str, u16)> {
    match *o {
        Operand::Text(t) => Some(("say", t)),
        Operand::Name(t) => Some(("name", t)),
        Operand::Objective(t) => Some(("objective", t)),
        Operand::Choice(t) => Some(("choice", t)),
        _ => None,
    }
}

impl NpcPack {
    /// Both Tier-1 trigger tables (pass 0, then pass 1). Empty when they don't parse.
    pub fn triggers(&self, b: &[u8]) -> Vec<TriggerGroup> {
        trigger::parse_tables(self.d_block(b), self.actors.len()).unwrap_or_default()
    }

    /// The placement ladder of one actor (screen 10, flag xref).
    pub fn placement(&self, b: &[u8], slot: usize) -> Vec<PlacementBranch> {
        self.actors.get(slot).map(|a| trigger::placement_ladder(self.d_block(b), a.placement)).unwrap_or_default()
    }

    /// The program of every actor record, one per distinct entry, in record order.
    pub fn actor_programs(&self, b: &[u8]) -> Vec<(u32, Result<Vec<tier2::Instr>, tier2::DisasmError>)> {
        let d = self.d_block(b);
        let mut seen = BTreeSet::new();
        self.actors
            .iter()
            .filter(|a| seen.insert(a.script))
            .map(|a| (a.script, tier2::disassemble(d, a.script)))
            .collect()
    }

    /// Actor records with speaker, tags and size of their script.
    pub fn actor_views(&self, b: &[u8]) -> Vec<ActorView> {
        let d = self.d_block(b);
        let scenes: BTreeSet<u16> = self
            .triggers(b)
            .iter()
            .flat_map(|g| g.rungs.iter().flat_map(|r| r.records.iter()))
            .filter_map(|r| match r {
                TriggerRecord::Talk { actor, .. } => Some(*actor),
                _ => None,
            })
            .collect();
        self.actors
            .iter()
            .map(|a| {
                let (speaker, tags, ops, bytes, error) = match tier2::disassemble(d, a.script) {
                    Ok(code) => {
                        let speaker = code.iter().find_map(|i| match i.operands.first() {
                            Some(Operand::Name(t)) => Some(text::plain(&text::decode(text::c_string(d, *t as usize)))),
                            _ => None,
                        });
                        let has = |op: u8| code.iter().any(|i| i.op == op);
                        let tags = [(0x06, "inn"), (0x07, "shop"), (0x28, "chest"), (0x19, "warp"), (0x08, "battle"), (0x44, "objective")]
                            .into_iter()
                            .filter(|(op, _)| has(*op))
                            .map(|(_, t)| t)
                            .collect();
                        let bytes = code.iter().map(|i| i.bytes.len()).sum();
                        (speaker, tags, code.len(), bytes, None)
                    }
                    Err(e) => (None, Vec::new(), 0, 0, Some(format!("{e:?}"))),
                };
                ActorView {
                    slot: a.slot,
                    script: a.script,
                    placement: a.placement,
                    chunk: (a.b & 0xFFFF) as u16,
                    speaker,
                    tags,
                    scene: scenes.contains(&(a.slot as u16)),
                    ops,
                    bytes,
                    error,
                }
            })
            .collect()
    }

    /// D-relative range of the bytecode: the lowest actor entry to the end of
    /// the furthest actor program.
    pub fn code_span(&self, b: &[u8]) -> (u32, u32) {
        self.code_span_below(b, u32::MAX)
    }

    /// [`Self::code_span`] over the programs that start below `limit`. A
    /// project's pack passes its retail D-span, so programs an edit appended
    /// after it don't stretch the range over the relocation area.
    pub fn code_span_below(&self, b: &[u8], limit: u32) -> (u32, u32) {
        let mut start = u32::MAX;
        let mut end = 0u32;
        for (entry, res) in self.actor_programs(b) {
            if entry >= limit {
                continue;
            }
            start = start.min(entry);
            if let Ok(code) = res {
                if let Some(last) = code.last() {
                    end = end.max(last.at + last.bytes.len() as u32);
                }
            }
        }
        if start == u32::MAX {
            (0, 0)
        } else {
            (start, end.max(start))
        }
    }

    /// Runs of the bytecode region that no actor program (or GOTO target) covers.
    pub fn code_gaps(&self, b: &[u8]) -> Vec<CodeGap> {
        self.code_gaps_below(b, u32::MAX)
    }

    /// [`Self::code_gaps`] within [`Self::code_span_below`]. A program that an
    /// edit replaced shows up here as unreferenced retail code.
    pub fn code_gaps_below(&self, b: &[u8], limit: u32) -> Vec<CodeGap> {
        let d = self.d_block(b);
        let (start, end) = self.code_span_below(b, limit);
        if end <= start {
            return Vec::new();
        }
        let mut covered = vec![false; (end - start) as usize];
        for (_, _, res) in self.programs(b) {
            if let Ok(code) = res {
                for i in &code {
                    for k in i.at..i.at + i.bytes.len() as u32 {
                        if k >= start && k < end {
                            covered[(k - start) as usize] = true;
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        let mut k = 0usize;
        while k < covered.len() {
            if covered[k] {
                k += 1;
                continue;
            }
            let s = k;
            while k < covered.len() && !covered[k] {
                k += 1;
            }
            let at = start + s as u32;
            let bytes = &d[at as usize..(start as usize + k).min(d.len())];
            out.push(CodeGap { at, len: (k - s) as u32, padding: bytes.iter().all(|&x| x == 0) });
        }
        out
    }

    /// Every pool string: the ones operands point at, plus orphans found by
    /// walking the 4-byte aligned pool up to the bytecode.
    pub fn pool(&self, b: &[u8]) -> Vec<PoolString> {
        self.pool_below(b, u32::MAX)
    }

    /// [`Self::pool`], walking up to [`Self::code_span_below`]'s start.
    pub fn pool_below(&self, b: &[u8], limit: u32) -> Vec<PoolString> {
        let d = self.d_block(b);
        let mut refs: BTreeMap<u32, &'static str> = BTreeMap::new();
        for (_, res) in self.actor_programs(b) {
            if let Ok(code) = res {
                for i in &code {
                    for o in &i.operands {
                        if let Some((kind, at)) = text_kind(o) {
                            refs.entry(at as u32).or_insert(kind);
                        }
                    }
                }
            }
        }
        let Some(&first) = refs.keys().next() else { return Vec::new() };
        let (code_start, _) = self.code_span_below(b, limit);
        let entry = |at: u32, kind: &'static str| {
            let raw = text::c_string(d, at as usize);
            let len = raw.len() as u32 + 1;
            PoolString { offset: at, kind, markup: text::markup(&text::decode(raw)), len, slot: len.div_ceil(4) * 4 }
        };
        let mut out = Vec::new();
        let mut walked = BTreeSet::new();
        let mut o = first;
        while o < code_start && (o as usize) < d.len() {
            walked.insert(o);
            let e = entry(o, refs.get(&o).copied().unwrap_or("orphan"));
            o += e.slot;
            out.push(e);
        }
        // Operands that point outside the walk (none in retail) still get a row.
        for (&at, &kind) in &refs {
            if !walked.contains(&at) {
                out.push(entry(at, kind));
            }
        }
        out.sort_by_key(|s| s.offset);
        // Slot = distance to the next string when they are contiguous.
        for k in 0..out.len().saturating_sub(1) {
            let gap = out[k + 1].offset - out[k].offset;
            if gap >= out[k].len {
                out[k].slot = gap;
            }
        }
        out
    }

    /// Totals for the pack pickers.
    pub fn summary(&self, b: &[u8]) -> PackSummary {
        let actors = self.actor_views(b);
        let triggers = self.triggers(b);
        let pool = self.pool(b);
        let mut speakers: Vec<String> = Vec::new();
        for a in &actors {
            if let Some(s) = &a.speaker {
                if !s.is_empty() && !speakers.contains(s) {
                    speakers.push(s.clone());
                }
            }
        }
        let chunks: BTreeSet<u16> = triggers
            .iter()
            .filter(|g| g.chunk >= 0)
            .map(|g| g.chunk as u16)
            .chain(actors.iter().filter(|a| a.slot > 1 && a.chunk > 0 && a.chunk < 524).map(|a| a.chunk))
            .collect();
        let records = || triggers.iter().flat_map(|g| g.rungs.iter().flat_map(|r| r.records.iter()));
        PackSummary {
            actors: actors.len(),
            lines: pool.iter().filter(|s| matches!(s.kind, "say" | "objective" | "choice")).count(),
            orphans: pool.iter().filter(|s| s.kind == "orphan").count(),
            speakers,
            chunks: chunks.into_iter().collect(),
            triggers: triggers.iter().map(|g| g.rungs.len()).sum(),
            doors: records().filter(|r| matches!(r, TriggerRecord::Door { .. })).count(),
            scenes: records().filter(|r| matches!(r, TriggerRecord::Talk { .. })).count(),
        }
    }
}
