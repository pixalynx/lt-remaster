//! The story graph (screen 21, research 08): every event the player can start
//! as a *page* of *nodes*, and the *links* between pages through event flags.
//! Spec: `docs/screens/21-story.md`; findings: `docs/research/08-story-graph.md`.
//!
//! - A **page** is one program plus how it starts: a walk-in scene (a type-10
//!   record wakes its actor), a conversation (talking to an actor), an examined
//!   object, an examine spot (a type-4 record, run as the Knight's script), a
//!   silent actor program with effects, or a Tier-1 step record that sets or
//!   clears a flag.
//! - Its **flow** is the program's Structured tree ([`ast::lift`]) with two
//!   display folds: a speaker's lines in a row become one dialogue node, and
//!   runs of camera, wait, animation, fade and sound ops become one staging
//!   row. Each [`Step`] keeps the D-relative address of its first instruction,
//!   which screen 11 selects and links name (`?at=`).
//! - Pages never name each other. A page *sets* a flag and other pages *read*
//!   it: a scene's rung (it starts), an actor's placement (it appears or
//!   leaves), a walk word (it moves) or an `if flag` (it branches). Those are
//!   the [`Link`]s. Scratch flags (0x000–0x03F) mean something else in every
//!   place, so they link within a place only.
//!
//! The graph names nothing: flags, items and places are named by the app, so a
//! project's own flag names apply.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::flags;
use crate::npc::{ActorLook, NpcPack};
use crate::placement::{self, WalkRead};
use crate::scene::{self, SceneKind};
use crate::text;
use crate::tier2::ast::{self, CodeRef, Stmt, TextRef, Val, WalkPath};
use crate::tier2::{self, Operand};
use crate::trigger::{Outcome, TriggerRecord};

/// One pack to read: its bytes as the project has them, and how many actor
/// records retail has (records past them are the project's).
pub struct PackIn<'a> {
    pub index: u16,
    pub bytes: &'a [u8],
    pub retail_actors: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PageKind {
    /// Walking onto a type-10 record wakes the actor.
    Scene,
    /// Talking to a drawn actor.
    Talk,
    /// ✕ on an object actor (kind 2/3: chests with lids, signs).
    Examine,
    /// An actor program with effects but no line.
    Silent,
    /// ✕ on a type-4 record (beds, tables, jars, chests).
    Spot,
    /// A Tier-1 type-8/9 record: walking there sets or clears a flag.
    Step,
}

/// How a page starts.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Start {
    /// The Knight walks within 0x20 of (x, z) while `flag` holds (0 = always).
    #[serde(rename_all = "camelCase")]
    WalkIn { x: i16, z: i16, flag: u16 },
    /// The player talks to (or examines) the actor while its placement ladder
    /// puts it on the map: `(cond, outcome)` in the order the game tries them.
    /// `cond` 0 = always, 1–flag count = a flag, ≥ 1000 = holding item cond − 1000.
    #[serde(rename_all = "camelCase")]
    Talk { ladder: Vec<(u16, Presence)> },
    /// Walking onto (x, z) sets or clears `flag`, while `rung` holds.
    #[serde(rename_all = "camelCase")]
    Step { x: i16, z: i16, flag: u16, set: bool, rung: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Presence {
    Here,
    Away,
    Arrival,
}

/// A node, at the D-relative address of its first instruction.
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub at: u32,
    #[serde(flatten)]
    pub node: Node,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Node {
    /// Lines in a row from one speaker (`SPEAKER`, `SAY`/`SAY2`/`SAY3`).
    /// `texts` are the lines' D-offsets (screen 12 opens `?pack=&line=`).
    Say { speaker: Option<String>, lines: Vec<String>, texts: Vec<u16> },
    /// Camera, waits, animation, facing, fades, shakes and sound, folded.
    /// `sfx` and `bgm` are the sound requests' operands.
    Stage { ops: Vec<String>, sfx: Vec<u16>, bgm: Vec<u16> },
    Flag { flag: u16, set: bool },
    /// An IF card. `kind`: flag · item · rubies · yes · choice · other.
    /// `value` is the flag, item or ruby amount; `question` the line asked
    /// before a yes/no or a choice; `yes`/`no` the two answers of a choice.
    Branch {
        kind: &'static str,
        value: i64,
        question: Option<String>,
        yes: Option<String>,
        no: Option<String>,
        then: Vec<Step>,
        els: Vec<Step>,
    },
    /// `WALK` (the Knight) or `WALK_BABY` (with the Baby) to (x, z).
    Walk { baby: bool, x: i16, z: i16 },
    Warp { chunk: u16, x: i16, z: i16, facing: i16 },
    Inn { chunk: u16, x: i16, z: i16 },
    /// A zone (< 0x100) or story battle (≥ 0x100).
    Battle { id: u16 },
    Give { item: u16 },
    Chest { flag: u16, item: u16 },
    Rubies { n: i64 },
    Shop { id: u16 },
    Objective { text: String, offset: u16 },
    /// GOTO a label of this program; `back` when it jumps backwards (a loop).
    Jump { label: String, back: bool },
    Label { name: String },
    Op { op: u8, name: String },
    End,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    /// `"<pack>.<record>"` for an actor's program, `"<pack>.t<offset>"` for a step record.
    pub id: String,
    pub pack: u16,
    /// Actor record (what screens 11 and 20 call the slot); none for a step.
    pub slot: Option<u32>,
    pub kind: PageKind,
    /// The chunk it plays on: the record's group, or the actor's home.
    pub chunk: u16,
    /// The catalogue's place for the pack ("Orgo").
    pub place: String,
    /// First speaker and first line, taking each branch's longer lane.
    pub speaker: Option<String>,
    pub line: Option<String>,
    pub start: Start,
    /// The curated chapter (1–4) of what it sets, else of what unlocks it.
    pub chapter: Option<u8>,
    /// Story order: the longest chain of unlock and cue links before it.
    pub layer: u32,
    /// A record the project added.
    pub added: bool,
    /// Taken out of the game (the blank slate, or removed by hand): it can't start.
    pub removed: bool,
    /// Where its program starts, D-relative: what the running game's script entry reads
    /// (none for a step record, which is no program).
    pub script: u32,
    /// Instructions (block markers left out) and the flow's own counts.
    pub stmts: u32,
    pub nodes: u32,
    pub stages: u32,
    pub branches: u32,
    pub flow: Vec<Step>,
    pub sets: Vec<u16>,
    pub clears: Vec<u16>,
    pub tests: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkKind {
    /// The reader starts (a scene's rung) or its actor appears (a placement branch).
    Unlocks,
    /// The reader's actor leaves (placement `7FFF`, walk `7nnn`/`Bnnn`).
    Removes,
    /// The reader branches on it (`if flag`).
    Changes,
    /// The reader's actor walk waits for it (`0nnn`).
    Cues,
    /// The writer clears the flag a scene waits for: the cue is spent.
    Clears,
}

/// Page `from` sets (or clears) `flag`, and page `to` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub from: u32,
    pub to: u32,
    pub kind: LinkKind,
    pub flag: u16,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    pub pages: Vec<Page>,
    pub links: Vec<Link>,
    /// Flags some page reads but no page sets (engine code sets them, or nothing does).
    pub unset: Vec<u16>,
    /// Doors that lead out of their place: the story map's exits.
    pub exits: Vec<Exit>,
}

/// A Tier-1 door from a chunk of one place to another place or the world map.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exit {
    pub pack: u16,
    pub place: String,
    /// The chunk the door is on, and the chunk it leads to.
    pub chunk: u16,
    pub to: u16,
    /// The pack the door loads when the chunk changes; none keeps the pack the Knight has.
    pub to_pack: Option<u16>,
    /// The destination's place; none for a world-map chunk with no pack of its own.
    pub to_place: Option<String>,
    /// The rung's flag: the door works while it holds (0 = always).
    pub flag: u16,
}

/// World-map chunks (the 18 × 18 overworld grid) are 0–323.
const OVERWORLD_CHUNKS: u16 = 324;

/// The doors of one pack that leave its place.
fn pack_exits(p: &PackIn, place: &str, places: &BTreeMap<u16, String>, out: &mut BTreeSet<Exit>) {
    let Some(pack) = NpcPack::parse(p.bytes) else { return };
    for g in pack.triggers(p.bytes) {
        if g.chunk < 0 {
            continue;
        }
        for r in &g.rungs {
            for rec in &r.records {
                let TriggerRecord::Door { to, pack: byte, .. } = *rec else { continue };
                // npc/17 is npc/0's file; a byte past the table keeps the current pack.
                let to_pack = match byte as u16 {
                    17 => Some(0),
                    b if b < 50 => Some(b),
                    _ => None,
                };
                let to_place = to_pack.map(|t| places.get(&t).cloned().unwrap_or_else(|| format!("npc/{t}")));
                // Doors within the place aren't exits. A door that keeps the pack leaves only onto the world map.
                if to_place.as_deref() == Some(place) || (to_pack.is_none() && to >= OVERWORLD_CHUNKS) {
                    continue;
                }
                out.insert(Exit { pack: p.index, place: place.to_string(), chunk: g.chunk as u16, to, to_pack, to_place, flag: r.flag });
            }
        }
    }
}

/* ───────────────────────── Flow ───────────────────────── */

/// Opcodes folded into staging rows.
fn staging(op: u8) -> bool {
    matches!(op, 0x09 | 0x10..=0x15 | 0x18 | 0x1A | 0x1B | 0x22..=0x27 | 0x29..=0x2C | 0x30 | 0x32 | 0x35..=0x39 | 0x41..=0x43 | 0x45..=0x49)
}

fn plain(d: &[u8], at: u16) -> String {
    let s = text::plain(&text::decode(text::c_string(d, at as usize)));
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Markup without its `{…}` codes, spaces collapsed.
fn strip_markup(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Flow<'a> {
    d: &'a [u8],
    /// Label → address, for back jumps.
    labels: BTreeMap<String, u32>,
    speaker: Option<String>,
    last_line: Option<String>,
}

impl Flow<'_> {
    fn text(&self, v: Option<&Val>) -> (String, u16) {
        match v {
            Some(Val::Text(TextRef::Pool(o))) => (plain(self.d, *o), *o),
            _ => (String::new(), 0),
        }
    }

    fn steps(&mut self, stmts: &[Stmt], at: &mut dyn Iterator<Item = u32>) -> Vec<Step> {
        let mut out: Vec<Step> = Vec::new();
        let mut last = 0u32;
        for s in stmts {
            match s {
                Stmt::Label { name, .. } => out.push(Step { at: last, node: Node::Label { name: name.clone() } }),
                Stmt::If { op, arg, then, els, .. } => {
                    let cond_at = at.next().unwrap_or(0);
                    at.next(); // 0D
                    let value = match arg {
                        Some(Val::Num(n)) => *n,
                        _ => 0,
                    };
                    let (kind, question, yes, no) = match op {
                        0x0A => ("yes", self.last_line.clone(), None, None),
                        0x0B => ("flag", None, None, None),
                        0x0C => ("rubies", None, None, None),
                        0x1D | 0x1E => ("item", None, None, None),
                        0x3A => {
                            // Two answers joined by a newline.
                            let raw = match arg {
                                Some(Val::Text(TextRef::Pool(p))) => text::markup_at(self.d, *p as usize),
                                _ => String::new(),
                            };
                            let mut parts = raw.split("{nl}").map(strip_markup);
                            ("choice", self.last_line.clone(), parts.next(), parts.next())
                        }
                        _ => ("other", None, None, None),
                    };
                    let speaker = self.speaker.clone();
                    let then = self.steps(then, at);
                    at.next(); // 0E
                    self.speaker = speaker.clone();
                    let els = self.steps(els, at);
                    at.next(); // 0F
                    self.speaker = speaker;
                    out.push(Step { at: cond_at, node: Node::Branch { kind, value, question, yes, no, then, els } });
                    last = cond_at;
                }
                Stmt::Op { op, args, .. } => {
                    let here = at.next().unwrap_or(0);
                    last = here;
                    let n = |k: usize| match args.get(k) {
                        Some(Val::Num(v)) => *v,
                        _ => 0,
                    };
                    let node = match op {
                        0x1F => {
                            self.speaker = Some(self.text(args.first()).0).filter(|s| !s.is_empty());
                            continue;
                        }
                        0x20 => {
                            self.speaker = None;
                            continue;
                        }
                        0x01..=0x03 => {
                            let (line, off) = self.text(args.first());
                            self.last_line = Some(line.clone());
                            if let Some(Step { node: Node::Say { speaker, lines, texts }, .. }) = out.last_mut() {
                                if *speaker == self.speaker {
                                    lines.push(line);
                                    texts.push(off);
                                    continue;
                                }
                            }
                            Node::Say { speaker: self.speaker.clone(), lines: vec![line], texts: vec![off] }
                        }
                        0x04 | 0x05 => Node::Flag { flag: n(0) as u16, set: *op == 0x04 },
                        0x1C | 0x34 => {
                            let p = match args.first() {
                                Some(Val::Walk(t)) => WalkPath::read(self.d, *t as usize),
                                Some(Val::Path(p)) => Some(p.clone()),
                                _ => None,
                            };
                            Node::Walk { baby: *op == 0x34, x: p.as_ref().map_or(0, |p| p.x), z: p.as_ref().map_or(0, |p| p.z) }
                        }
                        0x19 => Node::Warp { facing: n(0) as i16, chunk: n(1) as u16, x: n(2) as i16, z: n(3) as i16 },
                        0x06 => Node::Inn { chunk: n(0) as u16, x: n(1) as i16, z: n(2) as i16 },
                        0x08 => Node::Battle { id: n(0) as u16 },
                        0x16 => Node::Give { item: n(0) as u16 },
                        0x28 => Node::Chest { flag: n(0) as u16, item: n(1) as u16 },
                        0x33 => Node::Rubies { n: n(0) },
                        0x07 => Node::Shop { id: n(0) as u16 },
                        0x44 => {
                            let (text, offset) = self.text(args.first());
                            Node::Objective { text, offset }
                        }
                        0x2E | 0x2F => {
                            let (label, back) = match args.first() {
                                Some(Val::Code(CodeRef::Label(l))) => (l.clone(), self.labels.get(l).is_some_and(|t| *t <= here)),
                                Some(Val::Code(CodeRef::At(t))) => (format!("D+0x{t:04X}"), false),
                                _ => (String::new(), false),
                            };
                            Node::Jump { label, back }
                        }
                        0x00 => Node::End,
                        op if staging(*op) => {
                            let name = ast::mnemonic(*op).name.to_string();
                            let (sfx, bgm) = match op {
                                0x12 => (vec![n(0) as u16], vec![]),
                                0x22 => (vec![], vec![n(0) as u16]),
                                // Canned tasks 3 and 9 (`Event_SceneSetupTask`, the pillar scenes) load BGM 0x2F
                                // and queue 0x201-0x203 themselves (ovr02.c:3146, ovr23.c:174/341/276).
                                0x23 if matches!(n(0), 3 | 9) => (vec![0x201, 0x202, 0x203], vec![0x2F]),
                                _ => (vec![], vec![]),
                            };
                            if let Some(Step { node: Node::Stage { ops, sfx: s0, bgm: b0 }, .. }) = out.last_mut() {
                                ops.push(name);
                                s0.extend(sfx);
                                b0.extend(bgm);
                                continue;
                            }
                            Node::Stage { ops: vec![name], sfx, bgm }
                        }
                        _ => Node::Op { op: *op, name: ast::mnemonic(*op).name.to_string() },
                    };
                    out.push(Step { at: here, node });
                }
            }
        }
        out
    }
}

/// The flow of the program at `entry`, and the instructions it came from.
pub fn flow(d: &[u8], entry: u32) -> Option<(Vec<Step>, Vec<tier2::Instr>)> {
    let code = tier2::disassemble(d, entry).ok()?;
    let stmts = ast::lift(&code, &TextRef::Pool);
    let starts: BTreeSet<u32> = code.iter().map(|i| i.at).collect();
    let mut targets: Vec<u32> = code
        .iter()
        .flat_map(|i| i.operands.iter())
        .filter_map(|o| match o {
            Operand::Code(t) if starts.contains(&(*t as u32)) => Some(*t as u32),
            _ => None,
        })
        .collect();
    targets.sort_unstable();
    targets.dedup();
    // The names `ast::lift` gives GOTO targets.
    let labels = targets.iter().enumerate().map(|(k, &v)| (format!("L{}", k + 1), v)).collect();
    let mut f = Flow { d, labels, speaker: None, last_line: None };
    let steps = f.steps(&stmts, &mut code.iter().map(|i| i.at));
    Some((steps, code))
}

/// Story nodes, staging rows and branch cards in a flow.
pub fn counts(flow: &[Step]) -> (u32, u32, u32) {
    let (mut n, mut s, mut b) = (0, 0, 0);
    for x in flow {
        match &x.node {
            Node::Stage { .. } => s += 1,
            Node::Label { .. } => {}
            Node::Branch { then, els, .. } => {
                b += 1;
                n += 1;
                for lane in [then, els] {
                    let (a, c, d) = counts(lane);
                    n += a;
                    s += c;
                    b += d;
                }
            }
            _ => n += 1,
        }
    }
    (n, s, b)
}

/// The first speaker and line of a flow, taking the longer lane of each
/// branch first (retail scenes open with "already played?" checks).
fn first_say(f: &[Step]) -> (Option<String>, Option<String>) {
    for x in f {
        match &x.node {
            Node::Say { speaker, lines, .. } => return (speaker.clone(), lines.first().map(|l| l.replace('|', " "))),
            Node::Branch { then, els, .. } => {
                let (a, b) = if counts(els).0 > counts(then).0 { (els, then) } else { (then, els) };
                let r = first_say(a);
                if r.1.is_some() {
                    return r;
                }
                let r = first_say(b);
                if r.1.is_some() {
                    return r;
                }
            }
            _ => {}
        }
    }
    (None, None)
}

/// Flags a program sets (chests set theirs), clears and tests.
fn flag_use(code: &[tier2::Instr]) -> (BTreeSet<u16>, BTreeSet<u16>, BTreeSet<u16>) {
    let (mut s, mut c, mut t) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for i in code {
        match (i.op, i.operands.first()) {
            (0x04 | 0x28, Some(Operand::Flag(f))) => {
                s.insert(*f);
            }
            (0x05, Some(Operand::Flag(f))) => {
                c.insert(*f);
            }
            (0x0B | 0x2D, Some(Operand::Flag(f))) => {
                t.insert(*f);
            }
            _ => {}
        }
    }
    (s, c, t)
}

/// A flag that links pages: not "always", not the engine's band, in range.
fn linking(f: u16) -> bool {
    f != 0 && f < flags::count() && !(0xF0..=0xFF).contains(&f)
}

/* ───────────────────────── Graph ───────────────────────── */

struct Entry {
    slot: u32,
    kind: PageKind,
    start: Start,
    chunk: u16,
    removed: bool,
}

/// The pages of one pack, each with the flags it reads before it runs.
fn pack_pages(p: &PackIn, place: &str, out: &mut Vec<(Page, Vec<(u16, LinkKind)>)>) {
    let Some(pack) = NpcPack::parse(p.bytes) else { return };
    let d = pack.d_block(p.bytes);
    let ladder = |slot: u32| -> Vec<(u16, Presence)> {
        crate::trigger::placement_ladder(d, pack.actors[slot as usize].placement)
            .iter()
            .map(|br| {
                let out = match br.outcome {
                    Outcome::Place { .. } => Presence::Here,
                    Outcome::Absent => Presence::Away,
                    Outcome::Arrival => Presence::Arrival,
                };
                (br.cond, out)
            })
            .collect()
    };
    let mut entries: Vec<Entry> = Vec::new();
    for e in scene::catalog(&pack, p.bytes) {
        let look = ActorLook::of(&pack.actors[e.slot as usize]);
        let kind = match e.kind {
            SceneKind::Trigger => PageKind::Scene,
            SceneKind::Talk if look.drawn() && !matches!(look.kind & 0xF, 2 | 3) => PageKind::Talk,
            SceneKind::Talk => PageKind::Examine,
        };
        let start = match e.kind {
            SceneKind::Trigger => Start::WalkIn { x: e.x, z: e.z, flag: e.flag },
            SceneKind::Talk => Start::Talk { ladder: ladder(e.slot) },
        };
        entries.push(Entry { slot: e.slot, kind, start, chunk: e.chunk, removed: e.removed });
    }
    // Examine spots (type-4 records: the scene catalogue leaves them out) and
    // silent programs with effects.
    let listed: BTreeSet<u32> = entries.iter().map(|e| e.slot).collect();
    for a in pack.actors.iter().filter(|a| a.slot >= 2 && !listed.contains(&a.slot)) {
        let look = ActorLook::of(a);
        let home = (a.b & 0xFFFF) as u16;
        if a.script == 0 || a.script as usize >= d.len() || look.kind & 0xF == 5 || !crate::world::valid_chunk(home as u32) {
            continue;
        }
        let Ok(code) = tier2::disassemble(d, a.script) else { continue };
        let effects = code.iter().any(|i| matches!(i.op, 0x04..=0x08 | 0x16 | 0x19 | 0x28 | 0x33));
        if look.kind & 0xF == 4 {
            if effects || code.iter().any(|i| (0x01..=0x03).contains(&i.op)) {
                entries.push(Entry { slot: a.slot, kind: PageKind::Spot, start: Start::Talk { ladder: Vec::new() }, chunk: home, removed: false });
            }
        } else if effects {
            entries.push(Entry { slot: a.slot, kind: PageKind::Silent, start: Start::Talk { ladder: ladder(a.slot) }, chunk: home, removed: crate::edit::remove::placement_removed(d, a.placement, 0) });
        }
    }
    for e in entries {
        let a = &pack.actors[e.slot as usize];
        let Some((flow, code)) = flow(d, a.script) else { continue };
        let (sets, clears, tests) = flag_use(&code);
        let (nodes, stages, branches) = counts(&flow);
        let (speaker, line) = first_say(&flow);
        let mut reads: Vec<(u16, LinkKind)> = Vec::new();
        match &e.start {
            Start::WalkIn { flag, .. } => reads.push((*flag, LinkKind::Unlocks)),
            Start::Talk { ladder } => {
                for &(cond, out) in ladder {
                    reads.push((cond, if out == Presence::Away { LinkKind::Removes } else { LinkKind::Unlocks }));
                }
            }
            Start::Step { .. } => {}
        }
        for w in placement::walk_flags(d, a.placement, (a.d >> 8) as u8) {
            reads.push((w.flag, if w.read == WalkRead::Wait { LinkKind::Cues } else { LinkKind::Removes }));
        }
        reads.extend(tests.iter().map(|&f| (f, LinkKind::Changes)));
        let page = Page {
            id: format!("{}.{}", p.index, e.slot),
            pack: p.index,
            slot: Some(e.slot),
            kind: e.kind,
            chunk: e.chunk,
            place: place.to_string(),
            speaker,
            line,
            start: e.start,
            chapter: None,
            layer: 0,
            added: e.slot as usize >= p.retail_actors,
            removed: e.removed,
            script: a.script,
            stmts: code.iter().filter(|i| !matches!(i.op, 0x0D..=0x0F)).count() as u32,
            nodes,
            stages,
            branches,
            flow,
            sets: sets.into_iter().collect(),
            clears: clears.into_iter().collect(),
            tests: tests.into_iter().collect(),
        };
        out.push((page, reads));
    }
    // Tier-1 step records.
    for g in pack.triggers(p.bytes) {
        if g.chunk < 0 {
            continue;
        }
        for r in &g.rungs {
            for rec in &r.records {
                let (at, x, z, flag, set) = match *rec {
                    TriggerRecord::Set { at, x, z, flag } => (at, x, z, flag, true),
                    TriggerRecord::Clear { at, x, z, flag } => (at, x, z, flag, false),
                    _ => continue,
                };
                let page = Page {
                    id: format!("{}.t{at}", p.index),
                    pack: p.index,
                    slot: None,
                    kind: PageKind::Step,
                    chunk: g.chunk as u16,
                    place: place.to_string(),
                    speaker: None,
                    line: None,
                    start: Start::Step { x, z, flag, set, rung: r.flag },
                    chapter: None,
                    layer: 0,
                    added: false,
                    removed: crate::edit::trigger::is_off(x, z),
                    script: 0,
                    stmts: 1,
                    nodes: 1,
                    stages: 0,
                    branches: 0,
                    flow: vec![Step { at, node: Node::Flag { flag, set } }],
                    sets: if set { vec![flag] } else { Vec::new() },
                    clears: if set { Vec::new() } else { vec![flag] },
                    tests: Vec::new(),
                };
                out.push((page, if r.flag != 0 { vec![(r.flag, LinkKind::Unlocks)] } else { Vec::new() }));
            }
        }
    }
}

/// The story graph of `packs` (pass every pack once; `npc/17` aliases `npc/0`).
pub fn build(packs: &[PackIn]) -> Graph {
    let places = flags::places();
    let mut rows: Vec<(Page, Vec<(u16, LinkKind)>)> = Vec::new();
    let mut exits = BTreeSet::new();
    for p in packs {
        let place = places.get(&p.index).cloned().unwrap_or_else(|| format!("npc/{}", p.index));
        pack_pages(p, &place, &mut rows);
        pack_exits(p, &place, &places, &mut exits);
    }
    let (mut pages, reads): (Vec<Page>, Vec<Vec<(u16, LinkKind)>>) = rows.into_iter().unzip();

    // Links: every page that sets a flag → every page that reads it.
    let mut writers: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    let mut clearers: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    for (k, pg) in pages.iter().enumerate() {
        for &f in pg.sets.iter().filter(|f| linking(**f)) {
            writers.entry(f).or_default().push(k);
        }
        for &f in pg.clears.iter().filter(|f| linking(**f)) {
            clearers.entry(f).or_default().push(k);
        }
    }
    let mut links: BTreeSet<Link> = BTreeSet::new();
    let mut unset: BTreeSet<u16> = BTreeSet::new();
    for (k, rs) in reads.iter().enumerate() {
        for &(f, kind) in rs.iter().filter(|(f, _)| linking(*f)) {
            // Scratch flags outlive a pack change (until `SET 0xFD`), so they link within one place.
            let same = |w: &usize| f >= 0x40 || pages[*w].place == pages[k].place;
            let ws: Vec<usize> = writers.get(&f).map(|v| v.iter().copied().filter(same).collect()).unwrap_or_default();
            if ws.is_empty() {
                unset.insert(f);
            }
            for w in ws.into_iter().filter(|w| *w != k) {
                links.insert(Link { from: w as u32, to: k as u32, kind, flag: f });
            }
            if kind == LinkKind::Unlocks {
                for &c in clearers.get(&f).map(Vec::as_slice).unwrap_or(&[]).iter().filter(|c| same(c) && **c != k) {
                    links.insert(Link { from: c as u32, to: k as u32, kind: LinkKind::Clears, flag: f });
                }
            }
        }
    }
    let links: Vec<Link> = links.into_iter().collect();

    // Story order: the longest path over unlock and cue links, back edges dropped.
    let n = pages.len();
    let mut next: Vec<Vec<Option<usize>>> = vec![Vec::new(); n];
    for l in links.iter().filter(|l| matches!(l.kind, LinkKind::Unlocks | LinkKind::Cues)) {
        next[l.from as usize].push(Some(l.to as usize));
    }
    let mut state = vec![0u8; n];
    let mut order = Vec::with_capacity(n);
    for s in 0..n {
        if state[s] != 0 {
            continue;
        }
        let mut stack = vec![(s, 0usize)];
        state[s] = 1;
        while let Some((v, i)) = stack.pop() {
            if i < next[v].len() {
                stack.push((v, i + 1));
                let Some(w) = next[v][i] else { continue };
                match state[w] {
                    0 => {
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    1 => next[v][i] = None,
                    _ => {}
                }
            } else {
                state[v] = 2;
                order.push(v);
            }
        }
    }
    let mut layer = vec![0u32; n];
    for &v in order.iter().rev() {
        for w in next[v].iter().flatten() {
            layer[*w] = layer[*w].max(layer[v] + 1);
        }
    }

    // Chapter: the curated chapter of what the page sets, else of what unlocks it.
    let names = flags::meanings();
    for (k, pg) in pages.iter_mut().enumerate() {
        pg.layer = layer[k];
        let ch = |f: &u16| {
            let pack = (*f < 0x40).then_some(pg.pack);
            names.get(&(*f, pack)).or_else(|| names.get(&(*f, None))).and_then(|m| m.chapter)
        };
        pg.chapter = pg.sets.iter().filter_map(ch).min().or_else(|| reads[k].iter().filter(|(_, kd)| *kd == LinkKind::Unlocks).filter_map(|(f, _)| ch(f)).max());
    }
    Graph { pages, links, unset: unset.into_iter().collect(), exits: exits.into_iter().collect() }
}

/* ───────────────────────── Back-links ───────────────────────── */

/// What a node touches, keyed the way screen 21's reverse index keys it
/// (`keysOf` in `screens/21-story/model.ts`; [`UseKey`]'s `Display` prints
/// the same strings): `text:6:4292`, `flag:151`, `flag:5@Orgo`, `item:12`,
/// `chunk:78`, `battle:257`, `shop:3`, `sfx:40`, `bgm:7`. Sounds differ: the
/// screen keys them by the bank and cue they play (`sound:/p/audio?…`, which
/// needs the BGM a page loaded before), these by the raw operand.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UseKey {
    /// A pool line, by its D-offset in a pack.
    Text { pack: u16, offset: u16 },
    /// A flag. Scratch flags (0x000–0x03F) carry the place they mean
    /// something in; [`keys_of`] gives `None` for the others. As a query to
    /// [`uses`], a scratch flag with `place: None` matches in every place.
    Flag { flag: u16, place: Option<String> },
    /// An item, or a Living Toy as 1000 + n.
    Item(u16),
    Chunk(u16),
    /// A zone (< 0x100) or story battle (≥ 0x100).
    Battle(u16),
    Shop(u16),
    /// An SFX request word (`0x201` = slot 2, cue 1) or BGM operand, as written.
    Sfx(u16),
    Bgm(u16),
}

impl std::fmt::Display for UseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UseKey::Text { pack, offset } => write!(f, "text:{pack}:{offset}"),
            UseKey::Flag { flag, place: Some(p) } => write!(f, "flag:{flag}@{p}"),
            UseKey::Flag { flag, place: None } => write!(f, "flag:{flag}"),
            UseKey::Item(n) => write!(f, "item:{n}"),
            UseKey::Chunk(n) => write!(f, "chunk:{n}"),
            UseKey::Battle(n) => write!(f, "battle:{n}"),
            UseKey::Shop(n) => write!(f, "shop:{n}"),
            UseKey::Sfx(n) => write!(f, "sfx:{n}"),
            UseKey::Bgm(n) => write!(f, "bgm:{n}"),
        }
    }
}

/// The keys one node of `page` touches (its branch lanes not included).
pub fn keys_of(node: &Node, page: &Page) -> Vec<UseKey> {
    let flag = |f: u16| UseKey::Flag { flag: f, place: (f < 0x40).then(|| page.place.clone()) };
    let num = |v: i64| u16::try_from(v).ok();
    match node {
        Node::Say { texts, .. } => texts.iter().map(|&offset| UseKey::Text { pack: page.pack, offset }).collect(),
        Node::Objective { offset, .. } => vec![UseKey::Text { pack: page.pack, offset: *offset }],
        Node::Flag { flag: f, .. } => vec![flag(*f)],
        Node::Branch { kind: "flag", value, .. } => num(*value).map(flag).into_iter().collect(),
        Node::Branch { kind: "item", value, .. } => num(*value).map(UseKey::Item).into_iter().collect(),
        Node::Give { item } => vec![UseKey::Item(*item)],
        Node::Chest { flag: f, item } => vec![UseKey::Item(*item), flag(*f)],
        Node::Warp { chunk, .. } | Node::Inn { chunk, .. } => vec![UseKey::Chunk(*chunk)],
        Node::Battle { id } => vec![UseKey::Battle(*id)],
        Node::Shop { id } => vec![UseKey::Shop(*id)],
        Node::Stage { sfx, bgm, .. } => sfx.iter().map(|&n| UseKey::Sfx(n)).chain(bgm.iter().map(|&n| UseKey::Bgm(n))).collect(),
        _ => Vec::new(),
    }
}

/// Every node that touches `key`, as (page index, node address), in page
/// order and then flow order (a branch before its lanes).
pub fn uses(graph: &Graph, key: &UseKey) -> Vec<(u32, u32)> {
    let hit = |k: &UseKey| match (k, key) {
        (UseKey::Flag { flag: a, place: pa }, UseKey::Flag { flag: b, place: pb }) => a == b && (*a >= 0x40 || pb.is_none() || pa == pb),
        _ => k == key,
    };
    fn walk(flow: &[Step], page: &Page, k: u32, hit: &dyn Fn(&UseKey) -> bool, out: &mut Vec<(u32, u32)>) {
        for s in flow {
            if keys_of(&s.node, page).iter().any(hit) {
                out.push((k, s.at));
            }
            if let Node::Branch { then, els, .. } = &s.node {
                walk(then, page, k, hit, out);
                walk(els, page, k, hit, out);
            }
        }
    }
    let mut out = Vec::new();
    for (k, p) in graph.pages.iter().enumerate() {
        walk(&p.flow, p, k as u32, &hit, &mut out);
    }
    out
}

/// The node at `at` in a flow (a branch before its lanes; labels skipped).
pub fn step_at(flow: &[Step], at: u32) -> Option<&Step> {
    for s in flow {
        if s.at == at && !matches!(s.node, Node::Label { .. }) {
            return Some(s);
        }
        if let Node::Branch { then, els, .. } = &s.node {
            if let Some(x) = step_at(then, at).or_else(|| step_at(els, at)) {
                return Some(x);
            }
        }
    }
    None
}
