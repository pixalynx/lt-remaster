//! Bases (research 06 §6, phase 2): a project can be built on a patched
//! game instead of the retail disc. A base travels as a folder (a "base
//! package"): `base.json` ([`BaseManifest`]), a BPS from the verified retail
//! image to the base image, and optionally a flag catalogue and a boot save
//! state. Living Tool applies the BPS once into a per-base cache and opens
//! the result as the project's reference disc; the retail disc stays open
//! for art harvested from retail chunks, and players' patches are made
//! against retail so they apply one file to their own disc.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::bps;
use crate::build::pread;
use crate::disc::RAW_SECTOR;
use crate::identity::RETAIL_SECTORS;
use crate::savestate::SaveState;

pub mod published;

/// The first chunk id new areas may use on retail (one past the last
/// retail chunk index range, see [`crate::world::is_new_area`]).
pub const RETAIL_NEW_AREA_FIRST: u32 = crate::world::NEW_AREA_FIRST;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseSource {
    pub serial: String,
    /// CRC32 of the image the BPS applies to (lowercase hex).
    pub crc32: String,
    pub sectors: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BasePatch {
    /// The BPS file, relative to the package folder.
    pub bps: String,
    /// SHA-1 of the base image the BPS produces (lowercase hex).
    pub target_sha1: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseLayout {
    /// The first chunk id this project's new areas may take: the base's own
    /// appended chunks (and files) sit below it.
    #[serde(default)]
    pub new_area_first: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseBoot {
    pub chunk: u32,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub h: i32,
    #[serde(default)]
    pub z: i32,
    #[serde(default)]
    pub dir: i32,
}

/// Ids the base uses for itself; editors warn before a project takes one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseReserved {
    /// Ranges like `"0x0EE-0x0FF"` or single ids `"0x120"`.
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub battles: Vec<String>,
    #[serde(default)]
    pub chunks: Vec<String>,
    /// NPC pack numbers (`released` only: retail's pack swaps a base took out).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseCatalogues {
    /// A flag catalogue (the shape of `src/data/flag-catalogue.json`), relative to the package.
    #[serde(default)]
    pub flags: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseSavestates {
    /// A PCSX-Redux save state at the base's boot point, relative to the package.
    #[serde(default)]
    pub boot: Option<String>,
}

/// `base.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseManifest {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub name: Option<String>,
    pub source: BaseSource,
    pub patch: BasePatch,
    #[serde(default)]
    pub layout: BaseLayout,
    #[serde(default)]
    pub boot: Option<BaseBoot>,
    #[serde(default)]
    pub reserved: BaseReserved,
    /// Retail engine ids the base's code no longer reacts to (HackerGC's blank profile points the
    /// checks elsewhere or takes them out): Living Tool stops flagging them ([`crate::engine`]).
    #[serde(default)]
    pub released: BaseReserved,
    #[serde(default)]
    pub catalogues: BaseCatalogues,
    #[serde(default)]
    pub savestates: BaseSavestates,
    /// Engine-profile overrides; empty means the retail layout.
    #[serde(default)]
    pub profile: serde_json::Map<String, serde_json::Value>,
}

impl BaseManifest {
    /// `id@version`, how projects name their base.
    pub fn tag(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    /// Where the base's EXE moved main's tables (`profile.exeLayout`, written by the decomp's
    /// shift-safe build), if it moved them.
    pub fn exe_layout(&self) -> Option<crate::layout::Layout> {
        self.profile.get("exeLayout").and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// The first chunk id a project on this base may give a new area.
    pub fn new_area_first(&self) -> u32 {
        self.layout.new_area_first.unwrap_or(RETAIL_NEW_AREA_FIRST).max(RETAIL_NEW_AREA_FIRST)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BaseError {
    pub code: &'static str,
    pub message: String,
}

fn bad(code: &'static str, m: impl Into<String>) -> BaseError {
    BaseError { code, message: m.into() }
}

/// A base package on disk: its folder and manifest.
#[derive(Debug, Clone)]
pub struct BasePackage {
    pub dir: PathBuf,
    pub manifest: BaseManifest,
}

impl BasePackage {
    /// Reads `dir/base.json` and checks the files it names exist.
    pub fn open(dir: &Path) -> Result<Self, BaseError> {
        let path = dir.join("base.json");
        let text = std::fs::read_to_string(&path).map_err(|e| bad("BASE_NO_MANIFEST", format!("{}: {e}", path.display())))?;
        let manifest: BaseManifest = serde_json::from_str(&text).map_err(|e| bad("BASE_BAD_MANIFEST", format!("base.json doesn't read: {e}")))?;
        if manifest.id.is_empty() || manifest.id.contains(['/', '\\', '@']) {
            return Err(bad("BASE_BAD_MANIFEST", "base.json needs an id without slashes or @."));
        }
        let p = Self { dir: dir.to_path_buf(), manifest };
        // A shifted base: EXEs with its hash read main's tables through its layout from now on.
        if let Some(l) = p.manifest.exe_layout() {
            crate::layout::register(l);
        }
        for f in [Some(p.manifest.patch.bps.as_str()), p.manifest.catalogues.flags.as_deref(), p.manifest.savestates.boot.as_deref()].into_iter().flatten() {
            if !p.dir.join(f).is_file() {
                return Err(bad("BASE_MISSING_FILE", format!("base.json names {f}, which isn't in the package.")));
            }
        }
        Ok(p)
    }

    pub fn file(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    /// The boot save state, inflated and checked; `None` when the base ships none.
    pub fn boot_state(&self) -> Result<Option<SaveState>, BaseError> {
        let Some(rel) = self.manifest.savestates.boot.as_deref() else { return Ok(None) };
        let bytes = std::fs::read(self.file(rel)).map_err(|e| bad("BASE_IO", format!("{rel}: {e}")))?;
        SaveState::read(&bytes).map(Some).map_err(|e| BaseError { message: format!("{rel}: {}", e.message), ..e })
    }
}

/// The name a package's boot save state gets ([`set_boot_state`]).
pub const BOOT_STATE: &str = "boot.sstate";

/// Makes `state` (a PCSX-Redux save state, gzipped as its GUI writes `.sstate`
/// or raw as `/api/v1/state-raw` returns it) the boot save state of the
/// package in `dir`: written gzipped as `boot.sstate`, named in `base.json`
/// (`savestates.boot`). It must be taken on the image the package's BPS makes,
/// idle on a field map; nothing here can check the image, since a state
/// doesn't record its disc.
pub fn set_boot_state(dir: &Path, state: &[u8]) -> Result<BasePackage, BaseError> {
    let mut pkg = BasePackage::open(dir)?;
    let s = SaveState::read(state)?;
    std::fs::write(dir.join(BOOT_STATE), s.gzip()).map_err(|e| bad("BASE_IO", format!("{BOOT_STATE}: {e}")))?;
    if let Some(old) = pkg.manifest.savestates.boot.replace(BOOT_STATE.into()).filter(|o| o != BOOT_STATE) {
        let _ = std::fs::remove_file(dir.join(old));
    }
    std::fs::write(dir.join("base.json"), serde_json::to_vec_pretty(&pkg.manifest).expect("json")).map_err(|e| bad("BASE_IO", e.to_string()))?;
    pkg = BasePackage::open(dir)?;
    Ok(pkg)
}

impl BasePackage {
    /// Copies the manifest and the files it names into `dest` (a project's
    /// `base/` folder), so the project carries its base with it.
    pub fn copy_to(&self, dest: &Path) -> Result<BasePackage, BaseError> {
        std::fs::create_dir_all(dest).map_err(|e| bad("BASE_IO", format!("{}: {e}", dest.display())))?;
        let m = &self.manifest;
        for f in ["base.json"].into_iter().chain([Some(m.patch.bps.as_str()), m.catalogues.flags.as_deref(), m.savestates.boot.as_deref()].into_iter().flatten()) {
            let to = dest.join(f);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| bad("BASE_IO", format!("{}: {e}", parent.display())))?;
            }
            std::fs::copy(self.dir.join(f), &to).map_err(|e| bad("BASE_IO", format!("copying {f}: {e}")))?;
        }
        BasePackage::open(dest)
    }
}

/// A base applied to retail: the cached image and its fingerprint
/// (`<cache>/<tag>-<sha1 prefix>/ready.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Materialised {
    pub tag: String,
    pub image: PathBuf,
    pub cue: PathBuf,
    pub crc32: String,
    pub sha1: String,
    pub bytes: u64,
    pub sectors: u32,
}

/// The folder a base is materialised into under `cache_root` (one per base
/// version and target, shared by every project on it).
pub fn cache_dir(cache_root: &Path, m: &BaseManifest) -> PathBuf {
    let sha = m.patch.target_sha1.get(..12).unwrap_or(&m.patch.target_sha1);
    cache_root.join(format!("{}-{sha}", m.tag()))
}

fn read_ready(dir: &Path) -> Option<Materialised> {
    let r: Materialised = serde_json::from_slice(&std::fs::read(dir.join("ready.json")).ok()?).ok()?;
    (std::fs::metadata(&r.image).ok()?.len() == r.bytes).then_some(r)
}

/// The cached base whose image has this CRC32, if one was materialised (how
/// a base's image is recognised when opened as a disc).
pub fn find_cached(cache_root: &Path, crc32: &str) -> Option<Materialised> {
    std::fs::read_dir(cache_root).ok()?.flatten().filter_map(|e| read_ready(&e.path())).find(|r| r.crc32.eq_ignore_ascii_case(crc32))
}

fn file_crc32(path: &Path, cancel: &AtomicBool, mut progress: impl FnMut(u64)) -> Result<(u32, u64), BaseError> {
    let mut f = File::open(path).map_err(|e| bad("BASE_IO", format!("{}: {e}", path.display())))?;
    let (mut crc, mut buf, mut done) = (crc32fast::Hasher::new(), vec![0u8; 4 << 20], 0u64);
    loop {
        let n = f.read(&mut buf).map_err(|e| bad("BASE_IO", format!("{}: {e}", path.display())))?;
        if n == 0 {
            return Ok((crc.finalize(), done));
        }
        crc.update(&buf[..n]);
        done += n as u64;
        progress(done);
        if cancel.load(Ordering::Relaxed) {
            return Err(bad("CANCELLED", "cancelled"));
        }
    }
}

/// The target of [`bps::apply_stream`]: a file written in order, hashed as it goes.
struct FileTarget<'a> {
    out: BufWriter<File>,
    read: File,
    sha: sha1_smol::Sha1,
    crc: crc32fast::Hasher,
    written: u64,
    reported: u64,
    on_write: &'a mut dyn FnMut(u64),
}

impl bps::Target for FileTarget<'_> {
    fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.sha.update(bytes);
        self.crc.update(bytes);
        self.written += bytes.len() as u64;
        if self.written - self.reported >= 4 << 20 {
            self.reported = self.written;
            (self.on_write)(self.written);
        }
        self.out.write_all(bytes).map_err(|e| e.to_string())
    }
    fn read_back(&mut self, at: u64, buf: &mut [u8]) -> Result<(), String> {
        self.out.flush().map_err(|e| e.to_string())?;
        pread(&self.read, buf, at).map_err(|e| e.to_string())
    }
}

/// Applies the package's BPS to the retail image at `retail` into the base's
/// cache folder, unless it is already there. Checks the retail CRC32 against
/// the one the patch needs and the result against `targetSha1`.
/// `progress(done, total)` covers checking retail and writing the base.
pub fn materialise(pkg: &BasePackage, retail: &Path, cache_root: &Path, cancel: &AtomicBool, mut progress: impl FnMut(u64, u64)) -> Result<Materialised, BaseError> {
    let m = &pkg.manifest;
    let dir = cache_dir(cache_root, m);
    if let Some(r) = read_ready(&dir).filter(|r| r.sha1.eq_ignore_ascii_case(&m.patch.target_sha1)) {
        return Ok(r);
    }
    let patch = std::fs::read(pkg.file(&m.patch.bps)).map_err(|e| bad("BASE_IO", format!("{}: {e}", m.patch.bps)))?;
    let info = bps::inspect(&patch).map_err(|e| bad("BASE_BAD_PATCH", format!("{}: {e}", m.patch.bps)))?;
    if !info.patch_ok {
        return Err(bad("BASE_BAD_PATCH", format!("{} is damaged (its own checksum doesn't match).", m.patch.bps)));
    }
    if !info.source_crc32.eq_ignore_ascii_case(&m.source.crc32) {
        return Err(bad("BASE_BAD_MANIFEST", format!("base.json says the patch applies to CRC32 {}, but {} needs {}.", m.source.crc32, m.patch.bps, info.source_crc32)));
    }
    let total = info.source_size + info.target_size;
    let (crc, size) = file_crc32(retail, cancel, |d| progress(d, total))?;
    if size != info.source_size || format!("{crc:08x}") != info.source_crc32 {
        return Err(bad(
            "BASE_WRONG_RETAIL",
            format!("{} applies to the disc with CRC32 {} ({} bytes); this one is {crc:08x} ({size} bytes).", m.tag(), info.source_crc32, info.source_size),
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| bad("BASE_IO", format!("{}: {e}", dir.display())))?;
    let part = dir.join("base.bin.part");
    let file = File::create(&part).map_err(|e| bad("BASE_IO", format!("{}: {e}", part.display())))?;
    let read = file.try_clone().map_err(|e| bad("BASE_IO", e.to_string()))?;
    let mut on_write = |w: u64| progress(info.source_size + w, total);
    let mut target = FileTarget {
        out: BufWriter::with_capacity(4 << 20, file),
        read,
        sha: sha1_smol::Sha1::new(),
        crc: crc32fast::Hasher::new(),
        written: 0,
        reported: 0,
        on_write: &mut on_write,
    };
    let src = File::open(retail).map_err(|e| bad("BASE_IO", format!("{}: {e}", retail.display())))?;
    let applied = bps::apply_stream(
        &patch,
        |at, buf| {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            pread(&src, buf, at).map_err(|e| e.to_string())
        },
        &mut target,
    );
    let done = applied.and_then(|_| target.out.flush().map_err(|e| e.to_string()));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&part);
        return Err(if cancel.load(Ordering::Relaxed) { bad("CANCELLED", "cancelled") } else { bad("BASE_BAD_PATCH", format!("applying {}: {e}", m.patch.bps)) });
    }
    let sha1 = target.sha.digest().to_string();
    if !sha1.eq_ignore_ascii_case(&m.patch.target_sha1) || target.written % RAW_SECTOR as u64 != 0 {
        let _ = std::fs::remove_file(&part);
        return Err(bad("BASE_BAD_RESULT", format!("{} produced an image with SHA-1 {sha1}; base.json expects {}.", m.patch.bps, m.patch.target_sha1)));
    }
    let image = dir.join("base.bin");
    std::fs::rename(&part, &image).map_err(|e| bad("BASE_IO", e.to_string()))?;
    let cue = dir.join("base.cue");
    std::fs::write(&cue, crate::build::cue_sheet("base.bin")).map_err(|e| bad("BASE_IO", e.to_string()))?;
    let r = Materialised {
        tag: m.tag(),
        image,
        cue,
        crc32: format!("{:08x}", target.crc.finalize()),
        sha1,
        bytes: target.written,
        sectors: (target.written / RAW_SECTOR as u64) as u32,
    };
    std::fs::write(dir.join("ready.json"), serde_json::to_vec_pretty(&r).expect("json")).map_err(|e| bad("BASE_IO", e.to_string()))?;
    Ok(r)
}

/// Makes a base package from a retail image and a base image built from it:
/// a BPS against retail, `base.json` with the source and target filled in,
/// and copies of the catalogue and save state `manifest` names (relative to
/// `extras`). How HackerGC releases (phase 3) and the tests make packages.
pub fn make_package(retail: &Path, image: &Path, out_dir: &Path, mut manifest: BaseManifest, extras: Option<&Path>, cancel: &AtomicBool) -> Result<BasePackage, BaseError> {
    std::fs::create_dir_all(out_dir).map_err(|e| bad("BASE_IO", format!("{}: {e}", out_dir.display())))?;
    let meta = format!("{{\"tool\":\"Living Tool {}\",\"base\":\"{}\"}}", crate::project::APP_VERSION, manifest.tag());
    let made = crate::build::make_bps(retail, image, &meta, &|_| None, cancel, |_, _| {}).map_err(|e| bad("BASE_BAD_PATCH", e.message))?;
    let bps_name = format!("{}-{}.bps", manifest.id, manifest.version);
    std::fs::write(out_dir.join(&bps_name), &made.patch).map_err(|e| bad("BASE_IO", e.to_string()))?;
    let retail_bytes = std::fs::metadata(retail).map_err(|e| bad("BASE_IO", e.to_string()))?.len();
    let mut sha = sha1_smol::Sha1::new();
    let mut f = File::open(image).map_err(|e| bad("BASE_IO", format!("{}: {e}", image.display())))?;
    let mut buf = vec![0u8; 4 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| bad("BASE_IO", e.to_string()))?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    manifest.source = BaseSource {
        serial: if manifest.source.serial.is_empty() { "SLUS-00811".into() } else { manifest.source.serial.clone() },
        crc32: format!("{:08x}", made.source_crc32),
        sectors: (retail_bytes / RAW_SECTOR as u64) as u32,
    };
    manifest.patch = BasePatch { bps: bps_name, target_sha1: sha.digest().to_string() };
    // A base that appends files past retail's end (HackerGC's spare files) starts the project's
    // new areas past them: chunk n reads at the world archive's LBA + n·125. The first id is
    // always written, so a package says where its new areas start (LT-64).
    let image_sectors = (std::fs::metadata(image).map_err(|e| bad("BASE_IO", e.to_string()))?.len() / RAW_SECTOR as u64) as u32;
    let mut first = manifest.new_area_first();
    if let Some(world) = world_lba(retail) {
        first = first.max(image_sectors.saturating_sub(world).div_ceil(crate::world::CHUNK_SECTORS));
    }
    manifest.layout.new_area_first = Some(first);
    if let Some(extras) = extras {
        for f in [manifest.catalogues.flags.clone(), manifest.savestates.boot.clone()].into_iter().flatten() {
            std::fs::copy(extras.join(&f), out_dir.join(&f)).map_err(|e| bad("BASE_IO", format!("copying {f}: {e}")))?;
        }
    }
    std::fs::write(out_dir.join("base.json"), serde_json::to_vec_pretty(&manifest).expect("json")).map_err(|e| bad("BASE_IO", e.to_string()))?;
    BasePackage::open(out_dir)
}

/// Where retail's world archive (chunk 0) starts, which no base moves.
fn world_lba(retail: &Path) -> Option<u32> {
    let image = crate::DiscImage::open(retail).ok()?;
    let exe = crate::PsxExe::from_disc(&image).ok()?;
    let table = crate::FileTable::parse(&exe, image.sectors()).ok()?;
    Some(table.by_id("world")?.extent.lba)
}

/// A manifest to fill in with [`make_package`]: `id@version` on retail.
pub fn manifest_template(id: &str, version: &str) -> BaseManifest {
    BaseManifest {
        id: id.into(),
        version: version.into(),
        name: None,
        source: BaseSource { serial: "SLUS-00811".into(), crc32: String::new(), sectors: RETAIL_SECTORS },
        patch: BasePatch { bps: String::new(), target_sha1: String::new() },
        layout: BaseLayout::default(),
        boot: None,
        reserved: BaseReserved::default(),
        released: BaseReserved::default(),
        catalogues: BaseCatalogues::default(),
        savestates: BaseSavestates::default(),
        profile: Default::default(),
    }
}

/// Parses a reserved range list (`"0x0EE-0x0FF"`, `"0x120"`, decimal too).
pub fn parse_ranges(list: &[String]) -> Vec<(u32, u32)> {
    let num = |s: &str| {
        let s = s.trim();
        s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).map_or_else(|| s.parse().ok(), |h| u32::from_str_radix(h, 16).ok())
    };
    list.iter()
        .filter_map(|r| match r.split_once('-') {
            Some((a, b)) => Some((num(a)?, num(b)?)),
            None => num(r).map(|v| (v, v)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_reads_the_research_sketch() {
        let json = r#"{
          "id": "hackergc-blank", "version": "0.1.0",
          "source": { "serial": "SLUS-00811", "crc32": "cdc72c62", "sectors": 185985 },
          "patch": { "bps": "hackergc-blank-0.1.0.bps", "targetSha1": "00" },
          "layout": { "newAreaFirst": 1400 },
          "boot": { "chunk": 1400, "x": 0, "h": 0, "z": 0, "dir": 0 },
          "reserved": { "flags": ["0x0EE-0x0FF"], "battles": ["0x101-0x123"], "chunks": [] },
          "catalogues": { "flags": "flags.json" },
          "savestates": { "boot": "boot.sstate" },
          "profile": {}
        }"#;
        let m: BaseManifest = serde_json::from_str(json).unwrap();
        assert_eq!((m.tag().as_str(), m.new_area_first()), ("hackergc-blank@0.1.0", 1400));
        assert_eq!(parse_ranges(&m.reserved.flags), [(0xEE, 0xFF)]);
        // Older than retail's first new area isn't allowed.
        let mut low = m.clone();
        low.layout.new_area_first = Some(10);
        assert_eq!(low.new_area_first(), RETAIL_NEW_AREA_FIRST);
    }

    /// @LT-11/1 @LT-11/2: a package gets a boot save state, which then reads back with the
    /// chunk it starts on and travels with the package; one without a state says so.
    #[test]
    fn a_boot_save_state_goes_into_a_package_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("lt-base-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = manifest_template("hackergc-blank", "1.0.0");
        m.patch = BasePatch { bps: "hackergc-blank-1.0.0.bps".into(), target_sha1: "00".into() };
        std::fs::write(dir.join(&m.patch.bps), b"BPS1").unwrap();
        std::fs::write(dir.join("base.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
        // No state: the action has nothing to load.
        assert!(BasePackage::open(&dir).unwrap().boot_state().unwrap().is_none());

        // Not a state: refused, and base.json is left alone.
        assert_eq!(set_boot_state(&dir, b"not a state").unwrap_err().code, "SAVESTATE_BAD");
        assert_eq!(BasePackage::open(&dir).unwrap().manifest.savestates.boot, None);

        // Raw, as /api/v1/state-raw returns it: stored gzipped as boot.sstate.
        let raw = crate::savestate::tests::sample(4, 326);
        let pkg = set_boot_state(&dir, &raw).unwrap();
        assert_eq!(pkg.manifest.savestates.boot.as_deref(), Some(BOOT_STATE));
        assert!(std::fs::read(dir.join(BOOT_STATE)).unwrap().starts_with(&[0x1F, 0x8B]));
        let state = pkg.boot_state().unwrap().unwrap();
        assert_eq!((state.chunk(), state.raw()), (326, raw.as_slice()));

        // A project's copy carries it.
        let copy = pkg.copy_to(&dir.join("project-base")).unwrap();
        assert_eq!(copy.boot_state().unwrap().unwrap().chunk(), 326);

        // A damaged state file fails to read with its name, not silently.
        std::fs::write(dir.join(BOOT_STATE), [0x1F, 0x8B, 8, 0]).unwrap();
        let e = BasePackage::open(&dir).unwrap().boot_state().unwrap_err();
        assert!(e.code == "SAVESTATE_BAD" && e.message.starts_with("boot.sstate: "), "{e:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
