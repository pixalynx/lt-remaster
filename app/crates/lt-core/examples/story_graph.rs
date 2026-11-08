//! Research 08 (story graph): every retail event as a *page* of *nodes*, and
//! the *links* between pages: the flags one page sets and another reads, the
//! maps it warps to, the battles it starts and the code it shares. Prints the
//! survey and writes the whole graph as JSON for the prototype viewer.
//!
//! `cargo run --release --example story_graph -- <disc.bin> <out.json | out.js>`
//!
//! A page is one program a player can start: a walk-in scene (a type-10
//! record wakes its actor), a conversation (talking to an actor), an examine
//! spot (an object's program), a silent actor program with effects, or a
//! Tier-1 step record that sets or clears a flag. Its flow is the program's
//! structured tree ([`ast::lift`]) with consecutive lines of one speaker
//! grouped into a dialogue node and camera/wait/animation ops into staging.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use lt_core::gamedata::{GameData, Source};
use lt_core::npc::{ActorLook, NpcPack};
use lt_core::placement::{self, WalkRead};
use lt_core::scene::{self, SceneKind};
use lt_core::tier2::ast::{self, CodeRef, Stmt, TextRef, Val, WalkPath};
use lt_core::tier2::{self, Operand};
use lt_core::trigger::{Outcome, TriggerRecord};
use lt_core::worldmap::WorldDisc;
use lt_core::{flags, text, DiscImage, FileTable, PsxExe};

/// `FileTable_NpcBin` entry 17 aliases entry 0.
const PACK_ALIAS: u32 = 17;

/// A node and the D-relative address of its first instruction, which screen
/// 11 can select and the Story screen can name in a link (`?at=`).
#[derive(Serialize)]
struct Step {
    at: u32,
    #[serde(flatten)]
    node: Node,
}

#[derive(Serialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum Node {
    /// Lines in a row from one speaker. `ask`: the last line is a SAY3 question.
    Say {
        speaker: Option<String>,
        lines: Vec<String>,
        /// D-relative offset of each line (screen 12 opens `?pack=&line=`).
        texts: Vec<u16>,
    },
    /// Camera, waits, animation, facing, fades, sound: shown as one slim row.
    Stage {
        ops: Vec<String>,
        /// Sound requests (SFX and BGM operands) for links to screen 14.
        sfx: Vec<u16>,
        bgm: Vec<u16>,
    },
    Flag {
        flag: u16,
        set: bool,
        name: Option<String>,
    },
    /// An IF card: `then` is the True/Yes/Has lane, `els` the other.
    Branch {
        kind: &'static str,
        label: String,
        flag: Option<u16>,
        yes: String,
        no: String,
        then: Vec<Step>,
        els: Vec<Step>,
    },
    Walk {
        who: &'static str,
        x: i16,
        z: i16,
    },
    Warp {
        chunk: u16,
        x: i16,
        z: i16,
    },
    Inn {
        chunk: u16,
    },
    Battle {
        id: u16,
    },
    Item {
        label: String,
        /// Item id (< 500), Living Toy (≥ 1000), none for rubies.
        item: Option<u16>,
    },
    Shop {
        id: u16,
    },
    Objective {
        text: String,
        offset: u16,
    },
    Jump {
        label: String,
        page: Option<String>,
        back: bool,
    },
    Label {
        name: String,
    },
    Op {
        name: String,
    },
    End,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Start {
    /// The Knight walks onto (x, z) while `flag` is set (0 = always).
    WalkIn { x: i16, z: i16, flag: u16 },
    /// The player talks to (or examines) the actor while it is on the map.
    Talk { ladder: Vec<(u16, &'static str)> },
    /// A Tier-1 record: walking onto (x, z) sets or clears a flag.
    Step { x: i16, z: i16, flag: u16, set: bool, rung: u16 },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    id: String,
    pack: u32,
    slot: Option<u32>,
    kind: &'static str,
    chunk: u16,
    title: String,
    start: Start,
    chapter: Option<u8>,
    area: String,
    layer: u32,
    /// Instructions without block markers, and the flow's own counts.
    stmts: usize,
    nodes: usize,
    stages: usize,
    branches: usize,
    flow: Vec<Step>,
    sets: BTreeSet<u16>,
    clears: BTreeSet<u16>,
    tests: BTreeSet<u16>,
    exits: Vec<String>,
}

#[derive(Serialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
struct Link {
    from: usize,
    to: usize,
    /// unlocks · removes · changes · cues · clears
    kind: &'static str,
    flag: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Graph {
    pages: Vec<Page>,
    links: Vec<Link>,
    flags: BTreeMap<String, (String, String)>,
    places: BTreeMap<u32, String>,
}

/// What one pack's flow conversion needs.
struct Ctx<'a> {
    d: &'a [u8],
    pack: u32,
    items: &'a BTreeMap<u16, String>,
    names: &'a BTreeMap<(u16, Option<u16>), flags::Meaning>,
    /// Instruction address → page id, for GOTOs into another page's code.
    owner: &'a BTreeMap<u32, String>,
    /// Addresses of the program being converted (for back jumps).
    here: BTreeMap<String, u32>,
    speaker: Option<String>,
    last_line: Option<String>,
}

/// Markup without its `{…}` codes.
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

fn plain(d: &[u8], at: u16) -> String {
    let s = text::plain(&text::decode(text::c_string(d, at as usize)));
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Ctx<'_> {
    fn flag_name(&self, f: u16) -> Option<String> {
        let pack = (f < 0x40).then_some(self.pack as u16);
        self.names.get(&(f, pack)).or_else(|| self.names.get(&(f, None))).map(|m| m.name.clone())
    }

    fn item(&self, id: i64) -> String {
        let id = id as u16;
        self.items.get(&id).cloned().unwrap_or_else(|| format!("item {id}"))
    }

    fn text(&self, v: &Val) -> String {
        match v {
            Val::Text(TextRef::Pool(o)) => plain(self.d, *o),
            _ => String::new(),
        }
    }

    fn flow(&mut self, stmts: &[Stmt], at: &mut dyn Iterator<Item = u32>) -> Vec<Step> {
        let mut out: Vec<Step> = Vec::new();
        let mut last = 0u32;
        for s in stmts {
            match s {
                Stmt::Label { name, .. } => out.push(Step { at: last, node: Node::Label { name: name.clone() } }),
                Stmt::If { op, arg, then, els, .. } => {
                    let cond_at = at.next().unwrap_or(0);
                    at.next(); // 0D
                    let num = |a: &Option<Val>| match a {
                        Some(Val::Num(n)) => *n,
                        _ => 0,
                    };
                    let (kind, label, flag, yes, no) = match op {
                        0x0A => ("yes", self.last_line.clone().unwrap_or_else(|| "Yes or no?".into()), None, "Yes".into(), "No".into()),
                        0x0B => {
                            let f = num(arg) as u16;
                            let n = self.flag_name(f).unwrap_or_else(|| format!("0x{f:03X}"));
                            ("flag", n, Some(f), "True".into(), "False".into())
                        }
                        0x0C => ("rubies", format!("Rubies ≥ {}", num(arg)), None, "Enough".into(), "Not enough".into()),
                        0x1D | 0x1E => ("item", format!("Has {}", self.item(num(arg))), None, "Has it".into(), "Doesn't".into()),
                        0x3A => {
                            // Two answers joined by a newline.
                            let raw = match arg {
                                Some(Val::Text(TextRef::Pool(p))) => text::markup_at(self.d, *p as usize),
                                _ => String::new(),
                            };
                            let parts: Vec<String> = raw.split("{nl}").map(strip_markup).collect();
                            let a = parts.first().cloned().unwrap_or_default();
                            let b = parts.get(1).cloned().unwrap_or_default();
                            ("choice", self.last_line.clone().unwrap_or_else(|| "Choose".into()), None, a, b)
                        }
                        _ => ("other", ast::mnemonic(*op).name.to_string(), None, "Then".into(), "Else".into()),
                    };
                    let speaker = self.speaker.clone();
                    let then = self.flow(then, at);
                    at.next(); // 0E
                    self.speaker = speaker.clone();
                    let els = self.flow(els, at);
                    at.next(); // 0F
                    self.speaker = speaker;
                    out.push(Step { at: cond_at, node: Node::Branch { kind, label, flag, yes, no, then, els } });
                }
                Stmt::Op { op, args, .. } => {
                    let here = at.next();
                    let h = here.unwrap_or(0);
                    last = h;
                    let off = |k: usize| match args.get(k) {
                        Some(Val::Text(TextRef::Pool(o))) => *o,
                        _ => 0,
                    };
                    let n = |k: usize| match args.get(k) {
                        Some(Val::Num(v)) => *v,
                        _ => 0,
                    };
                    let node = match op {
                        0x1F => {
                            self.speaker = args.first().map(|a| self.text(a)).filter(|s| !s.is_empty());
                            continue;
                        }
                        0x20 => {
                            self.speaker = None;
                            continue;
                        }
                        0x01..=0x03 => {
                            let line = args.first().map(|a| self.text(a)).unwrap_or_default();
                            self.last_line = Some(line.clone());
                            if let Some(Step { node: Node::Say { speaker, lines, texts }, .. }) = out.last_mut() {
                                if *speaker == self.speaker {
                                    lines.push(line);
                                    texts.push(off(0));
                                    continue;
                                }
                            }
                            Node::Say { speaker: self.speaker.clone(), lines: vec![line], texts: vec![off(0)] }
                        }
                        0x04 | 0x05 => {
                            let f = n(0) as u16;
                            Node::Flag { flag: f, set: *op == 0x04, name: self.flag_name(f) }
                        }
                        0x1C | 0x34 => {
                            let who = if *op == 0x1C { "Knight" } else { "Knight + Baby" };
                            let p = match args.first() {
                                Some(Val::Walk(t)) => WalkPath::read(self.d, *t as usize),
                                _ => None,
                            };
                            Node::Walk { who, x: p.as_ref().map_or(0, |p| p.x), z: p.as_ref().map_or(0, |p| p.z) }
                        }
                        0x19 => Node::Warp { chunk: n(1) as u16, x: n(2) as i16, z: n(3) as i16 },
                        0x06 => Node::Inn { chunk: n(0) as u16 },
                        0x08 => Node::Battle { id: n(0) as u16 },
                        0x16 => Node::Item { label: format!("Give {}", self.item(n(0))), item: Some(n(0) as u16) },
                        0x28 => Node::Item { label: format!("Chest: {} (0x{:03X})", self.item(n(1)), n(0)), item: Some(n(1) as u16) },
                        0x33 => Node::Item { label: format!("Rubies {:+}", n(0)), item: None },
                        0x07 => Node::Shop { id: n(0) as u16 },
                        0x44 => Node::Objective { text: args.first().map(|a| self.text(a)).unwrap_or_default(), offset: off(0) },
                        0x2E | 0x2F => {
                            let (label, page, back) = match args.first() {
                                Some(Val::Code(CodeRef::Label(l))) => {
                                    let back = matches!((self.here.get(l), here), (Some(t), Some(h)) if *t <= h);
                                    (l.clone(), None, back)
                                }
                                Some(Val::Code(CodeRef::At(t))) => (format!("D+0x{t:04X}"), self.owner.get(&(*t as u32)).cloned(), false),
                                _ => (String::new(), None, false),
                            };
                            Node::Jump { label, page, back }
                        }
                        0x00 => Node::End,
                        0x10..=0x15 | 0x18 | 0x1B | 0x22..=0x27 | 0x29..=0x2C | 0x30 | 0x32 | 0x35..=0x39 | 0x41..=0x43 | 0x45..=0x49 | 0x09 | 0x1A => {
                            let name = ast::mnemonic(*op).name.to_string();
                            let (sfx, bgm) = match op {
                                0x12 => (vec![n(0) as u16], vec![]),
                                0x22 => (vec![], vec![n(0) as u16]),
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
                        _ => Node::Op { name: ast::mnemonic(*op).name.to_string() },
                    };
                    out.push(Step { at: h, node });
                }
            }
        }
        out
    }
}

/// Story nodes, staging rows and branch cards in a flow.
fn counts(flow: &[Step]) -> (usize, usize, usize) {
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

fn exits(flow: &[Step], out: &mut Vec<String>) {
    for x in flow {
        match &x.node {
            Node::Warp { chunk, .. } => out.push(format!("warp chunk/{chunk}")),
            Node::Inn { chunk } => out.push(format!("inn chunk/{chunk}")),
            Node::Battle { id } => out.push(format!("battle 0x{id:X}")),
            Node::Shop { id } => out.push(format!("shop {id}")),
            Node::Jump { page: Some(p), .. } => out.push(format!("page {p}")),
            Node::Branch { then, els, .. } => {
                exits(then, out);
                exits(els, out);
            }
            _ => {}
        }
    }
}

/// Flags a program sets, clears and tests (chests set theirs).
fn flag_use(code: &[tier2::Instr]) -> (BTreeSet<u16>, BTreeSet<u16>, BTreeSet<u16>) {
    let (mut s, mut c, mut t) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for i in code {
        match (i.op, i.operands.first()) {
            (0x04, Some(Operand::Flag(f))) | (0x28, Some(Operand::Flag(f))) => {
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

/// A flag that links pages: not "always", not the engine's band, not the no-op.
fn linking(f: u16) -> bool {
    f != 0 && f < flags::count() && !(0xF0..=0xFF).contains(&f)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let image = DiscImage::open(&a[1]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let names = flags::meanings();
    let mut items: BTreeMap<u16, String> = BTreeMap::new();
    if let Ok(gd) = GameData::load(Source::new(&image, &exe, &table)) {
        for it in &gd.items.items {
            items.insert(it.id, it.name.clone());
        }
        for t in &gd.toys.roster {
            items.insert(1000 + t.row as u16, format!("{} (toy)", t.name));
        }
    }
    let curated: serde_json::Value = serde_json::from_str(include_str!("../../../src/data/flag-catalogue.json")).unwrap();
    let mut places = BTreeMap::new();
    for (k, v) in curated["places"].as_object().unwrap() {
        if let (Some(n), Some(s)) = (k.strip_prefix("npc/").and_then(|n| n.parse::<u32>().ok()), v.as_str()) {
            places.insert(n, s.split(" — ").next().unwrap_or(s).to_string());
        }
    }

    let mut pages: Vec<Page> = Vec::new();
    // Per page: (pack, gate reads, presence reads, walk reads) for the link pass.
    let mut gates: Vec<Vec<(u16, &'static str)>> = Vec::new();
    let mut programs = 0usize;
    let mut stats: BTreeMap<&str, usize> = BTreeMap::new();
    for p in (0..50u32).filter(|&p| p != PACK_ALIAS) {
        let Ok(b) = wd.file(&format!("npc/{p}")) else {
            continue;
        };
        let Some(pack) = NpcPack::parse(&b) else {
            continue;
        };
        let d = pack.d_block(&b);
        let area = places.get(&p).cloned().unwrap_or_else(|| format!("npc/{p}"));
        // Who starts how: the scene catalogue (walk-ins first, then conversations).
        let cat = scene::catalog(&pack, &b);
        let mut entries: Vec<(u32, &'static str, Start, u16)> = Vec::new();
        let ladder = |slot: u32| -> Vec<(u16, &'static str)> {
            let a = &pack.actors[slot as usize];
            lt_core::trigger::placement_ladder(d, a.placement)
                .iter()
                .map(|br| {
                    (
                        br.cond,
                        match br.outcome {
                            Outcome::Place { .. } => "here",
                            Outcome::Absent => "away",
                            Outcome::Arrival => "arrival",
                        },
                    )
                })
                .collect()
        };
        for e in &cat {
            let a = &pack.actors[e.slot as usize];
            let look = ActorLook::of(a);
            let kind = match e.kind {
                SceneKind::Trigger => "scene",
                SceneKind::Talk if look.drawn() && !matches!(look.kind & 0xF, 2 | 3) => "talk",
                SceneKind::Talk => "examine",
            };
            let start = match e.kind {
                SceneKind::Trigger => Start::WalkIn { x: e.x, z: e.z, flag: e.flag },
                SceneKind::Talk => Start::Talk { ladder: ladder(e.slot) },
            };
            entries.push((e.slot, kind, start, e.chunk));
        }
        // Examine spots (type-4 records: run as the Knight's script on ✕, so the
        // scene catalogue leaves them out), and silent programs with effects.
        let listed: BTreeSet<u32> = entries.iter().map(|e| e.0).collect();
        for a in pack.actors.iter().filter(|a| a.slot >= 2 && !listed.contains(&a.slot)) {
            let look = ActorLook::of(a);
            let home = (a.b & 0xFFFF) as u16;
            if a.script == 0 || a.script as usize >= d.len() || look.kind & 0xF == 5 || !lt_core::world::valid_chunk(home as u32) {
                continue;
            }
            let Ok(code) = tier2::disassemble(d, a.script) else {
                continue;
            };
            let effects = code.iter().any(|i| matches!(i.op, 0x04 | 0x05 | 0x06 | 0x07 | 0x08 | 0x16 | 0x19 | 0x28 | 0x33));
            if look.kind & 0xF == 4 {
                if effects || code.iter().any(|i| (0x01..=0x03).contains(&i.op)) {
                    entries.push((a.slot, "spot", Start::Talk { ladder: Vec::new() }, home));
                }
            } else if effects {
                entries.push((a.slot, "silent", Start::Talk { ladder: ladder(a.slot) }, home));
            }
        }
        // Every instruction of every entry program, for GOTOs between pages.
        let mut owner: BTreeMap<u32, String> = BTreeMap::new();
        for (slot, ..) in &entries {
            if let Ok(code) = tier2::disassemble(d, pack.actors[*slot as usize].script) {
                for i in &code {
                    owner.entry(i.at).or_insert_with(|| format!("{p}.{slot}"));
                }
            }
        }
        for (slot, kind, start, chunk) in entries {
            let a = &pack.actors[slot as usize];
            let Ok(code) = tier2::disassemble(d, a.script) else {
                *stats.entry("disasm errors").or_default() += 1;
                continue;
            };
            programs += 1;
            let stmts = ast::lift(&code, &TextRef::Pool);
            let labels: BTreeMap<String, u32> = {
                let starts: BTreeSet<u32> = code.iter().map(|i| i.at).collect();
                let mut t: Vec<u32> = code
                    .iter()
                    .flat_map(|i| i.operands.iter())
                    .filter_map(|o| match o {
                        Operand::Code(t) if starts.contains(&(*t as u32)) => Some(*t as u32),
                        _ => None,
                    })
                    .collect();
                t.sort_unstable();
                t.dedup();
                t.iter().enumerate().map(|(k, &v)| (format!("L{}", k + 1), v)).collect()
            };
            let mut cx = Ctx { d, pack: p, items: &items, names: &names, owner: &owner, here: labels, speaker: None, last_line: None };
            let mut ats = code.iter().map(|i| i.at);
            let flow = cx.flow(&stmts, &mut ats);
            let (sets, clears, tests) = flag_use(&code);
            let (nodes, stages, branches) = counts(&flow);
            let mut ex = Vec::new();
            exits(&flow, &mut ex);
            let (speaker, first) = first_say(&flow);
            let title = match (&speaker, &first) {
                (Some(s), Some(l)) => format!("{s}: {}", l.chars().take(48).collect::<String>()),
                (None, Some(l)) => l.chars().take(56).collect(),
                _ => format!("actor {slot}"),
            };
            // What the page reads before it runs: its rung, its ladder, its walk waits.
            let mut reads: Vec<(u16, &'static str)> = Vec::new();
            match &start {
                Start::WalkIn { flag, .. } => reads.push((*flag, "unlocks")),
                Start::Talk { ladder } => {
                    for (cond, out) in ladder {
                        if *cond > 0 && *cond < flags::count() {
                            reads.push((*cond, if *out == "away" { "removes" } else { "unlocks" }));
                        }
                    }
                }
                Start::Step { .. } => {}
            }
            let typ = (a.d >> 8) as u8;
            for w in placement::walk_flags(d, a.placement, typ) {
                reads.push((w.flag, if w.read == WalkRead::Wait { "cues" } else { "removes" }));
            }
            for &f in &tests {
                reads.push((f, "changes"));
            }
            *stats.entry(kind).or_default() += 1;
            pages.push(Page {
                id: format!("{p}.{slot}"),
                pack: p,
                slot: Some(slot),
                kind,
                chunk,
                title,
                start,
                chapter: None,
                area: area.clone(),
                layer: 0,
                stmts: code.iter().filter(|i| !matches!(i.op, 0x0D..=0x0F)).count(),
                nodes,
                stages,
                branches,
                flow,
                sets,
                clears,
                tests,
                exits: ex,
            });
            gates.push(reads);
        }
        // Tier-1 step records: set or clear a flag where the Knight walks.
        for g in pack.triggers(&b) {
            if g.chunk < 0 {
                continue;
            }
            for r in &g.rungs {
                for rec in &r.records {
                    let (x, z, flag, set) = match *rec {
                        TriggerRecord::Set { x, z, flag, .. } => (x, z, flag, true),
                        TriggerRecord::Clear { x, z, flag, .. } => (x, z, flag, false),
                        _ => continue,
                    };
                    let name = names.get(&(flag, (flag < 0x40).then_some(p as u16))).or_else(|| names.get(&(flag, None))).map(|m| m.name.clone());
                    let (s, c) = if set { (BTreeSet::from([flag]), BTreeSet::new()) } else { (BTreeSet::new(), BTreeSet::from([flag])) };
                    *stats.entry("step").or_default() += 1;
                    pages.push(Page {
                        id: format!("{p}.t{}", rec_at(rec)),
                        pack: p,
                        slot: None,
                        kind: "step",
                        chunk: g.chunk as u16,
                        title: format!("Step: {} {}", if set { "set" } else { "clear" }, name.clone().unwrap_or_else(|| format!("0x{flag:03X}"))),
                        start: Start::Step { x, z, flag, set, rung: r.flag },
                        chapter: None,
                        area: area.clone(),
                        layer: 0,
                        stmts: 1,
                        nodes: 1,
                        stages: 0,
                        branches: 0,
                        flow: vec![Step { at: rec_at(rec), node: Node::Flag { flag, set, name } }],
                        sets: s,
                        clears: c,
                        tests: BTreeSet::new(),
                        exits: Vec::new(),
                    });
                    gates.push(if r.flag != 0 { vec![(r.flag, "unlocks")] } else { Vec::new() });
                }
            }
        }
    }

    // Links: every page that sets a flag → every page that reads it. Scratch
    // flags (0x000–0x03F) mean something else in each pack, so they link
    // only inside one pack.
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
    let mut unread = 0usize;
    let mut orphan: BTreeMap<u16, usize> = BTreeMap::new();
    for (k, reads) in gates.iter().enumerate() {
        for &(f, kind) in reads.iter().filter(|(f, _)| linking(*f)) {
            let ws = writers.get(&f).map(Vec::as_slice).unwrap_or(&[]);
            // Scratch flags outlive a pack change (until `SET 0xFD`), so they link within one place.
            let same = |w: &usize| f >= 0x40 || pages[*w].area == pages[k].area;
            let mut any = false;
            for &w in ws.iter().filter(|w| same(w)) {
                any = true;
                if w != k {
                    links.insert(Link { from: w, to: k, kind, flag: f });
                }
            }
            if !any {
                unread += 1;
                *orphan.entry(f).or_default() += 1;
            }
            // Clearing a gate flag ends what it unlocked (a cue that's spent).
            if kind == "unlocks" {
                for &c in clearers.get(&f).map(Vec::as_slice).unwrap_or(&[]).iter().filter(|c| same(c) && **c != k) {
                    links.insert(Link { from: c, to: k, kind: "clears", flag: f });
                }
            }
        }
    }
    let links: Vec<Link> = links.into_iter().collect();

    // Story order: longest path over unlock/cue links, back edges dropped.
    let n = pages.len();
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for l in links.iter().filter(|l| matches!(l.kind, "unlocks" | "cues")) {
        out[l.from].push(l.to);
    }
    let mut state = vec![0u8; n];
    let mut order = Vec::with_capacity(n);
    let mut back = 0usize;
    for s in 0..n {
        if state[s] != 0 {
            continue;
        }
        let mut stack = vec![(s, 0usize)];
        state[s] = 1;
        while let Some((v, i)) = stack.pop() {
            if i < out[v].len() {
                stack.push((v, i + 1));
                let w = out[v][i];
                match state[w] {
                    0 => {
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    1 => {
                        out[v][i] = usize::MAX;
                        back += 1;
                    }
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
        for &w in out[v].iter().filter(|w| **w != usize::MAX) {
            layer[w] = layer[w].max(layer[v] + 1);
        }
    }
    // Chapter: the curated chapter of what the page sets, else of what unlocks it.
    for (k, pg) in pages.iter_mut().enumerate() {
        pg.layer = layer[k];
        let ch = |f: &u16| {
            let pack = (*f < 0x40).then_some(pg.pack as u16);
            names.get(&(*f, pack)).or_else(|| names.get(&(*f, None))).and_then(|m| m.chapter)
        };
        pg.chapter = pg.sets.iter().filter_map(ch).min().or_else(|| gates[k].iter().filter(|(_, kd)| *kd == "unlocks").filter_map(|(f, _)| ch(f)).max());
    }

    // Survey.
    let total = |f: fn(&Page) -> usize| pages.iter().map(f).sum::<usize>();
    let (st, nd, sg, br) = (total(|p| p.stmts), total(|p| p.nodes), total(|p| p.stages), total(|p| p.branches));
    println!("pages {} ({} start an actor or spot program) · {:?}", pages.len(), programs, stats);
    println!("instructions {st} → story nodes {nd} + staging rows {sg} (branch cards {br})");
    println!("per page: {:.1} instructions → {:.1} nodes + {:.1} staging", st as f64 / n as f64, nd as f64 / n as f64, sg as f64 / n as f64);
    let biggest = pages.iter().max_by_key(|p| p.nodes).unwrap();
    println!("largest page: {} ({}) {} nodes, {} instructions", biggest.id, biggest.title, biggest.nodes, biggest.stmts);
    let mut bk: BTreeMap<&str, usize> = BTreeMap::new();
    let mut jumps = (0usize, 0usize, 0usize, 0usize);
    fn walk(f: &[Step], bk: &mut BTreeMap<&'static str, usize>, j: &mut (usize, usize, usize, usize)) {
        for x in f {
            match &x.node {
                Node::Branch { kind, then, els, .. } => {
                    *bk.entry(*kind).or_default() += 1;
                    walk(then, bk, j);
                    walk(els, bk, j);
                }
                Node::Jump { page: Some(_), .. } => j.0 += 1,
                Node::Jump { back: true, .. } => j.1 += 1,
                Node::Jump { label, .. } if label.starts_with("D+") => j.2 += 1,
                Node::Jump { .. } => j.3 += 1,
                _ => {}
            }
        }
    }
    for p in &pages {
        walk(&p.flow, &mut bk, &mut jumps);
    }
    let with_jump = pages.iter().filter(|p| p.flow.iter().any(|x| matches!(x.node, Node::Jump { .. })) || has_nested_jump(&p.flow)).count();
    println!("branches {:?}", bk);
    println!(
        "jumps: to another page {} · back (loop) {} · into unlisted code {} · forward {} · pages with any jump {}",
        jumps.0, jumps.1, jumps.2, jumps.3, with_jump
    );
    let mut lk: BTreeMap<&str, usize> = BTreeMap::new();
    for l in &links {
        *lk.entry(l.kind).or_default() += 1;
    }
    let flags_linking: BTreeSet<u16> = links.iter().map(|l| l.flag).collect();
    let cross_pack = links.iter().filter(|l| pages[l.from].pack != pages[l.to].pack).count();
    let with_in: BTreeSet<usize> = links.iter().map(|l| l.to).collect();
    let with_out: BTreeSet<usize> = links.iter().map(|l| l.from).collect();
    let isolated = (0..n).filter(|k| !with_in.contains(k) && !with_out.contains(k)).count();
    println!("links {} {:?} · {} flags · {} across packs · reads with no writer {}", links.len(), lk, flags_linking.len(), cross_pack, unread);
    println!("pages with an incoming link {} · with an outgoing link {} · isolated {}", with_in.len(), with_out.len(), isolated);
    let mut top: Vec<(&u16, &usize)> = orphan.iter().collect();
    top.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    println!("read flags no page sets: {} flags ({} story-band)", orphan.len(), orphan.keys().filter(|f| **f >= 0x40).count());
    for (f, c) in top.iter().filter(|(f, _)| **f >= 0x40).take(20) {
        let m = names.get(&(**f, None)).map(|m| format!("{} — {}", m.name, m.summary)).unwrap_or_default();
        println!("  0x{f:03X} ×{c} {m}");
    }
    println!("story layers {} (back edges dropped {})", layer.iter().max().unwrap_or(&0) + 1, back);
    let mut ex: BTreeMap<String, usize> = BTreeMap::new();
    for p in &pages {
        for e in &p.exits {
            *ex.entry(e.split(' ').next().unwrap().to_string()).or_default() += 1;
        }
    }
    println!("exits {:?}", ex);
    let mut ch: BTreeMap<Option<u8>, usize> = BTreeMap::new();
    for p in &pages {
        *ch.entry(p.chapter).or_default() += 1;
    }
    println!("pages per chapter {:?}", ch);

    let flag_names =
        names.iter().map(|((f, p), m)| (format!("0x{f:03X}{}", p.map(|p| format!("@{p}")).unwrap_or_default()), (m.name.clone(), m.summary.clone()))).collect();
    let g = Graph { pages, links, flags: flag_names, places };
    // `.js` writes a script the viewer can load from disk (`window.GRAPH = …`).
    let json = serde_json::to_string(&g).unwrap();
    let body = if a[2].ends_with(".js") { format!("window.GRAPH = {json};\n") } else { json };
    std::fs::write(&a[2], body).unwrap();
}

/// The first speaker and line of a flow, taking the longer lane of each
/// branch (retail scenes open with "already played?" checks).
fn first_say(f: &[Step]) -> (Option<String>, Option<String>) {
    for x in f {
        match &x.node {
            Node::Say { speaker, lines, .. } => return (speaker.clone(), lines.first().map(|l| l.replace('|', " "))),
            Node::Branch { then, els, .. } => {
                let (a, b) = if counts(els).0 > counts(then).0 { (els, then) } else { (then, els) };
                match first_say(a) {
                    (_, None) => {
                        let r = first_say(b);
                        if r.1.is_some() {
                            return r;
                        }
                    }
                    r => return r,
                }
            }
            _ => {}
        }
    }
    (None, None)
}

fn has_nested_jump(f: &[Step]) -> bool {
    f.iter().any(|x| match &x.node {
        Node::Jump { .. } => true,
        Node::Branch { then, els, .. } => has_nested_jump(then) || has_nested_jump(els),
        _ => false,
    })
}

fn rec_at(r: &TriggerRecord) -> u32 {
    match *r {
        TriggerRecord::Door { at, .. }
        | TriggerRecord::Set { at, .. }
        | TriggerRecord::Clear { at, .. }
        | TriggerRecord::Talk { at, .. }
        | TriggerRecord::Other { at, .. } => at,
    }
}
