//! The disc writer (`build::Rebuilder`, `docs/screens/15-file-patcher.md` §5):
//! plan where each staged file goes, write a patched copy of the source image,
//! verify it, make a BPS, and record the build in the project. A port of
//! `gc_decomp/tools/inject_disc.py`, generalised to all 493 table entries.
//!
//! - The source image is only read. The output is written as
//!   `<name>.bin.partial` and renamed once verified, so a failed or cancelled
//!   build leaves no image behind.
//! - A file that fits its allocation is written in place over the whole
//!   allocation (zero-padded). The sectors keep their header and subheader, so
//!   the EOF/EOR marker stays on the extent's last sector as in retail.
//! - A file that grew is appended past the end of the image (LBA 185,985 on
//!   retail), in global-index order, and every table entry of its alias group
//!   is repointed. Its sectors follow the file's own subheader pattern
//!   (`00 00 08 00`, last `00 00 89 00`).
//! - Chunks are written in place inside the world archive at 20,011 + n × 125.
//! - The EXE (LBA 24, 155 sectors, size-locked) gets the user's edits plus the
//!   table rewrites; only sectors that differ are written.
//! - When anything was appended, a 150-sector postgap follows (sync + header,
//!   as retail 185,835–185,984) and the PVD volume space size is updated.
//! - Every written sector has its EDC and ECC recomputed; sectors whose bytes
//!   come out unchanged aren't written, so untouched sectors stay identical.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::bps;
use crate::cdrom::{self, Raw};
use crate::disc::{DiscImage, RAW_SECTOR, USER_DATA};
use crate::error::LtError;
use crate::exe::PsxExe;
use crate::fs::{lba_to_bcd_msf, Extent, FileTable, Kind, ENTRY_SIZE, TABLE_LEN};
use crate::identity::{self, RETAIL_SECTORS};
use crate::iso9660;
use crate::project::{now_rfc3339, sha256_hex, write_atomic, write_json, Project, APP_VERSION};
use crate::world::{CHUNK_COUNT, CHUNK_SECTORS};

pub const POSTGAP_SECTORS: u32 = 150;
/// A 74-minute CD-R.
pub const CAPACITY_SECTORS: u32 = 333_000;
pub const PVD_LBA: u32 = 16;
pub const SCHEMA: u32 = 1;

/// The job's steps (PRD 15 §3.1). "Make BPS" only runs when asked for. The
/// boot test needs the PCSX-Redux bridge, which doesn't exist yet, so it is
/// not a step.
pub const STEPS: [&str; 7] =
    ["Prepare", "Copy retail image", "Write files", "Patch file table", "Update volume size", "Verify", "Make BPS"];
pub const STEP_PREPARE: usize = 0;
pub const STEP_COPY: usize = 1;
pub const STEP_WRITE: usize = 2;
pub const STEP_TABLE: usize = 3;
pub const STEP_VOLUME: usize = 4;
pub const STEP_VERIFY: usize = 5;
pub const STEP_BPS: usize = 6;

/* ───────────────────────── Errors and events ───────────────────────── */

/// A build failure: a stable code (`BUDGET_NPC_CAP`, `BASE_MISMATCH`…), a
/// plain-English message and the asset it concerns, if any.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuildError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
}

impl BuildError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_string(), message: message.into(), asset: None }
    }

    pub fn on(mut self, asset: &str) -> Self {
        self.asset = Some(asset.to_string());
        self
    }

    pub fn cancelled() -> Self {
        Self::new("BUILD_CANCELLED", "The build was cancelled. Nothing was kept.")
    }

    pub fn is_cancel(&self) -> bool {
        self.code == "BUILD_CANCELLED"
    }
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl From<LtError> for BuildError {
    fn from(e: LtError) -> Self {
        Self::new(e.code(), e.to_string())
    }
}

type Res<T> = std::result::Result<T, BuildError>;

fn io(path: &Path, e: std::io::Error) -> BuildError {
    BuildError::new("IO", format!("{}: {e}", path.display()))
}

/// Progress of a build, mapped onto the job events of `ARCHITECTURE.md` §6.
#[derive(Debug, Clone)]
pub enum BuildEvent {
    Started { title: String, steps: Vec<String> },
    Progress { step: usize, done: u64, total: u64, detail: Option<String> },
    StepDone { step: usize, detail: Option<String> },
}

fn check(cancel: &AtomicBool) -> Res<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(BuildError::cancelled())
    } else {
        Ok(())
    }
}

/* ───────────────────────── Inputs ───────────────────────── */

/// The image a build starts from (read-only), with its EXE and file table.
pub struct Source<'a> {
    pub image: &'a DiscImage,
    pub exe: &'a PsxExe,
    pub table: &'a FileTable,
}

impl Source<'_> {
    fn world(&self) -> Res<Extent> {
        self.table
            .by_id("world")
            .map(|e| e.extent)
            .ok_or_else(|| BuildError::new("TABLE_UNKNOWN_ENTRY", "the table has no world archive"))
    }

    /// Bytes of a canonical id as the disc has them, sector-padded (what a
    /// project records as an edit's base).
    pub fn retail(&self, id: &str) -> Res<Vec<u8>> {
        if id == "exe" {
            return Ok(self.exe.bytes.clone());
        }
        if let Some(n) = chunk_number(id) {
            let w = self.world()?;
            let lba = w.lba + n * CHUNK_SECTORS;
            if crate::world::is_new_area(n) && lba + CHUNK_SECTORS > self.image.sectors() {
                return Ok(vec![0; CHUNK_SECTORS as usize * USER_DATA]);
            }
            return Ok(self.image.read_sectors(lba, CHUNK_SECTORS)?);
        }
        let e = self.table.by_id(id).ok_or_else(|| BuildError::new("TABLE_UNKNOWN_ENTRY", format!("no {id}")).on(id))?;
        Ok(self.image.read_sectors(e.extent.lba, e.extent.sectors)?)
    }
}

/// One staged file of the project.
#[derive(Debug, Clone)]
pub struct Staged {
    /// Canonical id of the alias group's lowest member (`npc/7`, `chunk/39`, `exe`).
    pub id: String,
    pub bytes: Arc<Vec<u8>>,
    /// SHA-256 of the retail bytes the edit was made against (manifest `base`).
    pub base_sha256: Option<String>,
}

pub fn chunk_number(id: &str) -> Option<u32> {
    id.strip_prefix("chunk/")?.parse().ok().filter(|n| *n < CHUNK_COUNT || crate::world::is_new_area(*n))
}

fn sectors_for(bytes: usize) -> u32 {
    bytes.div_ceil(USER_DATA) as u32
}

/* ───────────────────────── Budgets ───────────────────────── */

/// A loader cap in sectors (loaders compare `sectors << 11`).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cap {
    pub sectors: u32,
    pub bytes: u32,
    /// The refusal code when a file is over it.
    pub code: &'static str,
    pub label: &'static str,
    /// Inferred from the RAM map rather than read from a loader.
    pub inference: bool,
}

const STAGING: Cap = Cap { sectors: 64, bytes: 0x20000, code: "BUDGET_STAGING", label: "Staging buffer cap", inference: false };

/// The cap of a table entry's loader (PRD 04 §4.2; the UI's `capFor`).
pub fn cap_for(kind: Kind, index: u16) -> Option<Cap> {
    match kind {
        Kind::Npc => Some(Cap { sectors: 98, bytes: 0x31000, code: "BUDGET_NPC_CAP", label: "NPC pack loader cap", inference: false }),
        // bgm/57 loads to 0x80010000, not the staging buffer.
        Kind::Bgm if index == 57 => None,
        Kind::Bgm | Kind::NpcSnd | Kind::Fe | Kind::Bf | Kind::Toy => Some(STAGING),
        // ovr/6..22 and ovr/25 load into the 0x4000 area slot at 0x8014F000.
        Kind::Ovr if (5..=21).contains(&index) || index == 24 => {
            Some(Cap { sectors: 8, bytes: 0x4000, code: "BUDGET_SLOT", label: "Area overlay slot", inference: true })
        }
        _ => None,
    }
}

/* ───────────────────────── Plan ───────────────────────── */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    InPlace,
    Relocate,
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub id: String,
    /// Table index; the world archive's for chunks; `None` for the EXE.
    pub global: Option<u16>,
    pub chunk: Option<u32>,
    pub included: bool,
    pub verdict: Verdict,
    /// Why a blocked item can't be built.
    pub refusal: Option<BuildError>,
    pub bytes: u64,
    /// The extent it has now (EXE: 24 + 155; chunk n: 20,011 + n × 125, 125).
    pub lba: u32,
    pub alloc: u32,
    /// Sectors the staged bytes need (EXE: sectors the edit changes).
    pub need: u32,
    pub cap: Option<Cap>,
    /// Where a relocated, included file lands.
    pub new_lba: Option<u32>,
    /// Table entries that point at the extent (its alias group).
    pub entries: Vec<u16>,
    /// Bytes free in the last needed sector.
    pub spare: u64,
    /// A movie: the staged bytes are 2336-byte Mode 2 payloads written as
    /// they are (Form 1 video, Form 2 XA audio), not 2048-byte user data.
    pub raw: bool,
}

impl PlanItem {
    /// Included and buildable.
    pub fn live(&self) -> bool {
        self.included && self.verdict != Verdict::Blocked
    }

    /// Sectors this item re-encodes (a movie only its own; the rest of a
    /// longer old extent stays as it was).
    fn written_sectors(&self) -> u32 {
        match self.verdict {
            Verdict::Relocate => self.need,
            _ if self.raw => self.need,
            _ => self.alloc,
        }
    }
}

/// One table entry the build rewrites.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryWrite {
    pub global: u16,
    pub id: String,
    pub lba: u32,
    pub sectors: u32,
    /// The 8 stored bytes: BCD m, s, f, 0, u32 LE sectors.
    pub raw: [u8; 8],
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// In build order: EXE first, then by global index (chunks by number).
    pub items: Vec<PlanItem>,
    pub source_sectors: u32,
    /// First appended sector (the end of the image, at least 185,985).
    pub cursor_start: u32,
    pub cursor_end: u32,
    pub appended: u32,
    pub postgap: u32,
    /// Sectors in the output image (the PVD volume space size).
    pub new_volume: u32,
    pub image_bytes: u64,
    pub entries_repointed: u32,
    pub exe_entries: Vec<EntryWrite>,
    /// LBAs of the EXE sectors that change (edits and table rewrites).
    pub exe_sectors: Vec<u32>,
    pub pvd_changed: bool,
    /// Sectors that get EDC/ECC recomputed.
    pub edc_sectors: u32,
    pub blocked_included: bool,
    /// Nothing included would change the image.
    pub nothing: bool,
    pub capacity: u32,
    /// The EXE as the build writes it.
    #[serde(skip)]
    pub exe_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct PlanOptions {
    /// Add 150 postgap sectors after appended files.
    pub postgap: bool,
    /// The first chunk id the project's new areas may take: 1328 on retail,
    /// a base's `layout.newAreaFirst` on a base. Ids below it are the base's
    /// (editable only where its image holds sectors); ids from it on must not
    /// overlap anything the base appended.
    pub new_area_first: u32,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self { postgap: false, new_area_first: crate::world::NEW_AREA_FIRST }
    }
}

fn sort_key(src: &Source, id: &str) -> (i64, u32, String) {
    if id == "exe" {
        return (-1, 0, String::new());
    }
    if let Some(n) = chunk_number(id) {
        let g = src.table.by_id("world").map_or(37, |e| e.global);
        return (g as i64, n, String::new());
    }
    match src.table.by_id(id) {
        Some(e) => (e.global as i64, 0, String::new()),
        None => (i64::MAX, 0, id.to_string()),
    }
}

/// The table's EXE file offset (0x41B04 on retail).
fn table_offset(src: &Source) -> usize {
    src.table.entries.first().map_or(0x41B04, |e| e.exe_offset as usize)
}

/// Checks an `exe` edit; returns the number of sectors it changes.
fn check_exe_edit(src: &Source, edit: &[u8]) -> Res<u32> {
    let retail = &src.exe.bytes;
    if edit.len() != retail.len() {
        return Err(BuildError::new(
            "EXE_SIZE_CHANGED",
            format!(
                "SLUS_008.11 must stay {} bytes (about 23,000 overlay references depend on it); this edit is {} bytes",
                grouped(retail.len() as u64),
                grouped(edit.len() as u64)
            ),
        ));
    }
    if &edit[..8] != b"PS-X EXE" {
        return Err(BuildError::new("EXE_BAD_HEADER", "the edited SLUS_008.11 doesn't start with \"PS-X EXE\""));
    }
    let t_size = u32::from_le_bytes(edit[0x1C..0x20].try_into().expect("4 bytes"));
    if t_size as usize != edit.len() - 0x800 {
        return Err(BuildError::new("EXE_SIZE_CHANGED", format!("the edited EXE header declares t_size 0x{t_size:X}, not 0x{:X}", edit.len() - 0x800)));
    }
    let off = table_offset(src);
    let range = off..off + TABLE_LEN * ENTRY_SIZE;
    if edit[range.clone()] != retail[range] {
        return Err(BuildError::new(
            "EXE_TABLE_EDITED",
            "the edit changes the hidden file table (EXE 0x41B04–0x42A6B), which only the build rewrites",
        ));
    }
    Ok(edit.chunks(USER_DATA).zip(retail.chunks(USER_DATA)).filter(|(a, b)| a != b).count() as u32)
}

/// A base's ground (research 06 §6.3): new areas below `first` belong to the
/// base and can only be edited where its image holds them; from `first` on
/// they must not land on anything the base appended past the retail end.
fn territory(item: &mut PlanItem, source_sectors: u32, first: u32) {
    let Some(n) = item.chunk.filter(|&n| crate::world::is_new_area(n)) else { return };
    if item.verdict == Verdict::Blocked {
        return;
    }
    let inside = item.lba + item.alloc <= source_sectors;
    let refusal = if n < first && !inside {
        Some(BuildError::new("BUILD_AREA_RESERVED", format!("chunk {n} is reserved by the base; this project's new areas start at chunk {first}")))
    } else if n >= first && item.lba < source_sectors {
        Some(BuildError::new(
            "BUILD_AREA_TAKEN",
            format!("chunk {n} would overwrite sectors the base already uses (its image ends at {source_sectors}); new areas need a higher id"),
        ))
    } else {
        None
    };
    if let Some(e) = refusal {
        item.verdict = Verdict::Blocked;
        item.refusal = Some(e.on(&item.id));
    }
}

fn classify(src: &Source, s: &Staged) -> PlanItem {
    let bytes = s.bytes.len();
    let need = sectors_for(bytes);
    let mut item = PlanItem {
        id: s.id.clone(),
        global: None,
        chunk: None,
        included: true,
        verdict: Verdict::InPlace,
        refusal: None,
        bytes: bytes as u64,
        lba: 0,
        alloc: 0,
        need,
        cap: None,
        new_lba: None,
        entries: Vec::new(),
        spare: need as u64 * USER_DATA as u64 - bytes as u64,
        raw: false,
    };
    let block = |item: &mut PlanItem, e: BuildError| {
        item.verdict = Verdict::Blocked;
        item.refusal = Some(e.on(&s.id));
    };

    if s.id == "exe" {
        item.lba = src.exe.lba;
        item.alloc = sectors_for(src.exe.bytes.len());
        item.spare = 0;
        match check_exe_edit(src, &s.bytes) {
            Ok(changed) => item.need = changed,
            Err(e) => block(&mut item, e),
        }
        return item;
    }

    if let Some(n) = chunk_number(&s.id) {
        match src.table.by_id("world") {
            Some(w) => {
                item.global = Some(w.global);
                item.chunk = Some(n);
                item.lba = w.extent.lba + n * CHUNK_SECTORS;
                item.alloc = CHUNK_SECTORS;
                if need > CHUNK_SECTORS {
                    let msg = format!("a chunk is a fixed {CHUNK_SECTORS} sectors (0x3E800 bytes); this one needs {need}");
                    block(&mut item, BuildError::new("BUDGET_CHUNK", msg));
                }
            }
            None => block(&mut item, BuildError::new("TABLE_UNKNOWN_ENTRY", "the table has no world archive")),
        }
        return item;
    }

    let Some(e) = src.table.by_id(&s.id) else {
        let e = if s.id.starts_with("new/") {
            BuildError::new("NEW_FILE_NEEDS_EXE_GROWTH", "new files need a spare file entry: a base built with HackerGC's file registry has them (new/0, new/1, ...); this disc has none left")
        } else {
            BuildError::new("TABLE_UNKNOWN_ENTRY", format!("{} isn't a file-table entry", s.id))
        };
        block(&mut item, e);
        return item;
    };
    item.global = Some(e.global);
    item.lba = e.extent.lba;
    item.alloc = e.extent.sectors;
    let mut entries: Vec<u16> = std::iter::once(e.global).chain(e.aliases.iter().copied()).collect();
    entries.sort_unstable();
    item.entries = entries;
    item.cap = cap_for(e.kind, e.index);
    if e.kind == Kind::Movie {
        item.raw = true;
        item.spare = 0;
        if !bytes.is_multiple_of(cdrom::PAYLOAD) || bytes == 0 {
            let msg = format!("a staged movie is whole 2336-byte Mode 2 sectors; {} has {bytes} bytes", s.id);
            block(&mut item, BuildError::new("BUILD_MOVIE_SECTORS", msg));
        } else {
            item.need = (bytes / cdrom::PAYLOAD) as u32;
            if item.need > item.alloc {
                item.verdict = Verdict::Relocate;
            }
        }
    } else if e.extent.sectors == 0 && e.kind == Kind::Spare {
        // A base's free spare entry: the new file is appended and the entry written.
        item.verdict = Verdict::Relocate;
    } else if e.extent.sectors == 0 {
        block(&mut item, BuildError::new("TABLE_EMPTY_ENTRY", format!("{} has no extent to replace", s.id)));
    } else if let Some(cap) = item.cap.filter(|c| need > c.sectors) {
        let msg = format!(
            "{} needs {need} sectors; the {} is 0x{:X} bytes = {} sectors, so the game would stop loading it",
            s.id,
            cap.label.to_lowercase(),
            cap.bytes,
            cap.sectors
        );
        block(&mut item, BuildError::new(cap.code, msg));
    } else if need > item.alloc {
        item.verdict = Verdict::Relocate;
    }
    item
}

/// The 8 bytes of a table entry.
pub fn entry_bytes(lba: u32, sectors: u32) -> [u8; 8] {
    let m = lba_to_bcd_msf(lba);
    let s = sectors.to_le_bytes();
    [m[0], m[1], m[2], 0, s[0], s[1], s[2], s[3]]
}

/// Dry run: where every staged file goes (no sector IO). `included(id)`
/// says whether a staged file is part of this build; excluded files stay as
/// the source has them.
pub fn plan(src: &Source, staged: &[Staged], included: impl Fn(&str) -> bool, opts: PlanOptions) -> Plan {
    let source_sectors = src.image.sectors();
    let cursor_start = source_sectors.max(RETAIL_SECTORS);
    let mut ordered: Vec<&Staged> = staged.iter().collect();
    ordered.sort_by_key(|s| sort_key(src, &s.id));
    let mut items: Vec<PlanItem> = ordered
        .into_iter()
        .map(|s| {
            let mut item = classify(src, s);
            item.included = included(&s.id);
            territory(&mut item, source_sectors, opts.new_area_first);
            item
        })
        .collect();
    // New areas sit at fixed LBAs past the end; relocated files go after the last of them.
    let reserved_end = items
        .iter()
        .filter(|i| i.live() && i.chunk.is_some_and(crate::world::is_new_area))
        .map(|i| i.lba + i.alloc)
        .max()
        .unwrap_or(0);
    let mut cursor = cursor_start.max(reserved_end);
    for item in &mut items {
        if item.verdict == Verdict::Relocate && item.included {
            item.new_lba = Some(cursor);
            cursor += item.need;
        }
    }

    // The EXE as written: the user's edit (if included) plus the table rewrites.
    let mut exe_bytes = match items.iter().find(|i| i.id == "exe" && i.live()) {
        Some(_) => staged.iter().find(|s| s.id == "exe").map(|s| s.bytes.as_ref().clone()).unwrap_or_default(),
        None => src.exe.bytes.clone(),
    };
    let mut exe_entries = Vec::new();
    for i in items.iter().filter(|i| i.live() && i.verdict == Verdict::Relocate) {
        let lba = i.new_lba.expect("live relocations have a new LBA");
        for &g in &i.entries {
            let raw = entry_bytes(lba, i.need);
            // The entry wherever it lives (a base's spare entries are outside the 493).
            let at = src.table.get(g).map_or(table_offset(src) + g as usize * ENTRY_SIZE, |e| e.exe_offset as usize);
            exe_bytes[at..at + ENTRY_SIZE].copy_from_slice(&raw);
            let id = src.table.get(g).map(|e| e.id.clone()).unwrap_or_default();
            exe_entries.push(EntryWrite { global: g, id, lba, sectors: i.need, raw });
        }
    }
    let exe_sectors: Vec<u32> = exe_bytes
        .chunks(USER_DATA)
        .zip(src.exe.bytes.chunks(USER_DATA))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(n, _)| src.exe.lba + n as u32)
        .collect();

    let appended = cursor - cursor_start;
    let postgap = if appended > 0 && opts.postgap { POSTGAP_SECTORS } else { 0 };
    let new_volume = if appended > 0 { cursor + postgap } else { source_sectors };
    let pvd_changed = new_volume != source_sectors;
    let live = || items.iter().filter(|i| i.live());
    let edc_sectors = live().filter(|i| i.id != "exe").map(PlanItem::written_sectors).sum::<u32>()
        + exe_sectors.len() as u32
        + pvd_changed as u32;
    Plan {
        source_sectors,
        cursor_start,
        cursor_end: cursor,
        appended,
        postgap,
        new_volume,
        image_bytes: new_volume as u64 * RAW_SECTOR as u64,
        entries_repointed: exe_entries.len() as u32,
        exe_entries,
        exe_sectors,
        pvd_changed,
        edc_sectors,
        blocked_included: items.iter().any(|i| i.included && i.verdict == Verdict::Blocked),
        nothing: live().count() == 0,
        capacity: CAPACITY_SECTORS,
        exe_bytes,
        items,
    }
}

/* ───────────────────────── Writing ───────────────────────── */

/// The only writable handle of a build: the `.partial` output, or (for the
/// in-process emulator, [`overlay`]) the written sectors kept in memory over
/// the read-only source.
struct Out<'a> {
    sink: Sink<'a>,
    sectors: u32,
}

enum Sink<'a> {
    File { file: File, path: PathBuf },
    Memory { base: &'a DiscImage, written: BTreeMap<u32, Raw> },
}

impl Out<'_> {
    fn read(&self, lba: u32) -> Res<Option<Raw>> {
        if lba >= self.sectors {
            return Ok(None);
        }
        match &self.sink {
            Sink::File { file, path } => {
                let mut raw = [0u8; RAW_SECTOR];
                pread(file, &mut raw, lba as u64 * RAW_SECTOR as u64).map_err(|e| io(path, e))?;
                Ok(Some(raw))
            }
            Sink::Memory { base, written } => match written.get(&lba) {
                Some(raw) => Ok(Some(*raw)),
                None => Ok(Some(base.read_raw(lba)?)),
            },
        }
    }

    fn write(&mut self, lba: u32, raw: &Raw) -> Res<()> {
        // Never leave a hole: anything between the end and `lba` is postgap.
        while self.sectors < lba {
            let gap = cdrom::postgap(self.sectors);
            self.put(self.sectors, &gap)?;
            self.sectors += 1;
        }
        self.put(lba, raw)?;
        self.sectors = self.sectors.max(lba + 1);
        Ok(())
    }

    fn put(&mut self, lba: u32, raw: &Raw) -> Res<()> {
        match &mut self.sink {
            Sink::File { file, path } => pwrite(file, raw, lba as u64 * RAW_SECTOR as u64).map_err(|e| io(path, e)),
            Sink::Memory { written, .. } => {
                written.insert(lba, *raw);
                Ok(())
            }
        }
    }
}

/// How the sectors of a written extent get their subheaders.
#[derive(Clone, Copy)]
enum Subs {
    /// In place: keep each sector's sync, header and subheader.
    Keep,
    /// New sectors: `body` on all but the last, which gets `last`.
    Fresh { body: [u8; 4], last: [u8; 4] },
}

/// What a write step did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteReport {
    /// `inject_disc.py`-style lines for `log.txt`.
    pub log: Vec<String>,
    pub records: Vec<PlanRecord>,
    /// Sector ranges `(lba, count)` re-encoded as Form 1 (each one is verified).
    pub form1: Vec<(u32, u32)>,
    /// Movie sectors `(lba, count)` written from Mode 2 payloads (Form 1 and 2).
    #[serde(default)]
    pub mode2: Vec<(u32, u32)>,
    /// Postgap sectors appended `(lba, count)`.
    pub postgap: Option<(u32, u32)>,
    pub sectors_encoded: u32,
    /// Sectors whose bytes changed (the rest came out identical and were skipped).
    pub sectors_changed: u32,
    pub exe_sectors_written: u32,
    pub volume_from: u32,
    pub volume_to: u32,
}

/// One line of `build.json.plan`: what was written where.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRecord {
    pub id: String,
    /// inPlace | relocate | chunk | exe | excluded | blocked
    pub action: String,
    pub bytes: u64,
    pub from: Span,
    #[serde(default)]
    pub to: Option<Span>,
    #[serde(default)]
    pub entries: Vec<u16>,
    /// Sectors whose bytes changed.
    #[serde(default)]
    pub sectors_changed: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub lba: u32,
    pub sectors: u32,
}

struct Writer<'a> {
    out: Out<'a>,
    report: WriteReport,
    cancel: &'a AtomicBool,
}

impl Writer<'_> {
    /// Writes `data` over `sectors` sectors from `lba` (zero-padded), with
    /// EDC/ECC recomputed. Returns how many sectors changed.
    fn extent(&mut self, lba: u32, data: &[u8], sectors: u32, subs: Subs, mut tick: impl FnMut()) -> Res<u32> {
        let mut changed = 0;
        for i in 0..sectors {
            let at = lba + i;
            let from = (i as usize * USER_DATA).min(data.len());
            let chunk = &data[from..(from + USER_DATA).min(data.len())];
            let existing = self.out.read(at)?;
            let mut raw = match subs {
                Subs::Keep => {
                    let raw = existing.ok_or_else(|| BuildError::new("BUILD_PAST_END", format!("sector {at} is past the end of the image")))?;
                    if raw[..12] != cdrom::SYNC {
                        return Err(BuildError::new("BUILD_BAD_SECTOR", format!("sector {at} has no Mode 2 sync pattern")));
                    }
                    if cdrom::is_form2(&raw) {
                        return Err(BuildError::new("BUILD_FORM2_TARGET", format!("sector {at} is a Form 2 sector, which the writer can't produce")));
                    }
                    raw
                }
                Subs::Fresh { body, last } => cdrom::blank(at, if i + 1 == sectors { last } else { body }),
            };
            cdrom::encode_form1(&mut raw, chunk);
            if existing.as_ref() != Some(&raw) {
                self.out.write(at, &raw)?;
                changed += 1;
            }
            tick();
            if i.is_multiple_of(32) {
                check(self.cancel)?;
            }
        }
        match self.report.form1.last_mut() {
            Some((l, n)) if *l + *n == lba => *n += sectors,
            _ => self.report.form1.push((lba, sectors)),
        }
        self.report.sectors_encoded += sectors;
        self.report.sectors_changed += changed;
        Ok(changed)
    }
}

impl Writer<'_> {
    /// Writes a movie's 2336-byte payloads from `lba`, each with a fresh
    /// sync, header and checksums. Returns how many sectors changed.
    fn mode2_extent(&mut self, lba: u32, data: &[u8], sectors: u32, mut tick: impl FnMut()) -> Res<u32> {
        let mut changed = 0;
        for i in 0..sectors {
            let at = lba + i;
            let raw = cdrom::from_payload(at, &data[i as usize * cdrom::PAYLOAD..]);
            if self.out.read(at)?.as_ref() != Some(&raw) {
                self.out.write(at, &raw)?;
                changed += 1;
            }
            tick();
            if i.is_multiple_of(32) {
                check(self.cancel)?;
            }
        }
        self.report.mode2.push((lba, sectors));
        self.report.sectors_encoded += sectors;
        self.report.sectors_changed += changed;
        Ok(changed)
    }
}

/// The subheaders a relocated file's sectors get: its first sector's (with
/// EOF/EOR cleared) on the body and its last sector's on the last.
fn relocated_subs(src: &Source, from: Extent) -> Res<Subs> {
    // A new file (a base's empty spare entry) has no sectors to copy: plain data sectors.
    if from.sectors == 0 {
        return Ok(Subs::Fresh { body: cdrom::SUB_DATA, last: cdrom::SUB_LAST });
    }
    let first = cdrom::subheader(&src.image.read_raw(from.lba)?);
    let last = cdrom::subheader(&src.image.read_raw(from.lba + from.sectors.max(1) - 1)?);
    let mut body = first;
    body[2] &= !(cdrom::SM_EOF | cdrom::SM_EOR);
    Ok(Subs::Fresh { body, last })
}

fn staged_bytes<'s>(staged: &'s [Staged], id: &str) -> Res<&'s [u8]> {
    staged
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.bytes.as_slice())
        .ok_or_else(|| BuildError::new("BUILD_PLAN_STALE", format!("{id} is in the plan but not staged")).on(id))
}

/// Copies the source to `partial` and writes the plan into it (steps 1–4).
/// `partial` must not exist. On error the caller deletes it.
pub fn write_image(
    src: &Source,
    plan: &Plan,
    staged: &[Staged],
    partial: &Path,
    cancel: &AtomicBool,
    events: &mut dyn FnMut(BuildEvent),
) -> Res<WriteReport> {
    // 1. Copy. `fs::copy` clones on APFS, so this is usually instant.
    let source_path = src.image.path();
    events(BuildEvent::Progress { step: STEP_COPY, done: 0, total: src.image.size(), detail: None });
    fs::copy(source_path, partial).map_err(|e| io(partial, e))?;
    make_writable(partial)?;
    let file = OpenOptions::new().read(true).write(true).open(partial).map_err(|e| io(partial, e))?;
    let len = file.metadata().map_err(|e| io(partial, e))?.len();
    if len != src.image.size() {
        return Err(BuildError::new("BUILD_COPY_SHORT", format!("the copy is {len} bytes, not {}", src.image.size())));
    }
    check(cancel)?;
    events(BuildEvent::StepDone { step: STEP_COPY, detail: Some(format!("{} sectors", grouped(src.image.sectors() as u64))) });

    let out = Out { sink: Sink::File { file, path: partial.to_path_buf() }, sectors: src.image.sectors() };
    let (report, out) = write_plan(src, plan, staged, out, cancel, events)?;
    if let Sink::File { file, .. } = &out.sink {
        file.sync_all().map_err(|e| io(partial, e))?;
    }
    Ok(report)
}

/// The sectors a build of `plan` writes, kept in memory instead of an image
/// file: each changed sector (EDC/ECC recomputed) by LBA, and the image's
/// length in sectors. The in-process emulator reads these over the source.
pub fn overlay(src: &Source, plan: &Plan, staged: &[Staged]) -> Res<(BTreeMap<u32, Raw>, u32)> {
    let never = AtomicBool::new(false);
    let out = Out { sink: Sink::Memory { base: src.image, written: BTreeMap::new() }, sectors: src.image.sectors() };
    let (_, out) = write_plan(src, plan, staged, out, &never, &mut |_| {})?;
    let sectors = out.sectors;
    match out.sink {
        Sink::Memory { written, .. } => Ok((written, sectors)),
        Sink::File { .. } => unreachable!("overlay writes to memory"),
    }
}

/// Steps 2–4 of a build into `out`: files, EXE and table, postgap and volume size.
fn write_plan<'a>(
    src: &Source,
    plan: &Plan,
    staged: &[Staged],
    out: Out<'a>,
    cancel: &'a AtomicBool,
    events: &mut dyn FnMut(BuildEvent),
) -> Res<(WriteReport, Out<'a>)> {
    let mut w = Writer {
        out,
        report: WriteReport { volume_from: src.image.sectors(), volume_to: src.image.sectors(), ..Default::default() },
        cancel,
    };

    // 2. Files, in plan order (relocations land in cursor order).
    let total: u64 = plan.items.iter().filter(|i| i.live() && i.id != "exe").map(|i| i.written_sectors() as u64).sum();
    let mut done = 0u64;
    let mut files = 0;
    for item in &plan.items {
        if item.id == "exe" {
            continue;
        }
        let from = Span { lba: item.lba, sectors: item.alloc };
        if !item.live() {
            let action = if item.included { "blocked" } else { "excluded" };
            w.report.records.push(PlanRecord { id: item.id.clone(), action: action.into(), bytes: item.bytes, from, to: None, entries: item.entries.clone(), sectors_changed: 0 });
            continue;
        }
        let data = staged_bytes(staged, &item.id)?;
        let mut tick = |d: &mut u64| {
            *d += 1;
            if (*d).is_multiple_of(16) || *d == total {
                events(BuildEvent::Progress { step: STEP_WRITE, done: *d, total, detail: Some(item.id.clone()) });
            }
        };
        let (action, at, sectors, subs) = match item.verdict {
            Verdict::Relocate => {
                let at = item.new_lba.expect("live relocations have a new LBA");
                ("relocate", at, item.need, relocated_subs(src, Extent { lba: item.lba, sectors: item.alloc })?)
            }
            // A new area past the retail end: fresh world-archive sectors (data, the last EOF|EOR).
            _ if item.chunk.is_some_and(crate::world::is_new_area) => {
                ("newArea", item.lba, item.alloc, Subs::Fresh { body: cdrom::SUB_DATA, last: cdrom::SUB_LAST })
            }
            _ if item.chunk.is_some() => ("chunk", item.lba, item.alloc, Subs::Keep),
            _ => ("inPlace", item.lba, item.alloc, Subs::Keep),
        };
        let changed = if item.raw {
            w.mode2_extent(at, data, item.need, || tick(&mut done))?
        } else {
            w.extent(at, data, sectors, subs, || tick(&mut done))?
        };
        let sectors = if item.raw { item.need } else { sectors };
        let note = match action {
            "relocate" => " RELOCATED",
            "chunk" => " (world archive)",
            "newArea" => " (new area)",
            _ => "",
        };
        w.report.log.push(format!(
            "wrote {} {} bytes over {} sector(s) at LBA {at}{note}, {changed} changed",
            item.id, item.bytes, sectors
        ));
        w.report.records.push(PlanRecord {
            id: item.id.clone(),
            action: action.into(),
            bytes: item.bytes,
            from,
            to: (action == "relocate").then_some(Span { lba: at, sectors }),
            entries: item.entries.clone(),
            sectors_changed: changed,
        });
        files += 1;
    }
    events(BuildEvent::StepDone {
        step: STEP_WRITE,
        detail: Some(format!("{files} file{} · {} sectors", if files == 1 { "" } else { "s" }, grouped(total))),
    });

    // 3. EXE: the user's edits and the table rewrites, sector by sector.
    for e in &plan.exe_entries {
        w.report.log.push(format!("file table: {} (#{}) -> LBA {} ({} sectors)", e.id, e.global, e.lba, e.sectors));
    }
    let exe_total = plan.exe_sectors.len() as u64;
    for (k, &lba) in plan.exe_sectors.iter().enumerate() {
        let n = (lba - src.exe.lba) as usize;
        let data = &plan.exe_bytes[n * USER_DATA..((n + 1) * USER_DATA).min(plan.exe_bytes.len())];
        let changed = w.extent(lba, data, 1, Subs::Keep, || {})?;
        w.report.exe_sectors_written += changed;
        events(BuildEvent::Progress { step: STEP_TABLE, done: k as u64 + 1, total: exe_total, detail: None });
    }
    if let Some(item) = plan.items.iter().find(|i| i.id == "exe") {
        let from = Span { lba: item.lba, sectors: item.alloc };
        let action = if item.live() { "exe" } else if item.included { "blocked" } else { "excluded" };
        w.report.records.push(PlanRecord {
            id: "exe".into(),
            action: action.into(),
            bytes: item.bytes,
            from,
            to: None,
            entries: Vec::new(),
            sectors_changed: if item.live() { item.need } else { 0 },
        });
        if item.live() {
            w.report.log.push(format!("wrote exe: {} sector(s) of SLUS_008.11 edited", item.need));
        }
    }
    if !plan.exe_sectors.is_empty() {
        w.report.log.push(format!(
            "wrote SLUS_008.11 sector(s) at LBA {}",
            plan.exe_sectors.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
        ));
    }
    events(BuildEvent::StepDone {
        step: STEP_TABLE,
        detail: Some(match (plan.exe_entries.len(), plan.exe_sectors.len()) {
            (0, 0) => "unchanged".to_string(),
            (e, s) => format!("{e} entr{} · {s} EXE sector{}", if e == 1 { "y" } else { "ies" }, if s == 1 { "" } else { "s" }),
        }),
    });
    check(cancel)?;

    // 4. Postgap and the PVD's volume space size (LE at 80, BE at 84).
    if plan.new_volume > w.out.sectors {
        let start = plan.cursor_end.max(w.out.sectors);
        for lba in start..plan.new_volume {
            w.out.write(lba, &cdrom::postgap(lba))?;
        }
        if plan.new_volume > start {
            w.report.postgap = Some((start, plan.new_volume - start));
            w.report.log.push(format!("postgap: {} sector(s) at LBA {start}", plan.new_volume - start));
        }
    }
    if w.out.sectors != src.image.sectors() {
        let mut pvd = src.image.read_user(PVD_LBA)?;
        pvd[80..84].copy_from_slice(&w.out.sectors.to_le_bytes());
        pvd[84..88].copy_from_slice(&w.out.sectors.to_be_bytes());
        w.extent(PVD_LBA, &pvd, 1, Subs::Keep, || {})?;
        w.report.log.push(format!(
            "volume size: {} -> {} sectors (+{:.1} MB)",
            src.image.sectors(),
            w.out.sectors,
            (w.out.sectors - src.image.sectors()) as f64 * RAW_SECTOR as f64 / 1e6
        ));
    }
    w.report.volume_to = w.out.sectors;
    events(BuildEvent::StepDone {
        step: STEP_VOLUME,
        detail: Some(if w.report.volume_to == w.report.volume_from {
            "unchanged".to_string()
        } else {
            format!("{} → {}", grouped(w.report.volume_from as u64), grouped(w.report.volume_to as u64))
        }),
    });
    Ok((w.report, w.out))
}

/* ───────────────────────── Verifying ───────────────────────── */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub crc32: String,
    pub sha1: String,
    pub bytes: u64,
    pub sectors: u32,
    pub files_checked: u32,
    pub sectors_checked: u32,
}

/// Reopens the output read-only and checks it (PRD 15 §5.5): every written
/// file reads back as planned, every written sector passes EDC/ECC with the
/// right header, the file table equals the source's plus the planned
/// rewrites (alias groups consistent), and the PVD names the image size.
/// Ends with the output's CRC32 and SHA-1.
pub fn verify_image(
    path: &Path,
    src: &Source,
    plan: &Plan,
    staged: &[Staged],
    report: &WriteReport,
    cancel: &AtomicBool,
    events: &mut dyn FnMut(BuildEvent),
) -> Res<VerifyReport> {
    let fail = |m: String| BuildError::new("BUILD_VERIFY_FAILED", m);
    let img = DiscImage::open(path)?;
    if img.sectors() != plan.new_volume {
        return Err(fail(format!("the image has {} sectors, not the planned {}", img.sectors(), plan.new_volume)));
    }

    // Files read back as planned (zero-padded to what was written).
    let mut files = 0;
    for item in plan.items.iter().filter(|i| i.live() && i.id != "exe") {
        let data = staged_bytes(staged, &item.id)?;
        let (lba, sectors) = match item.new_lba {
            Some(l) => (l, item.need),
            None => (item.lba, item.alloc),
        };
        if item.raw {
            // A movie: every sector is its payload with the checksums the writer computed.
            for i in 0..item.need {
                let raw = img.read_raw(lba + i)?;
                if raw != cdrom::from_payload(lba + i, &data[i as usize * cdrom::PAYLOAD..]) {
                    return Err(fail(format!("{} sector {i} doesn't read back as written at LBA {}", item.id, lba + i)).on(&item.id));
                }
            }
            files += 1;
            continue;
        }
        let back = img.read_sectors(lba, sectors)?;
        if back[..data.len()] != *data || back[data.len()..].iter().any(|&b| b != 0) {
            return Err(fail(format!("{} doesn't read back as written at LBA {lba}", item.id)).on(&item.id));
        }
        files += 1;
        check(cancel)?;
    }

    // Every written sector is a valid Form 1 sector at its LBA.
    let mut checked = 0;
    for &(lba, n) in &report.form1 {
        for at in lba..lba + n {
            if let Err(fault) = cdrom::check_form1(&img.read_raw(at)?, at) {
                return Err(fail(format!("sector {at} fails its check ({fault:?})")));
            }
            checked += 1;
        }
    }
    for &(lba, n) in &report.mode2 {
        for at in lba..lba + n {
            if let Err(fault) = cdrom::check_mode2(&img.read_raw(at)?, at) {
                return Err(fail(format!("movie sector {at} fails its check ({fault:?})")));
            }
            checked += 1;
        }
    }
    if let Some((lba, n)) = report.postgap {
        for at in lba..lba + n {
            if img.read_raw(at)? != cdrom::postgap(at) {
                return Err(fail(format!("postgap sector {at} isn't blank")));
            }
            checked += 1;
        }
    }

    // The EXE and its file table.
    let exe = PsxExe::from_disc(&img)?;
    if exe.lba != src.exe.lba || exe.bytes != plan.exe_bytes {
        return Err(fail("SLUS_008.11 doesn't read back as planned".into()));
    }
    let table = FileTable::parse(&exe, img.sectors())?;
    for (out, before) in table.entries.iter().zip(&src.table.entries) {
        let want = plan.exe_entries.iter().find(|w| w.global == out.global).map_or(before.raw, |w| w.raw);
        if out.raw != want {
            return Err(fail(format!("table entry #{} ({}) is {:02x?}, expected {:02x?}", out.global, out.id, out.raw, want)));
        }
    }
    for item in plan.items.iter().filter(|i| i.live() && i.verdict == Verdict::Relocate) {
        let extents: Vec<Extent> = item.entries.iter().map(|&g| table.entries[g as usize].extent).collect();
        if extents.windows(2).any(|w| w[0] != w[1]) {
            return Err(fail(format!("the alias group of {} points at different extents", item.id)).on(&item.id));
        }
    }
    let vol = iso9660::read_volume(&img)?;
    if vol.volume_space_size != img.sectors() {
        return Err(fail(format!("the PVD says {} sectors; the image has {}", vol.volume_space_size, img.sectors())));
    }

    // CRC32 + SHA-1 of the whole output.
    let fp = identity::fingerprint(&img, |done, total| {
        events(BuildEvent::Progress { step: STEP_VERIFY, done, total, detail: Some("CRC32 · SHA-1".into()) });
        !cancel.load(Ordering::Relaxed)
    })?
    .ok_or_else(BuildError::cancelled)?;
    events(BuildEvent::StepDone {
        step: STEP_VERIFY,
        detail: Some(format!("{files} file{} · {} sectors · CRC32 {}", if files == 1 { "" } else { "s" }, grouped(checked as u64), fp.crc32)),
    });
    Ok(VerifyReport { crc32: fp.crc32, sha1: fp.sha1, bytes: fp.size, sectors: fp.sectors, files_checked: files, sectors_checked: checked })
}

/* ───────────────────────── BPS ───────────────────────── */

pub struct BpsMade {
    pub patch: Vec<u8>,
    pub source_crc32: u32,
    pub target_crc32: u32,
    /// Target bytes carried over from the source / stored in the patch.
    pub same: u64,
    pub new: u64,
}

fn read_full(f: &mut File, buf: &mut [u8], path: &Path) -> Res<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io(path, e)),
        }
    }
    Ok(n)
}

/// For a target sector past the source's end, a source sector it resembles:
/// a relocated file's sector at the same place in its old extent (only the
/// header differs, as EDC/ECC skip it), or the source's last sector for the
/// postgap and gaps (retail ends with 150 blank postgap sectors).
pub fn bps_hint(plan: &Plan) -> impl Fn(u32) -> Option<u32> + '_ {
    move |t| {
        for i in plan.items.iter().filter(|i| i.live() && i.verdict == Verdict::Relocate) {
            let at = i.new_lba?;
            if t >= at && t < at + i.need {
                return (t - at < i.alloc).then_some(i.lba + t - at);
            }
        }
        // The postgap, and the blank gap before a new area's fixed LBA.
        let new_area = plan.items.iter().any(|i| i.live() && i.chunk.is_some_and(crate::world::is_new_area) && t >= i.lba && t < i.lba + i.alloc);
        (t >= plan.source_sectors && t < plan.new_volume && !new_area && plan.source_sectors > 0).then_some(plan.source_sectors - 1)
    }
}

/// A BPS that turns `source` into `target`: a same-offset diff streamed in
/// sector-aligned chunks; sectors past the source's end are diffed against the
/// source sector `hint(lba)` names, if any (SourceCopy). Checked by applying it
/// to the source and comparing the result's size and CRC32 with the target's.
pub fn make_bps(
    source: &Path,
    target: &Path,
    metadata: &str,
    hint: &dyn Fn(u32) -> Option<u32>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Res<BpsMade> {
    const CHUNK: usize = RAW_SECTOR * 512;
    let mut fs_ = File::open(source).map_err(|e| io(source, e))?;
    let mut ft = File::open(target).map_err(|e| io(target, e))?;
    let s_size = fs_.metadata().map_err(|e| io(source, e))?.len();
    let t_size = ft.metadata().map_err(|e| io(target, e))?.len();
    let mut enc = bps::Encoder::new(s_size, t_size, metadata);
    let (mut sc, mut tc) = (crc32fast::Hasher::new(), crc32fast::Hasher::new());
    let (mut sb, mut tb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let src_file = File::open(source).map_err(|e| io(source, e))?;
    let mut like = [0u8; RAW_SECTOR];
    let mut off = 0u64;
    loop {
        let tn = read_full(&mut ft, &mut tb, target)?;
        if tn == 0 {
            break;
        }
        let sn = read_full(&mut fs_, &mut sb[..tn], source)?;
        sc.update(&sb[..sn]);
        tc.update(&tb[..tn]);
        enc.feed(&sb[..sn], &tb[..sn]);
        // Past the source's end: sector by sector, against a similar source sector.
        let mut at = sn;
        while at < tn {
            let end = (at + RAW_SECTOR).min(tn);
            let lba = ((off + at as u64) / RAW_SECTOR as u64) as u32;
            // A hint past the source's end (a base's own sectors, when the patch is made
            // against retail) falls back to its last sector, blank postgap on retail.
            let last = (s_size / RAW_SECTOR as u64).checked_sub(1).map(|l| l as u32);
            match hint(lba).and_then(|s| last.map(|l| s.min(l))) {
                Some(s) => {
                    pread(&src_file, &mut like, s as u64 * RAW_SECTOR as u64).map_err(|e| io(source, e))?;
                    enc.feed_from(&like, s as u64 * RAW_SECTOR as u64, &tb[at..end]);
                }
                None => enc.feed(&[], &tb[at..end]),
            }
            at = end;
        }
        off += tn as u64;
        progress(off, t_size * 2);
        check(cancel)?;
    }
    // A target shorter than the source (never for builds): finish the source CRC.
    loop {
        let sn = read_full(&mut fs_, &mut sb, source)?;
        if sn == 0 {
            break;
        }
        sc.update(&sb[..sn]);
    }
    let (same, new) = (enc.same, enc.new);
    let (source_crc32, target_crc32) = (sc.finalize(), tc.finalize());
    let patch = enc.finish(source_crc32, target_crc32).map_err(|m| BuildError::new("BPS_FAILED", m))?;

    // Verify by applying (into a CRC-only sink).
    let mut applied = 0u64;
    let (size, crc) = bps::apply_crc(&patch, |at, buf| {
        pread(&src_file, buf, at).map_err(|e| e.to_string())?;
        applied += buf.len() as u64;
        progress(t_size + applied.min(t_size), t_size * 2);
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        Ok(())
    })
    .map_err(|m| if cancel.load(Ordering::Relaxed) { BuildError::cancelled() } else { BuildError::new("BPS_VERIFY_FAILED", m) })?;
    if size != t_size || crc != target_crc32 {
        return Err(BuildError::new("BPS_VERIFY_FAILED", "applying the new patch to the source doesn't reproduce the build"));
    }
    Ok(BpsMade { patch, source_crc32, target_crc32, same, new })
}

/* ───────────────────────── Builds in a project ───────────────────────── */

/// What the UI asks for (screen 15, Output).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildRequest {
    /// File name stem of the outputs (`english-v0.4`).
    pub name: String,
    /// Where the outputs go; `None` = the build's own folder `builds/NNNN/`.
    #[serde(default)]
    pub out_dir: Option<String>,
    /// Staged ids left out (built from the source's bytes).
    #[serde(default)]
    pub excluded: Vec<String>,
    /// Keep the `.bin` + `.cue`.
    pub image: bool,
    /// Write `<name>.bps` + a README.
    pub bps: bool,
    pub postgap: bool,
    /// On a base: the first chunk id the project's new areas may take.
    #[serde(default)]
    pub new_area_first: Option<u32>,
    /// On a base: the verified retail image the BPS is made against, so
    /// players apply one patch to their own disc. `None` = the source image.
    #[serde(skip)]
    pub bps_source: Option<DiscInput>,
}

/// The source image's identity, as the open disc knows it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscInput {
    pub path: String,
    pub sectors: u32,
    #[serde(default)]
    pub crc32: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    /// Matches the Redump dump (CRC32 cdc72c62): hashed when the disc was
    /// opened, or the project's record when the size matches. BPS needs it,
    /// and re-checks the CRC32 as it reads the source.
    pub verified: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInputs {
    pub app: String,
    /// The pre-build snapshot, relative to the project.
    #[serde(default)]
    pub snapshot: Option<String>,
    pub manifest_rev: u64,
    pub disc: DiscInput,
    /// `bin+cue`, `bps`.
    pub outputs: Vec<String>,
    pub postgap: bool,
    pub out_dir: String,
    #[serde(default)]
    pub excluded: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFile {
    pub path: String,
    pub bytes: u64,
    pub crc32: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildOutputs {
    /// The image (`None` when only the BPS was kept).
    #[serde(default)]
    pub bin: Option<OutputFile>,
    #[serde(default)]
    pub cue: Option<String>,
    #[serde(default)]
    pub bps: Option<OutputFile>,
    #[serde(default)]
    pub readme: Option<String>,
    /// The built image's size and checksums (also when it wasn't kept).
    pub image_bytes: u64,
    pub crc32: String,
    pub sha1: String,
    pub sectors_appended: u32,
    pub entries_rewritten: u32,
    pub exe_sectors_written: u32,
    pub sectors_encoded: u32,
    pub sectors_changed: u32,
    pub volume_from: u32,
    pub volume_to: u32,
    pub files_verified: u32,
    pub duration_ms: u64,
}

/// `builds/NNNN/build.json` (PRD 03 §4.5).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildRecord {
    pub schema: u32,
    pub number: u32,
    pub label: String,
    /// running | built | failed | cancelled (booted / boot-failed come with the emulator bridge)
    pub status: String,
    pub started: String,
    #[serde(default)]
    pub finished: Option<String>,
    /// The build's folder (absolute).
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub inputs: BuildInputs,
    #[serde(default)]
    pub plan: Vec<PlanRecord>,
    #[serde(default)]
    pub result: Option<BuildOutputs>,
    #[serde(default)]
    pub error: Option<BuildError>,
    /// The boot test (screen 16). Always null for now: it needs the PCSX-Redux bridge.
    #[serde(default)]
    pub test: Option<serde_json::Value>,
}

/// The next build number: one past the highest `builds/NNNN`.
pub fn next_number(project_root: &Path) -> u32 {
    fs::read_dir(project_root.join("builds"))
        .map(|rd| rd.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok()).max().unwrap_or(0))
        .unwrap_or(0)
        + 1
}

/// Every `builds/*/build.json` of a project, newest first.
pub fn list(project_root: &Path) -> Vec<BuildRecord> {
    let mut out: Vec<BuildRecord> = fs::read_dir(project_root.join("builds"))
        .map(|rd| {
            rd.filter_map(|e| {
                let dir = e.ok()?.path();
                let text = fs::read_to_string(dir.join("build.json")).ok()?;
                let mut r: BuildRecord = serde_json::from_str(&text).ok()?;
                r.dir = dir.display().to_string();
                Some(r)
            })
            .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|r| std::cmp::Reverse(r.number));
    out
}

/// A file name stem that is safe on every OS.
pub fn file_stem(name: &str, number: u32) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_control() || r#"/\:*?"<>|"#.contains(c) { '-' } else { c })
        .collect();
    let s = s.trim().trim_start_matches('.').trim().to_string();
    if s.is_empty() {
        format!("build-{number}")
    } else {
        s
    }
}

fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn readme(title: &str, patch: &str, stem: &str, project: &str, number: u32, source: (u64, u32), target: (u64, u32)) -> String {
    format!(
        "{title}\n{underline}\n\n\
A patch for Guardian's Crusade (USA, PlayStation, SLUS-00811), made with Living Tool\n\
(project \"{project}\", build #{number}). You need your own copy of the game as a disc\n\
image: the patch holds none of the game, only what the mod changes.\n\n\
1. CHECK YOUR IMAGE. It must be ONE .bin file (one track) of exactly\n   {s_size} bytes with CRC32 {s_crc:08x} (the Redump dump of the USA disc).\n   If yours differs, stop: the patched disc would not work.\n\n\
2. PATCH IT with any BPS patcher:\n   - Rom Patcher JS, in a browser: https://www.marcrobledo.com/RomPatcher.js/\n   - Floating IPS:  flips --apply {patch} \"your game.bin\" \"{stem}.bin\"\n   - Living Tool:   File patcher > Apply a patch\n   The result is {t_size} bytes, CRC32 {t_crc:08x}.\n\n\
3. MAKE A .CUE named \"{stem}.cue\" next to the patched .bin:\n\n     FILE \"{stem}.bin\" BINARY\n       TRACK 01 MODE2/2352\n         INDEX 01 00:00:00\n\n\
4. PLAY. Open the .cue in your emulator (DuckStation, PCSX-Redux, ...).\n",
        underline = "=".repeat(title.chars().count()),
        s_size = grouped(source.0),
        s_crc = source.1,
        t_size = grouped(target.0),
        t_crc = target.1,
    )
}

pub fn cue_sheet(bin_name: &str) -> String {
    format!("FILE \"{bin_name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n")
}

/// Runs a whole build into the project (PRD 15 §5.3): numbers it
/// (`builds/NNNN/`), takes a pre-build snapshot, plans, writes, verifies,
/// renames, writes the `.cue` and optionally the BPS + README, then records
/// `build.json`, `log.txt` and the `build.started` / `build.finished`
/// history events. Returns the record; `status` says how it ended. On
/// failure or cancel nothing but `build.json` and `log.txt` is left behind.
pub fn run(
    project: &Project,
    src: &Source,
    disc: DiscInput,
    staged: &[Staged],
    req: &BuildRequest,
    cancel: &AtomicBool,
    events: &mut dyn FnMut(BuildEvent),
) -> BuildRecord {
    let t0 = Instant::now();
    let number = next_number(&project.root);
    let dir = project.root.join("builds").join(format!("{number:04}"));
    let stem = file_stem(&req.name, number);
    let out_dir = req.out_dir.as_ref().map(PathBuf::from).unwrap_or_else(|| dir.clone());
    let mut outputs = Vec::new();
    if req.image {
        outputs.push("bin+cue".to_string());
    }
    if req.bps {
        outputs.push("bps".to_string());
    }
    let mut record = BuildRecord {
        schema: SCHEMA,
        number,
        label: stem.clone(),
        status: "running".into(),
        started: now_rfc3339(),
        dir: dir.display().to_string(),
        inputs: BuildInputs {
            app: APP_VERSION.into(),
            snapshot: None,
            manifest_rev: project.manifest.rev,
            disc,
            outputs,
            postgap: req.postgap,
            out_dir: out_dir.display().to_string(),
            excluded: req.excluded.clone(),
        },
        ..Default::default()
    };
    let mut steps: Vec<String> = STEPS.iter().map(|s| s.to_string()).collect();
    if !req.bps {
        steps.truncate(STEP_BPS);
    }
    events(BuildEvent::Started { title: format!("Building {stem}"), steps });

    let mut log: Vec<String> = vec![format!("build #{number} {stem} · Living Tool {APP_VERSION} · {}", record.started)];
    let mut created: Vec<PathBuf> = Vec::new();
    let result = (|| -> Res<StepsOk> {
        fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        write_json(&dir.join("build.json"), &record)?;
        let mut fields = serde_json::Map::new();
        fields.insert("number".into(), number.into());
        fields.insert("status".into(), "running".into());
        fields.insert("label".into(), stem.clone().into());
        project.append_history("build.started", fields)?;
        run_steps(project, src, staged, req, &mut record, &out_dir, &stem, &mut created, &mut log, cancel, events, t0)
    })();

    record.finished = Some(now_rfc3339());
    match result {
        Ok((outputs, plan)) => {
            record.status = "built".into();
            record.plan = plan;
            record.result = Some(outputs);
        }
        Err(e) => {
            // No half-written image: remove everything this build created.
            for p in created.iter().rev() {
                let _ = fs::remove_file(p);
            }
            log.push(format!("{} {}", if e.is_cancel() { "CANCELLED" } else { "FAILED" }, e));
            record.status = if e.is_cancel() { "cancelled" } else { "failed" }.into();
            record.error = Some(e);
        }
    }
    let _ = write_json(&dir.join("build.json"), &record);
    let _ = write_atomic(&dir.join("log.txt"), (log.join("\n") + "\n").as_bytes());
    let mut fields = serde_json::Map::new();
    fields.insert("number".into(), number.into());
    fields.insert("status".into(), record.status.clone().into());
    fields.insert("label".into(), stem.into());
    if let Some(e) = &record.error {
        fields.insert("error".into(), serde_json::to_value(e).unwrap_or_default());
    }
    let _ = project.append_history("build.finished", fields);
    record
}

type StepsOk = (BuildOutputs, Vec<PlanRecord>);

#[allow(clippy::too_many_arguments)]
fn run_steps(
    project: &Project,
    src: &Source,
    staged: &[Staged],
    req: &BuildRequest,
    record: &mut BuildRecord,
    out_dir: &Path,
    stem: &str,
    created: &mut Vec<PathBuf>,
    log: &mut Vec<String>,
    cancel: &AtomicBool,
    events: &mut dyn FnMut(BuildEvent),
    t0: Instant,
) -> Res<StepsOk> {
    // 0. Prepare: plan, refusals, base hashes, output paths, snapshot.
    if !src.exe.release.editable() {
        let title = src.exe.release.info().title;
        return Err(BuildError::new("RELEASE_READ_ONLY", format!("{title} opens read-only: Living Tool builds the USA release only, for now")));
    }
    if !req.image && !req.bps {
        return Err(BuildError::new("BUILD_NO_OUTPUT", "choose a disc image, a BPS patch or both"));
    }
    let bps_verified = req.bps_source.as_ref().map_or(record.inputs.disc.verified, |d| d.verified);
    if req.bps && !bps_verified {
        return Err(BuildError::new("BUILD_BPS_UNVERIFIED", "a BPS needs the verified Redump dump as its source"));
    }
    let plan = plan(src, staged, |id| !req.excluded.iter().any(|x| x == id), PlanOptions { postgap: req.postgap, new_area_first: req.new_area_first.unwrap_or(crate::world::NEW_AREA_FIRST) });
    if let Some(i) = plan.items.iter().find(|i| i.included && i.verdict == Verdict::Blocked) {
        let e = i.refusal.clone().unwrap_or_else(|| BuildError::new("BUILD_BLOCKED", format!("{} can't be built", i.id)));
        return Err(e.on(&i.id));
    }
    if plan.nothing {
        return Err(BuildError::new("BUILD_NOTHING", "Nothing to build: your project matches the retail disc."));
    }
    if plan.new_volume > plan.capacity {
        return Err(BuildError::new(
            "BUILD_CAPACITY",
            format!("the image would be {} sectors; a 74-minute CD-R holds {}", grouped(plan.new_volume as u64), grouped(plan.capacity as u64)),
        ));
    }
    for item in plan.items.iter().filter(|i| i.live()) {
        let base = staged.iter().find(|s| s.id == item.id).and_then(|s| s.base_sha256.as_deref());
        if let Some(base) = base {
            if sha256_hex(&src.retail(&item.id)?) != base {
                let msg = format!("{}'s bytes on the disc aren't the ones the edit was made against", item.id);
                return Err(BuildError::new("BASE_MISMATCH", msg).on(&item.id));
            }
        }
    }
    fs::create_dir_all(out_dir).map_err(|e| io(out_dir, e))?;
    let bin = out_dir.join(format!("{stem}.bin"));
    let cue = out_dir.join(format!("{stem}.cue"));
    let bps_path = out_dir.join(format!("{stem}.bps"));
    let readme_path = out_dir.join(format!("{stem}-README.txt"));
    let partial = out_dir.join(format!("{stem}.bin.partial"));
    let source_path = src.image.path();
    let same_file = |p: &Path| fs::canonicalize(p).ok().is_some_and(|c| fs::canonicalize(source_path).ok().is_some_and(|s| s == c));
    if same_file(&bin) || same_file(&partial) {
        return Err(BuildError::new("BUILD_OUT_IS_SOURCE", "the output would overwrite the source image"));
    }
    let mut targets = vec![&partial];
    if req.image {
        targets.extend([&bin, &cue]);
    }
    if req.bps {
        targets.extend([&bps_path, &readme_path]);
    }
    if let Some(p) = targets.iter().find(|p| p.exists()) {
        return Err(BuildError::new("BUILD_OUT_EXISTS", format!("{} already exists; choose another name or folder", p.display())));
    }
    // The snapshot is a project file, not a build output: it stays if the build fails.
    let snap = project.snapshot(&format!("pre-build #{}", record.number), "pre-build")?;
    let snapshot = format!("snapshots/{}", snap.file);
    log.push(format!("pre-build snapshot: {snapshot}"));
    record.inputs.snapshot = Some(snapshot);
    for item in &plan.items {
        let what = match (item.included, item.verdict) {
            (false, _) => "excluded (built from the source)".to_string(),
            (true, Verdict::Blocked) => "blocked".to_string(),
            (true, Verdict::Relocate) => format!("OVERFLOW -> relocate to LBA {}", item.new_lba.unwrap_or(0)),
            (true, Verdict::InPlace) => "ok (in place)".to_string(),
        };
        log.push(format!("plan {:<10} {:>9} B  lba {:>6}  alloc {:>5}  need {:>5}  {what}", item.id, item.bytes, item.lba, item.alloc, item.need));
    }
    check(cancel)?;
    let live = plan.items.iter().filter(|i| i.live()).count();
    events(BuildEvent::StepDone {
        step: STEP_PREPARE,
        detail: Some(format!("{live} file{} · {} sectors", if live == 1 { "" } else { "s" }, grouped(plan.edc_sectors as u64))),
    });

    // 1–4. Copy and write.
    created.push(partial.clone());
    let report = write_image(src, &plan, staged, &partial, cancel, events)?;
    log.extend(report.log.iter().cloned());

    // 5. Verify.
    let verify = verify_image(&partial, src, &plan, staged, &report, cancel, events)?;
    log.push(format!(
        "verified {}/{} file(s), {} sector(s) re-checked; CRC32 {} SHA-1 {}",
        verify.files_checked,
        verify.files_checked,
        verify.sectors_checked,
        verify.crc32,
        verify.sha1
    ));

    // 6. BPS (from the verified partial, before the rename).
    let mut bps_out = None;
    let mut readme_out = None;
    if req.bps {
        let meta = serde_json::json!({
            "tool": "Living Tool",
            "version": APP_VERSION,
            "project": project.file.name,
            "build": record.number,
            "label": stem,
            "source": match req.bps_source.as_ref() {
                Some(d) => serde_json::json!({ "crc32": d.crc32, "sectors": d.sectors, "base": src.image.path().display().to_string() }),
                None => serde_json::json!({ "bytes": src.image.size(), "crc32": record.inputs.disc.crc32 }),
            },
        })
        .to_string();
        // Against retail when the project is on a base (the base's sectors are mostly
        // retail's, so the same sector hints apply).
        let bps_from = req.bps_source.as_ref().map(|d| Path::new(&d.path)).unwrap_or(source_path);
        let made = make_bps(bps_from, &partial, &meta, &bps_hint(&plan), cancel, |done, total| {
            events(BuildEvent::Progress { step: STEP_BPS, done, total, detail: None });
        })?;
        if let Some(want) = req.bps_source.as_ref().and_then(|d| d.crc32.as_deref()) {
            if !want.eq_ignore_ascii_case(&format!("{:08x}", made.source_crc32)) {
                return Err(BuildError::new("BUILD_BPS_SOURCE_CHANGED", format!("the retail disc's CRC32 is {:08x} now, not {want}; open the verified retail disc again", made.source_crc32)));
            }
        }
        // `verified` may come from the project's record rather than a hash of
        // this file; the CRC32 make_bps took while reading it is the proof.
        if made.source_crc32 != identity::REDUMP_CRC32 {
            let msg = format!(
                "a BPS needs the verified Redump dump as its source; {} has CRC32 {:08x}, not {:08x}",
                bps_from.display(),
                made.source_crc32,
                identity::REDUMP_CRC32
            );
            return Err(BuildError::new("BUILD_BPS_UNVERIFIED", msg));
        }
        created.push(bps_path.clone());
        write_atomic(&bps_path, &made.patch)?;
        created.push(readme_path.clone());
        let text = readme(
            stem,
            &format!("{stem}.bps"),
            stem,
            &project.file.name,
            record.number,
            (fs::metadata(bps_from).map_err(|e| io(bps_from, e))?.len(), made.source_crc32),
            (verify.bytes, made.target_crc32),
        );
        write_atomic(&readme_path, text.as_bytes())?;
        log.push(format!(
            "wrote {stem}.bps {} bytes ({} new, {} carried over); applying it to the source (CRC32 {:08x}) reproduces CRC32 {:08x}",
            made.patch.len(),
            made.new,
            made.same,
            made.source_crc32,
            made.target_crc32
        ));
        events(BuildEvent::StepDone { step: STEP_BPS, detail: Some(format!("{} B · verified", grouped(made.patch.len() as u64))) });
        bps_out = Some(OutputFile {
            path: bps_path.display().to_string(),
            bytes: made.patch.len() as u64,
            crc32: format!("{:08x}", crc32fast::hash(&made.patch)),
            sha1: None,
        });
        readme_out = Some(readme_path.display().to_string());
    }

    // Keep the image (rename) or drop it (BPS only).
    let (bin_out, cue_out) = if req.image {
        check(cancel)?;
        created.push(bin.clone());
        fs::rename(&partial, &bin).map_err(|e| io(&bin, e))?;
        created.push(cue.clone());
        write_atomic(&cue, cue_sheet(&format!("{stem}.bin")).as_bytes())?;
        log.push(format!("wrote {stem}.bin and {stem}.cue in {}", out_dir.display()));
        (
            Some(OutputFile { path: bin.display().to_string(), bytes: verify.bytes, crc32: verify.crc32.clone(), sha1: Some(verify.sha1.clone()) }),
            Some(cue.display().to_string()),
        )
    } else {
        let _ = fs::remove_file(&partial);
        (None, None)
    };

    let outputs = BuildOutputs {
        bin: bin_out,
        cue: cue_out,
        bps: bps_out,
        readme: readme_out,
        image_bytes: verify.bytes,
        crc32: verify.crc32,
        sha1: verify.sha1,
        sectors_appended: plan.appended,
        entries_rewritten: plan.entries_repointed,
        exe_sectors_written: report.exe_sectors_written,
        sectors_encoded: report.sectors_encoded,
        sectors_changed: report.sectors_changed,
        volume_from: report.volume_from,
        volume_to: report.volume_to,
        files_verified: verify.files_checked,
        duration_ms: t0.elapsed().as_millis() as u64,
    };
    Ok((outputs, report.records))
}

/* ───────────────────────── IO helpers ───────────────────────── */

fn make_writable(path: &Path) -> Res<()> {
    let meta = fs::metadata(path).map_err(|e| io(path, e))?;
    if !meta.permissions().readonly() {
        return Ok(());
    }
    #[cfg(unix)]
    let perms = {
        use std::os::unix::fs::PermissionsExt;
        fs::Permissions::from_mode(meta.permissions().mode() | 0o200)
    };
    #[cfg(not(unix))]
    let perms = {
        let mut p = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        p
    };
    fs::set_permissions(path, perms).map_err(|e| io(path, e))
}

#[cfg(unix)]
pub(crate) fn pread(file: &File, buf: &mut [u8], pos: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, pos)
}

#[cfg(unix)]
fn pwrite(file: &File, buf: &[u8], pos: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, pos)
}

#[cfg(windows)]
pub(crate) fn pread(file: &File, mut buf: &mut [u8], mut pos: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, pos) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                pos += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn pwrite(file: &File, mut buf: &[u8], mut pos: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, pos) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                buf = &buf[n..];
                pos += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_bytes_match_the_prd() {
        // npc/7 relocated to 185,985 with 98 sectors (PRD 15 §4.2).
        assert_eq!(entry_bytes(185_985, 98), [0x41, 0x21, 0x60, 0x00, 0x62, 0x00, 0x00, 0x00]);
        assert_eq!(entry_bytes(86_013, 96), [0x19, 0x08, 0x63, 0x00, 0x60, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn caps() {
        assert_eq!(cap_for(Kind::Npc, 2).unwrap().sectors, 98);
        assert!(cap_for(Kind::Bgm, 57).is_none());
        assert_eq!(cap_for(Kind::Toy, 22).unwrap().bytes, 0x20000);
        assert!(cap_for(Kind::Ovr, 1).is_none()); // ovr/2
        assert!(cap_for(Kind::Ovr, 5).unwrap().inference); // ovr/6
        assert!(cap_for(Kind::Enemy, 17).is_none());
    }

    #[test]
    fn names() {
        assert_eq!(file_stem("english/v0.4", 4), "english-v0.4");
        assert_eq!(file_stem("  ", 4), "build-4");
        assert_eq!(grouped(437_436_720), "437,436,720");
        assert_eq!(grouped(98), "98");
    }
}
