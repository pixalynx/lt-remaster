//! The hidden file system: 493 × 8-byte entries inside `SLUS_008.11`.
//!
//! Each entry is `{u8 BCD minute, u8 BCD second, u8 BCD frame, u8 0; u32 sector
//! count}` — no names, no byte sizes. Thirteen named slices of the array are the
//! sub-tables the loaders index into. See `docs/screens/05-files.md` §4–5.
//!
//! The EU release has the same table; the Japanese one has 491 entries (11 movies and 23 system
//! files, then the USA slices from `world` on). Files keep their USA canonical ids everywhere.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::{LtError, Result};
use crate::exe::PsxExe;
use crate::release::Release;

/// RAM address of the table (`D_801B4304`); EXE file offset 0x41B04. Other releases keep it
/// elsewhere ([`PsxExe::va`]).
pub const TABLE_VADDR: u32 = 0x801B_4304;
/// Entries in the USA table ([`crate::release::ReleaseInfo::table_len`] for the others).
pub const TABLE_LEN: usize = 493;
pub const ENTRY_SIZE: usize = 8;

/// The sub-table an entry belongs to. Ranges are global table indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Movie,
    Null,
    Sys,
    World,
    Ovr,
    Npc,
    NpcSnd,
    Fe,
    Bgm,
    Bf,
    Toy,
    Pb,
    Knight,
    Enemy,
    /// A base's spare entries (`new/N`, HackerGC's file registry), outside the 493.
    Spare,
}

/// One named slice of the table, with what loads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubTable {
    pub kind: Kind,
    /// Canonical ID prefix, e.g. `npc`.
    pub prefix: &'static str,
    pub label: &'static str,
    pub first: u16,
    pub count: u16,
    /// Symbol in `gc_decomp/config/symbol_addrs.txt`.
    pub symbol: &'static str,
    pub loaded_to: &'static str,
    /// Loader cap in sectors (loaders compare `sectors << 11`), when known.
    pub cap_sectors: Option<u32>,
}

/// The USA (and EU) table's slices.
pub const SUB_TABLES: &[SubTable] = &[
    st(Kind::Movie, "movie", "Movies", 0, 14, "D_801B4304", "streamed (OVR01 Fmv_DecodeFrameTask)", None),
    st(Kind::Null, "null", "Null entry", 14, 1, "—", "unused (00:02:00, 0 sectors)", None),
    st(Kind::Sys, "sys", "System", 15, 22, "FileTable_CharacterPackets", "various loaders", None),
    st(Kind::World, "world", "World archive", 37, 1, "FileTable_AreaOverlays[0]", "chunk slots 0x80010000 × 4", None),
    st(Kind::Ovr, "ovr", "Overlays", 38, 25, "FileTable_AreaOverlays[1..]", "overlay slots", None),
    st(Kind::Npc, "npc", "NPC packs", 63, 50, "FileTable_NpcBin", "0x80111600", Some(98)),
    st(Kind::NpcSnd, "npcsnd", "NPC sounds", 113, 9, "FileTable_NpcSpecialSound", "staging 0x80153000", Some(64)),
    st(Kind::Fe, "fe", "Field textures", 122, 57, "FileTable_ChunkTextureSets", "staging → VRAM (512,256)", Some(64)),
    st(Kind::Bgm, "bgm", "Music", 179, 61, "FileTable_AreaBGM", "staging 0x80153000", Some(64)),
    st(Kind::Bf, "bf", "Arenas", 240, 48, "FileTable_BattleFieldGfx", "staging 0x80153000", Some(64)),
    st(Kind::Toy, "toy", "Living Toys", 288, 73, "FileTable_LivingToy", "staging 0x80153000", Some(64)),
    st(Kind::Pb, "pb", "Baby forms", 361, 19, "FileTable_BabyForm", "0x80101800", None),
    st(Kind::Knight, "knight", "Knight", 380, 1, "D_801B4EE4", "battle heap (ctx+0x54)", None),
    st(Kind::Enemy, "enemy", "Enemies", 381, 112, "FileTable_Enemy", "battle heap (ctx+0x54)", None),
];

/// The Japanese release's slices (Knight & Baby, SLPS-01531): 11 movies, the null entry at 11
/// and 23 system files (sys/22 is its own), then the USA slices two entries earlier.
pub const JP_SUB_TABLES: &[SubTable] = &[
    st(Kind::Movie, "movie", "Movies", 0, 11, "D_801B4304", "streamed (OVR01 Fmv_DecodeFrameTask)", None),
    st(Kind::Null, "null", "Null entry", 11, 1, "—", "unused (00:02:00, 0 sectors)", None),
    st(Kind::Sys, "sys", "System", 12, 23, "FileTable_CharacterPackets", "various loaders", None),
    st(Kind::World, "world", "World archive", 35, 1, "FileTable_AreaOverlays[0]", "chunk slots 0x80010000 × 4", None),
    st(Kind::Ovr, "ovr", "Overlays", 36, 25, "FileTable_AreaOverlays[1..]", "overlay slots", None),
    st(Kind::Npc, "npc", "NPC packs", 61, 50, "FileTable_NpcBin", "0x80111600", Some(98)),
    st(Kind::NpcSnd, "npcsnd", "NPC sounds", 111, 9, "FileTable_NpcSpecialSound", "staging 0x80153000", Some(64)),
    st(Kind::Fe, "fe", "Field textures", 120, 57, "FileTable_ChunkTextureSets", "staging → VRAM (512,256)", Some(64)),
    st(Kind::Bgm, "bgm", "Music", 177, 61, "FileTable_AreaBGM", "staging 0x80153000", Some(64)),
    st(Kind::Bf, "bf", "Arenas", 238, 48, "FileTable_BattleFieldGfx", "staging 0x80153000", Some(64)),
    st(Kind::Toy, "toy", "Living Toys", 286, 73, "FileTable_LivingToy", "staging 0x80153000", Some(64)),
    st(Kind::Pb, "pb", "Baby forms", 359, 19, "FileTable_BabyForm", "0x80101800", None),
    st(Kind::Knight, "knight", "Knight", 378, 1, "D_801B4EE4", "battle heap (ctx+0x54)", None),
    st(Kind::Enemy, "enemy", "Enemies", 379, 112, "FileTable_Enemy", "battle heap (ctx+0x54)", None),
];

#[allow(clippy::too_many_arguments)]
const fn st(
    kind: Kind,
    prefix: &'static str,
    label: &'static str,
    first: u16,
    count: u16,
    symbol: &'static str,
    loaded_to: &'static str,
    cap_sectors: Option<u32>,
) -> SubTable {
    SubTable { kind, prefix, label, first, count, symbol, loaded_to, cap_sectors }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Extent {
    pub lba: u32,
    pub sectors: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Index in the 493-entry table (0..=492).
    pub global: u16,
    pub kind: Kind,
    /// Index inside the sub-table, used in the canonical ID.
    pub index: u16,
    /// Canonical ID, e.g. `npc/7`, `ovr/2`, `enemy/17`.
    pub id: String,
    /// The 8 bytes as stored in the EXE.
    pub raw: [u8; 8],
    /// Decoded minute, second, frame.
    pub msf: (u8, u8, u8),
    pub extent: Extent,
    /// Other entries pointing at the same extent.
    pub aliases: Vec<u16>,
    /// EXE file offset of this entry.
    pub exe_offset: u32,
    /// RAM address of this entry.
    pub vaddr: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTable {
    /// The release the EXE belongs to; it decides the table's length and slices.
    pub release: Release,
    pub entries: Vec<Entry>,
    /// Distinct non-empty extents.
    pub unique_files: usize,
    pub alias_entries: usize,
    pub last_lba: u32,
}

impl FileTable {
    pub fn parse(exe: &PsxExe, image_sectors: u32) -> Result<Self> {
        // Where this EXE keeps the table (retail's address, moved on a shifted base or another release).
        let table_va = exe.va(TABLE_VADDR);
        let release = exe.release.info();
        let bytes = exe.slice(table_va, release.table_len * ENTRY_SIZE)?;
        let base_offset = exe.vaddr_to_offset(table_va)? as u32;
        let mut entries = Vec::with_capacity(release.table_len);
        for (g, raw) in bytes.chunks_exact(ENTRY_SIZE).enumerate() {
            let global = g as u16;
            let bcd = |b: u8| bcd(b).ok_or(LtError::TableBadBcd { global, byte: b });
            let (m, s, f) = (bcd(raw[0])?, bcd(raw[1])?, bcd(raw[2])?);
            let sectors = u32::from_le_bytes(raw[4..8].try_into().unwrap());
            let lba = msf_to_lba(m, s, f);
            if sectors > 0 && (lba < 0 || lba as u64 + sectors as u64 > image_sectors as u64) {
                return Err(LtError::TableOutOfBounds { global });
            }
            let (kind, index, id) = canonical_in(release.sub_tables, global);
            entries.push(Entry {
                global,
                kind,
                index,
                id,
                raw: raw.try_into().unwrap(),
                msf: (m, s, f),
                extent: Extent { lba: lba.max(0) as u32, sectors },
                aliases: Vec::new(),
                exe_offset: base_offset + (g * ENTRY_SIZE) as u32,
                vaddr: table_va + (g * ENTRY_SIZE) as u32,
            });
        }

        // Alias groups: several entries pointing at one extent.
        let mut by_extent: BTreeMap<Extent, Vec<u16>> = BTreeMap::new();
        for e in entries.iter().filter(|e| e.extent.sectors > 0) {
            by_extent.entry(e.extent).or_default().push(e.global);
        }
        let mut alias_entries = 0;
        for members in by_extent.values().filter(|m| m.len() > 1) {
            alias_entries += members.len() - 1;
            for &g in members {
                entries[g as usize].aliases = members.iter().copied().filter(|&o| o != g).collect();
            }
        }
        // A base's spare entries (HackerGC's file registry): `new/N`, after the 493.
        if let Some((va, count)) = exe.layout.as_ref().and_then(|l| l.spare_files) {
            let bytes = exe.slice(va, count as usize * ENTRY_SIZE)?;
            let base = exe.vaddr_to_offset(va)? as u32;
            for (n, raw) in bytes.chunks_exact(ENTRY_SIZE).enumerate() {
                let global = (release.table_len + n) as u16;
                let bcd = |b: u8| bcd(b).ok_or(LtError::TableBadBcd { global, byte: b });
                let (m, s, f) = (bcd(raw[0])?, bcd(raw[1])?, bcd(raw[2])?);
                let sectors = u32::from_le_bytes(raw[4..8].try_into().unwrap());
                let lba = if sectors == 0 { 0 } else { msf_to_lba(m, s, f) };
                if sectors > 0 && (lba < 0 || lba as u64 + sectors as u64 > image_sectors as u64) {
                    return Err(LtError::TableOutOfBounds { global });
                }
                entries.push(Entry {
                    global,
                    kind: Kind::Spare,
                    index: n as u16,
                    id: format!("new/{n}"),
                    raw: raw.try_into().unwrap(),
                    msf: (m, s, f),
                    extent: Extent { lba: lba.max(0) as u32, sectors },
                    aliases: Vec::new(),
                    exe_offset: base + (n * ENTRY_SIZE) as u32,
                    vaddr: va + (n * ENTRY_SIZE) as u32,
                });
                if sectors > 0 {
                    by_extent.entry(Extent { lba: lba as u32, sectors }).or_default().push(global);
                }
            }
        }
        let last_lba = by_extent.keys().map(|e| e.lba + e.sectors).max().unwrap_or(0);
        Ok(Self { release: exe.release, unique_files: by_extent.len(), alias_entries, last_lba, entries })
    }

    pub fn get(&self, global: u16) -> Result<&Entry> {
        self.entries.get(global as usize).ok_or(LtError::UnknownEntry(global))
    }

    pub fn by_id(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| e.kind == kind)
    }
}

/// Maps a global index of the USA table to its sub-table, local index and canonical ID.
pub fn canonical(global: u16) -> (Kind, u16, String) {
    canonical_in(SUB_TABLES, global)
}

/// [`canonical`] for a release's slices.
pub fn canonical_in(sub_tables: &[SubTable], global: u16) -> (Kind, u16, String) {
    let t = sub_tables
        .iter()
        .find(|t| global >= t.first && global < t.first + t.count)
        .expect("the sub-tables cover the whole table");
    let index = global - t.first;
    let id = match t.kind {
        Kind::Null => "null".to_string(),
        Kind::World => "world".to_string(),
        // Overlays are numbered from 1 like the decomp's OVR01..OVR25.
        Kind::Ovr => format!("ovr/{}", index + 1),
        _ => format!("{}/{}", t.prefix, index),
    };
    (t.kind, index, id)
}

pub fn sub_table(kind: Kind) -> &'static SubTable {
    SUB_TABLES.iter().find(|t| t.kind == kind).expect("every kind has a sub-table")
}

fn bcd(b: u8) -> Option<u8> {
    let (hi, lo) = (b >> 4, b & 0x0F);
    (hi <= 9 && lo <= 9).then_some(hi * 10 + lo)
}

/// Minute/second/frame (75 frames per second) to LBA; the first 150 frames are
/// the lead-in pregap.
pub fn msf_to_lba(m: u8, s: u8, f: u8) -> i64 {
    (m as i64 * 60 + s as i64) * 75 + f as i64 - 150
}

/// LBA to the BCD bytes the table stores.
pub fn lba_to_bcd_msf(lba: u32) -> [u8; 3] {
    let a = lba + 150;
    let enc = |n: u32| (((n / 10) << 4) | (n % 10)) as u8;
    [enc(a / 4500), enc((a / 75) % 60), enc(a % 75)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_tables_cover_the_table_contiguously() {
        for r in Release::ALL {
            let mut next = 0;
            for t in r.info().sub_tables {
                assert_eq!(t.first, next, "{r:?} {:?}", t.kind);
                next += t.count;
            }
            assert_eq!(next as usize, r.info().table_len);
        }
        assert_eq!(TABLE_LEN, Release::Usa.info().table_len);
    }

    #[test]
    fn jp_ids_name_the_same_files() {
        assert_eq!(canonical_in(JP_SUB_TABLES, 68).2, "npc/7");
        assert_eq!(canonical_in(JP_SUB_TABLES, 35).2, "world");
        assert_eq!(canonical_in(JP_SUB_TABLES, 37).2, "ovr/2");
        assert_eq!(canonical_in(JP_SUB_TABLES, 34).2, "sys/22");
        assert_eq!(canonical_in(JP_SUB_TABLES, 490).2, "enemy/111");
    }

    #[test]
    fn canonical_ids() {
        assert_eq!(canonical(70).2, "npc/7");
        assert_eq!(canonical(310).2, "toy/22");
        assert_eq!(canonical(398).2, "enemy/17");
        assert_eq!(canonical(37).2, "world");
        assert_eq!(canonical(39).2, "ovr/2");
        assert_eq!(canonical(380).2, "knight/0");
    }

    #[test]
    fn msf_round_trip() {
        // npc/7 is stored as 19 08 63 → LBA 86,013.
        assert_eq!(msf_to_lba(19, 8, 63), 86_013);
        assert_eq!(lba_to_bcd_msf(86_013), [0x19, 0x08, 0x63]);
        // The append point after the retail image.
        assert_eq!(lba_to_bcd_msf(185_985), [0x41, 0x21, 0x60]);
    }
}
