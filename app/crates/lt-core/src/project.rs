//! The project folder (`<name>.ltproj`): the save system. Plain files, so a
//! project can be backed up, synced or put under git. Spec:
//! `docs/screens/03-projects-and-saving.md` §4–5.
//!
//! The retail image is never written; a project only stores edited asset bytes
//! (content-addressed blobs) and the manifest that says which asset they replace.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::base::{BaseError, BasePackage};
use crate::error::{LtError, Result};
use crate::identity::Fingerprint;
use crate::release::Release;

pub const EXTENSION: &str = "ltproj";
pub const SCHEMA: u32 = 1;
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

const GITIGNORE: &str = "local.json\n.lock\nedits/journal.jsonl\nemu/\ncache/\nbuilds/*/*.bin\nbuilds/*/*.cue\n";

/* ───────────────────────── Files ───────────────────────── */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFile {
    pub schema: u32,
    pub kind: String,
    pub id: String,
    pub name: String,
    pub created: String,
    pub app: AppVersions,
    pub disc: DiscRef,
    /// The base the project is built on; `None` is retail (research 06 §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<BaseRef>,
    pub settings: ProjectSettings,
}

/// A project's base: its package is copied into `base/` and the image is
/// materialised into the app's base cache when the project opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseRef {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub name: Option<String>,
    pub target_sha1: String,
    pub new_area_first: u32,
}

impl BaseRef {
    pub fn tag(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppVersions {
    pub created: String,
    pub last_written: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscRef {
    pub title: String,
    pub serial: String,
    /// "redump" | "unverified"
    pub verified: String,
    pub fingerprint: Option<DiscFingerprint>,
    /// Display hint only; the real path lives in `local.json`.
    pub hint_path: String,
}

impl DiscRef {
    /// The release the project was made from, by its serial (USA for anything unknown).
    pub fn release(&self) -> Release {
        Release::of_serial(&self.serial).unwrap_or_default()
    }

    /// The fingerprint this project recorded when it was made from the Redump
    /// dump, if an image of `size` bytes and `sectors` sectors can stand for
    /// it. Reopening a project skips the full checksum, so this is what says
    /// its disc is verified; a BPS build still checks the source's CRC32 as it
    /// reads it.
    pub fn trusted_fingerprint(&self, size: u64, sectors: u32) -> Option<Fingerprint> {
        let f = self.fingerprint.as_ref()?;
        let release = self.release();
        let redump = self.verified == "redump" && u32::from_str_radix(&f.crc32, 16) == Ok(release.info().crc32);
        (redump && f.bytes == size && f.sectors == sectors).then(|| Fingerprint {
            crc32: f.crc32.clone(),
            sha1: f.sha1.clone(),
            size,
            sectors,
            matches_redump: true,
            release: Some(release),
            from_project: true,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscFingerprint {
    pub crc32: String,
    pub sha1: String,
    pub bytes: u64,
    pub sectors: u32,
}

impl From<&Fingerprint> for DiscFingerprint {
    fn from(f: &Fingerprint) -> Self {
        Self { crc32: f.crc32.clone(), sha1: f.sha1.clone(), bytes: f.size, sectors: f.sectors }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettings {
    pub autosave_delay_ms: u32,
    pub autosave_max_wait_ms: u32,
    pub auto_snapshot_minutes: u32,
    pub keep_auto_snapshots: u32,
    pub keep_build_images: u32,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            autosave_delay_ms: 2000,
            autosave_max_wait_ms: 10_000,
            auto_snapshot_minutes: 30,
            keep_auto_snapshots: 20,
            keep_build_images: 2,
        }
    }
}

/// Machine-local file (git-ignored): where this computer keeps the disc.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFile {
    pub schema: u32,
    pub disc_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema: u32,
    pub rev: u64,
    pub journal_seq: u64,
    pub updated: String,
    /// Keyed by canonical id of the alias group's lowest member.
    pub entries: BTreeMap<String, ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub alias_group: Vec<String>,
    pub base: BlobRef,
    pub current: BlobRef,
    pub sidecar: Option<SidecarRef>,
    /// "editor" | "raw-replace" | "import"
    pub origin: String,
    pub created: String,
    pub modified: String,
    #[serde(default)]
    pub stats: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BlobRef {
    pub sha256: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sectors: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidecarRef {
    pub sha256: String,
    pub editor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub schema: u32,
    pub id: String,
    pub name: String,
    /// named | auto | pre-build | pre-restore | pre-migration
    pub kind: String,
    pub created: String,
    pub manifest_rev: u64,
    pub manifest: Manifest,
}

/// One line of `history.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEvent {
    pub v: u32,
    pub t: String,
    pub kind: String,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/* ───────────────────────── Project ───────────────────────── */

/// An open project folder.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub file: ProjectFile,
    pub local: LocalFile,
    pub manifest: Manifest,
}

/// What the projects screen shows without loading editors.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub path: String,
    pub id: String,
    pub name: String,
    pub disc: DiscRef,
    pub disc_path: Option<String>,
    pub base: Option<BaseRef>,
    pub changed_files: usize,
    pub manifest_rev: u64,
    pub snapshots: Vec<SnapshotInfo>,
    pub history: Vec<HistoryEvent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    pub file: String,
    pub name: String,
    pub kind: String,
    pub created: String,
    pub manifest_rev: u64,
}

impl Project {
    /// Creates `<parent>/<name>.ltproj` with an empty manifest.
    pub fn create(parent: &Path, name: &str, disc: DiscRef, disc_path: Option<String>) -> Result<Self> {
        let root = parent.join(format!("{}.{EXTENSION}", sanitize(name)));
        if root.exists() {
            return Err(LtError::ProjectExists(root));
        }
        for dir in ["edits/objects", "snapshots", "builds", "cache"] {
            fs::create_dir_all(root.join(dir)).map_err(|e| LtError::io(&root, e))?;
        }
        let now = now_rfc3339();
        let file = ProjectFile {
            schema: SCHEMA,
            kind: "living-tool.project".into(),
            id: new_id(),
            name: name.to_string(),
            created: now.clone(),
            app: AppVersions { created: APP_VERSION.into(), last_written: APP_VERSION.into() },
            disc,
            base: None,
            settings: ProjectSettings::default(),
        };
        let local = LocalFile { schema: SCHEMA, disc_path };
        let manifest = Manifest { schema: SCHEMA, updated: now, ..Default::default() };
        let p = Self { root, file, local, manifest };
        write_atomic(&p.root.join(".gitignore"), GITIGNORE.as_bytes())?;
        p.save_project_file()?;
        p.save_local()?;
        p.save_manifest()?;
        let mut fields = serde_json::Map::new();
        fields.insert("verified".into(), p.file.disc.verified.clone().into());
        p.append_history("project.created", fields)?;
        Ok(p)
    }

    pub fn open(root: &Path) -> Result<Self> {
        let file: ProjectFile = read_json(&root.join("project.json"))?;
        let local: LocalFile = read_json(&root.join("local.json")).unwrap_or_default();
        let manifest: Manifest = read_json(&root.join("edits/manifest.json"))?;
        Ok(Self { root: root.to_path_buf(), file, local, manifest })
    }

    pub fn summary(&self, history_limit: usize) -> Result<ProjectSummary> {
        Ok(ProjectSummary {
            path: self.root.display().to_string(),
            id: self.file.id.clone(),
            name: self.file.name.clone(),
            disc: self.file.disc.clone(),
            disc_path: self.local.disc_path.clone(),
            base: self.file.base.clone(),
            changed_files: self.manifest.entries.len(),
            manifest_rev: self.manifest.rev,
            snapshots: self.snapshots()?,
            history: self.history(history_limit)?,
        })
    }

    /// Puts the project on `pkg`: copies the package into `base/` and records
    /// it in `project.json`. Only before the first edit, since every staged
    /// file was made against the reference disc.
    pub fn set_base(&mut self, pkg: &BasePackage) -> std::result::Result<BasePackage, BaseError> {
        if !self.file.disc.release().editable() {
            return Err(BaseError { code: "RELEASE_READ_ONLY", message: "Bases apply to the USA release only.".into() });
        }
        if !self.manifest.entries.is_empty() {
            return Err(BaseError { code: "BASE_PROJECT_HAS_EDITS", message: "A base can only be chosen before the project's first edit.".into() });
        }
        let dir = self.root.join("base");
        let _ = fs::remove_dir_all(&dir);
        let copy = pkg.copy_to(&dir)?;
        let m = &copy.manifest;
        self.file.base = Some(BaseRef {
            id: m.id.clone(),
            version: m.version.clone(),
            name: m.name.clone(),
            target_sha1: m.patch.target_sha1.clone(),
            new_area_first: m.new_area_first(),
        });
        self.save_project_file().map_err(|e| BaseError { code: "BASE_IO", message: e.to_string() })?;
        let mut fields = serde_json::Map::new();
        fields.insert("base".into(), m.tag().into());
        let _ = self.append_history("base.set", fields);
        Ok(copy)
    }

    /// The project's base package (`base/`), if it has one.
    pub fn base_package(&self) -> Option<std::result::Result<BasePackage, BaseError>> {
        self.file.base.as_ref().map(|_| BasePackage::open(&self.root.join("base")))
    }

    /// The first chunk id new areas may take (1328 on USA retail; the disc's own for the others).
    pub fn new_area_first(&self) -> u32 {
        self.file.base.as_ref().map_or_else(|| self.file.disc.release().info().new_area_first(), |b| b.new_area_first)
    }

    pub fn blobs(&self) -> Blobs {
        Blobs { root: self.root.join("edits/objects") }
    }

    /// Stages an edited asset: stores its bytes and upserts the manifest entry.
    /// If the bytes equal the retail base, the entry is removed ("reverted").
    /// A project on the EU or JP release is read-only (`RELEASE_READ_ONLY`).
    pub fn stage(&mut self, id: &str, alias_group: Vec<String>, base: &[u8], current: &[u8], origin: &str) -> Result<()> {
        let release = self.file.disc.release();
        if !release.editable() {
            return Err(LtError::ReadOnlyRelease(release.info().title));
        }
        let blobs = self.blobs();
        let base_ref = BlobRef { sha256: sha256_hex(base), bytes: base.len() as u64, sectors: Some((base.len() / 2048) as u32) };
        let now = now_rfc3339();
        // Disc reads are sector-padded; bytes equal to retail up to that padding are "reverted".
        let same = base == current
            || (current.len() < base.len() && base[..current.len()] == *current && base[current.len()..].iter().all(|&b| b == 0));
        if same {
            self.manifest.entries.remove(id);
        } else {
            let current_ref = BlobRef { sha256: blobs.put(current)?, bytes: current.len() as u64, sectors: None };
            let created = self.manifest.entries.get(id).map(|e| e.created.clone()).unwrap_or_else(|| now.clone());
            self.manifest.entries.insert(
                id.to_string(),
                ManifestEntry {
                    alias_group,
                    base: base_ref,
                    current: current_ref,
                    sidecar: None,
                    origin: origin.to_string(),
                    created,
                    modified: now.clone(),
                    stats: BTreeMap::new(),
                },
            );
        }
        self.manifest.rev += 1;
        self.manifest.updated = now;
        self.save_manifest()?;
        let mut fields = serde_json::Map::new();
        fields.insert("rev".into(), self.manifest.rev.into());
        fields.insert("assets".into(), vec![id.to_string()].into());
        self.append_history("autosave", fields)
    }

    /// Freezes the current manifest into `snapshots/`.
    pub fn snapshot(&self, name: &str, kind: &str) -> Result<SnapshotInfo> {
        let created = now_rfc3339();
        let snap = Snapshot {
            schema: SCHEMA,
            id: new_id(),
            name: name.to_string(),
            kind: kind.to_string(),
            created: created.clone(),
            manifest_rev: self.manifest.rev,
            manifest: self.manifest.clone(),
        };
        let file = format!("{}--{}.json", created.replace(':', "-"), slug(name));
        write_json(&self.root.join("snapshots").join(&file), &snap)?;
        let mut fields = serde_json::Map::new();
        fields.insert("id".into(), snap.id.clone().into());
        fields.insert("name".into(), name.into());
        fields.insert("snapshotKind".into(), kind.into());
        self.append_history("snapshot.created", fields)?;
        Ok(SnapshotInfo { file, name: snap.name, kind: snap.kind, created, manifest_rev: snap.manifest_rev })
    }

    pub fn snapshots(&self) -> Result<Vec<SnapshotInfo>> {
        let dir = self.root.join("snapshots");
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| LtError::io(&dir, e))? {
            let path = entry.map_err(|e| LtError::io(&dir, e))?.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(s) = read_json::<Snapshot>(&path) {
                    out.push(SnapshotInfo {
                        file: path.file_name().unwrap().to_string_lossy().into_owned(),
                        name: s.name,
                        kind: s.kind,
                        created: s.created,
                        manifest_rev: s.manifest_rev,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.created.cmp(&a.created));
        Ok(out)
    }

    /// Newest first.
    pub fn history(&self, limit: usize) -> Result<Vec<HistoryEvent>> {
        let path = self.root.join("history.jsonl");
        let text = fs::read_to_string(&path).unwrap_or_default();
        let mut events: Vec<HistoryEvent> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        events.reverse();
        events.truncate(limit);
        Ok(events)
    }

    pub fn append_history(&self, kind: &str, fields: serde_json::Map<String, serde_json::Value>) -> Result<()> {
        let path = self.root.join("history.jsonl");
        let event = HistoryEvent { v: 1, t: now_rfc3339(), kind: kind.into(), fields };
        let mut f = OpenOptions::new().create(true).append(true).open(&path).map_err(|e| LtError::io(&path, e))?;
        let line = serde_json::to_string(&event).expect("history serializes");
        writeln!(f, "{line}").map_err(|e| LtError::io(&path, e))
    }

    fn save_project_file(&self) -> Result<()> {
        write_json(&self.root.join("project.json"), &self.file)
    }

    fn save_local(&self) -> Result<()> {
        write_json(&self.root.join("local.json"), &self.local)
    }

    fn save_manifest(&self) -> Result<()> {
        write_json(&self.root.join("edits/manifest.json"), &self.manifest)
    }
}

/* ───────────────────────── Blobs ───────────────────────── */

/// Content-addressed store: `edits/objects/ab/cdef…` named by SHA-256.
pub struct Blobs {
    root: PathBuf,
}

impl Blobs {
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let h = sha256_hex(bytes);
        let p = self.path(&h);
        if !p.exists() {
            fs::create_dir_all(p.parent().unwrap()).map_err(|e| LtError::io(&p, e))?;
            write_atomic(&p, bytes)?;
        }
        Ok(h)
    }

    /// Verified read: a blob whose hash doesn't match is reported as corrupt.
    pub fn get(&self, sha256: &str) -> Result<Vec<u8>> {
        let p = self.path(sha256);
        let b = fs::read(&p).map_err(|e| LtError::io(&p, e))?;
        if sha256_hex(&b) != sha256 {
            return Err(LtError::BlobCorrupt(sha256.to_string()));
        }
        Ok(b)
    }

    fn path(&self, h: &str) -> PathBuf {
        self.root.join(&h[..2]).join(&h[2..])
    }
}

/* ───────────────────────── Recent projects ───────────────────────── */

/// App-level index of known projects (lives in the app's data folder).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsIndex {
    pub schema: u32,
    pub projects: Vec<RecentProject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentProject {
    pub path: String,
    pub name: String,
    pub last_opened: String,
}

impl ProjectsIndex {
    pub fn load(path: &Path) -> Self {
        read_json(path).unwrap_or(Self { schema: SCHEMA, projects: Vec::new() })
    }

    pub fn touch(&mut self, project: &Project) {
        let path = project.root.display().to_string();
        self.projects.retain(|p| p.path != path);
        self.projects.insert(0, RecentProject { path, name: project.file.name.clone(), last_opened: now_rfc3339() });
        self.projects.truncate(50);
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| LtError::io(dir, e))?;
        }
        write_json(path, self)
    }
}

/* ───────────────────────── Helpers ───────────────────────── */

/// Write to a temp file, fsync, then rename over the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", new_id()));
    {
        let mut f = File::create(&tmp).map_err(|e| LtError::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| LtError::io(&tmp, e))?;
        f.sync_all().map_err(|e| LtError::io(&tmp, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| LtError::io(path, e))
}

/// Pretty JSON with sorted keys and a trailing newline, for stable git diffs.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    // Round-trip through Value: its maps are BTreeMaps, so keys come out sorted.
    let v = serde_json::to_value(value).expect("serializable");
    let mut s = serde_json::to_string_pretty(&v).expect("serializable");
    s.push('\n');
    write_atomic(path, s.as_bytes())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).map_err(|e| LtError::io(path, e))?;
    serde_json::from_str(&text).map_err(|e| LtError::BadProjectFile(path.to_path_buf(), e.to_string()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Time-ordered unique id (hex of nanoseconds + process id).
pub fn new_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos:x}{:04x}", std::process::id() & 0xffff)
}

/// RFC 3339 UTC timestamp, second precision.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if r#"/\:*?"<>|"#.contains(c) { '-' } else { c }).collect();
    s.trim().to_string()
}

fn slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc() -> DiscRef {
        DiscRef {
            title: "Guardian's Crusade (USA)".into(),
            serial: "SLUS-00811".into(),
            verified: "redump".into(),
            fingerprint: None,
            hint_path: "~/Games/gc.cue".into(),
        }
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_728), (2026, 10, 2));
    }

    #[test]
    fn create_stage_snapshot_reopen() {
        let parent = std::env::temp_dir().join(format!("lt-project-test-{}", new_id()));
        fs::create_dir_all(&parent).unwrap();
        let mut p = Project::create(&parent, "English script fixes", disc(), Some("/discs/gc.bin".into())).unwrap();
        assert!(p.root.ends_with("English script fixes.ltproj"));

        let base = vec![0u8; 4096];
        let mut current = base.clone();
        current[10] = 0x41;
        p.stage("npc/7", vec!["npc/7".into()], &base, &current, "editor").unwrap();
        assert_eq!(p.manifest.entries.len(), 1);
        let entry = &p.manifest.entries["npc/7"];
        assert_eq!(p.blobs().get(&entry.current.sha256).unwrap(), current);

        let snap = p.snapshot("before font swap", "named").unwrap();
        assert!(snap.file.ends_with("--before-font-swap.json"));

        // Reverting to the retail bytes removes the entry.
        p.stage("npc/7", vec!["npc/7".into()], &base, &base, "editor").unwrap();
        assert!(p.manifest.entries.is_empty());

        let reopened = Project::open(&p.root).unwrap();
        let summary = reopened.summary(10).unwrap();
        assert_eq!(summary.manifest_rev, 2);
        assert_eq!(summary.snapshots.len(), 1);
        let kinds: Vec<_> = summary.history.iter().map(|h| h.kind.as_str()).collect();
        assert_eq!(kinds, ["autosave", "snapshot.created", "autosave", "project.created"]);
        assert_eq!(reopened.local.disc_path.as_deref(), Some("/discs/gc.bin"));

        // Sorted keys + trailing newline.
        let json = fs::read_to_string(p.root.join("project.json")).unwrap();
        assert!(json.ends_with("}\n"));
        assert!(json.find("\"app\"").unwrap() < json.find("\"created\"").unwrap());
        fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn trusted_fingerprint_needs_redump_and_the_same_size() {
        let (bytes, sectors) = (185_985 * 2352, 185_985);
        let mut d = disc();
        d.fingerprint = Some(DiscFingerprint {
            crc32: "cdc72c62".into(),
            sha1: "d7eda1f08b91721842511348aff58818e12f486b".into(),
            bytes,
            sectors,
        });
        let fp = d.trusted_fingerprint(bytes, sectors).unwrap();
        assert!(fp.matches_redump && fp.from_project);
        assert_eq!((fp.crc32.as_str(), fp.size, fp.sectors), ("cdc72c62", bytes, sectors));

        // Another image, or a project that wasn't made from the Redump dump.
        assert!(d.trusted_fingerprint(bytes + 2352, sectors + 1).is_none());
        assert!(d.trusted_fingerprint(bytes, sectors - 1).is_none());
        let mut unverified = d.clone();
        unverified.verified = "unverified".into();
        assert!(unverified.trusted_fingerprint(bytes, sectors).is_none());
        let mut other_crc = d.clone();
        other_crc.fingerprint.as_mut().unwrap().crc32 = "12345678".into();
        assert!(other_crc.trusted_fingerprint(bytes, sectors).is_none());
        // No fingerprint recorded (the test helper's default).
        assert!(disc().trusted_fingerprint(bytes, sectors).is_none());
    }
}
