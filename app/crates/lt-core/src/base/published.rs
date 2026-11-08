//! Published bases (LT-48): the bases catalogue at `downloads.livingtool.app`
//! lists base packages anyone can build on, and Living Tool downloads one into
//! its data folder (`<app data>/bases/packages/<id>-<version>/`), checking each
//! file's size and SHA-256 before the package counts. The folder is then an
//! ordinary base package ([`BasePackage`]), set on a project the same way as one
//! the user picked from disk.
//!
//! `bases/index.json` (served no-cache; version folders never change):
//!
//! ```json
//! { "version": 1, "bases": [ {
//!     "id": "hackergc-blank", "version": "1.0.0", "name": "HackerGC v1.0.0 · blank",
//!     "summary": "One line for the picker", "minApp": "0.1.7", "published": "2026-10-18",
//!     "path": "bases/hackergc-blank/1.0.0/",
//!     "files": [ { "name": "base.json", "size": 4923, "sha256": "<hex>" }, … ] } ] }
//! ```
//!
//! A file's URL is the downloads root + `path` + `name`. The network side lives
//! in the desktop app; this module parses, compares versions and verifies.
//!
//! A finished download keeps the entry it was checked against as
//! `published.json` ([`SAVED`], LT-51), so when the catalogue can't be reached
//! the picker still offers it, re-checked file by file against that entry.

use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{bad, BaseError, BasePackage};

/// Where the app reads the catalogue (`LT_BASES_URL` overrides it in the desktop app).
pub const CATALOGUE_URL: &str = "https://downloads.livingtool.app/bases/index.json";

/// The catalogue format this build reads.
pub const FORMAT: u32 = 1;

/// Downloaded packages live in this folder under the base cache root.
pub const PACKAGES: &str = "packages";

/// One file of a published base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedFile {
    pub name: String,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

/// A base as the catalogue lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Published {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// The lowest Living Tool version that can use the base.
    #[serde(default = "any_app")]
    pub min_app: String,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub published: Option<String>,
    /// The version folder, relative to the downloads root, ending in `/`.
    pub path: String,
    pub files: Vec<PublishedFile>,
}

fn any_app() -> String {
    "0.0.0".into()
}

/// `bases/index.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalogue {
    pub version: u32,
    pub bases: Vec<Published>,
}

impl Published {
    /// `id@version`, the same tag the package's `base.json` gives.
    pub fn tag(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    /// The whole download.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// This app version can use the base.
    pub fn fits_app(&self, app_version: &str) -> bool {
        app_fits(&self.min_app, app_version)
    }

    /// A file's URL under the downloads root ([`files_root`]).
    pub fn url(&self, root: &str, file: &PublishedFile) -> String {
        format!("{root}{}{}", self.path, file.name)
    }
}

fn malformed(m: impl Into<String>) -> BaseError {
    bad("BASES_BAD_CATALOGUE", format!("The bases catalogue doesn't read: {}", m.into()))
}

/// A folder or file name the catalogue may use: letters, digits, `.`, `_`, `-`, `+`, not starting with a dot.
fn safe_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reads and checks `bases/index.json`: format 1, and every base with a safe
/// id, version, folder and file names, a `base.json`, sizes and SHA-256s, and
/// a `minApp` that reads as a version.
pub fn parse(bytes: &[u8]) -> Result<Catalogue, BaseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| malformed(format!("it isn't JSON ({e}).")))?;
    let format = value.get("version").and_then(|v| v.as_u64()).ok_or_else(|| malformed("it has no format version."))?;
    if format > FORMAT as u64 {
        return Err(bad(
            "BASES_CATALOGUE_NEWER",
            format!("The bases catalogue is format {format}, and this Living Tool reads format {FORMAT}. Update Living Tool to see its bases."),
        ));
    }
    let mut cat: Catalogue = serde_json::from_value(value).map_err(|e| malformed(format!("{e}.")))?;
    if cat.version != FORMAT {
        return Err(malformed(format!("format {} isn't one Living Tool knows.", cat.version)));
    }
    let mut tags = std::collections::HashSet::new();
    for (i, b) in cat.bases.iter_mut().enumerate() {
        check_entry(b, &format!("bases[{i}]")).map_err(malformed)?;
        if !tags.insert(b.tag()) {
            return Err(malformed(format!("{} is listed twice.", b.tag())));
        }
    }
    Ok(cat)
}

/// One entry's rules (`at` names it until its id reads): safe id, version,
/// folder and file names, a `base.json`, 64-hex SHA-256s (lowercased here),
/// and a `minApp` that reads as a version.
fn check_entry(b: &mut Published, at: &str) -> Result<(), String> {
    if !safe_name(&b.id) || !safe_name(&b.version) {
        return Err(format!("{at} needs an id and version of letters, digits, dots and dashes."));
    }
    let tag = b.tag();
    if parse_version(&b.min_app).is_none() {
        return Err(format!("{tag}'s minApp \"{}\" isn't a version.", b.min_app));
    }
    let segments: Vec<&str> = b.path.split('/').collect();
    let ok_path = b.path.ends_with('/') && segments.len() >= 2 && segments[..segments.len() - 1].iter().all(|s| safe_name(s));
    if !ok_path {
        return Err(format!("{tag}'s path \"{}\" must be a relative folder ending in /.", b.path));
    }
    let mut names = std::collections::HashSet::new();
    for f in &mut b.files {
        if !safe_name(&f.name) || !names.insert(f.name.clone()) {
            return Err(format!("{tag} lists a bad or repeated file name \"{}\".", f.name));
        }
        if !is_sha256(&f.sha256) {
            return Err(format!("{tag} gives {} a sha256 that isn't 64 hex digits.", f.name));
        }
        f.sha256.make_ascii_lowercase();
    }
    if !names.contains("base.json") {
        return Err(format!("{tag} has no base.json."));
    }
    Ok(())
}

/// The downloads root a catalogue URL's `path`s are relative to:
/// `https://downloads.livingtool.app/` for `…/bases/index.json`, else the
/// catalogue's own folder.
pub fn files_root(catalogue_url: &str) -> String {
    let url = catalogue_url.split(['?', '#']).next().unwrap_or(catalogue_url);
    if let Some(root) = url.strip_suffix("bases/index.json").filter(|r| r.ends_with('/')) {
        return root.into();
    }
    match url.rfind('/') {
        Some(i) if i > url.find("://").map_or(0, |s| s + 2) => url[..=i].into(),
        _ => format!("{url}/"),
    }
}

/* ---------------------------------------------------------------- versions */

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Text(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
            (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Text(a), Ident::Text(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A semantic version: `MAJOR.MINOR.PATCH`, missing parts read as 0, an
/// optional `-pre.release` (lower than the release) and `+build` (ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<Ident>,
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => self.pre.cmp(&other.pre),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let s = s.split('+').next()?;
    let (core, pre) = s.split_once('-').map_or((s, None), |(c, p)| (c, Some(p)));
    let parts: Vec<&str> = core.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut v = [0u64; 3];
    for (slot, p) in v.iter_mut().zip(&parts) {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    let pre = match pre {
        None => Vec::new(),
        Some(p) => p
            .split('.')
            .map(|id| {
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    None
                } else if id.bytes().all(|b| b.is_ascii_digit()) {
                    id.parse().ok().map(Ident::Num)
                } else {
                    Some(Ident::Text(id.into()))
                }
            })
            .collect::<Option<_>>()?,
    };
    Some(Version { core: v, pre })
}

/// `a` against `b` as semantic versions (`None` if either doesn't read).
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    Some(parse_version(a)?.cmp(&parse_version(b)?))
}

/// `app` is `min_app` or newer.
pub fn app_fits(min_app: &str, app: &str) -> bool {
    matches!(compare_versions(app, min_app), Some(Ordering::Greater | Ordering::Equal))
}

/* ---------------------------------------------------------------- packages */

/// `<cache root>/packages/<id>-<version>/`.
pub fn package_dir(cache_root: &Path, b: &Published) -> PathBuf {
    cache_root.join(PACKAGES).join(format!("{}-{}", b.id, b.version))
}

fn hash_file(path: &Path) -> Result<String, BaseError> {
    let mut f = File::open(path).map_err(|e| bad("BASE_IO", format!("{}: {e}", path.display())))?;
    let (mut sha, mut buf) = (Sha256::new(), vec![0u8; 1 << 20]);
    loop {
        let n = f.read(&mut buf).map_err(|e| bad("BASE_IO", format!("{}: {e}", path.display())))?;
        if n == 0 {
            return Ok(hex(&sha.finalize()));
        }
        sha.update(&buf[..n]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every file is in `dir` at its listed size (a quick look for the picker;
/// [`check_file`] hashes).
pub fn looks_present(dir: &Path, b: &Published) -> bool {
    b.files.iter().all(|f| std::fs::metadata(dir.join(&f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size))
}

/// The file is in `dir` with the listed size and SHA-256. A file that is there
/// but differs is deleted, so it downloads again.
pub fn check_file(dir: &Path, f: &PublishedFile) -> Result<bool, BaseError> {
    match file_state(dir, f)? {
        FileState::Ok => Ok(true),
        FileState::Missing => Ok(false),
        FileState::Differs => {
            let path = dir.join(&f.name);
            std::fs::remove_file(&path).map_err(|e| bad("BASE_IO", format!("{}: {e}", path.display())))?;
            Ok(false)
        }
    }
}

enum FileState {
    Ok,
    Missing,
    Differs,
}

/// Compares a file in `dir` with its entry, changing nothing.
fn file_state(dir: &Path, f: &PublishedFile) -> Result<FileState, BaseError> {
    let path = dir.join(&f.name);
    let Ok(meta) = std::fs::metadata(&path) else { return Ok(FileState::Missing) };
    Ok(if meta.is_file() && meta.len() == f.size && hash_file(&path)? == f.sha256 { FileState::Ok } else { FileState::Differs })
}

/* ---------------------------------------------- downloaded, catalogue offline */

/// The catalogue entry a download was checked against, saved beside the
/// package (`packages/<id>-<version>/published.json`) once it is complete
/// (LT-51). With it the package can be re-checked, every file's size and
/// SHA-256, and used when the catalogue can't be reached. It isn't named in
/// `base.json`, so [`BasePackage::copy_to`] leaves it out of projects.
pub const SAVED: &str = "published.json";

/// `published.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    /// The catalogue format the entry is in ([`FORMAT`]).
    pub version: u32,
    /// The catalogue the entry came from.
    pub catalogue: String,
    pub base: Published,
}

fn saved_bad(dir: &Path, m: impl std::fmt::Display) -> BaseError {
    bad("BASE_SAVED_BAD", format!("{}: {m}", dir.join(SAVED).display()))
}

/// Writes `published.json` into the package folder `dir` (through a part file, so it is whole or absent).
pub fn save_entry(dir: &Path, catalogue_url: &str, b: &Published) -> Result<(), BaseError> {
    let saved = Saved { version: FORMAT, catalogue: catalogue_url.into(), base: b.clone() };
    let (part, path) = (dir.join(format!("{SAVED}.part")), dir.join(SAVED));
    let io = |e: std::io::Error| bad("BASE_IO", format!("{}: {e}", path.display()));
    std::fs::write(&part, serde_json::to_vec_pretty(&saved).expect("json")).map_err(io)?;
    std::fs::rename(&part, &path).map_err(io)
}

/// Reads a package folder's `published.json`, with the catalogue's rules for
/// the entry, and checks it belongs to the folder (`<id>-<version>`).
pub fn read_saved(dir: &Path) -> Result<Saved, BaseError> {
    let bytes = std::fs::read(dir.join(SAVED)).map_err(|e| saved_bad(dir, e))?;
    let mut saved: Saved = serde_json::from_slice(&bytes).map_err(|e| saved_bad(dir, e))?;
    if saved.version != FORMAT {
        return Err(saved_bad(dir, format!("format {} isn't one this Living Tool reads.", saved.version)));
    }
    check_entry(&mut saved.base, "the entry").map_err(|m| saved_bad(dir, m))?;
    let folder = format!("{}-{}", saved.base.id, saved.base.version);
    if dir.file_name().and_then(|n| n.to_str()) != Some(folder.as_str()) {
        return Err(saved_bad(dir, format!("it is for {}, which belongs in {folder}/.", saved.base.tag())));
    }
    Ok(saved)
}

/// Every file of `b` is in `dir` with its size and SHA-256, changing nothing.
/// Otherwise `BASE_DOWNLOAD_CHANGED`, naming the first file that is missing or differs.
pub fn verify(dir: &Path, b: &Published) -> Result<(), BaseError> {
    for f in &b.files {
        let why = match file_state(dir, f)? {
            FileState::Ok => continue,
            FileState::Missing => "is missing",
            FileState::Differs => "no longer matches the SHA-256 it was downloaded with",
        };
        return Err(bad(
            "BASE_DOWNLOAD_CHANGED",
            format!("{}'s {} {why}, so it can't be used. Download it again when the bases catalogue can be reached.", b.tag(), f.name),
        ));
    }
    Ok(())
}

/// A package downloaded earlier, as [`downloaded`] finds it.
#[derive(Debug, Clone)]
pub struct Downloaded {
    pub dir: PathBuf,
    pub saved: Saved,
    /// [`verify`] against the saved entry: `Err` (`BASE_DOWNLOAD_CHANGED`) means it isn't offered.
    pub check: Result<(), BaseError>,
}

/// The packages under `<cache root>/packages/` with a readable `published.json`,
/// each re-checked against it (every file hashed), by id, newest version first.
/// Folders without one (downloads from before LT-51, or unfinished) are left out.
pub fn downloaded(cache_root: &Path) -> Vec<Downloaded> {
    let Ok(entries) = std::fs::read_dir(cache_root.join(PACKAGES)) else { return Vec::new() };
    let mut found: Vec<Downloaded> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|dir| dir.is_dir())
        .filter_map(|dir| {
            let saved = read_saved(&dir).ok()?;
            let check = verify(&dir, &saved.base);
            Some(Downloaded { dir, saved, check })
        })
        .collect();
    found.sort_by(|a, b| {
        let (a, b) = (&a.saved.base, &b.saved.base);
        a.id.cmp(&b.id).then_with(|| compare_versions(&b.version, &a.version).unwrap_or_else(|| b.version.cmp(&a.version)))
    });
    found
}

/// A package downloaded earlier, for when the catalogue can't be reached:
/// re-checked against its saved entry ([`verify`]) and opened as the base it
/// names ([`open_downloaded`]). `BASE_NOT_DOWNLOADED` if there is none.
pub fn open_saved(cache_root: &Path, id: &str, version: &str) -> Result<(BasePackage, Saved), BaseError> {
    let missing = || bad("BASE_NOT_DOWNLOADED", format!("{id}@{version} hasn't been downloaded, and the bases catalogue can't be reached to fetch it."));
    if !safe_name(id) || !safe_name(version) {
        return Err(missing());
    }
    let dir = cache_root.join(PACKAGES).join(format!("{id}-{version}"));
    if !dir.join(SAVED).is_file() {
        return Err(missing());
    }
    let saved = read_saved(&dir)?;
    if saved.base.id != id || saved.base.version != version {
        return Err(missing());
    }
    verify(&dir, &saved.base)?;
    Ok((open_downloaded(&dir, &saved.base)?, saved))
}

/// A file being downloaded: written to `<name>.part`, hashed as it arrives,
/// and renamed into place only once its size and SHA-256 match. Dropped
/// before [`Incoming::finish`] (an error, a cancel), the part file goes.
pub struct Incoming {
    part: PathBuf,
    dest: PathBuf,
    out: Option<BufWriter<File>>,
    sha: Sha256,
    written: u64,
    file: PublishedFile,
    done: bool,
}

impl Incoming {
    pub fn create(dir: &Path, file: &PublishedFile) -> Result<Self, BaseError> {
        std::fs::create_dir_all(dir).map_err(|e| bad("BASE_IO", format!("{}: {e}", dir.display())))?;
        let part = dir.join(format!("{}.part", file.name));
        let out = File::create(&part).map_err(|e| bad("BASE_IO", format!("{}: {e}", part.display())))?;
        Ok(Self { part, dest: dir.join(&file.name), out: Some(BufWriter::with_capacity(1 << 20, out)), sha: Sha256::new(), written: 0, file: file.clone(), done: false })
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<(), BaseError> {
        if self.written + bytes.len() as u64 > self.file.size {
            return Err(bad(
                "BASE_DOWNLOAD_SIZE",
                format!("{} came in bigger than the catalogue's {} bytes, so it was deleted.", self.file.name, self.file.size),
            ));
        }
        self.sha.update(bytes);
        self.written += bytes.len() as u64;
        let out = self.out.as_mut().expect("open until finish");
        out.write_all(bytes).map_err(|e| bad("BASE_IO", format!("{}: {e}", self.part.display())))
    }

    /// Checks the size and SHA-256 and puts the file in place.
    pub fn finish(mut self) -> Result<PathBuf, BaseError> {
        let mut out = self.out.take().expect("open until finish");
        out.flush().map_err(|e| bad("BASE_IO", format!("{}: {e}", self.part.display())))?;
        drop(out);
        let f = &self.file;
        if self.written != f.size {
            return Err(bad(
                "BASE_DOWNLOAD_SIZE",
                format!("{} stopped at {} of {} bytes, so the partial file was deleted. Try again.", f.name, self.written, f.size),
            ));
        }
        let got = hex(&std::mem::take(&mut self.sha).finalize());
        if got != f.sha256 {
            return Err(bad(
                "BASE_DOWNLOAD_HASH",
                format!("{} doesn't match the catalogue's SHA-256 (got {}…, expected {}…), so it was deleted. Try again.", f.name, &got[..12], &f.sha256[..12]),
            ));
        }
        std::fs::rename(&self.part, &self.dest).map_err(|e| bad("BASE_IO", format!("{}: {e}", self.dest.display())))?;
        self.done = true;
        Ok(self.dest.clone())
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        if !self.done {
            self.out.take();
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

/// Opens a downloaded package and checks it is the base the catalogue named.
pub fn open_downloaded(dir: &Path, b: &Published) -> Result<BasePackage, BaseError> {
    let pkg = BasePackage::open(dir)?;
    if pkg.manifest.id != b.id || pkg.manifest.version != b.version {
        return Err(bad(
            "BASE_DOWNLOAD_MISMATCH",
            format!("The catalogue lists {}, but the downloaded base.json is {}.", b.tag(), pkg.manifest.tag()),
        ));
    }
    Ok(pkg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lt-published-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const SAMPLE: &str = r#"{
      "version": 1,
      "bases": [
        {
          "id": "hackergc-blank",
          "version": "1.0.0",
          "name": "HackerGC v1.0.0 · blank",
          "summary": "One line for the picker",
          "minApp": "0.1.7",
          "published": "2026-10-18",
          "path": "bases/hackergc-blank/1.0.0/",
          "files": [
            { "name": "base.json", "size": 4923, "sha256": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" },
            { "name": "hackergc-blank-1.0.0.bps", "size": 6251543, "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
            { "name": "flags.json", "size": 397, "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc" }
          ]
        }
      ]
    }"#;

    /// @LT-48/1: the catalogue format from LT-47 reads, and file URLs resolve under the downloads root.
    #[test]
    fn catalogue_reads_the_published_format() {
        let cat = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(cat.version, 1);
        let b = &cat.bases[0];
        assert_eq!((b.tag().as_str(), b.min_app.as_str(), b.bytes()), ("hackergc-blank@1.0.0", "0.1.7", 4923 + 6251543 + 397));
        assert_eq!(b.summary.as_deref(), Some("One line for the picker"));
        // Hashes are compared lowercase.
        assert_eq!(b.files[0].sha256, "a".repeat(64));
        let root = files_root(CATALOGUE_URL);
        assert_eq!(root, "https://downloads.livingtool.app/");
        assert_eq!(b.url(&root, &b.files[1]), "https://downloads.livingtool.app/bases/hackergc-blank/1.0.0/hackergc-blank-1.0.0.bps");
        // A local mirror of the same layout, and a catalogue somewhere else.
        assert_eq!(files_root("http://127.0.0.1:8765/bases/index.json"), "http://127.0.0.1:8765/");
        assert_eq!(files_root("http://127.0.0.1:8765/test/cat.json?x=1"), "http://127.0.0.1:8765/test/");
        assert_eq!(package_dir(Path::new("/data/bases"), b), Path::new("/data/bases/packages/hackergc-blank-1.0.0"));
        // Fields a later format adds are ignored.
        let extra = SAMPLE.replace("\"published\"", "\"channel\": \"stable\", \"published\"");
        assert!(parse(extra.as_bytes()).is_ok());
    }

    /// @LT-48/3: a malformed catalogue fails with BASES_BAD_CATALOGUE, a newer format with BASES_CATALOGUE_NEWER.
    #[test]
    fn malformed_catalogues_are_refused() {
        let code = |s: &str| parse(s.as_bytes()).unwrap_err().code;
        assert_eq!(code("<html>502</html>"), "BASES_BAD_CATALOGUE");
        assert_eq!(code(r#"{"bases": []}"#), "BASES_BAD_CATALOGUE");
        assert_eq!(code(r#"{"version": 2, "bases": "elsewhere"}"#), "BASES_CATALOGUE_NEWER");
        assert_eq!(code(r#"{"version": 0, "bases": []}"#), "BASES_BAD_CATALOGUE");
        assert!(parse(br#"{"version": 1, "bases": []}"#).unwrap().bases.is_empty());
        for (from, to) in [
            ("\"bases/hackergc-blank/1.0.0/\"", "\"bases/../../etc/\""),
            ("\"bases/hackergc-blank/1.0.0/\"", "\"/bases/x/\""),
            ("\"bases/hackergc-blank/1.0.0/\"", "\"bases/hackergc-blank/1.0.0\""),
            ("\"flags.json\"", "\"../flags.json\""),
            ("\"flags.json\"", "\"base.json\""),
            ("\"base.json\"", "\"manifest.json\""),
            ("\"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\"", "\"cc\""),
            ("\"0.1.7\"", "\"soon\""),
            ("\"id\": \"hackergc-blank\"", "\"id\": \"hacker/gc\""),
            ("\"size\": 397", "\"size\": -1"),
        ] {
            assert!(SAMPLE.contains(from), "{from}");
            assert_eq!(code(&SAMPLE.replace(from, to)), "BASES_BAD_CATALOGUE", "{to}");
        }
        let dup = format!(r#""bases": [{{"id":"hackergc-blank","version":"1.0.0","path":"a/","files":[{{"name":"base.json","size":1,"sha256":"{}"}}]}},"#, "d".repeat(64));
        let twice = SAMPLE.replace(r#""bases": ["#, &dup);
        assert_eq!(code(&twice), "BASES_BAD_CATALOGUE");
    }

    /// @LT-48/3: minApp compares as a semantic version against the app's own.
    #[test]
    fn min_app_compares_as_semver() {
        use Ordering::*;
        assert_eq!(compare_versions("0.1.7", "0.1.7"), Some(Equal));
        assert_eq!(compare_versions("0.1.10", "0.1.9"), Some(Greater));
        assert_eq!(compare_versions("0.2", "0.2.0"), Some(Equal));
        assert_eq!(compare_versions("v1.2.3", "1.2.3+build.5"), Some(Equal));
        assert_eq!(compare_versions("1.0.0-beta", "1.0.0"), Some(Less));
        assert_eq!(compare_versions("1.0.0-alpha", "1.0.0-alpha.1"), Some(Less));
        assert_eq!(compare_versions("1.0.0-alpha.1", "1.0.0-alpha.beta"), Some(Less));
        assert_eq!(compare_versions("1.0.0-beta.2", "1.0.0-beta.11"), Some(Less));
        assert_eq!(compare_versions("10.0.0", "9.99.99"), Some(Greater));
        assert_eq!(compare_versions("1.x", "1.0"), None);
        assert_eq!(compare_versions("1.2.3.4", "1.0"), None);
        assert!(app_fits("0.1.7", "0.1.7"));
        assert!(app_fits("0.1.7", "0.2.0"));
        assert!(!app_fits("0.1.8", "0.1.7"));
        assert!(!app_fits("0.2.0", "0.2.0-rc.1"));
        assert!(!app_fits("later", "0.1.7"));
        let mut b = parse(SAMPLE.as_bytes()).unwrap().bases.remove(0);
        assert!(b.fits_app("0.1.7"));
        b.min_app = "0.3.0".into();
        assert!(!b.fits_app("0.1.7"));
    }

    fn file(name: &str, bytes: &[u8]) -> PublishedFile {
        PublishedFile { name: name.into(), size: bytes.len() as u64, sha256: sha(bytes) }
    }

    /// @LT-48/1: a file counts only once its size and SHA-256 match; a short, long or wrong one is deleted.
    #[test]
    fn downloads_are_checked_before_they_count() {
        let dir = scratch("incoming");
        let body = b"BPS1 pretend patch bytes".repeat(1000);
        let f = file("demo.bps", &body);

        // Whole and right, in pieces.
        let mut inc = Incoming::create(&dir, &f).unwrap();
        for piece in body.chunks(777) {
            inc.write(piece).unwrap();
        }
        assert_eq!(inc.written(), body.len() as u64);
        assert_eq!(inc.finish().unwrap(), dir.join("demo.bps"));
        assert!(check_file(&dir, &f).unwrap());
        assert!(!dir.join("demo.bps.part").exists());

        // Cut short (the connection dropped): nothing is left.
        let g = file("short.bps", &body);
        let mut inc = Incoming::create(&dir, &g).unwrap();
        inc.write(&body[..100]).unwrap();
        assert_eq!(inc.finish().unwrap_err().code, "BASE_DOWNLOAD_SIZE");
        assert!(!dir.join("short.bps").exists() && !dir.join("short.bps.part").exists());

        // Longer than listed: refused as it arrives.
        let mut inc = Incoming::create(&dir, &g).unwrap();
        inc.write(&body).unwrap();
        assert_eq!(inc.write(b"x").unwrap_err().code, "BASE_DOWNLOAD_SIZE");
        drop(inc);
        assert!(!dir.join("short.bps.part").exists());

        // Right size, wrong bytes.
        let mut tampered = body.clone();
        tampered[5] ^= 1;
        let mut inc = Incoming::create(&dir, &g).unwrap();
        inc.write(&tampered).unwrap();
        let e = inc.finish().unwrap_err();
        assert_eq!(e.code, "BASE_DOWNLOAD_HASH");
        assert!(e.message.contains("short.bps"));
        assert!(!dir.join("short.bps").exists() && !dir.join("short.bps.part").exists());

        // Cancelled midway: dropped unfinished.
        let mut inc = Incoming::create(&dir, &g).unwrap();
        inc.write(&body[..10]).unwrap();
        assert!(dir.join("short.bps.part").exists());
        drop(inc);
        assert!(!dir.join("short.bps.part").exists());

        // A file already there but damaged is deleted so it downloads again; a missing one isn't there.
        std::fs::write(dir.join("demo.bps"), &tampered).unwrap();
        assert!(!check_file(&dir, &f).unwrap());
        assert!(!dir.join("demo.bps").exists());
        assert!(!check_file(&dir, &g).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// @LT-48/2: a complete download is an ordinary base package, and it has to be the base the catalogue named.
    #[test]
    fn a_download_opens_as_the_listed_package() {
        let dir = scratch("package");
        let mut manifest = crate::base::manifest_template("demo-base", "0.1.0");
        manifest.patch = crate::base::BasePatch { bps: "demo-base-0.1.0.bps".into(), target_sha1: "00".into() };
        let json = serde_json::to_vec_pretty(&manifest).unwrap();
        let bps = b"BPS1".to_vec();
        let entry = Published {
            id: "demo-base".into(),
            version: "0.1.0".into(),
            name: None,
            summary: None,
            min_app: "0.1.0".into(),
            published: None,
            path: "bases/demo-base/0.1.0/".into(),
            files: vec![file("base.json", &json), file("demo-base-0.1.0.bps", &bps)],
        };
        assert!(!looks_present(&dir, &entry));
        for (f, bytes) in entry.files.iter().zip([&json, &bps]) {
            let mut inc = Incoming::create(&dir, f).unwrap();
            inc.write(bytes).unwrap();
            inc.finish().unwrap();
        }
        assert!(looks_present(&dir, &entry));
        let pkg = open_downloaded(&dir, &entry).unwrap();
        assert_eq!(pkg.manifest.tag(), "demo-base@0.1.0");
        let mut other = entry.clone();
        other.version = "0.2.0".into();
        assert_eq!(open_downloaded(&dir, &other).unwrap_err().code, "BASE_DOWNLOAD_MISMATCH");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const URL: &str = "https://downloads.livingtool.app/bases/index.json";

    /// Downloads a two-file demo package (`demo-base@<version>`, with a flag catalogue) into
    /// `<root>/packages/` the way `base_download` does, saving its entry at the end.
    fn download_demo(root: &Path, version: &str) -> (PathBuf, Published) {
        let mut manifest = crate::base::manifest_template("demo-base", version);
        manifest.patch = crate::base::BasePatch { bps: format!("demo-base-{version}.bps"), target_sha1: "00".into() };
        manifest.catalogues.flags = Some("flags.json".into());
        let json = serde_json::to_vec_pretty(&manifest).unwrap();
        let (bps, flags) = (b"BPS1 demo".repeat(500), br#"{"flags": []}"#.to_vec());
        let entry = Published {
            id: "demo-base".into(),
            version: version.into(),
            name: Some(format!("Demo base {version}")),
            summary: None,
            min_app: "0.1.7".into(),
            published: Some("2026-10-18".into()),
            path: format!("bases/demo-base/{version}/"),
            files: vec![file("base.json", &json), file(&format!("demo-base-{version}.bps"), &bps), file("flags.json", &flags)],
        };
        let dir = package_dir(root, &entry);
        for (f, bytes) in entry.files.iter().zip([&json, &bps, &flags]) {
            let mut inc = Incoming::create(&dir, f).unwrap();
            inc.write(bytes).unwrap();
            inc.finish().unwrap();
        }
        open_downloaded(&dir, &entry).unwrap();
        save_entry(&dir, URL, &entry).unwrap();
        (dir, entry)
    }

    /// @LT-51/1: a finished download saves its catalogue entry; with the catalogue out of reach the
    /// package is listed, re-checked against that entry and opened, and the entry stays out of projects.
    #[test]
    fn a_download_saves_its_entry_and_opens_offline() {
        let root = scratch("offline");
        assert!(downloaded(&root).is_empty());
        let (dir, entry) = download_demo(&root, "0.1.0");
        let (_, newer) = download_demo(&root, "0.10.0");
        // A download from before LT-51 (no published.json) and an unfinished folder aren't listed.
        std::fs::create_dir_all(root.join(PACKAGES).join("old-base-1.0.0")).unwrap();
        std::fs::write(root.join(PACKAGES).join("old-base-1.0.0/base.json"), b"{}").unwrap();

        let saved = read_saved(&dir).unwrap();
        assert_eq!(saved, Saved { version: FORMAT, catalogue: URL.into(), base: entry.clone() });
        assert!(!dir.join("published.json.part").exists());

        let found = downloaded(&root);
        let tags: Vec<String> = found.iter().map(|d| d.saved.base.tag()).collect();
        assert_eq!(tags, ["demo-base@0.10.0", "demo-base@0.1.0"], "newest first, as semver");
        assert!(found.iter().all(|d| d.check.is_ok()));
        assert_eq!(found[1].dir, dir);

        let (pkg, saved) = open_saved(&root, "demo-base", "0.1.0").unwrap();
        assert_eq!((pkg.manifest.tag(), pkg.dir.as_path()), ("demo-base@0.1.0".into(), dir.as_path()));
        assert_eq!(saved.base, entry);
        assert_eq!(open_saved(&root, "demo-base", "0.10.0").unwrap().1.base, newer);

        // Set on a project: base/ gets the files base.json names, not published.json.
        let project = root.join("project-base");
        let copy = pkg.copy_to(&project).unwrap();
        assert_eq!(copy.manifest.tag(), "demo-base@0.1.0");
        let mut names: Vec<String> = std::fs::read_dir(&project).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["base.json", "demo-base-0.1.0.bps", "flags.json"]);

        // Nothing there, or a name that would leave the packages folder.
        for (id, version) in [("demo-base", "0.2.0"), ("old-base", "1.0.0"), ("..", "x"), ("demo-base", "0.1.0/../../x")] {
            assert_eq!(open_saved(&root, id, version).unwrap_err().code, "BASE_NOT_DOWNLOADED", "{id}@{version}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// @LT-51/2: a downloaded package whose files no longer match its saved entry (changed, or missing)
    /// isn't offered and won't open, and the check deletes nothing; a saved entry that doesn't read or
    /// sits in another base's folder isn't listed.
    #[test]
    fn a_changed_download_is_not_offered() {
        let root = scratch("tampered");
        let (dir, entry) = download_demo(&root, "0.1.0");
        let bps = dir.join("demo-base-0.1.0.bps");

        // Same size, one bit flipped.
        let mut bytes = std::fs::read(&bps).unwrap();
        bytes[3] ^= 1;
        std::fs::write(&bps, &bytes).unwrap();
        let found = downloaded(&root);
        assert_eq!(found.len(), 1);
        let e = found[0].check.clone().unwrap_err();
        assert_eq!(e.code, "BASE_DOWNLOAD_CHANGED");
        assert!(e.message.contains("demo-base@0.1.0's demo-base-0.1.0.bps no longer matches the SHA-256"), "{}", e.message);
        let e = open_saved(&root, "demo-base", "0.1.0").unwrap_err();
        assert_eq!(e.code, "BASE_DOWNLOAD_CHANGED");
        assert_eq!(std::fs::read(&bps).unwrap(), bytes, "the offline check changes nothing");
        // Online, the download deletes it and fetches it again.
        assert!(!check_file(&dir, &entry.files[1]).unwrap());
        assert!(!bps.exists());
        assert!(downloaded(&root)[0].check.as_ref().unwrap_err().message.contains("demo-base-0.1.0.bps is missing"));

        // An edited base.json (or flags.json) counts too.
        let (dir, _) = download_demo(&root, "0.1.0");
        assert!(verify(&dir, &entry).is_ok());
        std::fs::write(dir.join("flags.json"), br#"{"flags": [1]}"#).unwrap();
        assert_eq!(open_saved(&root, "demo-base", "0.1.0").unwrap_err().code, "BASE_DOWNLOAD_CHANGED");

        // The saved entry itself: not JSON, a newer format, unsafe names, or another base's.
        let (dir, _) = download_demo(&root, "0.1.0");
        let good = std::fs::read_to_string(dir.join(SAVED)).unwrap();
        for bad_entry in [
            "<html>".to_string(),
            good.replacen("\"version\": 1", "\"version\": 2", 1),
            good.replace("\"name\": \"flags.json\"", "\"name\": \"../flags.json\""),
            good.replace("\"version\": \"0.1.0\"", "\"version\": \"0.2.0\""),
        ] {
            assert_ne!(bad_entry, good);
            std::fs::write(dir.join(SAVED), &bad_entry).unwrap();
            assert_eq!(read_saved(&dir).unwrap_err().code, "BASE_SAVED_BAD", "{bad_entry}");
            assert!(downloaded(&root).is_empty());
            assert_eq!(open_saved(&root, "demo-base", "0.1.0").unwrap_err().code, "BASE_SAVED_BAD");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
