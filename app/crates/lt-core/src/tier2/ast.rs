//! Tier-2 programs as a tree (PRD 11 §5.1): IF cards with a then and an else
//! lane, labels for GOTO targets, and typed operands. [`lift`] builds the tree
//! from disassembled instructions and [`flatten`] gives the exact instruction
//! stream back, so `flatten(lift(code))` re-encodes to the same bytes for any
//! program the disassembler accepts (malformed blocks stay flat markers).

use super::{Instr, Operand};

/// A text operand: a string of the retail pool, by its retail D-relative
/// offset, or a new string (encoded bytes, no NUL) the layout allocates.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TextRef {
    Pool(u16),
    New(Vec<u8>),
}

/// A code operand (GOTO): a label in the same program, or a D-relative
/// offset in the retail code (shared code of another program).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeRef {
    Label(String),
    At(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Val {
    Num(i64),
    Text(TextRef),
    Code(CodeRef),
    /// A walk program (D-relative, data the VM hands to `WalkActor_UpdateTask`).
    Walk(u16),
    /// A walk program the layout writes (a new one, or one the project wrote before).
    Path(WalkPath),
}

/// The data of `WALK` / `WALK_BABY` (`WalkActor_UpdateTask`): a skipped
/// word (retail writes `A000`), then `x, h, z, dir` — the walker turns toward
/// (x, z) in 16 directions, walks straight there (7 units a frame on the major
/// axis, the height from the terrain; `h` is never read), then turns to `dir`
/// — then walk words (`WalkActor_RunScript`) up to `0000`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WalkPath {
    pub x: i16,
    pub h: i16,
    pub z: i16,
    pub dir: u16,
    /// Walk words after arriving, without the closing `0000`.
    pub words: Vec<u16>,
}

impl WalkPath {
    /// Words `A000 x h z dir words… 0000`, little-endian.
    pub fn bytes(&self) -> Vec<u8> {
        let mut w = vec![0xA000, self.x as u16, self.h as u16, self.z as u16, self.dir];
        w.extend(&self.words);
        w.push(0);
        w.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// The path at D-relative `at` of `d` (up to its `0000`, at most 64 words).
    pub fn read(d: &[u8], at: usize) -> Option<Self> {
        let w = |k: usize| d.get(at + k * 2..at + k * 2 + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let mut words = Vec::new();
        for k in 5..69 {
            match w(k)? {
                0 => return Some(Self { x: w(1)? as i16, h: w(2)? as i16, z: w(3)? as i16, dir: w(4)?, words }),
                v => words.push(v),
            }
        }
        None
    }
}

/// One statement. `line` is the 1-based source line (0 when lifted from bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Op { op: u8, args: Vec<Val>, line: u32 },
    /// `cond 0D <then> 0E <else> 0F`. `arg` is the condition's operand.
    If { op: u8, arg: Option<Val>, then: Vec<Stmt>, els: Vec<Stmt>, line: u32 },
    Label { name: String, line: u32 },
}

/* ───────────────────────── Operand kinds ───────────────────────── */

/// How an operand is encoded, range-checked and printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Code,
    Walk,
    /// Event flag, printed `0x081`.
    Flag,
    /// Item id: < 500 item, ≥ 1000 Living Toy.
    Item,
    /// Chunk id, printed `chunk/78`.
    Chunk,
    /// Zone or story battle, printed hex.
    MapId,
    U16,
    U16Hex,
    S16,
    U8,
    I32,
    U32Hex,
}

impl Kind {
    pub fn size(self) -> usize {
        match self {
            Kind::U8 => 1,
            Kind::I32 | Kind::U32Hex => 4,
            _ => 2,
        }
    }

    /// Inclusive range of a numeric operand.
    pub fn range(self) -> (i64, i64) {
        match self {
            Kind::U8 => (0, 0xFF),
            Kind::S16 => (-0x8000, 0x7FFF),
            Kind::I32 => (i32::MIN as i64, i32::MAX as i64),
            Kind::U32Hex => (0, u32::MAX as i64),
            _ => (0, 0xFFFF),
        }
    }
}

/// The DSL name and operands of one opcode. `args` follow the byte order.
pub struct Mnemonic {
    pub op: u8,
    pub name: &'static str,
    pub args: &'static [(&'static str, Kind)],
}

use Kind as K;

const T: &[(&str, Kind)] = &[("text", K::Text)];
const F: &[(&str, Kind)] = &[("flag", K::Flag)];
const H: &[(&str, Kind)] = &[("n", K::U16)];
const B: &[(&str, Kind)] = &[("n", K::U8)];
const ACTOR: &[(&str, Kind)] = &[("actor", K::U8)];
const ITEM: &[(&str, Kind)] = &[("item", K::Item)];
const CAM4: &[(&str, Kind)] = &[("chunk", K::Chunk), ("x", K::S16), ("z", K::S16), ("h", K::S16)];
const ANG3: &[(&str, Kind)] = &[("a", K::U16), ("b", K::U16), ("c", K::U16)];
const NONE: &[(&str, Kind)] = &[];

macro_rules! m {
    ($op:literal $name:literal $args:expr) => {
        Mnemonic { op: $op, name: $name, args: $args }
    };
}

/// One entry per opcode 0x00–0x4A, lengths matching [`super::OPS`] (tested).
/// Names are the Structured view's mnemonics in lower case, except the block
/// markers (`block_begin`/`block_else`/`block_end`, written by `if` cards) and
/// 0x1D (`if_item_alt`, the twin of 0x1E).
pub static MNEMONICS: [Mnemonic; 75] = [
    m!(0x00 "end" NONE),
    m!(0x01 "say" T),
    m!(0x02 "say2" T),
    m!(0x03 "say3" T),
    m!(0x04 "set" F),
    m!(0x05 "clear" F),
    m!(0x06 "inn" &[("chunk", K::Chunk), ("x", K::S16), ("z", K::S16), ("f", K::U8)]),
    m!(0x07 "shop" H),
    m!(0x08 "battle" &[("map", K::MapId)]),
    m!(0x09 "nop09" H),
    m!(0x0A "if_yes" NONE),
    m!(0x0B "if_flag" F),
    m!(0x0C "if_rubies" &[("rubies", K::I32)]),
    m!(0x0D "block_begin" NONE),
    m!(0x0E "block_else" NONE),
    m!(0x0F "block_end" NONE),
    m!(0x10 "anim" H),
    m!(0x11 "face_knight" NONE),
    m!(0x12 "sfx" H),
    m!(0x13 "cam_speaker" CAM4),
    m!(0x14 "cam_ease" ANG3),
    m!(0x15 "cam_last" NONE),
    m!(0x16 "give" ITEM),
    m!(0x17 "ret2" H),
    m!(0x18 "knight_away" NONE),
    m!(0x19 "warp" &[("facing", K::S16), ("chunk", K::Chunk), ("x", K::S16), ("z", K::S16), ("flags", K::U16Hex)]),
    m!(0x1A "nop1a" NONE),
    m!(0x1B "toggle_talk" NONE),
    m!(0x1C "walk" &[("walk", K::Walk)]),
    m!(0x1D "if_item_alt" ITEM),
    m!(0x1E "if_item" ITEM),
    m!(0x1F "speaker" T),
    m!(0x20 "no_speaker" NONE),
    m!(0x21 "ret4" H),
    m!(0x22 "bgm" H),
    m!(0x23 "cutscene" H),
    m!(0x24 "actor_anim" &[("actor", K::U8), ("anim", K::U8)]),
    m!(0x25 "face_actor" &[("actor", K::U8), ("to", K::U8)]),
    m!(0x26 "cam_actor" ACTOR),
    m!(0x27 "wait" &[("frames", K::U8)]),
    m!(0x28 "chest" &[("flag", K::Flag), ("item", K::Item)]),
    m!(0x29 "fade_1" NONE),
    m!(0x2A "fade_2" NONE),
    m!(0x2B "fade_3" NONE),
    m!(0x2C "fade_4" NONE),
    m!(0x2D "if_flag2" F),
    m!(0x2E "goto2" &[("to", K::Code)]),
    m!(0x2F "goto" &[("to", K::Code)]),
    m!(0x30 "shake" &[("a", K::U8), ("b", K::U8), ("c", K::U8)]),
    m!(0x31 "ending" B),
    m!(0x32 "ctx_d8" &[("a", K::U8), ("b", K::U8)]),
    m!(0x33 "add_rubies" &[("rubies", K::I32)]),
    m!(0x34 "walk_baby" &[("walk", K::Walk)]),
    m!(0x35 "nop35" B),
    m!(0x36 "turn" ACTOR),
    m!(0x37 "about" ACTOR),
    m!(0x38 "actor_f40" NONE),
    m!(0x39 "hide_self" NONE),
    m!(0x3A "choice" T),
    m!(0x3B "choice_n" T),
    m!(0x3C "ret_u8" B),
    m!(0x3D "full_hp" NONE),
    m!(0x3E "full_mp" NONE),
    m!(0x3F "hurt10" NONE),
    m!(0x40 "rest_heal" NONE),
    m!(0x41 "cam_field" H),
    m!(0x42 "helper" &[("packed", K::U32Hex)]),
    m!(0x43 "kill_helper" H),
    m!(0x44 "objective" T),
    m!(0x45 "cam_speaker_nb" CAM4),
    m!(0x46 "cam_set" ANG3),
    m!(0x47 "cam_f0" H),
    m!(0x48 "cam_focus" ACTOR),
    m!(0x49 "bgm_fade" H),
    // HackerGC only ([`super::OP_EXT`]); the validator refuses it on retail.
    m!(0x4A "ext" &[("sub", K::U8), ("a", K::U16), ("b", K::S16)]),
];

pub fn mnemonic(op: u8) -> &'static Mnemonic {
    &MNEMONICS[op as usize]
}

pub fn by_name(name: &str) -> Option<&'static Mnemonic> {
    MNEMONICS.iter().find(|m| m.name == name)
}

/// Opcodes that open an IF card, with the DSL condition name.
pub const CONDS: &[(u8, &str)] = &[(0x0A, "yes"), (0x0B, "flag"), (0x0C, "rubies"), (0x1D, "item_alt"), (0x1E, "item"), (0x3A, "choice")];

pub fn cond_name(op: u8) -> Option<&'static str> {
    CONDS.iter().find(|(o, _)| *o == op).map(|(_, n)| *n)
}

pub fn cond_op(name: &str) -> Option<u8> {
    CONDS.iter().find(|(_, n)| *n == name).map(|(o, _)| *o)
}

/* ───────────────────────── Bytes → tree ───────────────────────── */

enum Item {
    Label(String),
    Op(u8, Vec<Val>),
}

fn val_of(o: Operand, text: &dyn Fn(u16) -> TextRef, label: &dyn Fn(u16) -> Option<String>) -> Val {
    match o {
        Operand::Text(t) | Operand::Name(t) | Operand::Objective(t) | Operand::Choice(t) => Val::Text(text(t)),
        Operand::Code(t) => Val::Code(label(t).map(CodeRef::Label).unwrap_or(CodeRef::At(t))),
        Operand::Walk(t) => Val::Walk(t),
        Operand::Flag(v) | Operand::Item(v) | Operand::Chunk(v) | Operand::MapId(v) | Operand::U16(v) => Val::Num(v as i64),
        Operand::S16(v) => Val::Num(v as i64),
        Operand::U8(v) => Val::Num(v as i64),
        Operand::I32(v) => Val::Num(v as i64),
        Operand::U32(v) => Val::Num(v as i64),
    }
}

/// The program as a tree. `text` names each text operand (pool string or new
/// string); GOTO targets inside the program become labels `L1`, `L2`, … in
/// address order, targets outside stay offsets.
pub fn lift(code: &[Instr], text: &dyn Fn(u16) -> TextRef) -> Vec<Stmt> {
    let starts: std::collections::BTreeSet<u32> = code.iter().map(|i| i.at).collect();
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
    let name = |t: u32| targets.binary_search(&t).ok().map(|k| format!("L{}", k + 1));
    let label = |t: u16| name(t as u32);

    let mut items = Vec::with_capacity(code.len() + targets.len());
    for i in code {
        if let Some(n) = name(i.at) {
            items.push(Item::Label(n));
        }
        items.push(Item::Op(i.op, i.operands.iter().map(|&o| val_of(o, text, &label)).collect()));
    }
    structure(&items, 0, items.len())
}

fn op_at(items: &[Item], k: usize) -> Option<u8> {
    match items.get(k) {
        Some(Item::Op(op, _)) => Some(*op),
        _ => None,
    }
}

/// For the `0D` at `begin`: the indices of its `0E` and `0F` at depth 0 within `end`.
fn match_block(items: &[Item], begin: usize, end: usize) -> Option<(usize, usize)> {
    let mut depth = 0u32;
    let mut k = begin + 1;
    let mut els = None;
    while k < end {
        match op_at(items, k) {
            Some(0x0D) => depth += 1,
            Some(0x0F) => {
                if depth == 0 {
                    return els.map(|e| (e, k));
                }
                depth -= 1;
            }
            Some(0x0E) if depth == 0 && els.is_none() => els = Some(k),
            _ => {}
        }
        k += 1;
    }
    None
}

fn structure(items: &[Item], mut k: usize, end: usize) -> Vec<Stmt> {
    let mut out = Vec::new();
    while k < end {
        match &items[k] {
            Item::Label(n) => {
                out.push(Stmt::Label { name: n.clone(), line: 0 });
                k += 1;
            }
            Item::Op(op, args) => {
                if cond_name(*op).is_some() && k + 1 < end && op_at(items, k + 1) == Some(0x0D) {
                    if let Some((e, f)) = match_block(items, k + 1, end) {
                        out.push(Stmt::If {
                            op: *op,
                            arg: args.first().cloned(),
                            then: structure(items, k + 2, e),
                            els: structure(items, e + 1, f),
                            line: 0,
                        });
                        k = f + 1;
                        continue;
                    }
                }
                out.push(Stmt::Op { op: *op, args: args.clone(), line: 0 });
                k += 1;
            }
        }
    }
    out
}

/* ───────────────────────── Tree → stream ───────────────────────── */

/// One element of the flat stream: an instruction (with its source line) or a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flat<'a> {
    Op { op: u8, args: &'a [Val], line: u32 },
    Label { name: &'a str, line: u32 },
}

/// The instruction stream: every IF card writes `cond 0D <then> 0E <else> 0F`
/// (always with its `0E`).
pub fn flatten(stmts: &[Stmt]) -> Vec<Flat<'_>> {
    let mut out = Vec::new();
    flatten_into(stmts, &mut out);
    out
}

fn flatten_into<'a>(stmts: &'a [Stmt], out: &mut Vec<Flat<'a>>) {
    for s in stmts {
        match s {
            Stmt::Op { op, args, line } => out.push(Flat::Op { op: *op, args, line: *line }),
            Stmt::Label { name, line } => out.push(Flat::Label { name, line: *line }),
            Stmt::If { op, arg, then, els, line } => {
                out.push(Flat::Op { op: *op, args: arg.as_ref().map(std::slice::from_ref).unwrap_or(&[]), line: *line });
                out.push(Flat::Op { op: 0x0D, args: &[], line: *line });
                flatten_into(then, out);
                out.push(Flat::Op { op: 0x0E, args: &[], line: *line });
                flatten_into(els, out);
                out.push(Flat::Op { op: 0x0F, args: &[], line: *line });
            }
        }
    }
}

/// Every text operand in stream order.
/// Every new walk path the statements write.
pub fn path_refs(stmts: &[Stmt]) -> Vec<&WalkPath> {
    flatten(stmts)
        .into_iter()
        .filter_map(|f| match f {
            Flat::Op { args, .. } => Some(args),
            _ => None,
        })
        .flat_map(|a| a.iter())
        .filter_map(|v| match v {
            Val::Path(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// Every operand, mutably (both lanes of IF cards, their conditions too).
fn each_val(stmts: &mut [Stmt], f: &mut dyn FnMut(&mut Val)) {
    for s in stmts {
        match s {
            Stmt::Op { args, .. } => args.iter_mut().for_each(&mut *f),
            Stmt::If { arg, then, els, .. } => {
                if let Some(a) = arg {
                    f(a);
                }
                each_val(then, f);
                each_val(els, f);
            }
            Stmt::Label { .. } => {}
        }
    }
}

/// The statements with each new walk path replaced by the offset `at` gives
/// it (what the assembler encodes). `None` leaves the path, which fails to assemble.
pub fn with_paths(stmts: &[Stmt], at: &dyn Fn(&WalkPath) -> Option<u16>) -> Vec<Stmt> {
    let mut out = stmts.to_vec();
    each_val(&mut out, &mut |v| {
        if let Val::Path(p) = v {
            if let Some(o) = at(p) {
                *v = Val::Walk(o);
            }
        }
    });
    out
}

/// Walk operands at or past `from` (data the project wrote, past the retail
/// D-span) read back from `d` as paths, so they edit and lay out again.
pub fn inline_paths(stmts: &mut [Stmt], from: u32, d: &[u8]) {
    each_val(stmts, &mut |v| {
        if let Val::Walk(t) = v {
            if *t as u32 >= from {
                if let Some(p) = WalkPath::read(d, *t as usize) {
                    *v = Val::Path(p);
                }
            }
        }
    });
}

pub fn text_refs(stmts: &[Stmt]) -> Vec<&TextRef> {
    flatten(stmts)
        .into_iter()
        .filter_map(|f| match f {
            Flat::Op { args, .. } => Some(args),
            _ => None,
        })
        .flat_map(|a| a.iter())
        .filter_map(|v| match v {
            Val::Text(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier2::OPS;

    #[test]
    fn mnemonics_match_the_length_table() {
        for (k, m) in MNEMONICS.iter().enumerate() {
            assert_eq!(m.op as usize, k);
            let size: usize = 1 + m.args.iter().map(|(_, a)| a.size()).sum::<usize>();
            assert_eq!(size, OPS[k].len as usize, "{} (0x{:02X})", m.name, m.op);
            assert_eq!(m.args.len(), OPS[k].args.len(), "{}", m.name);
        }
        let mut names: Vec<&str> = MNEMONICS.iter().map(|m| m.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), MNEMONICS.len(), "mnemonics are unique");
    }
}
