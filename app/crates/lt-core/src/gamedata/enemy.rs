//! Enemy profiles (PRD §4.3–4.4, `GuardiansCrusade_Docs/ENEMY_BIN_Format.md`).
//!
//! `enemy/n` (n = 2..=111; 0 and 1 alias the Knight files) starts with 7
//! self-relocating offsets: 0 poses, 1 fx model, 2 TSQ, 3 TIM, 4 TVB, 5 model,
//! 6 profile. The profile is the 0x398 bytes the battle copies to
//! `g_EnemyMetadataTable` (0x80014ED8 + slot·0x398); sequences and attack
//! scripts follow at +0x140. In 17 files the section is shorter than 0x398
//! (the copy runs past the file end); `raw` then holds what the file has.
//! Prototype files `enemy/31`, `enemy/53` and
//! `enemy/79` (ext `ENEMY030/052/078.BIN`) have a 21-byte name, so every later
//! field sits one byte further; they are decoded re-aligned.

use serde::Serialize;

use crate::container::{le16, le32, le_i16};

use super::encounters::{Encounters, Home};
use super::modules::{enemy_mask, EffectTable};
use super::{bad, fixed_str_in, Result, Source};
use crate::text::Encoding;

pub const PROFILE_LEN: usize = 0x398;
/// The fixed fields before the sequence/script data.
pub const HEADER_LEN: usize = 0x140;
pub const FIRST: u16 = 2;
pub const LAST: u16 = 111;
pub const PROTOTYPES: [u16; 3] = [31, 53, 79];
const HEADER_OFFSETS: usize = 7;

/// One 32-byte attack record at +0x44, serialised as
/// `[effect, name, hitMode, targetMode, powerBonus, element, status, flags]`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Attack(pub u16, pub String, pub u8, pub u8, pub u16, pub u16, pub u16, pub u16);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnemyProfile {
    /// Canonical index n of `enemy/n` = enemy table entry = species id.
    pub id: u16,
    /// Extractor file number (`ENEMYnnn.BIN`, n − 1 numbering).
    pub file: u16,
    /// The u16 at +0x00 as stored (equals `id` in retail).
    pub species: u16,
    pub name: String,
    pub proto: bool,
    /// File size in bytes (whole sectors).
    pub size: u32,
    /// File offset of the profile (section 6).
    pub prof_ofs: u32,
    pub unk16: u16,
    pub unk18: u8,
    pub unk19: u8,
    /// size, height, depth, width (+0x1A…+0x20).
    pub dims: [u16; 4],
    pub unk22: u16,
    pub unk24: u16,
    pub level: u16,
    pub hp_base: i16,
    /// speed, attack, defense, status-power bonus (+0x2A…+0x30).
    pub bonus: [i16; 4],
    pub ai: u16,
    pub aff: u32,
    pub imm: u16,
    pub gold: i16,
    pub exp: i16,
    pub drop_b: u16,
    pub drop_a: u16,
    pub chance_b: u8,
    pub chance_a: u8,
    pub atk: Vec<Attack>,
    pub w: [u8; 6],
    pub wl: [u8; 6],
    /// +0x110, zero in 109 of 110 files.
    pub unk110: [u8; 12],
    pub seq: [u16; 12],
    pub scr: [u16; 6],
    /// Arena-module mask (bit n = OVRn); 0xFFFFFFFF = fights in any arena.
    pub mask: u32,
    /// Zones and battles the retail game stages this species in.
    pub homes: Vec<Home>,
    /// The profile bytes as stored: 0x398 (0x399 for a prototype), or up to
    /// the file end when the section is shorter.
    #[serde(skip)]
    pub raw: Vec<u8>,
}

/// Decodes a profile. `section` is section 6 to the end of the file (the
/// scripts after +0x140 are needed for the module mask).
pub fn parse(id: u16, size: u32, prof_ofs: u32, section: &[u8], effects: &EffectTable) -> Result<EnemyProfile> {
    parse_in(id, size, prof_ofs, section, effects, Encoding::Ascii)
}

/// [`parse`] reading names in `enc` (Shift-JIS on the Japanese release).
pub fn parse_in(id: u16, size: u32, prof_ofs: u32, section: &[u8], effects: &EffectTable, enc: Encoding) -> Result<EnemyProfile> {
    let proto = PROTOTYPES.contains(&id);
    let sh = proto as usize;
    if section.len() < HEADER_LEN + sh {
        return Err(bad(format!("enemy/{id}: profile section is only {:#x} bytes", section.len())));
    }
    let p = &section[..section.len().min(PROFILE_LEN + sh)];
    let u16_ = |o: usize| le16(p, o + sh);
    let s16 = |o: usize| le_i16(p, o + sh);
    let u8_ = |o: usize| p[o + sh];
    let atk = (0..6)
        .map(|s| {
            let a = 0x44 + s * 32;
            Attack(
                u16_(a),
                fixed_str_in(&p[a + 2 + sh..a + 22 + sh], enc),
                u8_(a + 0x16),
                u8_(a + 0x17),
                u16_(a + 0x18),
                u16_(a + 0x1A),
                u16_(a + 0x1C),
                u16_(a + 0x1E),
            )
        })
        .collect();
    let arr6 = |o: usize| -> [u8; 6] { p[o + sh..o + sh + 6].try_into().unwrap() };
    Ok(EnemyProfile {
        id,
        file: id - 1,
        species: le16(p, 0),
        name: fixed_str_in(&p[2..0x16 + sh], enc),
        proto,
        size,
        prof_ofs,
        unk16: u16_(0x16),
        unk18: u8_(0x18),
        unk19: u8_(0x19),
        dims: [u16_(0x1A), u16_(0x1C), u16_(0x1E), u16_(0x20)],
        unk22: u16_(0x22),
        unk24: u16_(0x24),
        level: u16_(0x26),
        hp_base: s16(0x28),
        bonus: [s16(0x2A), s16(0x2C), s16(0x2E), s16(0x30)],
        ai: u16_(0x32),
        aff: le32(p, 0x34 + sh),
        imm: u16_(0x38),
        gold: s16(0x3A),
        exp: s16(0x3C),
        drop_b: u16_(0x3E),
        drop_a: u16_(0x40),
        chance_b: u8_(0x42),
        chance_a: u8_(0x43),
        atk,
        w: arr6(0x104),
        wl: arr6(0x10A),
        unk110: p[0x110 + sh..0x11C + sh].try_into().unwrap(),
        seq: std::array::from_fn(|i| u16_(0x11C + i * 2)),
        scr: std::array::from_fn(|i| u16_(0x134 + i * 2)),
        mask: enemy_mask(section, sh, effects),
        homes: Vec::new(),
        raw: p.to_vec(),
    })
}

/// Reads one `enemy/n`: the 7-offset header, then section 6 to the file end.
pub fn load(src: Source<'_>, id: u16, effects: &EffectTable) -> Result<EnemyProfile> {
    let name = format!("enemy/{id}");
    let size = src.file_len(&name)?;
    let head = src.file_part(&name, 0, HEADER_OFFSETS * 4)?;
    let prof_ofs = le32(&head, 6 * 4);
    if prof_ofs as usize + HEADER_LEN > size as usize {
        return Err(bad(format!("{name}: profile offset {prof_ofs:#x} is past the end")));
    }
    let section = src.file_part(&name, prof_ofs, (size - prof_ofs) as usize)?;
    parse_in(id, size, prof_ofs, &section, effects, src.encoding())
}

/// All 110 profiles, with their homes from the encounter tables.
pub fn load_all(src: Source<'_>, effects: &EffectTable, enc: &Encounters) -> Result<Vec<EnemyProfile>> {
    (FIRST..=LAST)
        .map(|id| {
            let mut e = load(src, id, effects)?;
            e.homes = enc.homes(id as u8);
            Ok(e)
        })
        .collect()
}
