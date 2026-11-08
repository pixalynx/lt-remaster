//! Minimal cue-sheet support: find the BINARY file a `.cue` points at.

use std::path::{Path, PathBuf};

use crate::error::{LtError, Result};

/// Returns the image to open for `path`: the `.bin` itself, or the first
/// `FILE "…" BINARY` named by a `.cue`. When that name isn't next to the cue but a file
/// differing only in punctuation or case is (a rip whose `Guardian's Crusade (EU).img` was saved
/// as `Guardian_s Crusade (EU).img`), that file.
pub fn resolve_image(path: &Path) -> Result<PathBuf> {
    let is_cue = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    if !is_cue {
        return Ok(path.to_path_buf());
    }
    let text = std::fs::read_to_string(path).map_err(|e| LtError::io(path, e))?;
    let name = text
        .lines()
        .map(str::trim)
        .filter(|l| l.to_ascii_uppercase().starts_with("FILE") && l.to_ascii_uppercase().ends_with("BINARY"))
        .find_map(parse_file_name)
        .ok_or_else(|| LtError::CueNoBinary(path.to_path_buf()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let named = dir.join(&name);
    if named.exists() {
        return Ok(named);
    }
    let want = loose(&name);
    let mut found = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && loose(&e.file_name().to_string_lossy()) == want);
    match (found.next(), found.next()) {
        (Some(only), None) => Ok(only.path()),
        _ => Ok(named),
    }
}

/// A file name with case and the characters copies tend to swap for `_` ignored.
fn loose(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || " .()[]-&,+!".contains(c) { c.to_ascii_lowercase() } else { '_' }).collect()
}

fn parse_file_name(line: &str) -> Option<String> {
    let rest = line.get(4..)?.trim();
    if let Some(stripped) = rest.strip_prefix('"') {
        return stripped.find('"').map(|end| stripped[..end].to_string());
    }
    rest.rsplit_once(' ').map(|(name, _)| name.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{loose, parse_file_name};

    #[test]
    fn punctuation_and_case_are_loose() {
        assert_eq!(loose("Guardian's Crusade (EU).img"), loose("guardian_s crusade (EU).IMG"));
        assert_ne!(loose("Guardian's Crusade (EU).img"), loose("Guardian's Crusade (EU).bin"));
    }

    #[test]
    fn quoted_and_bare_names() {
        assert_eq!(
            parse_file_name(r#"FILE "Guardian's Crusade.bin" BINARY"#).as_deref(),
            Some("Guardian's Crusade.bin")
        );
        assert_eq!(parse_file_name("FILE gc.bin BINARY").as_deref(), Some("gc.bin"));
    }
}
