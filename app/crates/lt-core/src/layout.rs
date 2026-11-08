//! Where a modified engine moved main's tables (research 06 phase 4, HackerGC v1).
//!
//! A base built with the decomp's shift-safe build (`gc_decomp/docs/HACKERGC.md`) may move main's
//! code, data and BSS. Its `base.json` profile carries the moves as `exeLayout`: runs of retail
//! addresses that moved by the same amount, and the SHA-1 of the EXE they belong to. Living Tool
//! keeps naming retail's addresses (`ITEM_TABLE`, the file table, ...) and asks the EXE where they
//! are now ([`crate::exe::PsxExe::va`]); pointers read out of the EXE are already real.
//!
//! Layouts are registered by EXE hash when a base package is opened, so the retail disc and a
//! base opened next to it each read with their own. The EU and JP releases' EXEs carry built-in
//! layouts ([`crate::release`]), with one more map per code overlay.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    /// SHA-1 of the EXE this layout describes (lowercase hex).
    pub exe_sha1: String,
    /// Its PS-EXE entry point and size: Living Tool's own edits change the EXE's bytes but
    /// never these, so an edited copy still finds its layout.
    #[serde(default)]
    pub pc0: u32,
    #[serde(default)]
    pub t_size: u32,
    /// `[first, end, delta]`: retail addresses `first..end` are now `delta` bytes away.
    pub ranges: Vec<(u32, u32, i32)>,
    /// `[address, count]` of the base's spare file entries (`FileTable_Spare`, HackerGC's file
    /// registry): new files a project adds as `new/0`, `new/1`, ...
    #[serde(default)]
    pub spare_files: Option<(u32, u32)>,
    /// `[address, count]` of the base's story-battle behaviour table (`g_HgcBattleFlags`, a u32
    /// per battle from 0x100; bits in [`crate::gamedata::encounters::BEHAVIOUR_BITS`]).
    #[serde(default)]
    pub battle_flags: Option<(u32, u32)>,
    /// Address of the base's boot map (`g_HgcBoot`: where a new game starts), which a project
    /// can move ([`crate::edit::boot`]).
    #[serde(default)]
    pub boot: Option<u32>,
    /// `[address, count]` of the base's new-area pack table (`g_HgcAreaPacks`: `{chunk, pack}`
    /// pairs for chunks the OVR02 chunk → pack table leaves out).
    #[serde(default)]
    pub area_packs: Option<(u32, u32)>,
    /// `[address, words]` of the base's event flags past retail's 512 (`g_HgcEventFlags`: flag n
    /// from 0x200 is bit n & 31 of word (n − 0x200) >> 5). The Live emulator story state reads the
    /// running game's flags there.
    #[serde(default)]
    pub event_flags_hi: Option<(u32, u32)>,
    /// `[address, count]` of the base's streamed-music table (`g_HgcMusic`: `{bgm, cue, spare,
    /// flags}` entries naming the spare file a BGM song streams from; [`crate::edit::music`]).
    #[serde(default)]
    pub music: Option<(u32, u32)>,
    /// `[address, count]` of the base's chunk variant table (`g_HgcChunkVariants`: `{chunk, flag,
    /// variant, pad}` u16s; while `flag` is set the game reads `chunk` from `variant`'s sectors;
    /// [`crate::edit::variants`]).
    #[serde(default)]
    pub chunk_variants: Option<(u32, u32)>,
    /// Per code overlay (`n` of OVRnn): `[first, end, delta]` runs of its USA addresses, for
    /// tables read out of overlay files. Only a release's built-in map has them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overlays: BTreeMap<u8, Vec<(u32, u32, i32)>>,
}

impl Layout {
    /// Where retail address `va` is in this EXE.
    pub fn map(&self, va: u32) -> u32 {
        map_in(&self.ranges, va)
    }

    /// Where USA address `va` in overlay `ovr` (OVRnn) is in this release's copy of it.
    pub fn map_overlay(&self, ovr: u8, va: u32) -> u32 {
        self.overlays.get(&ovr).map_or(va, |r| map_in(r, va))
    }
}

fn map_in(ranges: &[(u32, u32, i32)], va: u32) -> u32 {
    match ranges.iter().find(|&&(lo, hi, _)| (lo..hi).contains(&va)) {
        Some(&(_, _, d)) => va.wrapping_add(d as u32),
        None => va,
    }
}

static REGISTRY: RwLock<Vec<Arc<Layout>>> = RwLock::new(Vec::new());

/// Makes `layout` the one for the EXE it names (replacing an older one for that EXE).
pub fn register(layout: Layout) {
    let mut r = REGISTRY.write().expect("layout registry");
    r.retain(|l| l.exe_sha1 != layout.exe_sha1);
    r.push(Arc::new(layout));
}

/// The registered layout for an EXE with these bytes, else its release's built-in one (an EU or
/// JP EXE); `None` for retail USA.
pub fn for_exe(bytes: &[u8]) -> Option<Arc<Layout>> {
    registered(bytes).or_else(|| crate::release::Release::of_exe(bytes)?.layout())
}

fn registered(bytes: &[u8]) -> Option<Arc<Layout>> {
    let r = REGISTRY.read().expect("layout registry");
    if r.is_empty() {
        return None;
    }
    let mut h = sha1_smol::Sha1::new();
    h.update(bytes);
    let sha = h.digest().to_string();
    if let Some(l) = r.iter().find(|l| l.exe_sha1 == sha) {
        return Some(l.clone());
    }
    let word = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")));
    let (pc0, t_size) = (word(0x10)?, word(0x1C)?);
    r.iter().find(|l| l.pc0 != 0 && l.pc0 == pc0 && l.t_size == t_size).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_runs_and_leaves_the_rest() {
        let l = Layout { exe_sha1: "x".into(), ranges: vec![(0x801773B0, 0x801C0000, 0x100), (0x801C0000, 0x801DF000, 0x200)], ..Default::default() };
        assert_eq!(l.map(0x80173000), 0x80173000);
        assert_eq!(l.map(0x801B4304), 0x801B4404);
        assert_eq!(l.map(0x801CE810), 0x801CEA10);
        assert_eq!(l.map(0x801DF000), 0x801DF000);
        register(l.clone());
        assert!(for_exe(b"not that exe").is_none());
    }

    /// `hgc_layout.json` as `tools/hgc_build.py` writes it (base.json `profile.exeLayout`): a build
    /// with 0x2A0 flags names `g_HgcEventFlags` and its 5 words; a layout from before it, none.
    #[test]
    fn reads_the_extra_event_flags() {
        let json = r#"{"ranges": [[2149024688, 2149318656, 2048]], "exeSha1": "85014327068b1fb977186fdaef0dad929da76b13",
            "pc0": 2149251960, "tSize": 313344, "spareFiles": [2149313392, 32], "tier2Ext": 1, "flagCount": 672,
            "eventFlagsHi": [2149315372, 5]}"#;
        let l: Layout = serde_json::from_str(json).unwrap();
        assert_eq!(l.event_flags_hi, Some((0x801B_F32C, 5)));
        assert_eq!(l.spare_files, Some((0x801B_EB70, 32)));
        assert_eq!(serde_json::to_value(&l).unwrap()["eventFlagsHi"], serde_json::json!([0x801B_F32Cu32, 5]));
        let older: Layout = serde_json::from_str(r#"{"ranges": [], "exeSha1": "x", "flagCount": 672}"#).unwrap();
        assert_eq!(older.event_flags_hi, None);
    }
}
