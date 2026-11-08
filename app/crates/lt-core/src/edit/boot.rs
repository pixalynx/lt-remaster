//! A HackerGC base's boot map (`gc_decomp/docs/HACKERGC.md` "Boot map", `src/hgc/hgc_boot.c`):
//! where a new game starts, and the NPC pack of chunks the OVR02 chunk → pack table leaves out
//! (a project's new areas). Both are data in the base's EXE, at the addresses its layout names
//! (`boot`, `areaPacks`), so a project moves the start by rewriting them.
//!
//! - `g_HgcBoot`: `{u16 chunk; s16 x, h, z; u16 facing; u16 pad}`. New Game, the debug new game
//!   and the title (through `Hgc_NewGamePack`) read it.
//! - `g_HgcAreaPacks`: `{s16 chunk, s16 pack}` pairs, chunk 0 an empty slot.
//!   `Field_ChunkStreamTask` asks it before the NPC task, so a new game, a Continue and a warp
//!   into the area load that pack. A door that named a pack still wins, and a chunk in the
//!   OVR02 table still takes the table's.
//!
//! A blank base sets 0xFF ("Knight hidden") at a new game, which only a scene clears, so a
//! start somewhere new needs the starter scene there too ([`super::blank::starter_at`]).

use serde::{Deserialize, Serialize};

use super::{EditError, Result};
use crate::container::{le16, le32};

/// Bytes of `g_HgcBoot`.
pub const BOOT_SIZE: usize = 12;
/// 0x1000 is a full turn; Tier-2 facings are eighths of it.
pub const EIGHTH: u16 = 0x200;

/// Where a new game starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Boot {
    pub chunk: u16,
    /// The Knight's and the Baby's spawn, chunk-local (−0x800..0x800); height negative up.
    pub x: i16,
    pub h: i16,
    pub z: i16,
    /// 0x1000 = a full turn (retail 0x9FD, about five eighths).
    pub facing: u16,
}

impl Boot {
    /// The facing in eighths (0 = +z, 2 = +x), as a Tier-2 WARP takes it.
    pub fn eighths(&self) -> u8 {
        (((self.facing as u32 + EIGHTH as u32 / 2) / EIGHTH as u32) % 8) as u8
    }
}

fn bad(code: &'static str, message: impl Into<String>) -> EditError {
    EditError::new(code, message)
}

/// The file offset of `len` bytes at `va` in a PS-EXE.
pub(crate) fn offset(exe: &[u8], va: u32, len: usize) -> Option<usize> {
    let t_addr = le32(exe.get(..0x20)?, 0x18);
    let off = va.checked_sub(t_addr)? as usize + 0x800;
    (off + len <= exe.len()).then_some(off)
}

fn layout(exe: &[u8]) -> Option<std::sync::Arc<crate::layout::Layout>> {
    crate::layout::for_exe(exe)
}

/// Whether this EXE has a boot map a project can move.
pub fn supported(exe: &[u8]) -> bool {
    layout(exe).is_some_and(|l| l.boot.is_some() && l.area_packs.is_some())
}

/// The EXE's start, if it has a boot map.
pub fn read(exe: &[u8]) -> Option<Boot> {
    let at = offset(exe, layout(exe)?.boot?, BOOT_SIZE)?;
    let s = |k: usize| le16(exe, at + k) as i16;
    Some(Boot { chunk: le16(exe, at), x: s(2), h: s(4), z: s(6), facing: le16(exe, at + 8) })
}

/// `exe` starting a new game at `boot`.
pub fn set(exe: &[u8], boot: &Boot) -> Result<Vec<u8>> {
    let l = layout(exe).ok_or_else(needs_hackergc)?;
    let at = l.boot.and_then(|va| offset(exe, va, BOOT_SIZE)).ok_or_else(needs_hackergc)?;
    let mut out = exe.to_vec();
    for (k, v) in [boot.chunk, boot.x as u16, boot.h as u16, boot.z as u16, boot.facing].into_iter().enumerate() {
        out[at + 2 * k..at + 2 * k + 2].copy_from_slice(&v.to_le_bytes());
    }
    Ok(out)
}

fn needs_hackergc() -> EditError {
    bad("BOOT_NEEDS_HACKERGC", "Moving the start needs a HackerGC base with a boot map (v1, shift-safe); this EXE has none.")
}

/// The area pack table's slots: `(chunk, pack)`, chunk 0 for an empty one.
fn slots(exe: &[u8]) -> Option<(usize, Vec<(u16, u16)>)> {
    let (va, count) = layout(exe)?.area_packs?;
    let at = offset(exe, va, count as usize * 4)?;
    Some((at, (0..count as usize).map(|k| (le16(exe, at + 4 * k), le16(exe, at + 4 * k + 2))).collect()))
}

/// The base's new-area packs as the EXE has them: `(chunk, pack)`, empty slots left out.
pub fn area_packs(exe: &[u8]) -> Vec<(u16, u16)> {
    slots(exe).map(|(_, s)| s.into_iter().filter(|&(c, _)| c != 0).collect()).unwrap_or_default()
}

/// The pack the EXE names for `chunk`, if it does.
pub fn area_pack(exe: &[u8], chunk: u32) -> Option<u32> {
    area_packs(exe).into_iter().find(|&(c, _)| c as u32 == chunk).map(|(_, p)| p as u32)
}

/// How many new areas the table can name.
pub fn area_capacity(exe: &[u8]) -> u32 {
    layout(exe).and_then(|l| l.area_packs).map_or(0, |(_, n)| n)
}

/// `exe` naming `pack` for `chunk` (`None` takes its entry out).
pub fn set_area_pack(exe: &[u8], chunk: u16, pack: Option<u16>) -> Result<Vec<u8>> {
    if chunk == 0 {
        return Err(bad("BOOT_AREA_CHUNK", "Chunk 0 is in the OVR02 pack table; the area table names chunks it leaves out."));
    }
    let (at, s) = slots(exe).ok_or_else(needs_hackergc)?;
    let mine = s.iter().position(|&(c, _)| c == chunk);
    let k = match (mine, pack) {
        (Some(k), _) => k,
        (None, None) => return Ok(exe.to_vec()),
        (None, Some(_)) => s.iter().position(|&(c, _)| c == 0).ok_or_else(|| {
            bad("BOOT_AREA_FULL", format!("The base's area pack table is full ({} areas). Take an area's entry out first.", s.len()))
        })?,
    };
    let (c, p) = pack.map_or((0, 0), |p| (chunk, p));
    let mut out = exe.to_vec();
    out[at + 4 * k..at + 4 * k + 2].copy_from_slice(&c.to_le_bytes());
    out[at + 4 * k + 2..at + 4 * k + 4].copy_from_slice(&p.to_le_bytes());
    Ok(out)
}

/// The ground height at chunk-local (x, z) of a chunk file, as `GetTerrainHeightAt` reads it;
/// `None` outside the chunk or where it isn't walkable ground.
pub fn ground(chunk: &[u8], x: i16, z: i16) -> Option<i16> {
    if !(-0x800..0x800).contains(&x) || !(-0x800..0x800).contains(&z) {
        return None;
    }
    let heights = crate::world::heightmap(chunk);
    let cell = ((z as i32 + 0x800) >> 5) as usize * 128 + ((x as i32 + 0x800) >> 5) as usize;
    heights.get(cell).copied().filter(|&h| h != -1 && h < 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in EXE: header, then the boot map at 0x80010000 and a 4-slot area table after it.
    /// Its pc0 and t_size are its own, so edited copies keep the layout.
    fn exe() -> Vec<u8> {
        let mut e = vec![0u8; 0x800 + 0x40];
        e[0x10..0x14].copy_from_slice(&0x8001_B007u32.to_le_bytes());
        e[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        e[0x1C..0x20].copy_from_slice(&0x40u32.to_le_bytes());
        for (k, v) in [326u16, 0x1FC, (-0x21i16) as u16, 0x4C, 0x9FD].into_iter().enumerate() {
            e[0x800 + 2 * k..0x802 + 2 * k].copy_from_slice(&v.to_le_bytes());
        }
        e[0x30..0x38].copy_from_slice(b"bootmap!");
        let mut h = sha1_smol::Sha1::new();
        h.update(&e);
        crate::layout::register(crate::layout::Layout {
            exe_sha1: h.digest().to_string(),
            pc0: 0x8001_B007,
            t_size: 0x40,
            boot: Some(0x8001_0000),
            area_packs: Some((0x8001_0010, 4)),
            ..Default::default()
        });
        e
    }

    #[test]
    fn reads_and_moves_the_start() {
        let e = exe();
        assert!(supported(&e));
        let b = read(&e).unwrap();
        assert_eq!(b, Boot { chunk: 326, x: 0x1FC, h: -0x21, z: 0x4C, facing: 0x9FD });
        assert_eq!(b.eighths(), 5);
        let moved = Boot { chunk: 1328, x: -100, h: -40, z: 200, facing: 2 * EIGHTH };
        let out = set(&e, &moved).unwrap();
        assert_eq!(read(&out), Some(moved));
        assert_eq!(read(&out).unwrap().eighths(), 2);
        assert_eq!(set(&[0u8; 0x900], &moved).unwrap_err().code, "BOOT_NEEDS_HACKERGC");
    }

    #[test]
    fn names_fills_and_frees_area_packs() {
        let e = exe();
        assert_eq!(area_capacity(&e), 4);
        assert!(area_packs(&e).is_empty());
        let one = set_area_pack(&e, 1328, Some(23)).unwrap();
        assert_eq!(area_pack(&one, 1328), Some(23));
        let two = set_area_pack(&one, 1328, Some(7)).unwrap();
        assert_eq!(area_packs(&two), [(1328, 7)]);
        let mut full = two.clone();
        for c in 1329..1332 {
            full = set_area_pack(&full, c, Some(1)).unwrap();
        }
        assert_eq!(set_area_pack(&full, 1400, Some(1)).unwrap_err().code, "BOOT_AREA_FULL");
        let freed = set_area_pack(&full, 1328, None).unwrap();
        assert_eq!(area_pack(&freed, 1328), None);
        assert_eq!(area_packs(&freed).len(), 3);
        assert_eq!(set_area_pack(&e, 0, Some(1)).unwrap_err().code, "BOOT_AREA_CHUNK");
        assert_eq!(set_area_pack(&e, 1400, None).unwrap(), e);
    }
}
