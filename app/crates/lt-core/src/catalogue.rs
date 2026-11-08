//! A project's own flag names (research 06 §9, phase 0): what each event flag
//! means in the story the project tells. It sits on top of the curated retail
//! catalogue (`src/data/flag-catalogue.json`, [`crate::flags::meanings`]):
//!
//! - `base: retail` keeps the retail meanings and lets the project rename or
//!   add flags;
//! - `base: empty` drops the retail story's meanings (a blank slate) and
//!   keeps only what the engine itself does with a flag
//!   ([`crate::engine`]), plus the project's own names.
//!
//! Stored as `catalogue.json` in the `.ltproj` folder; keys are the
//! catalogue's (`"0x081"`, or `"0x004@npc/7"` for a scratch flag in one pack).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::edit::EditError;
use crate::error::Result;
use crate::project::{read_json, write_json, Project};

/// The file in the project folder.
pub const FILE: &str = "catalogue.json";
/// Longest name and summary (the catalogue's limits).
pub const NAME_MAX: usize = 40;
pub const SUMMARY_MAX: usize = 60;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Base {
    #[default]
    Retail,
    Empty,
}

/// One flag as the project names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub summary: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalogue {
    #[serde(default)]
    pub base: Base,
    #[serde(default)]
    pub flags: BTreeMap<String, Entry>,
}

fn path(root: &Path) -> std::path::PathBuf {
    root.join(FILE)
}

/// The project's catalogue (empty, on retail, when it has none).
pub fn load(project: &Project) -> Catalogue {
    let p = path(&project.root);
    if p.exists() {
        read_json(&p).unwrap_or_default()
    } else {
        Catalogue::default()
    }
}

/// Saves `c` and notes it in the project's history.
pub fn save(project: &Project, c: &Catalogue) -> Result<()> {
    write_json(&path(&project.root), c)?;
    let mut fields = serde_json::Map::new();
    fields.insert("base".into(), serde_json::to_value(c.base).unwrap_or_default());
    fields.insert("names".into(), c.flags.len().into());
    project.append_history("catalogue", fields)
}

/// A base's flag catalogue (`base.json` `catalogues.flags`, the shape of
/// `src/data/flag-catalogue.json`) as a project catalogue: the base's story
/// replaces retail's (`base: empty`) and its names become the project's.
/// Entries whose key or name the catalogue doesn't accept are skipped.
pub fn from_base(path: &Path) -> Result<Catalogue> {
    #[derive(Deserialize)]
    struct RawEntry {
        name: String,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        detail: String,
    }
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        flags: BTreeMap<String, RawEntry>,
    }
    let raw: Raw = read_json(path)?;
    let mut c = Catalogue { base: Base::Empty, flags: BTreeMap::new() };
    for (k, e) in raw.flags {
        let summary: String = e.summary.chars().take(SUMMARY_MAX).collect();
        let entry = Entry { name: e.name, summary, detail: e.detail };
        if let (Some((flag, pack)), Ok(())) = (parse_key(&k), check(&k, &entry)) {
            c.flags.insert(key(flag, pack), entry);
        }
    }
    Ok(c)
}

/// `"0x081"` → (0x81, None); `"0x004@npc/7"` → (4, Some(7)).
pub fn parse_key(key: &str) -> Option<(u16, Option<u16>)> {
    let (flag, pack) = match key.split_once("@npc/") {
        Some((f, p)) => (f, Some(p.parse().ok()?)),
        None => (key, None),
    };
    let n = u16::from_str_radix(flag.strip_prefix("0x")?, 16).ok()?;
    (n < crate::flags::count()).then_some((n, pack))
}

/// The key of a flag, as the catalogue writes it.
pub fn key(flag: u16, pack: Option<u16>) -> String {
    match pack {
        Some(p) => format!("0x{flag:03X}@npc/{p}"),
        None => format!("0x{flag:03X}"),
    }
}

/// Checks a key and an entry: a real flag; a name of letters, digits and
/// `_` (the catalogue's names read as identifiers); short summaries.
pub fn check(k: &str, e: &Entry) -> std::result::Result<(), EditError> {
    let bad = |m: String| EditError::new("CATALOGUE_ENTRY", m);
    let Some((flag, pack)) = parse_key(k) else { return Err(bad(format!("“{k}” isn't a flag key (0x000–0x{:03X}, optionally @npc/n).", crate::flags::count() - 1))) };
    if pack.is_some() && flag >= 0x40 {
        return Err(bad(format!("Only scratch flags (0x000–0x03F) mean something per pack; name 0x{flag:03X} for the whole game.")));
    }
    if e.name.is_empty() || e.name.len() > NAME_MAX || !e.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || e.name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(bad(format!("A name is 1–{NAME_MAX} letters, digits or _, not starting with a digit (“{}”).", e.name)));
    }
    if e.summary.chars().count() > SUMMARY_MAX {
        return Err(bad(format!("Keep the summary to {SUMMARY_MAX} characters ({} now).", e.summary.chars().count())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_entries() {
        assert_eq!(parse_key("0x081"), Some((0x81, None)));
        assert_eq!(parse_key("0x004@npc/7"), Some((4, Some(7))));
        assert_eq!(parse_key("0x200"), None);
        assert_eq!(parse_key("81"), None);
        assert_eq!(key(4, Some(7)), "0x004@npc/7");
        let e = |n: &str, s: &str| Entry { name: n.into(), summary: s.into(), detail: String::new() };
        assert!(check("0x081", &e("met_the_mayor", "The Mayor gave the first task")).is_ok());
        assert!(check("0x081", &e("met the mayor", "")).is_err());
        assert!(check("0x081", &e("9lives", "")).is_err());
        assert!(check("0x081@npc/7", &e("x", "")).is_err(), "only scratch flags are per pack");
        assert!(check("0x081", &e("x", &"s".repeat(61))).is_err());
        let c = Catalogue { base: Base::Empty, flags: [("0x081".to_string(), e("a", "b"))].into() };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"base\":\"empty\""));
        assert_eq!(serde_json::from_str::<Catalogue>(&json).unwrap(), c);
        assert_eq!(serde_json::from_str::<Catalogue>("{}").unwrap(), Catalogue::default());
    }
}
