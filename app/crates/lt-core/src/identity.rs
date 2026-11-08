//! Disc fingerprinting: CRC32 and SHA-1 of the whole image, as Redump lists them.
//! The known good dump of each release is in [`crate::release`].

use std::io::Read;

use serde::Serialize;

use crate::disc::DiscImage;
use crate::error::{LtError, Result};
use crate::release::Release;

/// Redump "Guardian's Crusade (USA)", the release Living Tool edits.
pub const REDUMP_CRC32: u32 = 0xcdc7_2c62;
pub const RETAIL_SECTORS: u32 = 185_985;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub crc32: String,
    pub sha1: String,
    pub size: u64,
    pub sectors: u32,
    /// The image is a release's known good dump ([`crate::release::ReleaseInfo::crc32`]).
    pub matches_redump: bool,
    /// Which release's dump, when it is one.
    pub release: Option<Release>,
    /// Taken from the open project's record of its disc
    /// (`DiscRef::trusted_fingerprint`), not hashed in this session.
    pub from_project: bool,
}

/// Hashes the image in 4 MB chunks. `progress(done, total)` is called after
/// each chunk; return `false` from it to cancel.
pub fn fingerprint(disc: &DiscImage, mut progress: impl FnMut(u64, u64) -> bool) -> Result<Option<Fingerprint>> {
    let mut file = std::fs::File::open(disc.path()).map_err(|e| LtError::io(disc.path(), e))?;
    let total = disc.size();
    let mut crc = crc32fast::Hasher::new();
    let mut sha = sha1_smol::Sha1::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        let n = file.read(&mut buf).map_err(|e| LtError::io(disc.path(), e))?;
        if n == 0 {
            break;
        }
        crc.update(&buf[..n]);
        sha.update(&buf[..n]);
        done += n as u64;
        if !progress(done, total) {
            return Ok(None);
        }
    }
    let crc32 = crc.finalize();
    Ok(Some(Fingerprint {
        crc32: format!("{crc32:08x}"),
        sha1: sha.digest().to_string(),
        size: total,
        sectors: disc.sectors(),
        matches_redump: Release::of_crc32(crc32).is_some(),
        release: Release::of_crc32(crc32),
        from_project: false,
    }))
}
