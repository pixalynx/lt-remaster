//! Game data tables (screen 13, `docs/screens/13-game-data-editor.md` §4):
//! enemy profiles, items and the drop remap, the Living Toy roster, shops,
//! encounter tables and party growth. Everything is decoded read-only from the
//! retail image; offsets and layouts come from the decomp.
//!
//! Address conventions: EXE offset = `va − 0x80173000 + 0x800`; OVR02/OVR04
//! load at 0x801DF000, so their file offset = `va − 0x801DF000`. Tables are named by
//! their USA address; the EU and JP releases keep them elsewhere (`PsxExe::va` for the
//! EXE, `PsxExe::ovr_va` inside an overlay, [`crate::release`]) and the Japanese one
//! holds Shift-JIS names.

pub mod encounters;
pub mod enemy;
pub mod growth;
pub mod items;
pub mod modules;
pub mod shops;
pub mod toys;

use serde::Serialize;

use crate::container::le32;
use crate::disc::DiscImage;
use crate::error::LtError;
use crate::exe::PsxExe;
use crate::fs::FileTable;
use crate::overlay::{self, Staged};
use crate::text::{self, Encoding};

/// Load address of the battle/field overlays OVR02 and OVR04 (the same in every release).
pub const OVR_LOAD: u32 = 0x801D_F000;

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error(transparent)]
    Lt(#[from] LtError),
    #[error("{0}")]
    Format(String),
}

impl DataError {
    /// Stable code for the UI (`LtError` codes, or `DATA_BAD_FORMAT`).
    pub fn code(&self) -> &'static str {
        match self {
            DataError::Lt(e) => e.code(),
            DataError::Format(_) => "DATA_BAD_FORMAT",
        }
    }
}

pub type Result<T> = std::result::Result<T, DataError>;

pub(crate) fn bad(msg: impl Into<String>) -> DataError {
    DataError::Format(msg.into())
}

/// The parts of an opened disc every table reader needs. With `staged`, files
/// are read as the open project sees them (its edited copies first); pass the
/// project's EXE as `exe` when the EXE is staged.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub image: &'a DiscImage,
    pub exe: &'a PsxExe,
    pub table: &'a FileTable,
    pub staged: Option<&'a Staged>,
    /// The disc's EXE (string slots are measured on it); `exe` unless set.
    pub retail_exe: &'a PsxExe,
}

impl<'a> Source<'a> {
    /// The retail disc only.
    pub fn new(image: &'a DiscImage, exe: &'a PsxExe, table: &'a FileTable) -> Self {
        Self { image, exe, table, staged: None, retail_exe: exe }
    }

    /// The disc's EXE, when `exe` is a project's.
    pub fn with_retail_exe(self, retail_exe: &'a PsxExe) -> Self {
        Self { retail_exe, ..self }
    }

    /// The disc seen through a project's staged files.
    pub fn with_staged(self, staged: Option<&'a Staged>) -> Self {
        Self { staged, ..self }
    }

    /// Size of a file in bytes (whole sectors on the disc; a staged copy's length).
    pub fn file_len(&self, id: &str) -> Result<u32> {
        Ok(overlay::file_len(self.table, self.staged, id)?)
    }

    /// A whole file by canonical id (`ovr/4`, `sys/9`, …).
    pub fn file(&self, id: &str) -> Result<Vec<u8>> {
        Ok(overlay::file(self.image, self.table, self.staged, id)?)
    }

    /// `len` bytes at `offset` inside a file, clipped to the file's end.
    pub fn file_part(&self, id: &str, offset: u32, len: usize) -> Result<Vec<u8>> {
        let size = self.file_len(id)? as usize;
        if offset as usize > size {
            return Err(bad(format!("{id}: offset {offset:#x} is past the end ({size:#x})")));
        }
        Ok(overlay::file_part(self.image, self.table, self.staged, id, offset, len)?)
    }

    /// Bytes of the EXE at a RAM address.
    pub fn exe_bytes(&self, va: u32, len: usize) -> Result<&'a [u8]> {
        Ok(self.exe.slice(va, len)?)
    }

    /// A NUL-terminated EXE string behind a `char*` (Latin-1; Shift-JIS on the Japanese
    /// release). `None` for null or out-of-range pointers.
    pub fn exe_str(&self, va: u32) -> Option<String> {
        if va == 0 {
            return None;
        }
        let off = self.exe.vaddr_to_offset(va).ok()?;
        let rest = &self.exe.bytes[off..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        Some(match self.encoding() {
            Encoding::Ascii => latin1(&rest[..end]),
            Encoding::ShiftJis => text::sjis(&rest[..end]),
        })
    }

    /// How this release encodes names.
    pub fn encoding(&self) -> Encoding {
        self.exe.release.info().text
    }

    pub fn exe_u32(&self, va: u32) -> Result<u32> {
        Ok(le32(self.exe_bytes(va, 4)?, 0))
    }
}

pub(crate) fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// [`fixed_str`] in a release's encoding: Shift-JIS on the Japanese release.
pub(crate) fn fixed_str_in(b: &[u8], enc: Encoding) -> String {
    match enc {
        Encoding::Ascii => fixed_str(b),
        Encoding::ShiftJis => text::sjis(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]),
    }
}

/// A fixed-size char field up to its first NUL. ASCII stays as is; Shift-JIS
/// kana (the "ダミー" leftovers in toy files) are decoded; anything else
/// becomes U+FFFD.
pub(crate) fn fixed_str(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let b = &b[..end];
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c < 0x80 {
            out.push(c as char);
            i += 1;
            continue;
        }
        let Some(&c2) = b.get(i + 1) else {
            out.push('\u{FFFD}');
            break;
        };
        out.push(sjis_kana(c, c2).unwrap_or('\u{FFFD}'));
        i += 2;
    }
    out
}

/// Shift-JIS hiragana (0x829F–0x82F1), katakana (0x8340–0x8396) and the long
/// vowel mark 0x815B.
fn sjis_kana(c1: u8, c2: u8) -> Option<char> {
    let cp = match (c1, c2) {
        (0x81, 0x5B) => 0x30FC,
        (0x82, 0x9F..=0xF1) => 0x3041 + (c2 as u32 - 0x9F),
        (0x83, 0x40..=0x7E) => 0x30A1 + (c2 as u32 - 0x40),
        (0x83, 0x80..=0x96) => 0x30A1 + (c2 as u32 - 0x41),
        _ => return None,
    };
    char::from_u32(cp)
}

/// Every table on screen 13, decoded once per disc.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameData {
    pub enemies: Vec<enemy::EnemyProfile>,
    pub items: items::ItemTable,
    pub toys: toys::Toys,
    pub shops: shops::Shops,
    pub encounters: encounters::Encounters,
    pub growth: growth::Growth,
}

impl GameData {
    pub fn load(src: Source<'_>) -> Result<Self> {
        let ovr04 = src.file("ovr/4")?;
        let effects = modules::EffectTable::parse_in(src.exe, &ovr04);
        let encounters = encounters::Encounters::load(src)?;
        let enemies = enemy::load_all(src, &effects, &encounters)?;
        Ok(Self {
            items: items::ItemTable::load(src, &ovr04)?,
            toys: toys::Toys::load(src)?,
            shops: shops::Shops::load(src)?,
            growth: growth::Growth::load(src)?,
            enemies,
            encounters,
        })
    }

    /// `enemy/n` (2..=111).
    pub fn enemy(&self, index: u16) -> Option<&enemy::EnemyProfile> {
        self.enemies.iter().find(|e| e.id == index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_sjis_leftovers() {
        assert_eq!(fixed_str(b"\x83_\x83~\x81[\0\0"), "ダミー");
        assert_eq!(fixed_str(b"\x82\xc8\x82\xb5"), "なし");
        assert_eq!(fixed_str(b"\x83C\x83R\x81[\x83\x8b\x83o\x83\x89\x83\x93\x83X"), "イコールバランス");
        assert_eq!(fixed_str(b"Holy Light\0junk"), "Holy Light");
        assert_eq!(fixed_str_in(b"\x83\x69\x83\x43\x83\x67\0\0", Encoding::ShiftJis), "ナイト");
        assert_eq!(fixed_str_in(b"\x97\xb3\0", Encoding::ShiftJis), "竜");
    }
}
