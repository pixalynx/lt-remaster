//! Enemy profile fields → bytes (screen 13, PRD 13 §4.3). The form edits the
//! 20 fields of [`EnemyFields`]; everything else in the 0x398-byte profile is
//! kept as stored.
//!
//! [`encode_head`] is the port of `src/screens/13-data/encode.ts` (the "Show
//! raw" view): it rebuilds the first 0x140 bytes from the decoded profile.
//! [`patch`] is what saving uses: it writes only the fields that changed over
//! the stored bytes, so anything the decoder doesn't model survives. The tests
//! prove both agree with the disc and with each other for every profile.
//!
//! Layout (offsets for the 107 regular files; the prototypes `enemy/31`, `53`
//! and `79` have a 21-byte name, so every field after it is one byte later):
//!
//! ```text
//! +0x00 u16 species   +0x02 char[20] name   +0x16 u16 · +0x18 u8 · +0x19 u8
//! +0x1A u16 size, height, depth, width       +0x22 u16 · +0x24 u16
//! +0x26 u16 level     +0x28 s16 hp base     +0x2A s16 speed, attack, defense, status bonus
//! +0x32 u16 ai        +0x34 u32 affinity    +0x38 u16 immunity
//! +0x3A s16 gold      +0x3C s16 exp         +0x3E u16 drop B  +0x40 u16 drop A
//! +0x42 u8 chance B   +0x43 u8 chance A     +0x44 6 × 32-byte attacks
//! +0x104 u8[6] w      +0x10A u8[6] wl       +0x110 u8[12]  +0x11C u16[12] seq  +0x134 u16[6] scr
//! ```

use serde::{Deserialize, Serialize};

use crate::gamedata::enemy::{EnemyProfile, HEADER_LEN};

use super::{EditError, Result};

/// The flat set of fields the screen-13 form edits (`EnemyFields` in `data.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnemyFields {
    pub name: String,
    pub size: u16,
    pub height: u16,
    pub depth: u16,
    pub width: u16,
    pub level: u16,
    pub hp_base: i16,
    pub spd: i16,
    pub atk: i16,
    pub def: i16,
    pub stat: i16,
    pub ai: u16,
    pub aff: u32,
    pub imm: u16,
    pub gold: i16,
    pub exp: i16,
    pub drop_a: u16,
    pub drop_b: u16,
    pub chance_a: u8,
    pub chance_b: u8,
}

impl EnemyFields {
    /// The form values of a decoded profile (`fieldsOf` in `data.ts`).
    pub fn of(p: &EnemyProfile) -> Self {
        Self {
            name: p.name.clone(),
            size: p.dims[0],
            height: p.dims[1],
            depth: p.dims[2],
            width: p.dims[3],
            level: p.level,
            hp_base: p.hp_base,
            spd: p.bonus[0],
            atk: p.bonus[1],
            def: p.bonus[2],
            stat: p.bonus[3],
            ai: p.ai,
            aff: p.aff,
            imm: p.imm,
            gold: p.gold,
            exp: p.exp,
            drop_a: p.drop_a,
            drop_b: p.drop_b,
            chance_a: p.chance_a,
            chance_b: p.chance_b,
        }
    }

    /// Names of the fields that differ from `other` (camelCase, as in the UI).
    pub fn diff(&self, other: &Self) -> Vec<&'static str> {
        let mut out = Vec::new();
        macro_rules! cmp {
            ($($f:ident $n:literal),*) => { $( if self.$f != other.$f { out.push($n); } )* };
        }
        cmp!(name "name", size "size", height "height", depth "depth", width "width", level "level",
             hp_base "hpBase", spd "spd", atk "atk", def "def", stat "stat", ai "ai", aff "aff", imm "imm",
             gold "gold", exp "exp", drop_a "dropA", drop_b "dropB", chance_a "chanceA", chance_b "chanceB");
        out
    }
}

/// Bytes of the name field: 20, or 21 in a prototype file.
pub fn name_len(p: &EnemyProfile) -> usize {
    20 + p.proto as usize
}

/// Bytes [`encode_head`] and [`patch`] cover: 0x140, or 0x141 in a prototype.
pub fn head_len(p: &EnemyProfile) -> usize {
    HEADER_LEN + p.proto as usize
}

fn name_bytes(name: &str, n: usize) -> Result<Vec<u8>> {
    if let Some(c) = name.chars().find(|c| !(' '..='~').contains(c)) {
        return Err(EditError::new("ENEMY_NAME_CHARS", format!("The name can only use printable ASCII; “{c}” isn't one.")));
    }
    if name.len() > n - 1 {
        return Err(EditError::new("ENEMY_NAME_LONG", format!("The name is {} characters; the field holds {}.", name.len(), n - 1)));
    }
    let mut b = name.as_bytes().to_vec();
    b.resize(n, 0);
    Ok(b)
}

/// Re-encodes the head of a profile from its decoded values with the form
/// fields `f` (port of `encodeProfile` in `encode.ts`, which writes the name
/// as Latin-1 code units, zero-padded).
pub fn encode_head(p: &EnemyProfile, f: &EnemyFields) -> Vec<u8> {
    let sh = p.proto as usize;
    let mut b = vec![0u8; head_len(p)];
    let u16_ = |b: &mut Vec<u8>, o: usize, v: u16| b[o + sh..o + sh + 2].copy_from_slice(&v.to_le_bytes());
    let str_ = |b: &mut Vec<u8>, o: usize, s: &str, n: usize| {
        let units: Vec<u16> = s.encode_utf16().collect();
        for i in 0..n {
            b[o + i] = units.get(i).map(|&u| u as u8).unwrap_or(0);
        }
    };
    b[0..2].copy_from_slice(&p.species.to_le_bytes());
    str_(&mut b, 0x02, &f.name, name_len(p));
    u16_(&mut b, 0x16, p.unk16);
    b[0x18 + sh] = p.unk18;
    b[0x19 + sh] = p.unk19;
    u16_(&mut b, 0x1A, f.size);
    u16_(&mut b, 0x1C, f.height);
    u16_(&mut b, 0x1E, f.depth);
    u16_(&mut b, 0x20, f.width);
    u16_(&mut b, 0x22, p.unk22);
    u16_(&mut b, 0x24, p.unk24);
    u16_(&mut b, 0x26, f.level);
    u16_(&mut b, 0x28, f.hp_base as u16);
    u16_(&mut b, 0x2A, f.spd as u16);
    u16_(&mut b, 0x2C, f.atk as u16);
    u16_(&mut b, 0x2E, f.def as u16);
    u16_(&mut b, 0x30, f.stat as u16);
    u16_(&mut b, 0x32, f.ai);
    b[0x34 + sh..0x38 + sh].copy_from_slice(&f.aff.to_le_bytes());
    u16_(&mut b, 0x38, f.imm);
    u16_(&mut b, 0x3A, f.gold as u16);
    u16_(&mut b, 0x3C, f.exp as u16);
    u16_(&mut b, 0x3E, f.drop_b);
    u16_(&mut b, 0x40, f.drop_a);
    b[0x42 + sh] = f.chance_b;
    b[0x43 + sh] = f.chance_a;
    for (i, a) in p.atk.iter().enumerate() {
        let o = 0x44 + i * 32;
        u16_(&mut b, o, a.0);
        str_(&mut b, o + 2 + sh, &a.1, 20);
        b[o + 0x16 + sh] = a.2;
        b[o + 0x17 + sh] = a.3;
        u16_(&mut b, o + 0x18, a.4);
        u16_(&mut b, o + 0x1A, a.5);
        u16_(&mut b, o + 0x1C, a.6);
        u16_(&mut b, o + 0x1E, a.7);
    }
    b[0x104 + sh..0x10A + sh].copy_from_slice(&p.w);
    b[0x10A + sh..0x110 + sh].copy_from_slice(&p.wl);
    b[0x110 + sh..0x11C + sh].copy_from_slice(&p.unk110);
    for (i, v) in p.seq.iter().enumerate() {
        u16_(&mut b, 0x11C + i * 2, *v);
    }
    for (i, v) in p.scr.iter().enumerate() {
        u16_(&mut b, 0x134 + i * 2, *v);
    }
    b
}

/// Writes the form fields that differ from the decoded profile `p` into its
/// stored head `raw` (`p.raw`, at least [`head_len`] bytes). Unchanged fields
/// keep their stored bytes, including a name's non-ASCII leftovers.
pub fn patch(p: &EnemyProfile, raw: &[u8], f: &EnemyFields) -> Result<Vec<u8>> {
    let sh = p.proto as usize;
    let len = head_len(p);
    if raw.len() < len {
        return Err(EditError::new("ENEMY_SHORT", format!("enemy/{} has only {:#x} profile bytes", p.id, raw.len())));
    }
    let mut b = raw[..len].to_vec();
    let old = EnemyFields::of(p);
    let mut put = |o: usize, bytes: &[u8]| b[o + sh..o + sh + bytes.len()].copy_from_slice(bytes);
    if f.name != old.name {
        let n = name_len(p);
        let bytes = name_bytes(&f.name, n)?;
        // The name starts before the shift.
        put(0x02 - sh, &bytes);
    }
    macro_rules! field {
        ($f:ident, $o:literal) => {
            if f.$f != old.$f {
                put($o, &f.$f.to_le_bytes());
            }
        };
    }
    field!(size, 0x1A);
    field!(height, 0x1C);
    field!(depth, 0x1E);
    field!(width, 0x20);
    field!(level, 0x26);
    field!(hp_base, 0x28);
    field!(spd, 0x2A);
    field!(atk, 0x2C);
    field!(def, 0x2E);
    field!(stat, 0x30);
    field!(ai, 0x32);
    field!(aff, 0x34);
    field!(imm, 0x38);
    field!(gold, 0x3A);
    field!(exp, 0x3C);
    field!(drop_b, 0x3E);
    field!(drop_a, 0x40);
    field!(chance_b, 0x42);
    field!(chance_a, 0x43);
    Ok(b)
}

/// Stages a profile into its file: `file` is `enemy/n` as the project has it
/// and `p` the profile decoded from it. Returns the new file bytes.
pub fn patch_file(p: &EnemyProfile, file: &[u8], f: &EnemyFields) -> Result<Vec<u8>> {
    let at = p.prof_ofs as usize;
    let len = head_len(p);
    let raw = file.get(at..at + len).ok_or_else(|| EditError::new("ENEMY_SHORT", format!("enemy/{}: the profile runs past the file end", p.id)))?;
    let head = patch(p, raw, f)?;
    let mut out = file.to_vec();
    out[at..at + len].copy_from_slice(&head);
    Ok(out)
}
