//! Sequenced audio: TSQ sequence banks, TVB voice banks and SPU-ADPCM.
//! Every sound in the game is one of these pairs — 61 BGM files, the system
//! and field SE banks, nine NPC special-sound files and the banks embedded in
//! Living Toy, enemy and Baby-form packages. See `docs/screens/14-audio.md`.

pub mod adpcm;
pub mod adsr;
pub mod bank;
pub mod decode;
pub mod midi;
pub mod stream;
pub mod tsq;
pub mod tvb;
pub mod view;
pub mod wav;

pub use bank::{catalog, split, Bank, BankKind, BankRef};
pub use view::{bank_view, row, BankRow, BankView};

/// RAM address of the note → SPU pitch table (128 × u16; note 48 = 0x1000).
pub const PITCH_TABLE_VADDR: u32 = 0x801B_7518;

/// The pitch table from the EXE.
pub fn pitch_table(exe: &crate::PsxExe) -> crate::Result<Vec<u16>> {
    let b = exe.slice(exe.va(PITCH_TABLE_VADDR), 256)?;
    Ok(b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect())
}
