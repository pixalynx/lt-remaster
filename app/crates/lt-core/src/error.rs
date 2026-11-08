use std::path::PathBuf;

/// Every error carries a stable `code` the UI maps to copy and fix actions
/// (`ARCHITECTURE.md` §6).
#[derive(Debug, thiserror::Error)]
pub enum LtError {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the cue sheet {0} names no BINARY file")]
    CueNoBinary(PathBuf),
    #[error("the image is {size} bytes, which is not a whole number of 2352-byte sectors")]
    NotRawImage { size: u64 },
    #[error("sector {lba} is past the end of the image ({sectors} sectors)")]
    SectorOutOfRange { lba: u32, sectors: u32 },
    #[error("no ISO9660 volume descriptor at sector 16")]
    NoVolumeDescriptor,
    #[error("{0} was not found on the disc")]
    FileNotFound(String),
    #[error("the disc boots {0}, which isn't Guardian's Crusade")]
    OtherGame(String),
    #[error("{0} opens read-only: Living Tool edits and builds the USA release only, for now")]
    ReadOnlyRelease(&'static str),
    #[error("the executable is not a PS-X EXE")]
    BadExe,
    #[error("address {0:#010x} is outside the executable")]
    AddressOutsideExe(u32),
    #[error("file-table entry {global} holds a byte that is not BCD ({byte:#04x})")]
    TableBadBcd { global: u16, byte: u8 },
    #[error("file-table entry {global} points past the end of the image")]
    TableOutOfBounds { global: u16 },
    #[error("unknown file-table index {0}")]
    UnknownEntry(u16),
    #[error("a project already exists at {0}")]
    ProjectExists(PathBuf),
    #[error("{0} is not a valid project file: {1}")]
    BadProjectFile(PathBuf, String),
    #[error("blob {0} is corrupt (its contents don't match its hash)")]
    BlobCorrupt(String),
}

impl LtError {
    /// Stable machine-readable code, e.g. `DISC_NOT_RAW`.
    pub fn code(&self) -> &'static str {
        match self {
            LtError::Io { .. } => "IO",
            LtError::CueNoBinary(_) => "CUE_NO_BINARY",
            LtError::NotRawImage { .. } => "DISC_NOT_RAW",
            LtError::SectorOutOfRange { .. } => "SECTOR_OUT_OF_RANGE",
            LtError::NoVolumeDescriptor => "DISC_NO_PVD",
            LtError::FileNotFound(_) => "DISC_FILE_NOT_FOUND",
            LtError::OtherGame(_) => "DISC_OTHER_GAME",
            LtError::ReadOnlyRelease(_) => "RELEASE_READ_ONLY",
            LtError::BadExe => "EXE_BAD_HEADER",
            LtError::AddressOutsideExe(_) => "EXE_ADDRESS",
            LtError::TableBadBcd { .. } => "TABLE_BAD_BCD",
            LtError::TableOutOfBounds { .. } => "TABLE_OUT_OF_BOUNDS",
            LtError::UnknownEntry(_) => "TABLE_UNKNOWN_ENTRY",
            LtError::ProjectExists(_) => "PROJECT_EXISTS",
            LtError::BadProjectFile(..) => "PROJECT_BAD_FILE",
            LtError::BlobCorrupt(_) => "BLOB_CORRUPT",
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        LtError::Io { path: path.into(), source }
    }
}

pub type Result<T> = std::result::Result<T, LtError>;
