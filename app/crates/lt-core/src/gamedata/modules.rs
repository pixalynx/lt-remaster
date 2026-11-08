//! Arena-module compatibility (PRD §5.2, port of `gc_decomp/tools/sim_catalog.py`).
//!
//! A battle loads one area module at 0x8014F000 by arena id (`AreaOverlayIndexTable_47`
//! / `_100` in the EXE, anything unlisted gets OVR06). The attack executor spawns
//! `D_801FDC98[effect]` (OVR04); 85 of those entry points lie inside the module
//! window and only exist in particular modules. An enemy whose scripts spawn one
//! of them must fight in an arena whose module defines it.

use std::collections::BTreeMap;

use crate::container::{le16, le32};
use crate::exe::PsxExe;

use super::OVR_LOAD;

/// The area-module window.
pub const MODULE_LO: u32 = 0x8014_F000;
pub const MODULE_HI: u32 = 0x8015_3000;
/// `D_801FDC98` in OVR04: effect id → task entry point.
pub const EFFECT_TABLE: u32 = 0x801F_DC98;
/// Area id → module number, for ids 0x47..0xF3 and 0x100..0x124 (indexed by the id itself).
pub const AREA_OVL_47: u32 = 0x801B_7575;
pub const AREA_OVL_100: u32 = 0x801B_756C;
/// Mask meaning "spawns no module-resident effect": fights in any arena.
pub const ANY_MODULE: u32 = 0xFFFF_FFFF;

/// Which area modules (bit n = OVRn) define a function at each module-window
/// address that the retail effect table points at. Generated from the decomp's
/// overlay builds (`mipsel-linux-gnu-nm build/ovrNN/ovrNN.elf`, n = 5..25) the
/// way `sim_catalog.py` does; code addresses are not data, so they are bundled.
const ENTRY_POINTS: [(u32, u32); 78] = [
    (0x8014F004, 0x02180F80), (0x8014F00C, 0x00100040), (0x8014F01C, 0x00440000),
    (0x8014F04C, 0x00208000), (0x8014F068, 0x00040000), (0x8014F084, 0x00200400),
    (0x8014F094, 0x00001800), (0x8014F09C, 0x00000800), (0x8014F0B4, 0x00002000),
    (0x8014F120, 0x00004000), (0x8014F2C0, 0x00100000), (0x8014F360, 0x00004000),
    (0x8014F404, 0x00008000), (0x8014F45C, 0x00000100), (0x8014F4CC, 0x00000200),
    (0x8014F4D0, 0x00001000), (0x8014F4FC, 0x00100000), (0x8014F52C, 0x00100000),
    (0x8014F6C4, 0x00400000), (0x8014F6F0, 0x00000040), (0x8014F78C, 0x00000800),
    (0x8014F888, 0x00100000), (0x8014F908, 0x00000080), (0x8014F938, 0x00020000),
    (0x8014FA88, 0x00200000), (0x8014FA98, 0x00002000), (0x8014FB08, 0x00040000),
    (0x8014FBA0, 0x00000200), (0x8014FBE4, 0x00000100), (0x8014FC28, 0x00400400),
    (0x8014FE68, 0x00000040), (0x8014FFC8, 0x00020000), (0x80150004, 0x00002000),
    (0x80150008, 0x00400000), (0x80150038, 0x00200000), (0x80150090, 0x00004000),
    (0x801500A8, 0x00100000), (0x80150158, 0x00000800), (0x801501AC, 0x00000400),
    (0x80150230, 0x00001000), (0x80150278, 0x00000040), (0x801502B8, 0x00200000),
    (0x801503BC, 0x00200000), (0x80150400, 0x00000200), (0x80150440, 0x00040080),
    (0x80150480, 0x00000100), (0x801504A4, 0x00000800), (0x80150580, 0x00004000),
    (0x80150590, 0x00000040), (0x80150760, 0x00020000), (0x8015079C, 0x00400000),
    (0x801507F0, 0x00000100), (0x80150800, 0x00004000), (0x801508E4, 0x00200000),
    (0x8015091C, 0x00000400), (0x80150998, 0x00300000), (0x80150B04, 0x00002040),
    (0x80150B60, 0x00400000), (0x80150B64, 0x00010000), (0x80150C38, 0x00100000),
    (0x80150D00, 0x00000100), (0x80150E50, 0x00400000), (0x80150E9C, 0x00000800),
    (0x80150F50, 0x00200000), (0x80150F74, 0x00000400), (0x8015100C, 0x00000040),
    (0x8015102C, 0x00001000), (0x80151040, 0x00000100), (0x8015105C, 0x00001000),
    (0x8015108C, 0x00001000), (0x80151090, 0x00400000), (0x801511E0, 0x00000040),
    (0x80151218, 0x00020000), (0x801512F8, 0x00100000), (0x80151314, 0x00000400),
    (0x801515B0, 0x00004000), (0x801519B0, 0x00010000), (0x801519D8, 0x00004800),
];

/// Modules defining a function at `addr` (0 = none known).
pub fn modules_at(addr: u32) -> u32 {
    ENTRY_POINTS.binary_search_by_key(&addr, |&(a, _)| a).map(|i| ENTRY_POINTS[i].1).unwrap_or(0)
}

/// The OVR04 effect table, read from the disc.
#[derive(Debug, Clone, Default)]
pub struct EffectTable {
    pub entries: Vec<u32>,
    /// [`ENTRY_POINTS`] at the release's addresses (empty: USA's own).
    entry_points: BTreeMap<u32, u32>,
}

impl EffectTable {
    /// Reads pointers from `D_801FDC98` until one is not a RAM address.
    pub fn parse(ovr04: &[u8]) -> Self {
        Self::parse_at(ovr04, (EFFECT_TABLE - OVR_LOAD) as usize)
    }

    /// [`EffectTable::parse`] for the release `exe` belongs to: the table where that release keeps
    /// it, and the module entry points where its area modules (OVR05–OVR25) have them.
    pub fn parse_in(exe: &PsxExe, ovr04: &[u8]) -> Self {
        let mut t = Self::parse_at(ovr04, (exe.ovr_va(4, EFFECT_TABLE) - OVR_LOAD) as usize);
        if exe.layout.as_ref().is_some_and(|l| !l.overlays.is_empty()) {
            for &(usa, mask) in ENTRY_POINTS.iter() {
                for n in (5..=25u8).filter(|n| mask & (1 << n) != 0) {
                    *t.entry_points.entry(exe.ovr_va(n, usa)).or_default() |= 1 << n;
                }
            }
        }
        t
    }

    fn parse_at(ovr04: &[u8], mut at: usize) -> Self {
        let mut entries = Vec::new();
        while at + 4 <= ovr04.len() {
            let v = le32(ovr04, at);
            if !(0x8000_0000..0x8020_0000).contains(&v) {
                break;
            }
            entries.push(v);
            at += 4;
        }
        Self { entries, entry_points: BTreeMap::new() }
    }

    /// `Some(mask)` when the effect's entry point is module-resident.
    pub fn module_mask(&self, effect: u16) -> Option<u32> {
        let v = *self.entries.get(effect as usize)?;
        let at = |v| if self.entry_points.is_empty() { modules_at(v) } else { self.entry_points.get(&v).copied().unwrap_or(0) };
        (MODULE_LO..MODULE_HI).contains(&v).then(|| at(v))
    }
}

/// Arena compatibility of one enemy: the intersection of the module sets of
/// every module-resident effect its attack scripts spawn. `section` is the
/// profile section to the end of the file; `shift` is 1 for the prototypes.
/// Scripts: `u16 scriptOfs[6]` at +0x134, each a 6-byte header at
/// `+0x140 + ofs`, then 7-halfword records up to an all-zero one; halfword 4
/// is the effect id.
pub fn enemy_mask(section: &[u8], shift: usize, effects: &EffectTable) -> u32 {
    let h = |at: usize| if at + 2 <= section.len() { le16(section, at) } else { 0 };
    let mut offsets: Vec<u16> = (0..6).map(|i| h(0x134 + shift + i * 2)).collect();
    offsets.sort_unstable();
    offsets.dedup();
    let mut need: Option<u32> = None;
    for so in offsets {
        let mut q = 0x140 + shift + so as usize + 6;
        for _ in 0..40 {
            if q + 14 > section.len() {
                break;
            }
            let rec: Vec<u16> = (0..7).map(|k| le16(section, q + k * 2)).collect();
            if rec.iter().all(|&w| w == 0) {
                break;
            }
            if let Some(m) = effects.module_mask(rec[4]) {
                need = Some(need.map_or(m, |n| n & m));
            }
            q += 14;
        }
    }
    need.unwrap_or(ANY_MODULE)
}

/// The area module a battle in `area` loads (`Load_ResidentModules`).
pub fn area_module(exe: &PsxExe, area: u16) -> u8 {
    let at = match area {
        0x47..=0xF3 => exe.va(AREA_OVL_47) + area as u32,
        0x100..=0x124 => exe.va(AREA_OVL_100) + area as u32,
        _ => return 6,
    };
    exe.slice(at, 1).map(|b| b[0]).unwrap_or(6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_points_are_sorted_for_binary_search() {
        assert!(ENTRY_POINTS.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(modules_at(0x8014_FBE4), 1 << 8, "Darkbeat's Hyper Punch lives in OVR08");
        assert_eq!(modules_at(0x8014_F000), 0);
    }
}
