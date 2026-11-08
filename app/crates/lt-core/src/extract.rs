//! Real sizes of table files, for exports (screen 05, Export raw / Extract).
//!
//! Files sit on the disc in whole 2048-byte sectors (`LoadFile` returns
//! `sectors << 11`), so most of them end in zero padding. Where the format
//! states its own length — a TIM, a TMD, a sequence + voice bank, an enemy
//! profile — an export can drop that padding. Everything else (NPC packs,
//! overlays, system packets, movies) keeps its whole sectors.
//!
//! Only padding is ever dropped: the bytes past the stated length must all be
//! zero and fit inside the last sector, otherwise the length is not trusted.

use crate::audio::tvb::Tvb;
use crate::container::{self, le32};
use crate::fs::Kind;
use crate::tim::Tim;

pub const SECTOR: usize = 2048;
/// Enemy profile record at the last offset of `enemy/n` (n ≥ 2).
pub const ENEMY_PROFILE: usize = 0x398;

/// Bytes `b` really uses, or `None` when the format doesn't say (write whole
/// sectors). `b` may be sector padded (retail) or exact (a staged edit).
pub fn content_len(kind: Kind, index: u16, b: &[u8]) -> Option<usize> {
    let len = format_len(kind, index, b)?;
    let pad = b.len().checked_sub(len)?;
    if len == 0 || pad >= SECTOR || b[len..].iter().any(|&x| x != 0) {
        return None;
    }
    Some(len)
}

fn format_len(kind: Kind, index: u16, b: &[u8]) -> Option<usize> {
    match kind {
        // Plain TIMs (field texture sets, most system pages).
        Kind::Fe => tim_len(b),
        Kind::Sys => match index {
            1 => tvb_len(b),      // SE.TVB
            8 => bank_len(b),     // field SE: TSQ + TVB
            _ => tim_len(b),      // TIM pages; packets return None
        },
        // `u32 seqOfs; u32 bankOfs` — the TVB ends the file.
        Kind::Bgm | Kind::NpcSnd => bank_len(b),
        // `u32 8; u32 modelOfs`: TIM at 8, then the model.
        Kind::Bf => {
            if le32_at(b, 0)? != 8 {
                return None;
            }
            let model = le32_at(b, 4)? as usize;
            Some(model + tmd_len(b.get(model..)?)?)
        }
        Kind::Toy => last_section(b, 8, None),
        Kind::Enemy if index >= 2 => last_section(b, 7, Some(6)),
        Kind::Enemy | Kind::Knight => last_section(b, 4, None),
        Kind::Pb => last_section(b, 6, None),
        _ => None,
    }
}

/// Whether `b`'s header has the shape its table expects (PRD 05 §5.7 step 3:
/// a replacement that fails this gets a "doesn't look like…" warning).
/// Kinds without a recognisable header always pass.
pub fn shape_ok(kind: Kind, index: u16, b: &[u8]) -> bool {
    match kind {
        Kind::Toy => container::sections(b, 8).is_some(),
        Kind::Enemy if index >= 2 => container::sections(b, 7).is_some(),
        Kind::Enemy | Kind::Knight => container::sections(b, 4).is_some(),
        Kind::Pb => container::sections(b, 6).is_some(),
        Kind::Fe => Tim::parse(b).is_some(),
        Kind::Bgm | Kind::NpcSnd => bank_len(b).is_some(),
        Kind::Bf => le32_at(b, 0) == Some(8) && b.get(8..).and_then(Tim::parse).is_some(),
        // `u32 P; u32 D; u32 models[]`: the model table runs from +0x08 to P.
        Kind::Npc => match (le32_at(b, 0), le32_at(b, 4)) {
            (Some(p), Some(d)) => p >= 8 && p % 4 == 0 && p < d && (d as usize) < b.len(),
            _ => false,
        },
        _ => true,
    }
}

fn le32_at(b: &[u8], at: usize) -> Option<u32> {
    (b.len() >= at + 4).then(|| le32(b, at))
}

/// A TIM at the start of `b`.
pub fn tim_len(b: &[u8]) -> Option<usize> {
    Tim::parse(b).map(|t| t.info.len)
}

/// A TVB voice bank at the start of `b`: header + ADPCM body.
pub fn tvb_len(b: &[u8]) -> Option<usize> {
    Tvb::parse(b).filter(|t| t.adpcm.len() == t.size as usize).map(|t| t.total_bytes())
}

/// `u32 seqOfs; u32 bankOfs` + TSQ + TVB (music, NPC sounds, field SE).
pub fn bank_len(b: &[u8]) -> Option<usize> {
    let (seq, bank) = (le32_at(b, 0)? as usize, le32_at(b, 4)? as usize);
    if seq < 8 || bank <= seq {
        return None;
    }
    Some(bank + tvb_len(b.get(bank..)?)?)
}

/// Length of a Sony TMD (ID 0x41) at the start of `b`: the furthest end of
/// any object's vertex, normal or primitive table.
pub fn tmd_len(b: &[u8]) -> Option<usize> {
    if le32_at(b, 0)? != 0x41 {
        return None;
    }
    let flags = le32(b, 4);
    let nobj = le32_at(b, 8)? as usize;
    if nobj == 0 || nobj > 256 {
        return None;
    }
    let table = 12usize;
    // FIXP clear: offsets are relative to the object table.
    let rel = |o: u32| if flags & 1 == 0 { table + o as usize } else { o as usize };
    let mut end = table + nobj * 28;
    for i in 0..nobj {
        let o = table + i * 28;
        let field = |k: usize| le32_at(b, o + k * 4);
        let (vert, n_vert, norm, n_norm, prim, n_prim) = (field(0)?, field(1)?, field(2)?, field(3)?, field(4)?, field(5)?);
        end = end.max(rel(vert) + n_vert as usize * 8).max(rel(norm) + n_norm as usize * 8);
        let mut p = rel(prim);
        for _ in 0..n_prim {
            let ilen = *b.get(p + 1)? as usize;
            p += 4 + ilen * 4;
        }
        end = end.max(p);
    }
    (end <= b.len()).then_some(end)
}

/// Offset containers: the section that comes last in the file decides the
/// length when it is a TMD, a TIM, a TVB or (enemies) the fixed-size profile.
fn last_section(b: &[u8], count: usize, profile: Option<usize>) -> Option<usize> {
    let sections = container::sections(b, count)?;
    let last = sections.iter().max_by_key(|s| s.offset)?;
    let off = last.offset as usize;
    let body = b.get(off..)?;
    if profile == Some(last.index) {
        return Some(off + ENEMY_PROFILE);
    }
    tmd_len(body).or_else(|| tim_len(body)).or_else(|| tvb_len(body)).map(|n| off + n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_only_zero_padding() {
        // A 16bpp TIM, 4×1 px, no CLUT: 8 + 12 + 8 = 28 bytes, padded to a sector.
        let mut b = vec![0u8; SECTOR];
        b[0] = 0x10;
        let bnum = 12u32 + 8;
        b[8..12].copy_from_slice(&bnum.to_le_bytes());
        b[16..18].copy_from_slice(&4u16.to_le_bytes()); // w = 4 halfwords
        b[18..20].copy_from_slice(&1u16.to_le_bytes()); // h = 1
        b[20] = 0x11;
        assert_eq!(content_len(Kind::Fe, 0, &b), Some(28));
        b[100] = 1;
        assert_eq!(content_len(Kind::Fe, 0, &b), None, "data past the TIM is not padding");
        assert_eq!(content_len(Kind::Npc, 7, &b), None);
    }
}
