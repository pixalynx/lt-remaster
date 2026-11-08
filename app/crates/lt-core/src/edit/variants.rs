//! Chunk variants on a HackerGC base (`gc_decomp/docs/HACKERGC.md` "Chunk variants",
//! `src/hgc/hgc_variants.c`): while a story flag is set, the game reads a chunk's geometry, walk
//! heights, zones and texture page from another chunk id, a new area holding a changed copy (the
//! bridge mended, the town burnt). Everything the game keys by the chunk's own id stays its own:
//! the grid cell, the NPC pack and its triggers, the music.
//!
//! - `g_HgcChunkVariants`: `{u16 chunk, flag, variant, pad}` entries at the layout's
//!   `chunkVariants`, variant 0 an empty one. The first entry for a chunk whose flag is set wins,
//!   so the table keeps the order the project made them in.
//! - A resident chunk keeps what it was read with. The change shows at the next warp, field entry
//!   or battle's end, which drop the slots holding the other version: a scene that sets the flag
//!   and warps to the same spot switches it behind the fade.

use serde::{Deserialize, Serialize};

use super::boot::offset;
use super::{EditError, Result};
use crate::container::le16;

/// Bytes of one entry.
pub const ENTRY: usize = 8;

/// One entry: while `flag` is set, `chunk` is read from `variant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    pub chunk: u16,
    pub flag: u16,
    pub variant: u16,
}

fn bad(code: &'static str, message: impl Into<String>) -> EditError {
    EditError::new(code, message)
}

fn needs_hackergc() -> EditError {
    bad("VARIANT_NEEDS_HACKERGC", "Chunk variants need a HackerGC base with a variant table (v1.2); this EXE has none.")
}

/// The table's offset in `exe` and its entries (empty ones included), in table order.
fn slots(exe: &[u8]) -> Option<(usize, Vec<Variant>)> {
    let (va, count) = crate::layout::for_exe(exe)?.chunk_variants?;
    let at = offset(exe, va, count as usize * ENTRY)?;
    let read = |k: usize| Variant { chunk: le16(exe, at + ENTRY * k), flag: le16(exe, at + ENTRY * k + 2), variant: le16(exe, at + ENTRY * k + 4) };
    Some((at, (0..count as usize).map(read).collect()))
}

/// Whether this EXE has a variant table a project can write.
pub fn supported(exe: &[u8]) -> bool {
    slots(exe).is_some()
}

/// How many variants the table holds at most.
pub fn capacity(exe: &[u8]) -> u32 {
    slots(exe).map_or(0, |(_, s)| s.len() as u32)
}

/// Every variant, in table order (the first set flag wins for a chunk).
pub fn list(exe: &[u8]) -> Vec<Variant> {
    slots(exe).map(|(_, s)| s.into_iter().filter(|v| v.variant != 0).collect()).unwrap_or_default()
}

/// `chunk`'s variants, the one the game prefers first.
pub fn of(exe: &[u8], chunk: u32) -> Vec<Variant> {
    list(exe).into_iter().filter(|v| v.chunk as u32 == chunk).collect()
}

/// The entry whose variant chunk is `id`, if `id` is one.
pub fn variant(exe: &[u8], id: u32) -> Option<Variant> {
    list(exe).into_iter().find(|v| v.variant as u32 == id)
}

/// The chunk id the game reads `chunk` from with these flags set.
pub fn shown(exe: &[u8], chunk: u32, set: &dyn Fn(u16) -> bool) -> u32 {
    of(exe, chunk).into_iter().find(|v| set(v.flag)).map_or(chunk, |v| v.variant as u32)
}

fn write(exe: &[u8], at: usize, k: usize, v: Variant) -> Vec<u8> {
    let mut out = exe.to_vec();
    for (i, w) in [v.chunk, v.flag, v.variant, 0].into_iter().enumerate() {
        let o = at + ENTRY * k + 2 * i;
        out[o..o + 2].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// `exe` with `v` in its table: the entry for `v.variant` changed, else a new one in the first
/// empty slot (after the chunk's other variants, so they keep their precedence).
pub fn put(exe: &[u8], v: Variant) -> Result<Vec<u8>> {
    let (at, s) = slots(exe).ok_or_else(needs_hackergc)?;
    if !crate::world::is_new_area(v.variant as u32) {
        return Err(bad("VARIANT_ID", format!("A variant is a new area (chunk {}–{}); chunk {} isn't one.", crate::world::NEW_AREA_FIRST, crate::world::NEW_AREA_LAST, v.variant)));
    }
    if !crate::world::valid_chunk(v.chunk as u32) || v.chunk == v.variant {
        return Err(bad("VARIANT_CHUNK", format!("Chunk {} can't have chunk {} as a variant.", v.chunk, v.variant)));
    }
    if s.iter().any(|e| e.variant != 0 && e.variant == v.chunk) {
        return Err(bad("VARIANT_OF_VARIANT", format!("Chunk {} is itself a variant; make the variant from the chunk it stands in for.", v.chunk)));
    }
    if s.iter().any(|e| e.variant != 0 && e.chunk == v.variant) {
        return Err(bad("VARIANT_HAS_VARIANTS", format!("Chunk {} has variants of its own, so it can't be one.", v.variant)));
    }
    let k = match s.iter().position(|e| e.variant == v.variant) {
        Some(k) if s[k].chunk != v.chunk => {
            return Err(bad("VARIANT_TAKEN", format!("Chunk {} is already a variant of chunk {}.", v.variant, s[k].chunk)));
        }
        Some(k) => k,
        None => {
            let last = s.iter().rposition(|e| e.variant != 0 && e.chunk == v.chunk);
            let free = |from: usize| (from..s.len()).find(|&k| s[k].variant == 0);
            last.map_or_else(|| free(0), |l| free(l + 1)).ok_or_else(|| {
                bad("VARIANT_FULL", format!("The base's variant table is full ({} variants), or has no free entry after chunk {}'s others.", s.len(), v.chunk))
            })?
        }
    };
    Ok(write(exe, at, k, v))
}

/// `exe` without the entry for variant chunk `variant` (the others keep their slots and order).
pub fn remove(exe: &[u8], variant: u16) -> Result<Vec<u8>> {
    let (at, s) = slots(exe).ok_or_else(needs_hackergc)?;
    match s.iter().position(|e| e.variant != 0 && e.variant == variant) {
        Some(k) => Ok(write(exe, at, k, Variant { chunk: 0, flag: 0, variant: 0 })),
        None => Ok(exe.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in EXE with a 4-entry table at 0x80010000. Its pc0 and t_size are its own, so
    /// edited copies keep the layout.
    fn exe() -> Vec<u8> {
        let mut e = vec![0u8; 0x800 + 0x40];
        e[0x10..0x14].copy_from_slice(&0x8001_C0DEu32.to_le_bytes());
        e[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        e[0x1C..0x20].copy_from_slice(&0x40u32.to_le_bytes());
        e[0x30..0x38].copy_from_slice(b"variants");
        let mut h = sha1_smol::Sha1::new();
        h.update(&e);
        crate::layout::register(crate::layout::Layout {
            exe_sha1: h.digest().to_string(),
            pc0: 0x8001_C0DE,
            t_size: 0x40,
            chunk_variants: Some((0x8001_0000, 4)),
            ..Default::default()
        });
        e
    }

    #[test]
    fn adds_orders_and_removes_variants() {
        let e = exe();
        assert!(supported(&e));
        assert_eq!(capacity(&e), 4);
        assert!(list(&e).is_empty());
        let mended = Variant { chunk: 42, flag: 0x200, variant: 1330 };
        let one = put(&e, mended).unwrap();
        assert_eq!(of(&one, 42), [mended]);
        assert_eq!(variant(&one, 1330), Some(mended));
        assert_eq!(shown(&one, 42, &|_| false), 42);
        assert_eq!(shown(&one, 42, &|f| f == 0x200), 1330);
        // Another chunk's, then a second variant of 42: it goes after 42's first, before nothing else.
        let two = put(&one, Variant { chunk: 78, flag: 0x201, variant: 1331 }).unwrap();
        let burnt = Variant { chunk: 42, flag: 0x202, variant: 1334 };
        let three = put(&two, burnt).unwrap();
        assert_eq!(of(&three, 42), [mended, burnt]);
        assert_eq!(shown(&three, 42, &|f| f == 0x200 || f == 0x202), 1330);
        assert_eq!(shown(&three, 42, &|f| f == 0x202), 1334);
        // A new flag for the same variant changes its entry in place.
        let moved = put(&three, Variant { flag: 0x203, ..mended }).unwrap();
        assert_eq!(of(&moved, 42)[0].flag, 0x203);
        assert_eq!(list(&moved).len(), 3);
        let gone = remove(&moved, 1330).unwrap();
        assert_eq!(of(&gone, 42), [burnt]);
        assert_eq!(remove(&gone, 1330).unwrap(), gone);
    }

    #[test]
    fn refuses_what_the_game_cant_read() {
        let e = exe();
        let code = |r: Result<Vec<u8>>| r.unwrap_err().code;
        assert_eq!(code(put(&e, Variant { chunk: 42, flag: 1, variant: 300 })), "VARIANT_ID");
        let one = put(&e, Variant { chunk: 42, flag: 1, variant: 1330 }).unwrap();
        assert_eq!(code(put(&one, Variant { chunk: 1330, flag: 1, variant: 1331 })), "VARIANT_OF_VARIANT");
        assert_eq!(code(put(&one, Variant { chunk: 78, flag: 1, variant: 1330 })), "VARIANT_TAKEN");
        assert_eq!(code(put(&one, Variant { chunk: 1331, flag: 1, variant: 42 })), "VARIANT_ID");
        let mut full = one.clone();
        for v in 1331..1334 {
            full = put(&full, Variant { chunk: 78, flag: 1, variant: v }).unwrap();
        }
        assert_eq!(code(put(&full, Variant { chunk: 79, flag: 1, variant: 1340 })), "VARIANT_FULL");
        assert_eq!(code(put(&[0u8; 0x900], Variant { chunk: 42, flag: 1, variant: 1330 })), "VARIANT_NEEDS_HACKERGC");
    }
}
