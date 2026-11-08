//! Tier-2 programs → NPC pack bytes (screen 11, PRD 11 §5.2 "Strategy A").
//!
//! An edited program is assembled into the pack's relocation area, after the
//! lines [`super::text`] moved there, together with the new strings it quotes.
//! Every actor record whose retail script pointed at the program's entry is
//! repointed to it. The retail bytes stay, so GOTOs from other programs and
//! anything the disassembler missed still land on working code.
//!
//! Edits are keyed by the program's retail entry. The project keeps only the
//! pack's bytes, so [`recover`] reads the edits back from them; the DSL is
//! printed again from those bytes, which means comments and label names are
//! not kept. Every save lays text and scripts out together from retail
//! ([`super::text::layout`]), so the result depends only on the edit set.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::npc::NpcPack;
use crate::text;
use crate::tier2::asm::{self, Limits};
use crate::tier2::ast::{self, CodeRef, Stmt, TextRef, Val, WalkPath};
use crate::tier2::dsl::{self, Diag, Severity};
use crate::tier2::{self, Operand};

use super::text::{edited_lines, TextReport};
use super::{EditError, Result};

/// Edited programs: retail entry (D-relative) → program.
pub type Scripts = BTreeMap<u32, Vec<Stmt>>;

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

/// Retail actor entries and the slots that use each.
fn entries(r: &NpcPack, retail: &[u8]) -> BTreeMap<u32, Vec<u32>> {
    let len = r.d_block(retail).len();
    let mut out: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for a in &r.actors {
        if a.script != 0 && (a.script as usize) < len {
            out.entry(a.script).or_default().push(a.slot);
        }
    }
    out
}

/// How the current bytes name text: relocated retail lines (current offset →
/// retail offset) and the retail D-span (below it every string is retail).
struct TextMap {
    relocated: BTreeMap<u32, u32>,
    /// Retail offset → current offset of the relocated lines.
    moved_to: BTreeMap<u32, u32>,
    span: u32,
}

impl TextMap {
    fn of(r: &NpcPack, retail: &[u8], current: &[u8]) -> Result<Self> {
        let mut relocated = BTreeMap::new();
        let mut moved_to = BTreeMap::new();
        for l in edited_lines(retail, current)? {
            if l.relocated {
                relocated.insert(l.at, l.off);
                moved_to.insert(l.off, l.at);
            }
        }
        Ok(Self { relocated, moved_to, span: r.d_span })
    }

    fn name(&self, cd: &[u8], t: u16) -> TextRef {
        let t32 = t as u32;
        match self.relocated.get(&t32) {
            Some(&off) => TextRef::Pool(off as u16),
            None if t32 < self.span => TextRef::Pool(t),
            None => TextRef::New(text::c_string(cd, t as usize).to_vec()),
        }
    }

    /// The program `code` of `cd` as a tree: text named by [`Self::name`],
    /// walk data past the retail D-span (written by the project) as paths.
    fn lift(&self, code: &[tier2::Instr], cd: &[u8]) -> Vec<Stmt> {
        let mut stmts = ast::lift(code, &|t| self.name(cd, t));
        ast::inline_paths(&mut stmts, self.span, cd);
        stmts
    }

    /// The current text of a pool string, for the DSL's comments.
    fn preview(&self, cd: &[u8], off: u16) -> Option<String> {
        let at = self.moved_to.get(&(off as u32)).copied().unwrap_or(off as u32) as usize;
        (at < cd.len()).then(|| text::plain(&text::decode(text::c_string(cd, at))).replace('|', " / "))
    }
}

/// Where each slot's script is now, for the slots of one retail entry.
fn current_entry(c: &NpcPack, slots: &[u32]) -> Result<u32> {
    let now: BTreeSet<u32> = slots.iter().filter_map(|&s| c.actors.get(s as usize).map(|a| a.script)).collect();
    match now.len() {
        1 => Ok(*now.first().expect("one")),
        _ => Err(EditError::new(
            "SCRIPT_SPLIT",
            format!("Actors {slots:?} share one retail program but point at different code in the project. Revert the pack."),
        )),
    }
}

/// The programs the project changed, read back from its bytes.
pub fn recover(retail: &[u8], current: &[u8]) -> Result<Scripts> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    if c.actors.len() < r.actors.len() {
        return Err(EditError::new("TEXT_LAYOUT_CHANGED", "This pack was replaced as a whole file; script edits need the retail layout. Revert it first."));
    }
    let map = TextMap::of(&r, retail, current)?;
    let cd = c.d_block(current);
    // Objects taken out point at the removal block's END, not at an edit.
    let silent = super::remove::silenced(retail, current);
    let mut out = Scripts::new();
    for (entry, slots) in entries(&r, retail) {
        let slots: Vec<u32> = slots.into_iter().filter(|s| !silent.contains(s)).collect();
        if slots.is_empty() {
            continue;
        }
        let at = current_entry(&c, &slots)?;
        if at == entry {
            continue;
        }
        let code = tier2::disassemble(cd, at)
            .map_err(|e| EditError::new("SCRIPT_UNREADABLE", format!("The project's program for D+0x{entry:04X} (at D+0x{at:04X}) doesn't decode: {e:?}.")))?;
        out.insert(entry, map.lift(&code, cd));
    }
    Ok(out)
}

/* ───────────────────────── Placing ───────────────────────── */

/// One edited program in the result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placed {
    /// Retail entry (the program's stable id).
    pub entry: u32,
    /// Where the program is now (D-relative).
    pub at: u32,
    /// Program bytes.
    pub len: u32,
    /// Actor records repointed.
    pub slots: Vec<u32>,
}

/// An added actor's program of its own in the result ([`super::actor`]).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacedOwn {
    /// Index among the added actors.
    pub added: usize,
    pub at: u32,
    pub len: u32,
}

/// The program at D-relative `at` of `current`, lifted for the DSL, its text
/// named as [`recover`] names it.
pub(crate) fn lift_at(retail: &[u8], current: &[u8], at: u32) -> Result<Vec<Stmt>> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let map = TextMap::of(&r, retail, current)?;
    let cd = c.d_block(current);
    let code = tier2::disassemble(cd, at).map_err(|e| EditError::new("SCRIPT_UNREADABLE", format!("The program at D+0x{at:04X} doesn't decode: {e:?}.")))?;
    Ok(map.lift(&code, cd))
}

/// Lays out the new strings, then the programs of `scripts`, then the
/// programs `actors` have of their own, starting at D-relative `base`. `text_at` gives the final offset of a retail pool string
/// (after text edits) or `None` when no pool string starts there. Returns the
/// bytes to append (a multiple of 4) and where each program went.
pub(crate) fn place(
    r: &NpcPack,
    retail: &[u8],
    scripts: &Scripts,
    actors: &[super::actor::AddedActor],
    base: u32,
    text_at: &dyn Fn(u16) -> Option<u16>,
) -> Result<(Vec<u8>, Vec<Placed>, Vec<PlacedOwn>)> {
    let mut out: Vec<u8> = Vec::new();
    let pad = |out: &mut Vec<u8>| {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    };
    // New strings, deduplicated, in the order the programs first quote them.
    let mut strings: BTreeMap<&[u8], u16> = BTreeMap::new();
    let own: Vec<(usize, &Vec<Stmt>)> = actors.iter().enumerate().filter_map(|(k, a)| a.own.as_ref().map(|s| (k, s))).collect();
    for stmts in scripts.values().chain(own.iter().map(|(_, s)| *s)) {
        for t in ast::text_refs(stmts) {
            if let TextRef::New(b) = t {
                if strings.contains_key(b.as_slice()) {
                    continue;
                }
                let at = base + out.len() as u32;
                if at > 0xFFFF {
                    return Err(EditError::new("TEXT_REACH", format!("A new string would start at D+0x{at:X}, past the u16 operand reach (0xFFFF). Shorten the new text.")));
                }
                strings.insert(b, at as u16);
                out.extend_from_slice(b);
                out.push(0);
                pad(&mut out);
            }
        }
    }
    // New walk paths, deduplicated, after the strings (WALK operands are u16 too).
    let mut paths: BTreeMap<&WalkPath, u16> = BTreeMap::new();
    for stmts in scripts.values().chain(own.iter().map(|(_, s)| *s)) {
        for p in ast::path_refs(stmts) {
            if paths.contains_key(p) {
                continue;
            }
            let at = base + out.len() as u32;
            if at > 0xFFFF {
                return Err(EditError::new("WALK_REACH", format!("A new walk path would start at D+0x{at:X}, past the u16 operand reach (0xFFFF).")));
            }
            paths.insert(p, at as u16);
            out.extend_from_slice(&p.bytes());
            pad(&mut out);
        }
    }
    let laid = |stmts: &[Stmt]| ast::with_paths(stmts, &|p| paths.get(p).copied());
    let resolve = |t: &TextRef| -> std::result::Result<u16, String> {
        match t {
            TextRef::Pool(o) => text_at(*o).ok_or_else(|| format!("No string of this pack starts at @0x{o:04X}.")),
            TextRef::New(b) => strings.get(b.as_slice()).copied().ok_or_else(|| "A new string was not laid out.".to_string()),
        }
    };
    let slots = entries(r, retail);
    let mut placed = Vec::new();
    for (&entry, stmts) in scripts {
        let at = base + out.len() as u32;
        let code = asm::assemble(&laid(stmts), at, &resolve).map_err(|d| {
            let first = d.into_iter().next().map(|d| d.message).unwrap_or_default();
            EditError::new("SCRIPT_ASM", format!("The program for D+0x{entry:04X} doesn't assemble at D+0x{at:X}: {first}"))
        })?;
        let Some(users) = slots.get(&entry) else {
            return Err(EditError::new("SCRIPT_UNKNOWN", format!("No actor's retail script starts at D+0x{entry:04X}.")));
        };
        placed.push(Placed { entry, at, len: code.len() as u32, slots: users.clone() });
        out.extend_from_slice(&code);
        pad(&mut out);
    }
    let mut own_placed = Vec::new();
    for (k, stmts) in own {
        let at = base + out.len() as u32;
        let slot = r.actors.len() + k;
        let code = asm::assemble(&laid(stmts), at, &resolve).map_err(|d| {
            let first = d.into_iter().next().map(|d| d.message).unwrap_or_default();
            EditError::new("SCRIPT_ASM", format!("Actor {slot}'s program doesn't assemble at D+0x{at:X}: {first}"))
        })?;
        own_placed.push(PlacedOwn { added: k, at, len: code.len() as u32 });
        out.extend_from_slice(&code);
        pad(&mut out);
    }
    Ok((out, placed, own_placed))
}

/* ───────────────────────── The screen's calls ───────────────────────── */

/// Validation limits from the retail pack.
struct PackLimits {
    actors: u32,
    pool: BTreeSet<u16>,
    code: BTreeSet<u16>,
    d_span: u32,
}

impl PackLimits {
    fn of(r: &NpcPack, retail: &[u8]) -> Self {
        let pool = r.pool(retail).into_iter().filter(|s| s.offset <= 0xFFFF).map(|s| s.offset as u16).collect();
        let code = r
            .programs(retail)
            .into_iter()
            .filter_map(|(_, _, res)| res.ok())
            .flatten()
            .filter(|i| i.at <= 0xFFFF)
            .map(|i| i.at as u16)
            .collect();
        Self { actors: r.actors.len() as u32, pool, code, d_span: r.d_span }
    }

    fn validate(&self, stmts: &[Stmt], areas: &[u32]) -> Vec<Diag> {
        asm::validate(
            stmts,
            &Limits {
                actors: self.actors,
                pool: &|o| self.pool.contains(&o),
                code: &|o| self.code.contains(&o),
                d_span: self.d_span,
                flags: crate::engine::active().flag_count,
                areas,
            },
        )
    }
}

/// The retail entry of an actor slot.
fn entry_of(r: &NpcPack, retail: &[u8], slot: u32) -> Result<(u32, Vec<u32>)> {
    let a = r.actors.get(slot as usize).ok_or_else(|| EditError::new("SCRIPT_NO_ACTOR", format!("The pack has no actor {slot}.")))?;
    let all = entries(r, retail);
    let users = all.get(&a.script).cloned().ok_or_else(|| EditError::new("SCRIPT_NO_PROGRAM", format!("Actor {slot} has no program.")))?;
    Ok((a.script, users))
}

/// One program as the editor shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub slot: u32,
    /// Retail entry (the program's id).
    pub entry: u32,
    /// Where the project's program is (D-relative); `entry` unless edited.
    pub at: u32,
    pub edited: bool,
    /// Every actor slot that runs this program.
    pub slots: Vec<u32>,
    pub dsl: String,
    pub ops: u32,
    pub bytes: u32,
    /// The program as the Structured view edits it.
    pub tree: Vec<Node>,
}

/// The DSL of actor `slot`'s program as the project has it. `label` names the
/// pack in the header comment (`npc/7`).
pub fn source(retail: &[u8], current: &[u8], slot: u32, label: &str) -> Result<Source> {
    let base = super::character::base(retail, current)?;
    let current: &[u8] = &base;
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    if let Some((_, a)) = added_of(&r, retail, current, slot)? {
        // An added actor: its own program, else its source's as the place to start.
        let at = c.actors[slot as usize].script;
        let map = TextMap::of(&r, retail, current)?;
        let cd = c.d_block(current);
        let code = tier2::disassemble(cd, at).map_err(|e| EditError::new("SCRIPT_UNREADABLE", format!("D+0x{at:04X} doesn't decode: {e:?}.")))?;
        let stmts = map.lift(&code, cd);
        let head = match (a.spot, a.from, &a.own) {
            (true, super::actor::NEW_SPOT, _) => format!("{label} · actor {slot} · added examine spot · runs as the Knight's · its own program at D+0x{at:04X}"),
            (true, from, Some(_)) => format!("{label} · actor {slot} · added examine spot, a copy of actor {from} · its own program at D+0x{at:04X}"),
            (true, from, None) => format!("{label} · actor {slot} · added examine spot, a copy of actor {from} · runs actor {from}'s program; saving gives it its own"),
            (false, from, Some(_)) => format!("{label} · actor {slot} · added, a copy of actor {from} · its own program at D+0x{at:04X}"),
            (false, from, None) => format!("{label} · actor {slot} · added, a copy of actor {from} · runs actor {from}'s program; saving gives it its own"),
        };
        let dsl = dsl::print(&stmts, Some(&head), &|o| map.preview(cd, o));
        return Ok(Source {
            slot,
            entry: at,
            at,
            edited: a.own.is_some(),
            slots: vec![slot],
            dsl,
            ops: code.len() as u32,
            bytes: code.iter().map(|i| i.bytes.len() as u32).sum(),
            tree: {
                let mut t = tree_of(&stmts, &code);
                preview_walks(&mut t, cd);
                t
            },
        });
    }
    let (entry, slots) = entry_of(&r, retail, slot)?;
    let at = current_entry(&c, &slots)?;
    let map = TextMap::of(&r, retail, current)?;
    let cd = c.d_block(current);
    let code = tier2::disassemble(cd, at).map_err(|e| EditError::new("SCRIPT_UNREADABLE", format!("D+0x{at:04X} doesn't decode: {e:?}.")))?;
    let stmts = map.lift(&code, cd);
    let others: Vec<String> = slots.iter().filter(|&&s| s != slot).map(|s| s.to_string()).collect();
    let mut head = format!("{label} · actor {slot} · D+0x{entry:04X}");
    if at != entry {
        head.push_str(&format!(" · edited, now at D+0x{at:04X}"));
    }
    if !others.is_empty() {
        head.push_str(&format!(" · also runs for actor{} {}", if others.len() == 1 { "" } else { "s" }, others.join(", ")));
    }
    let dsl = dsl::print(&stmts, Some(&head), &|o| map.preview(cd, o));
    Ok(Source {
        slot,
        entry,
        at,
        edited: at != entry,
        slots,
        dsl,
        ops: code.len() as u32,
        bytes: code.iter().map(|i| i.bytes.len() as u32).sum(),
        tree: {
            let mut t = tree_of(&stmts, &code);
            preview_walks(&mut t, cd);
            t
        },
    })
}

/// What saving a program would do.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub entry: u32,
    /// Same bytes as retail: saving reverts the program.
    pub identical: bool,
    pub ops: u32,
    /// Program bytes.
    pub bytes: u32,
    /// Where the program goes (D-relative); `None` when identical.
    pub at: Option<u32>,
    /// Actor records that point at it.
    pub slots: Vec<u32>,
    /// New strings the program quotes, and their bytes with NULs.
    pub new_strings: u32,
    pub new_string_bytes: u32,
    /// GOTO sites in other programs that land inside the retail program; they
    /// keep running the retail code.
    pub inbound: Vec<u32>,
    /// The pack after the save (budget panel).
    pub report: TextReport,
}

/// Parse, validate and price an edit. `diagnostics` holds every problem;
/// `plan` is set when the program assembles and the pack takes it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub diagnostics: Vec<Diag>,
    pub plan: Option<Plan>,
    /// The bytes the save would stage (not serialized).
    #[serde(skip)]
    pub bytes: Option<Vec<u8>>,
}

fn has_errors(d: &[Diag]) -> bool {
    d.iter().any(|d| d.severity == Severity::Error)
}

/// The retail program's bytes at `entry`.
fn retail_program(r: &NpcPack, retail: &[u8], entry: u32) -> Result<Vec<tier2::Instr>> {
    tier2::disassemble(r.d_block(retail), entry).map_err(|e| EditError::new("SCRIPT_UNREADABLE", format!("The retail program at D+0x{entry:04X} doesn't decode: {e:?}.")))
}

/// Instructions, bytes, new strings and their bytes (with NULs) of a program.
fn measure(s: &[Stmt]) -> (u32, u32, u32, u32) {
    let news: BTreeSet<&[u8]> = ast::text_refs(s).into_iter().filter_map(|t| if let TextRef::New(b) = t { Some(b.as_slice()) } else { None }).collect();
    let flat = ast::flatten(s);
    let end = u32::from(!matches!(s.last(), Some(Stmt::Op { op: 0, .. })));
    let ops = flat.iter().filter(|f| matches!(f, ast::Flat::Op { .. })).count() as u32 + end;
    let size: u32 = flat.iter().map(|f| if let ast::Flat::Op { op, .. } = f { tier2::OPS[*op as usize].len as u32 } else { 0 }).sum::<u32>() + end;
    (ops, size, news.len() as u32, news.iter().map(|b| b.len() as u32 + 1).sum())
}

/// An added actor's slot (`edit::actor`): its index among the added actors
/// and the actor; `None` for a retail slot.
fn added_of(r: &NpcPack, retail: &[u8], current: &[u8], slot: u32) -> Result<Option<(usize, super::actor::AddedActor)>> {
    let n = r.actors.len() as u32;
    if slot < n {
        return Ok(None);
    }
    let k = (slot - n) as usize;
    super::actor::recover(retail, current)?
        .into_iter()
        .nth(k)
        .map(|a| Some((k, a)))
        .ok_or_else(|| EditError::new("SCRIPT_NO_ACTOR", format!("The pack has no actor {slot}.")))
}

/// [`check`] for an added actor: `stmts` becomes the program of its own
/// (`None` = it runs its source's program again).
fn check_added(retail: &[u8], current: &[u8], slot: u32, k: usize, stmts: Option<Vec<Stmt>>, mut diagnostics: Vec<Diag>) -> Result<Check> {
    let c = header(current, "project's")?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    let from = set.actors[k].from as usize;
    let shared = c.actors.get(from).map_or(0, |a| a.script);
    let identical = stmts.is_none();
    let (ops, bytes, new_strings, new_string_bytes) = match &stmts {
        Some(s) => measure(s),
        None => {
            let code = tier2::disassemble(c.d_block(current), shared).unwrap_or_default();
            (code.len() as u32, code.iter().map(|i| i.bytes.len() as u32).sum(), 0, 0)
        }
    };
    set.actors[k].own = stmts;
    let (out, report, _) = match super::text::layout(retail, current, &set) {
        Ok(v) => v,
        Err(e) => {
            diagnostics.push(Diag::error(0, 0, e.code, e.message));
            return Ok(Check { diagnostics, plan: None, bytes: None });
        }
    };
    let at = header(&out, "new")?.actors.get(slot as usize).map(|a| a.script).filter(|_| !identical);
    Ok(Check {
        diagnostics,
        plan: Some(Plan { entry: shared, identical, ops, bytes, at, slots: vec![slot], new_strings, new_string_bytes, inbound: Vec::new(), report }),
        bytes: Some(out),
    })
}

/// Checks `src` as the new program of actor `slot` (`None` = back to retail)
/// against the project's pack, and lays the pack out without saving. `areas`
/// are the project's new areas, which chunk operands may name. Errors are
/// pack-level problems (no header, a pack replaced as a whole file).
pub fn check(retail: &[u8], current: &[u8], slot: u32, src: Option<&str>, areas: &[u32]) -> Result<Check> {
    check_parsed(retail, current, slot, src.map(dsl::parse), areas)
}

/// [`check`] for a program the Structured view edited as a tree.
pub fn check_tree(retail: &[u8], current: &[u8], slot: u32, tree: Option<&[Node]>, areas: &[u32]) -> Result<Check> {
    let parsed = tree.map(|t| match stmts_of(t) {
        Ok(s) => (s, Vec::new()),
        Err(d) => (Vec::new(), vec![d]),
    });
    check_parsed(retail, current, slot, parsed, areas)
}

/// [`check`] after parsing: the statements and the parser's diagnostics.
fn check_parsed(retail: &[u8], current: &[u8], slot: u32, parsed: Option<(Vec<Stmt>, Vec<Diag>)>, areas: &[u32]) -> Result<Check> {
    let r = header(retail, "retail")?;
    let added = added_of(&r, retail, current, slot)?;
    let mut diagnostics = Vec::new();
    let stmts = match parsed {
        None => None,
        Some((stmts, mut d)) => {
            if !has_errors(&d) {
                d.extend(PackLimits::of(&r, retail).validate(&stmts, areas));
                d.extend(crate::engine::active().ext_checks(&stmts));
                // Ids the engine's C reacts to (research 06 §4.2): notes, never errors.
                d.extend(crate::engine::active().script_notes(&stmts));
            }
            if !has_errors(&d) {
                // Labels, operand kinds and the END: where the program lands doesn't matter for these.
                let any = |_: &TextRef| Ok(0u16);
                if let Err(e) = asm::assemble(&ast::with_paths(&stmts, &|_| Some(0)), 0, &any) {
                    d.extend(e);
                }
            }
            diagnostics = d;
            if has_errors(&diagnostics) {
                return Ok(Check { diagnostics, plan: None, bytes: None });
            }
            Some(stmts)
        }
    };
    if let Some((k, _)) = added {
        return check_added(retail, current, slot, k, stmts, diagnostics);
    }
    let (entry, slots) = entry_of(&r, retail, slot)?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    let retail_code = retail_program(&r, retail, entry)?;
    let retail_bytes: Vec<u8> = retail_code.iter().flat_map(|i| i.bytes.iter().copied()).collect();

    // Same bytes as retail when assembled at the retail entry against the retail pool?
    let identical = match &stmts {
        None => true,
        Some(s) => {
            let at_retail = |t: &TextRef| match t {
                TextRef::Pool(o) => Ok(*o),
                TextRef::New(_) => Err(String::new()),
            };
            asm::assemble(s, entry, &at_retail).is_ok_and(|b| b == retail_bytes)
        }
    };
    let (ops, bytes, new_strings, new_string_bytes) = match &stmts {
        Some(s) if !identical => measure(s),
        _ => (retail_code.len() as u32, retail_bytes.len() as u32, 0, 0),
    };
    match stmts {
        Some(s) if !identical => {
            set.scripts.insert(entry, s);
        }
        _ => {
            set.scripts.remove(&entry);
        }
    }
    let (out, report, placed) = match super::text::layout(retail, current, &set) {
        Ok(v) => v,
        Err(e) => {
            diagnostics.push(Diag::error(0, 0, e.code, e.message));
            return Ok(Check { diagnostics, plan: None, bytes: None });
        }
    };

    // GOTOs in other programs into the retail range.
    let end = entry + retail_bytes.len() as u32;
    let mut inbound: Vec<u32> = r
        .actor_programs(retail)
        .into_iter()
        .filter(|(e, _)| *e != entry)
        .filter_map(|(_, res)| res.ok())
        .flatten()
        .filter(|i| i.operands.iter().any(|o| matches!(o, Operand::Code(t) if (entry..end).contains(&(*t as u32)))))
        .map(|i| i.at)
        .collect();
    inbound.sort_unstable();
    inbound.dedup();

    let at = placed.iter().find(|p| p.entry == entry).map(|p| p.at);
    Ok(Check {
        diagnostics,
        plan: Some(Plan { entry, identical, ops, bytes, at, slots, new_strings, new_string_bytes, inbound, report }),
        bytes: Some(out),
    })
}

/* ───────────────────────── The Structured view's tree ───────────────────────── */

/// One statement as the Structured view edits it (JSON). `at` is where its
/// instruction sits in the project's bytes, `elseAt` an IF's ELSE marker;
/// both are `None` for statements the screen added. Labels name GOTO targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Node {
    Op {
        op: u8,
        args: Vec<Arg>,
        #[serde(default)]
        at: Option<u32>,
    },
    If {
        op: u8,
        arg: Option<Arg>,
        then: Vec<Node>,
        els: Vec<Node>,
        #[serde(default)]
        at: Option<u32>,
        #[serde(default, rename = "elseAt")]
        else_at: Option<u32>,
    },
    Label {
        name: String,
    },
}

/// An operand of a [`Node`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Arg {
    Num { value: i64 },
    /// A string of the pack, by its retail offset.
    Pool { offset: u16 },
    /// A new string, in the Text & dialogue markup.
    Text { markup: String },
    /// A GOTO target in the same program.
    Label { name: String },
    /// A GOTO target in other code (D-relative).
    Code { at: u16 },
    /// A walk program of the pack (D-relative); `preview` is its data, for the form.
    Walk {
        at: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<WalkData>,
    },
    /// A walk program the layout writes ([`ast::WalkPath`]).
    Path {
        x: i16,
        h: i16,
        z: i16,
        dir: u16,
        #[serde(default)]
        words: Vec<u16>,
    },
}

/// Walk data as the form shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkData {
    pub x: i16,
    pub h: i16,
    pub z: i16,
    pub dir: u16,
    pub words: Vec<u16>,
}

/// Fills each walk operand's preview from the pack's bytes.
fn preview_walks(nodes: &mut [Node], cd: &[u8]) {
    let fill = |a: &mut Arg| {
        if let Arg::Walk { at, preview } = a {
            *preview = WalkPath::read(cd, *at as usize).map(|p| WalkData { x: p.x, h: p.h, z: p.z, dir: p.dir, words: p.words });
        }
    };
    for n in nodes {
        match n {
            Node::Op { args, .. } => args.iter_mut().for_each(fill),
            Node::If { arg, then, els, .. } => {
                if let Some(a) = arg {
                    fill(a);
                }
                preview_walks(then, cd);
                preview_walks(els, cd);
            }
            Node::Label { .. } => {}
        }
    }
}

fn arg_of(v: &Val) -> Arg {
    match v {
        Val::Num(n) => Arg::Num { value: *n },
        Val::Text(TextRef::Pool(o)) => Arg::Pool { offset: *o },
        Val::Text(TextRef::New(b)) => Arg::Text { markup: text::markup(&text::decode(b)) },
        Val::Code(CodeRef::Label(l)) => Arg::Label { name: l.clone() },
        Val::Code(CodeRef::At(t)) => Arg::Code { at: *t },
        Val::Walk(t) => Arg::Walk { at: *t, preview: None },
        Val::Path(p) => Arg::Path { x: p.x, h: p.h, z: p.z, dir: p.dir, words: p.words.clone() },
    }
}

fn val_of(a: &Arg) -> std::result::Result<Val, Diag> {
    Ok(match a {
        Arg::Num { value } => Val::Num(*value),
        Arg::Pool { offset } => Val::Text(TextRef::Pool(*offset)),
        Arg::Text { markup } => Val::Text(TextRef::New(super::text::encode_markup(markup).map_err(|e| Diag::error(0, 0, e.code, e.message))?)),
        Arg::Label { name } => Val::Code(CodeRef::Label(name.clone())),
        Arg::Code { at } => Val::Code(CodeRef::At(*at)),
        Arg::Walk { at, .. } => Val::Walk(*at),
        Arg::Path { x, h, z, dir, words } => Val::Path(WalkPath { x: *x, h: *h, z: *z, dir: *dir, words: words.clone() }),
    })
}

/// The tree of lifted statements, each with the address of its instruction
/// in `code` (the instructions `stmts` were lifted from, in the same order
/// [`ast::flatten`] writes them).
fn tree_of(stmts: &[Stmt], code: &[tier2::Instr]) -> Vec<Node> {
    fn nodes(stmts: &[Stmt], ats: &mut impl Iterator<Item = u32>) -> Vec<Node> {
        let mut out = Vec::with_capacity(stmts.len());
        for s in stmts {
            out.push(match s {
                Stmt::Op { op, args, .. } => Node::Op { op: *op, args: args.iter().map(arg_of).collect(), at: ats.next() },
                Stmt::Label { name, .. } => Node::Label { name: name.clone() },
                Stmt::If { op, arg, then, els, .. } => {
                    let at = ats.next();
                    ats.next(); // 0D
                    let then = nodes(then, ats);
                    let else_at = ats.next();
                    let els = nodes(els, ats);
                    ats.next(); // 0F
                    Node::If { op: *op, arg: arg.as_ref().map(arg_of), then, els, at, else_at }
                }
            });
        }
        out
    }
    nodes(stmts, &mut code.iter().map(|i| i.at))
}

/// The statements of a tree (new strings encoded).
pub fn stmts_of(tree: &[Node]) -> std::result::Result<Vec<Stmt>, Diag> {
    tree.iter()
        .map(|n| {
            Ok(match n {
                Node::Op { op, args, .. } => Stmt::Op { op: *op, args: args.iter().map(val_of).collect::<std::result::Result<_, _>>()?, line: 0 },
                Node::If { op, arg, then, els, .. } => Stmt::If {
                    op: *op,
                    arg: arg.as_ref().map(val_of).transpose()?,
                    then: stmts_of(then)?,
                    els: stmts_of(els)?,
                    line: 0,
                },
                Node::Label { name } => Stmt::Label { name: name.clone(), line: 0 },
            })
        })
        .collect()
}

/// The programs of `current` that differ from retail: retail entry → actor slots.
pub fn edited(retail: &[u8], current: &[u8]) -> Vec<(u32, Vec<u32>)> {
    let base = super::character::base(retail, current).unwrap_or(std::borrow::Cow::Borrowed(current));
    let current: &[u8] = &base;
    let (Some(r), Some(c)) = (NpcPack::parse(retail), NpcPack::parse(current)) else { return Vec::new() };
    if c.actors.len() < r.actors.len() {
        return Vec::new();
    }
    entries(&r, retail).into_iter().filter(|(entry, slots)| current_entry(&c, slots).is_ok_and(|at| at != *entry)).collect()
}
