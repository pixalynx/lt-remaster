//! Raw MODE2/2352 image access. The image is only ever opened for reading.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{LtError, Result};

pub const RAW_SECTOR: usize = 2352;
pub const USER_DATA: usize = 2048;
/// Sync (12) + header (4) + Mode 2 subheader (8).
pub const USER_DATA_OFFSET: usize = 24;

/// Raw image bytes from somewhere other than a file of their own: an emulator's disc, with the
/// sectors a project's build writes laid over the image (Living Tool Remaster reads its assets off
/// the disc the game runs, edits included).
pub trait SectorSource: Send + Sync {
    /// The image's length in bytes (whole 2352-byte sectors).
    fn size(&self) -> u64;
    /// Fills `out` with the image's bytes from `pos`; false when they can't be read.
    fn read_at(&self, pos: u64, out: &mut [u8]) -> bool;
}

enum Backing {
    File(File),
    Source(Arc<dyn SectorSource>),
}

impl std::fmt::Debug for Backing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backing::File(file) => file.fmt(f),
            Backing::Source(_) => f.write_str("SectorSource"),
        }
    }
}

/// A read-only handle on a raw `.bin` image. Reads are positional, so one
/// handle can be shared across threads.
#[derive(Debug)]
pub struct DiscImage {
    path: PathBuf,
    file: Backing,
    size: u64,
}

impl DiscImage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|e| LtError::io(&path, e))?;
        let size = file.metadata().map_err(|e| LtError::io(&path, e))?.len();
        if size == 0 || size % RAW_SECTOR as u64 != 0 {
            return Err(LtError::NotRawImage { size });
        }
        Ok(Self { path, file: Backing::File(file), size })
    }

    /// An image read through `source` (named `path` in messages).
    pub fn from_source(path: impl AsRef<Path>, source: Arc<dyn SectorSource>) -> Result<Self> {
        let size = source.size();
        if size == 0 || !size.is_multiple_of(RAW_SECTOR as u64) {
            return Err(LtError::NotRawImage { size });
        }
        Ok(Self { path: path.as_ref().to_path_buf(), file: Backing::Source(source), size })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn sectors(&self) -> u32 {
        (self.size / RAW_SECTOR as u64) as u32
    }

    /// The full 2352-byte sector.
    pub fn read_raw(&self, lba: u32) -> Result<[u8; RAW_SECTOR]> {
        self.check(lba)?;
        let mut buf = [0u8; RAW_SECTOR];
        self.read_at(lba as u64 * RAW_SECTOR as u64, &mut buf)?;
        Ok(buf)
    }

    /// The 2048 bytes of Form 1 user data.
    pub fn read_user(&self, lba: u32) -> Result<[u8; USER_DATA]> {
        self.check(lba)?;
        let mut buf = [0u8; USER_DATA];
        self.read_at(lba as u64 * RAW_SECTOR as u64 + USER_DATA_OFFSET as u64, &mut buf)?;
        Ok(buf)
    }

    /// `len` bytes of user data starting at `offset` within the file at `lba`.
    pub fn read_file_bytes(&self, lba: u32, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut sector = lba + (offset / USER_DATA as u64) as u32;
        let mut skip = (offset % USER_DATA as u64) as usize;
        while out.len() < len {
            let data = self.read_user(sector)?;
            let take = (USER_DATA - skip).min(len - out.len());
            out.extend_from_slice(&data[skip..skip + take]);
            skip = 0;
            sector += 1;
        }
        Ok(out)
    }

    /// Reads `sectors` whole sectors of user data.
    pub fn read_sectors(&self, lba: u32, sectors: u32) -> Result<Vec<u8>> {
        self.read_file_bytes(lba, 0, sectors as usize * USER_DATA)
    }

    fn check(&self, lba: u32) -> Result<()> {
        if lba >= self.sectors() {
            return Err(LtError::SectorOutOfRange { lba, sectors: self.sectors() });
        }
        Ok(())
    }

    fn read_at(&self, pos: u64, buf: &mut [u8]) -> Result<()> {
        match &self.file {
            Backing::File(f) => read_exact_at(f, buf, pos).map_err(|e| LtError::io(&self.path, e)),
            Backing::Source(s) if s.read_at(pos, buf) => Ok(()),
            Backing::Source(_) => Err(LtError::io(&self.path, std::io::ErrorKind::UnexpectedEof.into())),
        }
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], pos: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, pos)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut pos: u64) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Three sectors in memory, sector n's user data all n.
    struct Memory(Vec<u8>);

    impl SectorSource for Memory {
        fn size(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&self, pos: u64, out: &mut [u8]) -> bool {
            let at = pos as usize;
            match self.0.get(at..at + out.len()) {
                Some(b) => {
                    out.copy_from_slice(b);
                    true
                }
                None => false,
            }
        }
    }

    #[test]
    fn an_image_reads_through_a_source() {
        let bytes: Vec<u8> = (0..3u8).flat_map(|n| std::iter::repeat_n(n, RAW_SECTOR)).collect();
        let disc = DiscImage::from_source("memory", Arc::new(Memory(bytes))).unwrap();
        assert_eq!(disc.sectors(), 3);
        assert_eq!(disc.read_user(2).unwrap()[0], 2);
        assert_eq!(disc.read_file_bytes(1, USER_DATA as u64 - 1, 2).unwrap(), vec![1, 2]);
        assert!(disc.read_raw(3).is_err());
        assert!(DiscImage::from_source("short", Arc::new(Memory(vec![0; 100]))).is_err());
    }
}
