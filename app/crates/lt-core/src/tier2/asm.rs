//! Tier-2 assembler (PRD 11 §5.2) and validator (§5.3).
//!
//! Instruction sizes are fixed per opcode, so one pass places the labels and
//! a second emits the bytes. Text operands are resolved by the caller (the
//! pack layout knows where pool strings and new strings end up).

use std::collections::HashMap;

use crate::world;

use super::ast::{cond_name, flatten, mnemonic, CodeRef, Flat, Kind, Stmt, TextRef, Val};
use super::dsl::Diag;
use super::{disassemble, OPS};

/// Resolves a text operand to its D-relative offset, or says why it can't.
pub type TextResolver<'a> = &'a dyn Fn(&TextRef) -> Result<u16, String>;

fn needs_end(stmts: &[Stmt]) -> bool {
    !matches!(stmts.last(), Some(Stmt::Op { op: 0x00, .. }))
}

fn put_num(out: &mut Vec<u8>, kind: Kind, v: i64) {
    match kind.size() {
        1 => out.push(v as u8),
        4 => out.extend_from_slice(&(v as u32).to_le_bytes()),
        _ => out.extend_from_slice(&(v as u16).to_le_bytes()),
    }
}

/// Assembles the program at D-relative `base`. The final top-level `end` is
/// added when missing. Fails with every problem found, each with its line.
pub fn assemble(stmts: &[Stmt], base: u32, text: TextResolver) -> Result<Vec<u8>, Vec<Diag>> {
    let flat = flatten(stmts);
    let mut errors = Vec::new();

    // Pass 1: label offsets.
    let mut labels: HashMap<&str, u32> = HashMap::new();
    let mut at = 0u32;
    for f in &flat {
        match f {
            Flat::Label { name, line } => {
                if labels.insert(name, at).is_some() {
                    errors.push(Diag::error(*line, 0, "ASM_LABEL_TWICE", format!("Label `{name}` is defined twice.")));
                }
            }
            Flat::Op { op, .. } => at += OPS[*op as usize].len as u32,
        }
    }

    // Pass 2: bytes.
    let mut out = Vec::with_capacity(at as usize + 1);
    let mut lines: Vec<(u32, u32)> = Vec::new();
    for f in &flat {
        let Flat::Op { op, args, line } = f else { continue };
        let m = mnemonic(*op);
        lines.push((out.len() as u32, *line));
        out.push(*op);
        if args.len() != m.args.len() {
            errors.push(Diag::error(*line, 0, "ASM_ARITY", format!("`{}` needs {} operands.", m.name, m.args.len())));
            out.resize(out.len() + OPS[*op as usize].len as usize - 1, 0);
            continue;
        }
        for ((_, kind), v) in m.args.iter().zip(args.iter()) {
            match (kind, v) {
                (Kind::Text, Val::Text(t)) => match text(t) {
                    Ok(o) => put_num(&mut out, *kind, o as i64),
                    Err(e) => {
                        errors.push(Diag::error(*line, 0, "ASM_TEXT", e));
                        put_num(&mut out, *kind, 0);
                    }
                },
                (Kind::Code, Val::Code(CodeRef::Label(l))) => {
                    let target = labels.get(l.as_str()).map(|o| base + o);
                    match target {
                        Some(t) if t <= 0xFFFF => put_num(&mut out, *kind, t as i64),
                        Some(t) => {
                            errors.push(Diag::error(*line, 0, "ASM_REACH", format!("Label `{l}` would be at D+0x{t:X}, past the u16 operand reach.")));
                            put_num(&mut out, *kind, 0);
                        }
                        None => {
                            errors.push(Diag::error(*line, 0, "ASM_LABEL_UNKNOWN", format!("No label `{l}` in this program.")));
                            put_num(&mut out, *kind, 0);
                        }
                    }
                }
                (Kind::Code, Val::Code(CodeRef::At(t))) | (Kind::Walk, Val::Walk(t)) => put_num(&mut out, *kind, *t as i64),
                (Kind::Walk, Val::Path(_)) => {
                    errors.push(Diag::error(*line, 0, "ASM_PATH", "A new walk path has no place yet (the layout writes it, `ast::with_paths`)."));
                    put_num(&mut out, *kind, 0);
                }
                (k, Val::Num(n)) if !matches!(k, Kind::Text | Kind::Code | Kind::Walk) => {
                    let (lo, hi) = k.range();
                    if *n < lo || *n > hi {
                        errors.push(Diag::error(*line, 0, "ASM_RANGE", format!("{n} is out of range ({lo}–{hi}).")));
                    }
                    put_num(&mut out, *kind, *n);
                }
                _ => {
                    errors.push(Diag::error(*line, 0, "ASM_TYPE", format!("`{}` got the wrong kind of operand.", m.name)));
                    put_num(&mut out, *kind, 0);
                }
            }
        }
    }
    if needs_end(stmts) {
        lines.push((out.len() as u32, 0));
        out.push(0x00);
    }

    // The disassembler (and so the editor, and the next save) must read the
    // whole program back: an END at depth 0 before the last byte cuts it short.
    if errors.is_empty() {
        let line_at = |o: u32| lines.iter().rev().find(|(at, _)| *at <= o).map(|(_, l)| *l).unwrap_or(0);
        match disassemble(&out, 0) {
            Ok(code) => {
                let last = code.last().expect("END");
                let read = last.at + last.bytes.len() as u32;
                if read as usize != out.len() {
                    errors.push(Diag::error(
                        line_at(last.at),
                        0,
                        "ASM_ENDS_EARLY",
                        "The program stops here: an `end` outside every block ends it, so what follows can't be read back. Move it into a block or remove it.",
                    ));
                }
            }
            Err(_) => errors.push(Diag::error(0, 0, "ASM_NO_END", "The program never reaches END outside a block: a `block_begin` is never closed.")),
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/* ───────────────────────── Validation ───────────────────────── */

/// What the pack allows (from the retail pack), and the project's new areas.
pub struct Limits<'a> {
    /// Actor records (actor operands index the instantiated slots).
    pub actors: u32,
    /// A retail pool string starts at this D-relative offset.
    pub pool: &'a dyn Fn(u16) -> bool,
    /// A retail program has an instruction starting here (targets of `goto @…`).
    pub code: &'a dyn Fn(u16) -> bool,
    /// The retail D-span: walk programs are inside it.
    pub d_span: u32,
    /// Event flags the engine has (512 on retail; `engine::Profile::flag_count`).
    pub flags: u32,
    /// The new areas the project has (`world::is_new_area` ids). Chunk
    /// operands may name these as well as the retail chunks.
    pub areas: &'a [u32],
}

/// Highest retail chunk id (`chunk/523`). New areas start at
/// [`world::NEW_AREA_FIRST`].
pub const CHUNK_MAX: i64 = 523;
/// Shops `D_801DE…` 0–22.
pub const SHOPS: i64 = 23;
/// Canned tasks `D_801FC40C[16]`.
pub const CUTSCENES: i64 = 16;
/// `Character_Struct` slots the field instantiates.
pub const ACTOR_SLOTS: i64 = 87;
/// The objective buffer at 0x801CDB70 holds 0x68 bytes with the NUL.
pub const OBJECTIVE_MAX: usize = 0x67;

fn num(v: &Val) -> Option<i64> {
    match v {
        Val::Num(n) => Some(*n),
        _ => None,
    }
}

/// A chunk operand names a retail chunk or one of the project's new areas.
/// All four ops take the plain id: WARP and INN hand it to
/// `TriggerDialogueEvent` (`ctx+0x50 = 0x8000 + (s16)id`), the speaker
/// cameras compare it with the current chunk (`ctx+0xE8`).
fn chunk_exists(c: i64, areas: &[u32]) -> bool {
    (0..=CHUNK_MAX).contains(&c) || u32::try_from(c).is_ok_and(|c| world::is_new_area(c) && areas.contains(&c))
}

fn no_chunk(c: i64, areas: &[u32]) -> String {
    let list = areas.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ");
    match u32::try_from(c) {
        Ok(n) if world::is_new_area(n) && areas.is_empty() => format!("chunk/{c} is a new-area id, and the project has no new areas. Make one in the world editor."),
        Ok(n) if world::is_new_area(n) => format!("The project has no new area chunk/{c} (it has {list})."),
        _ if areas.is_empty() => format!("There is no chunk/{c} (0–{CHUNK_MAX})."),
        _ => format!("There is no chunk/{c} (0–{CHUNK_MAX}, or the project's new areas {list})."),
    }
}

/// Checks a program against the VM's rules and the pack. Errors block a save
/// (they hang, crash or corrupt the game); warnings and info don't.
pub fn validate(stmts: &[Stmt], lim: &Limits) -> Vec<Diag> {
    let mut out = Vec::new();
    let flat = flatten(stmts);
    let mut blocks: Vec<(u32, bool)> = Vec::new();
    let mut stray_else: Vec<u32> = Vec::new();
    let ops: Vec<(u8, &[Val], u32)> = flat
        .iter()
        .filter_map(|f| match f {
            Flat::Op { op, args, line } => Some((*op, *args, *line)),
            _ => None,
        })
        .collect();

    for (k, &(op, args, line)) in ops.iter().enumerate() {
        let name = mnemonic(op).name;
        let a0 = args.first().and_then(num);
        match op {
            0x2D | 0x2E | 0x3B => out.push(Diag::error(line, 0, "ASM_UNSUPPORTED", format!("`{name}` is unused in retail and its encoding is unverified, so the assembler doesn't write it."))),
            0x04 | 0x05 | 0x0B | 0x28 => {
                if let Some(f) = a0 {
                    if f >= lim.flags as i64 {
                        let why = if op == 0x05 { "would clear a bit past the flag array and corrupt memory" } else { "stops the game with “Event flag over”" };
                        out.push(Diag::error(line, 0, "FLAG_RANGE", format!("Flag 0x{f:03X} is past 0x{:03X}: `{name}` {why}.", lim.flags.saturating_sub(1))));
                    } else if op == 0x04 && f == 0xF8 {
                        out.push(Diag::warning(line, "FLAG_F8", "Flag 0x0F8 is ignored by SetEventFlag, so this does nothing."));
                    }
                }
            }
            _ => {}
        }
        // Chunks.
        for ((_, kind), v) in mnemonic(op).args.iter().zip(args.iter()) {
            if *kind == Kind::Chunk {
                if let Some(c) = num(v).filter(|&c| !chunk_exists(c, lim.areas)) {
                    out.push(Diag::error(line, 0, "CHUNK_RANGE", no_chunk(c, lim.areas)));
                }
            }
        }
        match op {
            0x19 => {
                if let Some(f) = a0 {
                    if !(0..8).contains(&f) {
                        out.push(Diag::error(line, 0, "WARP_FACING", format!("Facing {f} isn't 0–7 (eighths of a turn).")));
                    }
                }
            }
            0x07 if a0.is_some_and(|s| s >= SHOPS) => out.push(Diag::error(line, 0, "SHOP_RANGE", format!("There are {SHOPS} shops (0–{}).", SHOPS - 1))),
            0x23 if a0.is_some_and(|c| c >= CUTSCENES) => {
                out.push(Diag::error(line, 0, "CUTSCENE_RANGE", format!("Canned tasks are 0–{}; a higher index jumps through memory past the table.", CUTSCENES - 1)))
            }
            _ => {}
        }
        // Actor operands.
        let actor_args: &[usize] = match op {
            0x24 => &[0],
            0x25 => &[0, 1],
            0x26 | 0x36 | 0x37 | 0x48 => &[0],
            _ => &[],
        };
        for &i in actor_args {
            if let Some(a) = args.get(i).and_then(num) {
                if a >= ACTOR_SLOTS {
                    out.push(Diag::error(line, 0, "ACTOR_RANGE", format!("Actor {a} is past the {ACTOR_SLOTS} field slots.")));
                } else if a >= lim.actors as i64 {
                    out.push(Diag::warning(line, "ACTOR_RANGE", format!("This pack has {} actors (0–{}); slot {a} is empty.", lim.actors, lim.actors.saturating_sub(1))));
                }
            }
        }
        // References.
        for v in args.iter() {
            match v {
                Val::Text(TextRef::Pool(o)) if !(lim.pool)(*o) => {
                    out.push(Diag::error(line, 0, "TEXT_REF", format!("No string of this pack starts at @0x{o:04X}.")));
                }
                Val::Text(TextRef::New(b)) if op == 0x44 && b.len() > OBJECTIVE_MAX => out.push(Diag::error(
                    line,
                    0,
                    "OBJECTIVE_LONG",
                    format!("The objective is {} bytes; the quest-log buffer takes {OBJECTIVE_MAX} plus the NUL, and the copy has no bound.", b.len()),
                )),
                Val::Code(CodeRef::At(t)) if !(lim.code)(*t) => {
                    out.push(Diag::error(line, 0, "CODE_REF", format!("@0x{t:04X} is not the start of an instruction in this pack's retail code.")));
                }
                Val::Walk(t) if *t as u32 >= lim.d_span => {
                    out.push(Diag::error(line, 0, "WALK_REF", format!("@0x{t:04X} is outside the pack's walk data (D-span 0x{:X}).", lim.d_span)));
                }
                Val::Path(p) => {
                    if !(-0x800..0x800).contains(&p.x) || !(-0x800..0x800).contains(&p.z) {
                        out.push(Diag::error(line, 0, "WALK_OUTSIDE", format!("({}, {}) is outside the chunk (−2048…2047).", p.x, p.z)));
                    }
                    if p.dir > 4095 {
                        out.push(Diag::error(line, 0, "WALK_DIR", format!("Facing {} is out of range (0–4095).", p.dir)));
                    }
                    if p.words.len() > 32 {
                        out.push(Diag::error(line, 0, "WALK_LONG", format!("{} walk words; keep it to 32.", p.words.len())));
                    }
                }
                _ => {}
            }
        }
        // Blocks.
        if cond_name(op).is_some() && ops.get(k + 1).map(|o| o.0) != Some(0x0D) {
            out.push(Diag::warning(line, "COND_NO_BLOCK", format!("`{name}` without a block: when it fails, the skip scans forward to the next ELSE marker.")));
        }
        // An ELSE outside every block skips to the next ENDIF outside every
        // block (retail npc/1 uses that as a "go to the end"); it needs one.
        match op {
            0x0D => blocks.push((line, false)),
            0x0E => match blocks.last_mut() {
                Some(b) => b.1 = true,
                None => stray_else.push(line),
            },
            0x0F => match blocks.pop() {
                Some((_, true)) => {}
                Some((l, false)) => out.push(Diag::error(l, 0, "BLOCK_NO_ELSE", "This block has no ELSE marker: when its condition fails, the skip runs past ENDIF into the next program.")),
                None if stray_else.pop().is_some() => {}
                None => out.push(Diag::warning(line, "BLOCK_STRAY_END", "This ENDIF closes nothing (harmless when executed).")),
            },
            _ => {}
        }
    }
    for (l, _) in blocks {
        out.push(Diag::error(l, 0, "BLOCK_UNCLOSED", "This block is never closed."));
    }
    for l in stray_else {
        out.push(Diag::error(l, 0, "BLOCK_STRAY_ELSE", "This ELSE marker is outside every block and no ENDIF follows: executing it skips into the next program."));
    }
    out.sort_by_key(|d| (d.line, d.severity));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier2::dsl::{parse, Severity};

    fn chunk_errors(src: &str, areas: &[u32]) -> Vec<(u32, String)> {
        let (stmts, d) = parse(src);
        assert!(d.is_empty(), "{d:?}");
        let lim = Limits { actors: 8, pool: &|_| true, code: &|_| true, d_span: 0x8000, flags: 512, areas };
        validate(&stmts, &lim).into_iter().filter(|d| d.code == "CHUNK_RANGE" && d.severity == Severity::Error).map(|d| (d.line, d.message)).collect()
    }

    /// Every chunk operand (WARP, INN and both speaker cameras) takes retail
    /// chunks and the project's new areas, and nothing else.
    #[test]
    fn chunk_operands_take_the_projects_new_areas() {
        let src = |c: u32| format!("warp facing:2 chunk/{c} x:0 z:-512 flags:0x0000\ninn chunk/{c} x:0 z:0 f:0\ncam_speaker chunk/{c} x:0 z:0 h:-129\ncam_speaker_nb chunk/{c} x:0 z:0 h:-129\n");
        let lines = |e: Vec<(u32, String)>| e.into_iter().map(|(l, _)| l).collect::<Vec<_>>();
        for retail in [0, 78, 330, 523] {
            assert!(chunk_errors(&src(retail), &[]).is_empty(), "chunk/{retail}");
        }
        assert_eq!(lines(chunk_errors(&src(1328), &[])), vec![1, 2, 3, 4], "no project area");
        assert!(chunk_errors(&src(1328), &[1328, 1333]).is_empty());
        assert!(chunk_errors(&src(2499), &[2499]).is_empty());
        assert_eq!(lines(chunk_errors(&src(1329), &[1328, 1333])), vec![1, 2, 3, 4], "an id the project hasn't made");
        assert_eq!(lines(chunk_errors(&src(524), &[1328])), vec![1, 2, 3, 4], "between retail and the new areas");
        assert_eq!(lines(chunk_errors(&src(2500), &[2500])), vec![1, 2, 3, 4], "past the new-area range, even when listed");

        assert_eq!(chunk_errors("warp facing:0 chunk/524 x:0 z:0 flags:0\n", &[])[0].1, "There is no chunk/524 (0–523).");
        assert_eq!(chunk_errors("warp facing:0 chunk/524 x:0 z:0 flags:0\n", &[1328, 1333])[0].1, "There is no chunk/524 (0–523, or the project's new areas 1328, 1333).");
        assert_eq!(chunk_errors("warp facing:0 chunk/1329 x:0 z:0 flags:0\n", &[1328])[0].1, "The project has no new area chunk/1329 (it has 1328).");
        assert!(chunk_errors("warp facing:0 chunk/1328 x:0 z:0 flags:0\n", &[])[0].1.contains("the project has no new areas"));
    }

    /// The operand is the plain id, as for retail chunks (the game adds 0x8000).
    #[test]
    fn a_new_area_assembles_as_its_plain_id() {
        let (stmts, d) = parse("warp facing:2 chunk/1328 x:-32 z:-880 flags:0x0201\ninn chunk/1328 x:16 z:32 f:0\ncam_speaker chunk/1328 x:0 z:0 h:-129\n");
        assert!(d.is_empty(), "{d:?}");
        let b = assemble(&stmts, 0, &|_| Ok(0)).unwrap();
        assert_eq!(&b[..11], &[0x19, 2, 0, 0x30, 0x05, 0xE0, 0xFF, 0x90, 0xFC, 0x01, 0x02]);
        assert_eq!(&b[11..14], &[0x06, 0x30, 0x05]);
        assert_eq!(&b[19..22], &[0x13, 0x30, 0x05]);
        let code = disassemble(&b, 0).unwrap();
        assert_eq!(code[0].operands[1], crate::tier2::Operand::Chunk(1328));
    }
}
