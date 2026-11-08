//! Living Toys (PRD §4.1–4.2, `GuardiansCrusade_Docs/TOY_BIN_Format.md`).
//!
//! Roster: EXE `LivingToyRoster` 0x801B69E0 (file 0x441E0), 72 × 24 B
//! (`LTRoster2`, `include/game.h:320`). Abilities: section 1 of `toy/n`, three
//! 0x2C-byte phase records, then +0x84 u8[4], +0x88 legacy id, +0x8A/+0x8C/+0x8E
//! actor size/height/width. `toy/72` (ext `TOY73.BIN`) has no roster row.

use serde::Serialize;

use crate::container::{le16, le32};

use super::{fixed_str_in, Result, Source};
use crate::text::Encoding;

pub const ROSTER: u32 = 0x801B_69E0;
pub const ROSTER_ROWS: usize = 72;
pub const ROW_SIZE: usize = 24;
/// `toy/0..=72`.
pub const TOY_FILES: u16 = 73;
const PHASE_SIZE: usize = 0x2C;
/// Bytes of section 1 we decode (phases + actor block up to the offset table).
const ABILITY_HEAD: usize = 0x90;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Phase {
    /// char[20]; unused records carry "ダミー".
    pub name: String,
    /// +0x14, OR'd into `g_BattleFlags` by effect 0x1E.
    pub flag: u8,
    /// +0x17 → actor +0x2B.
    pub attr: u8,
    /// +0x18 resistance mask.
    pub resist: u32,
    /// +0x1C status mask inflicted.
    pub status: u16,
    /// +0x1E target flags (0x43 all enemies, 0x20 no select, 0xFF self).
    pub target: u16,
    /// Power range (9999 = full), or turn counter/limit for kinds 0xA0/0xB0.
    pub min: i32,
    pub max: i32,
    /// 4 = auto, 0 = on command.
    pub timing: u8,
    /// High nibble chaining 00–B0, low nibble 1 = attack path.
    pub kind: u8,
    /// 1–0x2E (jump table 0x80174644; 0x28 = do nothing), bit 0x40 variant.
    pub effect: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToyFile {
    /// Canonical id `toy/n`.
    pub asset: String,
    pub size: u32,
    pub phases: Vec<Phase>,
    pub unk84: [u8; 4],
    /// +0x88, overwritten with the roster row at summon time.
    pub legacy_id: u16,
    /// Actor size, height, width (+0x8A…+0x8E).
    pub dims: [u16; 3],
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Toy {
    pub row: u8,
    pub name: String,
    pub pp: u16,
    pub hp: u16,
    pub max_hp: u16,
    /// +0x0A, must equal the row.
    pub row_byte: u8,
    /// 6 support · 7 attack all · 8 special.
    pub category: u8,
    pub flags: u16,
    pub target: u16,
    /// +0x10 extra visual task (code pointer, read-only).
    pub fx: u32,
    pub desc: String,
    /// `char*` of the name and description, and the characters their slots hold in place.
    pub name_ptr: u32,
    pub desc_ptr: u32,
    pub name_room: u8,
    pub desc_room: u8,
    pub file: Option<ToyFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Toys {
    pub roster: Vec<Toy>,
    /// Toy files without a roster row (`toy/72`, the Peacemaker duplicate).
    pub unlisted: Vec<ToyFile>,
}

fn phase(r: &[u8], enc: Encoding) -> Phase {
    Phase {
        name: fixed_str_in(&r[..20], enc),
        flag: r[0x14],
        attr: r[0x17],
        resist: le32(r, 0x18),
        status: le16(r, 0x1C),
        target: le16(r, 0x1E),
        min: le32(r, 0x20) as i32,
        max: le32(r, 0x24) as i32,
        timing: r[0x28],
        kind: r[0x29],
        effect: le16(r, 0x2A),
    }
}

/// Section 1 of `toy/n`: the header's offset 1, read only as far as needed.
fn toy_file(src: Source<'_>, n: u16) -> Result<Option<ToyFile>> {
    let asset = format!("toy/{n}");
    let size = src.file_len(&asset)?;
    let head = src.file_part(&asset, 0, 8)?;
    let s1 = le32(&head, 4);
    if s1 as usize + ABILITY_HEAD > size as usize {
        return Ok(None);
    }
    let f1 = src.file_part(&asset, s1, ABILITY_HEAD)?;
    Ok(Some(ToyFile {
        asset,
        size,
        phases: (0..3).map(|k| phase(&f1[k * PHASE_SIZE..(k + 1) * PHASE_SIZE], src.encoding())).collect(),
        unk84: f1[0x84..0x88].try_into().unwrap(),
        legacy_id: le16(&f1, 0x88),
        dims: [le16(&f1, 0x8A), le16(&f1, 0x8C), le16(&f1, 0x8E)],
    }))
}

impl Toys {
    pub fn load(src: Source<'_>) -> Result<Self> {
        let b = src.exe_bytes(src.exe.va(ROSTER), ROSTER_ROWS * ROW_SIZE)?;
        let mut roster = Vec::with_capacity(ROSTER_ROWS);
        for row in 0..ROSTER_ROWS {
            let r = &b[row * ROW_SIZE..(row + 1) * ROW_SIZE];
            roster.push(Toy {
                row: row as u8,
                name: src.exe_str(le32(r, 0)).unwrap_or_default(),
                pp: le16(r, 4),
                hp: le16(r, 6),
                max_hp: le16(r, 8),
                row_byte: r[0x0A],
                category: r[0x0B],
                flags: le16(r, 0x0C),
                target: le16(r, 0x0E),
                fx: le32(r, 0x10),
                desc: src.exe_str(le32(r, 0x14)).unwrap_or_default(),
                name_ptr: le32(r, 0),
                desc_ptr: le32(r, 0x14),
                name_room: crate::edit::tables::string_room(&src.retail_exe.bytes, src.retail_exe.t_addr, le32(r, 0)),
                desc_room: crate::edit::tables::string_room(&src.retail_exe.bytes, src.retail_exe.t_addr, le32(r, 0x14)),
                file: toy_file(src, row as u16)?,
            });
        }
        let unlisted = (ROSTER_ROWS as u16..TOY_FILES).filter_map(|n| toy_file(src, n).transpose()).collect::<Result<_>>()?;
        Ok(Self { roster, unlisted })
    }
}
