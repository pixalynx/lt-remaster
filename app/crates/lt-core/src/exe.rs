//! The PS-X EXE container (`SLUS_008.11`; `SLES_017.93` and `SLPS_015.31` on the EU and JP
//! releases): a 0x800-byte header followed by the text segment loaded at `t_addr`.

use crate::disc::DiscImage;
use crate::error::{LtError, Result};
use crate::release::Release;

/// The USA release's EXE (the others: [`crate::release::ReleaseInfo::exe_name`]).
pub const EXE_NAME: &str = "SLUS_008.11";
const HEADER_SIZE: u32 = 0x800;

#[derive(Debug, Clone)]
pub struct PsxExe {
    pub lba: u32,
    pub bytes: Vec<u8>,
    /// Load address of the text segment (0x80173000 on retail).
    pub t_addr: u32,
    pub t_size: u32,
    pub pc0: u32,
    /// The release it was built for, by its header (USA when the header doesn't say).
    pub release: Release,
    /// Where this EXE keeps main's tables: a base built with the shift-safe build
    /// ([`crate::layout`]), or the EU or JP release's built-in map; `None` is USA retail's layout.
    pub layout: Option<std::sync::Arc<crate::layout::Layout>>,
}

impl PsxExe {
    pub fn parse(lba: u32, bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < HEADER_SIZE as usize || &bytes[..8] != b"PS-X EXE" {
            return Err(LtError::BadExe);
        }
        let u = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let release = Release::of_exe(&bytes).unwrap_or_default();
        let layout = crate::layout::for_exe(&bytes);
        Ok(Self { lba, pc0: u(0x10), t_addr: u(0x18), t_size: u(0x1C), release, layout, bytes })
    }

    /// Reads the boot EXE (`SLUS_008.11`, or the EU or JP one) from the disc via the ISO9660 root.
    pub fn from_disc(disc: &DiscImage) -> Result<Self> {
        let (_, entry) = crate::release::find_exe(disc)?;
        let bytes = disc.read_file_bytes(entry.lba, 0, entry.size as usize)?;
        Self::parse(entry.lba, bytes)
    }

    /// Where retail address `va` (a table the tools know by its USA retail address) is in this
    /// EXE: itself on USA retail, moved on a shifted base or another release. Pointers read out
    /// of the EXE are already real and don't go through this.
    pub fn va(&self, retail: u32) -> u32 {
        self.layout.as_ref().map_or(retail, |l| l.map(retail))
    }

    /// [`PsxExe::va`] for a table in code overlay `ovr` (OVRnn), known by its USA address: where
    /// the release this EXE belongs to keeps it in that overlay.
    pub fn ovr_va(&self, ovr: u8, retail: u32) -> u32 {
        self.layout.as_ref().map_or(retail, |l| l.map_overlay(ovr, retail))
    }

    /// File offset of a RAM address: `va − t_addr + 0x800`.
    pub fn vaddr_to_offset(&self, va: u32) -> Result<usize> {
        let off = va
            .checked_sub(self.t_addr)
            .map(|d| d + HEADER_SIZE)
            .filter(|&o| (o as usize) < self.bytes.len())
            .ok_or(LtError::AddressOutsideExe(va))?;
        Ok(off as usize)
    }

    pub fn slice(&self, va: u32, len: usize) -> Result<&[u8]> {
        let off = self.vaddr_to_offset(va)?;
        self.bytes.get(off..off + len).ok_or(LtError::AddressOutsideExe(va))
    }
}
