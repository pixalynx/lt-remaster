//! The Tier-2 event VM: 74 fixed-length byte opcodes (0x00–0x49), plus
//! HackerGC's EXT (0x4A, [`OP_EXT`]), which only a HackerGC base runs. Lengths
//! are the table at OVR02 0x801FC454 (file 0x1D454), asserted against the disc
//! in tests. IF/ELSE/ENDIF are marker bytes (0D/0E/0F) with no jump offsets.
//! Opcode reference: `docs/screens/11-event-script-editor.md` §4.2–4.4.
//!
//! - [`ast`]: programs as a tree (IF cards, labels), lifted from instructions.
//! - [`dsl`]: the text form, printer and parser.
//! - [`asm`]: the assembler and the validator.

pub mod asm;
pub mod ast;
pub mod dsl;

use serde::Serialize;

use crate::container::{le16, le32};

/// Operand encodings, one char each:
/// T text · N speaker name · O objective · C choice · G code ref · W walk ref
/// F flag · I item · K chunk · M map id · h u16 · s s16 · b u8 · i i32 · w u32
/// Opcodes the retail VM has (0x00–0x49).
pub const RETAIL_OPS: usize = 74;

/// HackerGC's extension opcode, `4A sub a:u16 b:s16` (gc_decomp
/// `src/hgc/hgc_ext.c`). The retail length table says 0 for it, so the retail
/// VM hangs on it; a HackerGC base (`base.json` `profile.tier2Ext`) runs it.
pub const OP_EXT: u8 = 0x4A;
/// EXT sub-op: run new overlay `a` (spare file `a`) with argument `b`.
pub const EXT_RUN_OVERLAY: u8 = 0x00;

pub struct OpInfo {
    pub op: u8,
    pub len: u8,
    pub name: &'static str,
    pub args: &'static str,
}

macro_rules! ops {
    ($( $op:literal $len:literal $name:literal $args:literal ),* $(,)?) => {
        pub static OPS: [OpInfo; 75] = [ $( OpInfo { op: $op, len: $len, name: $name, args: $args } ),* ];
    };
}

ops![
    0x00 1 "END" "", 0x01 3 "SAY" "T", 0x02 3 "SAY2" "T", 0x03 3 "SAY3" "T",
    0x04 3 "SET" "F", 0x05 3 "CLEAR" "F", 0x06 8 "INN" "Kssb", 0x07 3 "SHOP" "h",
    0x08 3 "BATTLE" "M", 0x09 3 "NOP09" "h", 0x0A 1 "IF_YES" "", 0x0B 3 "IF_FLAG" "F",
    0x0C 5 "IF_RUBIES" "i", 0x0D 1 "BEGIN" "", 0x0E 1 "ELSE" "", 0x0F 1 "ENDIF" "",
    0x10 3 "ANIM" "h", 0x11 1 "FACE_KNIGHT" "", 0x12 3 "SFX" "h", 0x13 9 "CAM_SPEAKER" "Ksss",
    0x14 7 "CAM_EASE" "hhh", 0x15 1 "CAM_LAST" "", 0x16 3 "GIVE" "I", 0x17 3 "RET2" "h",
    0x18 1 "KNIGHT_AWAY" "", 0x19 11 "WARP" "sKssh", 0x1A 1 "NOP1A" "", 0x1B 1 "TOGGLE_TALK" "",
    0x1C 3 "WALK" "W", 0x1D 3 "IF_ITEM" "I", 0x1E 3 "IF_ITEM" "I", 0x1F 3 "SPEAKER" "N",
    0x20 1 "NO_SPEAKER" "", 0x21 3 "RET4" "h", 0x22 3 "BGM" "h", 0x23 3 "CUTSCENE" "h",
    0x24 3 "ACTOR_ANIM" "bb", 0x25 3 "FACE_ACTOR" "bb", 0x26 2 "CAM_ACTOR" "b", 0x27 2 "WAIT" "b",
    0x28 5 "CHEST" "FI", 0x29 1 "FADE_1" "", 0x2A 1 "FADE_2" "", 0x2B 1 "FADE_3" "",
    0x2C 1 "FADE_4" "", 0x2D 3 "IF_FLAG2" "F", 0x2E 3 "GOTO2" "G", 0x2F 3 "GOTO" "G",
    0x30 4 "SHAKE" "bbb", 0x31 2 "ENDING" "b", 0x32 3 "CTX_D8" "bb", 0x33 5 "ADD_RUBIES" "i",
    0x34 3 "WALK_BABY" "W", 0x35 2 "NOP35" "b", 0x36 2 "TURN" "b", 0x37 2 "ABOUT" "b",
    0x38 1 "ACTOR_F40" "", 0x39 1 "HIDE_SELF" "", 0x3A 3 "CHOICE" "C", 0x3B 3 "CHOICE_N" "C",
    0x3C 2 "RET_U8" "b", 0x3D 1 "FULL_HP" "", 0x3E 1 "FULL_MP" "", 0x3F 1 "HURT10" "",
    0x40 1 "REST_HEAL" "", 0x41 3 "CAM_FIELD" "h", 0x42 5 "HELPER" "w", 0x43 3 "KILL_HELPER" "h",
    0x44 3 "OBJECTIVE" "O", 0x45 9 "CAM_SPEAKER_NB" "Ksss", 0x46 7 "CAM_SET" "hhh", 0x47 3 "CAM_F0" "h",
    0x48 2 "CAM_FOCUS" "b", 0x49 3 "BGM_FADE" "h",
    0x4A 6 "EXT" "bhs",
];

pub fn op_info(op: u8) -> Option<&'static OpInfo> {
    OPS.get(op as usize)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum Operand {
    Text(u16),
    Name(u16),
    Objective(u16),
    Choice(u16),
    Code(u16),
    Walk(u16),
    Flag(u16),
    Item(u16),
    Chunk(u16),
    MapId(u16),
    U16(u16),
    S16(i16),
    U8(u8),
    I32(i32),
    U32(u32),
}

impl Operand {
    /// D-relative target for ops that point into the pack.
    pub fn d_ref(&self) -> Option<u16> {
        match *self {
            Operand::Text(o) | Operand::Name(o) | Operand::Objective(o) | Operand::Choice(o) | Operand::Code(o) | Operand::Walk(o) => {
                Some(o)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Instr {
    /// D-relative offset.
    pub at: u32,
    pub op: u8,
    pub name: &'static str,
    pub bytes: Vec<u8>,
    pub operands: Vec<Operand>,
    /// Block depth before this instruction (ELSE/ENDIF are shown one level out).
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DisasmError {
    InvalidOpcode { at: u32, op: u8 },
    Truncated { at: u32 },
    TooLong { at: u32 },
}

/// Decodes from `entry` (D-relative) to the `END` at block depth 0.
pub fn disassemble(d: &[u8], entry: u32) -> Result<Vec<Instr>, DisasmError> {
    let mut out = Vec::new();
    let mut pc = entry as usize;
    let mut depth: u32 = 0;
    loop {
        let at = pc as u32;
        let op = *d.get(pc).ok_or(DisasmError::Truncated { at })?;
        let info = op_info(op).filter(|i| i.len > 0).ok_or(DisasmError::InvalidOpcode { at, op })?;
        let bytes = d.get(pc..pc + info.len as usize).ok_or(DisasmError::Truncated { at })?.to_vec();
        let operands = decode_operands(info.args, &bytes[1..]);
        let shown_depth = match op {
            0x0E | 0x0F => depth.saturating_sub(1),
            _ => depth,
        };
        match op {
            0x0D => depth += 1,
            0x0F => depth = depth.saturating_sub(1),
            _ => {}
        }
        out.push(Instr { at, op, name: info.name, bytes, operands, depth: shown_depth });
        pc += info.len as usize;
        if op == 0x00 && depth == 0 {
            return Ok(out);
        }
        if out.len() > 4096 {
            return Err(DisasmError::TooLong { at: entry });
        }
    }
}

fn decode_operands(spec: &str, b: &[u8]) -> Vec<Operand> {
    let mut out = Vec::new();
    let mut p = 0usize;
    for c in spec.chars() {
        let u16_at = |p: usize| le16(b, p);
        let (v, n) = match c {
            'T' => (Operand::Text(u16_at(p)), 2),
            'N' => (Operand::Name(u16_at(p)), 2),
            'O' => (Operand::Objective(u16_at(p)), 2),
            'C' => (Operand::Choice(u16_at(p)), 2),
            'G' => (Operand::Code(u16_at(p)), 2),
            'W' => (Operand::Walk(u16_at(p)), 2),
            'F' => (Operand::Flag(u16_at(p)), 2),
            'I' => (Operand::Item(u16_at(p)), 2),
            'K' => (Operand::Chunk(u16_at(p)), 2),
            'M' => (Operand::MapId(u16_at(p)), 2),
            'h' => (Operand::U16(u16_at(p)), 2),
            's' => (Operand::S16(u16_at(p) as i16), 2),
            'b' => (Operand::U8(b[p]), 1),
            'i' => (Operand::I32(le32(b, p) as i32), 4),
            'w' => (Operand::U32(le32(b, p)), 4),
            _ => unreachable!("bad operand spec"),
        };
        out.push(v);
        p += n;
    }
    out
}
