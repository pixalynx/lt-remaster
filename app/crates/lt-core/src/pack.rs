//! Content packs: curated changes a project adds or takes out as one piece.
//!
//! A pack is a manifest and one BPS patch per file, against that file's retail bytes in one
//! release. Applying a pack patches the disc's own bytes and stages the results in the project, like
//! any other edit, so the build plan, BPS export and the File patcher see them; taking it out
//! reverts those files. The patches hold only what the mod changes (`examples/make_pack.rs`).
//!
//! The manifests ship in the app; the patches are downloaded from [`PATCH_BASE`]
//! (`<base>/<id>/<version>/<patch>`) and checked against the manifest's size and SHA-256 before
//! they are used. Without them a pack still knows where it stands from the files' CRCs.
//!
//! Built in: `jp-prologue`, Knight & Baby's in-engine New Game prologue, in English, for the USA
//! release. It changes `ovr/1` (New Game enters the flight mode) and `ovr/25` (the flight and its
//! captions); see `docs/research/10-jp-prologue.md` and the decomp's `docs/JP_PROLOGUE.md`.
//!
//! A pack may carry a flyover table in one of its files ([`crate::flyover`]), found by a four-byte
//! magic, which Living Tool reads and rewrites in place.
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bps;
use crate::flyover::{self, Flyover};
use crate::release::Release;

/// Where pack patches are published.
pub const PATCH_BASE: &str = "https://downloads.livingtool.app/packs";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackFile {
    /// Canonical file id (`ovr/25`).
    pub id: String,
    pub patch: String,
    pub patch_size: u64,
    pub patch_sha256: String,
    /// The patch's source: this file's retail bytes, then those of `extra_sources` in order.
    pub source_size: u64,
    pub source_crc32: String,
    pub target_size: u64,
    pub target_crc32: String,
    /// Other files on the disc the new file is made from (a hero's body from an NPC pack), so the
    /// patch copies them from the player's own disc instead of carrying them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_sources: Vec<String>,
}

/// A few bytes of the executable a pack sets, by their USA retail address: mapped through the
/// executable's own layout (a shifted base's tables move), and written only over `from` (or the
/// pack's own `to`), so other edits to the executable stay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExeWrite {
    /// Retail RAM address, hex (`0x801B552C`).
    pub va: String,
    /// The bytes retail has there, hex.
    pub from: String,
    /// The pack's bytes, hex, the same length.
    pub to: String,
    /// What it is, for messages (`the hero's name`).
    #[serde(default)]
    pub what: String,
}

impl ExeWrite {
    fn parts(&self) -> Option<(u32, Vec<u8>, Vec<u8>)> {
        let hex = |t: &str| (0..t.len()).step_by(2).map(|i| u8::from_str_radix(t.get(i..i + 2)?, 16).ok()).collect::<Option<Vec<u8>>>();
        let va = u32::from_str_radix(self.va.trim_start_matches("0x"), 16).ok()?;
        let (from, to) = (hex(&self.from)?, hex(&self.to)?);
        (from.len() == to.len()).then_some((va, from, to))
    }
}

/// A pack that changes who you play (`darkbeat-ibkee`): the names the UI offers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlayAs {
    pub hero: String,
    pub partner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSpec {
    /// The file holding the table.
    pub file: String,
    /// Four ASCII bytes in front of the table.
    pub magic: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub version: u32,
    pub title: String,
    pub summary: String,
    pub release: Release,
    /// Where the modded files come from (a decomp branch and build flags).
    pub source: String,
    pub files: Vec<PackFile>,
    pub flyover: Option<TableSpec>,
    /// Set on packs that put another hero and partner in the Knight's and the Baby's place; a
    /// project has at most one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_as: Option<PlayAs>,
    /// Bytes of the executable the pack sets (the party's names).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exe_writes: Vec<ExeWrite>,
}

#[derive(Debug)]
pub struct Pack {
    pub manifest: Manifest,
}

/// A pack's refusal, with a stable code for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackError {
    pub code: &'static str,
    pub message: String,
}

impl PackError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

pub type PackResult<T> = std::result::Result<T, PackError>;

/// One file's bytes by canonical id (retail or as the project has it).
pub type Files<'a> = &'a dyn Fn(&str) -> Option<Vec<u8>>;
/// A pack file's checked patch, when it has been downloaded.
pub type Patches<'a> = &'a dyn Fn(&PackFile) -> Option<Vec<u8>>;

const MANIFESTS: &[&str] = &[
    include_str!("../data/packs/jp-prologue/pack.json"),
    include_str!("../data/packs/darkbeat-ibkee/pack.json"),
];

/// Every pack Living Tool ships.
pub fn builtin() -> &'static [Pack] {
    static PACKS: OnceLock<Vec<Pack>> = OnceLock::new();
    PACKS.get_or_init(|| MANIFESTS.iter().map(|json| Pack { manifest: serde_json::from_str(json).expect("built-in pack manifest") }).collect())
}

/// A built-in pack by id.
pub fn find(id: &str) -> Option<&'static Pack> {
    builtin().iter().find(|p| p.manifest.id == id)
}

/// Where a pack stands in a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PackState {
    /// Every file is still the release's retail bytes: the pack can be added.
    Available,
    /// Every file is the pack's (its flyover may be edited).
    Applied { edited: bool },
    /// A file the pack changes holds other edits, or isn't the retail file the pack patches
    /// (another base, another dump).
    Conflict { file: String, reason: String },
    /// The pack doesn't apply to this disc at all, or can't tell without its patches.
    Unavailable { reason: String },
}

/// `bytes` is the patch `f` names: its size and SHA-256.
pub fn patch_ok(f: &PackFile, bytes: &[u8]) -> bool {
    bytes.len() as u64 == f.patch_size && sha256_hex(bytes).eq_ignore_ascii_case(&f.patch_sha256)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

impl Pack {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// Where a patch is published under `base` ([`PATCH_BASE`] or a mirror).
    pub fn patch_url(&self, base: &str, f: &PackFile) -> String {
        format!("{}/{}/{}/{}", base.trim_end_matches('/'), self.manifest.id, self.manifest.version, f.patch)
    }

    /// A file's patch source: its retail bytes, then its extra sources'.
    fn source(f: &PackFile, retail: Files) -> PackResult<Vec<u8>> {
        let mut out = Vec::new();
        for id in std::iter::once(&f.id).chain(&f.extra_sources) {
            out.extend(retail(id).ok_or_else(|| PackError::new("PACK_NO_FILE", format!("{id} isn't on this disc")))?);
        }
        Ok(out)
    }

    /// The pack's files as the patches make them from `retail`.
    pub fn build(&self, retail: Files, patches: Patches) -> PackResult<Vec<(String, Vec<u8>)>> {
        self.manifest
            .files
            .iter()
            .map(|f| {
                let source = Self::source(f, retail)?;
                if !matches_spec(&source, f.source_size, &f.source_crc32) {
                    return Err(PackError::new("PACK_SOURCE_MISMATCH", mismatch(f)));
                }
                let patch = patches(f).ok_or_else(|| PackError::new("PACK_NOT_DOWNLOADED", format!("{}'s patch isn't downloaded", f.id)))?;
                let target = bps::apply(&patch, &source).map_err(|e| PackError::new("PACK_PATCH", format!("{}: {e}", f.id)))?;
                if !matches_spec(&target, f.target_size, &f.target_crc32) {
                    return Err(PackError::new("PACK_PATCH", format!("{}: the patch didn't produce the pack's file", f.id)));
                }
                Ok((f.id.clone(), target))
            })
            .collect()
    }

    /// The pack's state in a project. `retail` gives a file's retail bytes (the patches' sources:
    /// the retail disc, kept open next to a base), `base` the bytes the project starts from (the
    /// base's image, or retail), `current` the bytes it has now. Without `patches`, it goes by the
    /// files' CRCs.
    pub fn state(&self, release: Release, retail: Files, base: Files, current: Files, patches: Patches) -> PackState {
        if release != self.manifest.release {
            return PackState::Unavailable { reason: format!("This pack is for the {} release.", release_name(self.manifest.release)) };
        }
        let mut targets = Vec::new();
        for f in &self.manifest.files {
            let source = match Self::source(f, retail) {
                Ok(s) => s,
                Err(e) => return PackState::Unavailable { reason: e.message },
            };
            if !matches_spec(&source, f.source_size, &f.source_crc32) {
                return PackState::Unavailable { reason: format!("{}.", mismatch(f)) };
            }
            let (Some(own), Some(now)) = (base(&f.id), current(&f.id)) else {
                return PackState::Unavailable { reason: format!("{} isn't on this disc", f.id) };
            };
            let target = patches(f).and_then(|p| bps::apply(&p, &source).ok()).filter(|t| matches_spec(t, f.target_size, &f.target_crc32));
            targets.push((f, own, now, target));
        }
        let (mut packed, mut edited, mut missing) = (0, false, None);
        for (f, source, now, target) in &targets {
            let is_target = match target {
                Some(t) => now == t,
                None => matches_spec(now, f.target_size, &f.target_crc32),
            };
            if is_target {
                packed += 1;
            } else if now == source {
                missing.get_or_insert_with(|| f.id.clone());
            } else if let Some(t) = target {
                if self.same_but_flyover(&f.id, now, t) {
                    packed += 1;
                    edited = true;
                } else {
                    return PackState::Conflict { file: f.id.clone(), reason: format!("{} has other edits; revert it in Files to add this pack.", f.id) };
                }
            } else if self.flyover_file(&f.id) && now.len() as u64 == f.target_size && self.flyover(now).is_some() {
                // Can't compare without the patch: an edited flyover is the likely story.
                packed += 1;
                edited = true;
            } else {
                return PackState::Conflict { file: f.id.clone(), reason: format!("{} has other edits; revert it in Files to add this pack.", f.id) };
            }
        }
        let mut total = targets.len();
        if !self.manifest.exe_writes.is_empty() {
            total += 1;
            match current("exe").and_then(|exe| self.exe_side(&exe)) {
                Some(true) => packed += 1,
                Some(false) => {
                    missing.get_or_insert_with(|| "exe".to_string());
                }
                None => {
                    let what = self.manifest.exe_writes.iter().map(|w| w.what.as_str()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(", ");
                    return PackState::Conflict { file: "exe".into(), reason: format!("The executable's {} were changed by something else.", if what.is_empty() { "bytes this pack sets" } else { &what }) };
                }
            }
        }
        match (packed, missing) {
            (n, _) if n == total => PackState::Applied { edited },
            (0, _) => PackState::Available,
            (_, file) => {
                let file = file.unwrap_or_default();
                PackState::Conflict { reason: format!("{file} is missing this pack's change; add the pack again."), file }
            }
        }
    }

    fn flyover_file(&self, id: &str) -> bool {
        self.manifest.flyover.as_ref().is_some_and(|s| s.file == id)
    }

    /// `now` equals `target` apart from the flyover table's contents.
    fn same_but_flyover(&self, id: &str, now: &[u8], target: &[u8]) -> bool {
        if !self.flyover_file(id) || now.len() != target.len() {
            return false;
        }
        let Some(t) = self.flyover(target) else { return false };
        let (from, to) = (t.at + 5, t.at + t.room.table_len());
        now[..from] == target[..from] && now[to..] == target[to..] && now[t.at + 6..t.at + 12] == target[t.at + 6..t.at + 12]
    }

    /// The flyover table in `bytes` (the pack's flyover file as the project has it).
    pub fn flyover(&self, bytes: &[u8]) -> Option<flyover::Table> {
        let spec = self.manifest.flyover.as_ref()?;
        flyover::find(bytes, &spec.magic)
    }

    /// `bytes` with the flyover table rewritten to `f`, checked against the table's room, the font
    /// and `chunk_ok`.
    pub fn with_flyover(&self, bytes: &[u8], f: &Flyover, chunk_ok: &dyn Fn(u16) -> bool) -> PackResult<Vec<u8>> {
        let t = self.flyover(bytes).ok_or_else(|| PackError::new("PACK_NO_FLYOVER", "The flyover isn't in the project's file; add the pack again."))?;
        flyover::check(f, &t.room, chunk_ok).map_err(|m| PackError::new("PACK_FLYOVER", m))?;
        let mut out = bytes.to_vec();
        flyover::write(&mut out, &t, f);
        Ok(out)
    }

    /// Where the executable writes stand in `exe`: all the pack's (true), all retail (false), or
    /// neither (None: something else changed them, or the executable doesn't read).
    fn exe_side(&self, exe: &[u8]) -> Option<bool> {
        let x = crate::exe::PsxExe::parse(0, exe.to_vec()).ok()?;
        let mut side = None;
        for w in &self.manifest.exe_writes {
            let (va, from, to) = w.parts()?;
            let at = x.vaddr_to_offset(x.va(va)).ok()?;
            let now = exe.get(at..at + from.len())?;
            let this = if now == to.as_slice() { true } else if now == from.as_slice() { false } else { return None };
            if side.is_some_and(|s| s != this) {
                return None;
            }
            side = Some(this);
        }
        side
    }

    /// `exe` (the bytes the project has) with the pack's executable writes on (or back to retail),
    /// each over the bytes the other side expects.
    pub fn exe_write(&self, exe: &[u8], on: bool) -> PackResult<Vec<u8>> {
        let x = crate::exe::PsxExe::parse(0, exe.to_vec()).map_err(|e| PackError::new("PACK_EXE", e.to_string()))?;
        let mut out = exe.to_vec();
        for w in &self.manifest.exe_writes {
            let (va, from, to) = w.parts().ok_or_else(|| PackError::new("PACK_EXE", format!("bad executable write at {}", w.va)))?;
            let at = x.vaddr_to_offset(x.va(va)).map_err(|e| PackError::new("PACK_EXE", e.to_string()))?;
            let (old, new) = if on { (&from, &to) } else { (&to, &from) };
            let now = out.get(at..at + from.len()).ok_or_else(|| PackError::new("PACK_EXE", format!("{} is past the executable's end", w.va)))?;
            if now != old.as_slice() && now != new.as_slice() {
                return Err(PackError::new("PACK_EXE", format!("The executable's {} at {} isn't what this pack expects.", if w.what.is_empty() { "bytes" } else { &w.what }, w.va)));
            }
            out[at..at + from.len()].copy_from_slice(new);
        }
        Ok(out)
    }

    /// The canonical ids of the files the pack changes.
    pub fn file_ids(&self) -> Vec<String> {
        self.manifest.files.iter().map(|f| f.id.clone()).collect()
    }

    /// Bytes the pack's patches take.
    pub fn patch_bytes(&self) -> u64 {
        self.manifest.files.iter().map(|f| f.patch_size).sum()
    }
}

fn mismatch(f: &PackFile) -> String {
    if f.extra_sources.is_empty() {
        format!("{} isn't the retail file this pack patches (a base or another dump changed it)", f.id)
    } else {
        format!("{} or a file it's made from ({}) isn't retail (a base or another dump changed it)", f.id, f.extra_sources.join(", "))
    }
}

fn matches_spec(bytes: &[u8], size: u64, crc32: &str) -> bool {
    bytes.len() as u64 == size && bps::crc32_hex(bytes).eq_ignore_ascii_case(crc32)
}

fn release_name(r: Release) -> &'static str {
    r.info().region
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_packs_parse() {
        let p = find("jp-prologue").expect("jp-prologue");
        assert_eq!(p.manifest.release, Release::Usa);
        assert_eq!(p.file_ids(), ["ovr/1", "ovr/25"]);
        assert_eq!(p.manifest.flyover.as_ref().map(|s| s.magic.as_str()), Some("LTFY"));
        let f = &p.manifest.files[1];
        assert_eq!(p.patch_url(PATCH_BASE, f), format!("https://downloads.livingtool.app/packs/jp-prologue/{}/ovr25.bps", p.manifest.version));
        assert_eq!(f.patch_sha256.len(), 64);
    }

    #[test]
    fn a_pack_knows_its_state_from_crcs_without_patches() {
        let p = find("jp-prologue").unwrap();
        let none: Patches = &|_| None;
        let retail: Files = &|_| None;
        assert!(matches!(p.state(Release::Usa, retail, retail, retail, none), PackState::Unavailable { .. }));
        assert!(matches!(p.state(Release::Eu, retail, retail, retail, none), PackState::Unavailable { .. }));
        assert!(!patch_ok(&p.manifest.files[0], b"not the patch"));
    }
}
