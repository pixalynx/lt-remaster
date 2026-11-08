//! The ISO9660 view of the disc. In Guardian's Crusade it is a decoy: the root
//! holds `SYSTEM.CNF`, `SLUS_008.11` and 13 empty folders. We only need it to
//! find the executable and to show the decoy in the Files screen.

use serde::Serialize;

use crate::disc::{DiscImage, USER_DATA};
use crate::error::{LtError, Result};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    pub system_id: String,
    pub volume_id: String,
    /// Logical blocks declared in the PVD (185,985 on retail).
    pub volume_space_size: u32,
    pub root_lba: u32,
    pub root_size: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub lba: u32,
    pub size: u32,
    pub is_dir: bool,
}

pub fn read_volume(disc: &DiscImage) -> Result<VolumeInfo> {
    let pvd = disc.read_user(16)?;
    if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return Err(LtError::NoVolumeDescriptor);
    }
    let text = |a: usize, b: usize| String::from_utf8_lossy(&pvd[a..b]).trim().to_string();
    Ok(VolumeInfo {
        system_id: text(8, 40),
        volume_id: text(40, 72),
        volume_space_size: le32(&pvd, 80),
        root_lba: le32(&pvd, 156 + 2),
        root_size: le32(&pvd, 156 + 10),
    })
}

/// Lists a directory, skipping the `.` and `..` records.
pub fn read_dir(disc: &DiscImage, lba: u32, size: u32) -> Result<Vec<DirEntry>> {
    let sectors = size.div_ceil(USER_DATA as u32);
    let data = disc.read_sectors(lba, sectors)?;
    let mut entries = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let len = data[pos] as usize;
        if len == 0 {
            // Records never straddle sectors; skip the padding.
            pos = (pos / USER_DATA + 1) * USER_DATA;
            continue;
        }
        let rec = &data[pos..pos + len];
        let name_len = rec[32] as usize;
        let raw_name = &rec[33..33 + name_len];
        if !(name_len == 1 && (raw_name[0] == 0 || raw_name[0] == 1)) {
            let name = String::from_utf8_lossy(raw_name);
            entries.push(DirEntry {
                name: name.split(';').next().unwrap_or_default().to_string(),
                lba: le32(rec, 2),
                size: le32(rec, 10),
                is_dir: rec[25] & 0x02 != 0,
            });
        }
        pos += len;
    }
    Ok(entries)
}

/// Finds a file in the root directory by name (case-insensitive, no version).
pub fn find_root_file(disc: &DiscImage, name: &str) -> Result<DirEntry> {
    let vol = read_volume(disc)?;
    read_dir(disc, vol.root_lba, vol.root_size)?
        .into_iter()
        .find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| LtError::FileNotFound(name.to_string()))
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
