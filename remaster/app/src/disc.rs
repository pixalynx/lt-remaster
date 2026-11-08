//! The player's disc: checked to be the USA retail release. The core reads it through
//! `lt_emu_host::disc` (positional reads from one file handle, with a project's sectors laid over
//! it in `--host` mode), and the native view reads its assets off the same disc ([`Quiet`]).

use std::path::Path;
use std::sync::Arc;

use lt_core::release::{self, Release};
use lt_core::DiscImage;
use lt_emu_host::disc::Overlay;

/// What the check found.
pub struct Checked {
    pub title: &'static str,
}

/// Refuses anything but Guardian's Crusade (USA) with its retail EXE, with a message a player can act on.
pub fn check(path: &Path) -> Result<Checked, String> {
    let disc = DiscImage::open(path).map_err(|e| format!("{} isn't a disc image this can read: {e}", path.display()))?;
    check_image(&disc, &path.display().to_string())
}

/// [`check`] on an open image (`name` in the messages): the disc a project's build lays over the
/// player's, in `--host` mode. The native view knows the retail EXE's code and tables only (its
/// hooks are at their addresses); an EXE a project changed, or a base's, isn't that.
pub fn check_image(disc: &DiscImage, name: &str) -> Result<Checked, String> {
    let (rel, exe) = release::find_exe(disc).map_err(|e| match e {
        lt_core::LtError::OtherGame(game) => format!("{name} is another game (it boots {game}). The remaster needs Guardian's Crusade (USA)."),
        e => format!("{name} isn't Guardian's Crusade: {e}"),
    })?;
    let info = rel.info();
    if rel != Release::Usa {
        return Err(format!("{name} is {}. The remaster plays the USA release (SLUS-00811) for now.", info.title));
    }
    let bytes = disc.read_file_bytes(exe.lba, 0, exe.size as usize).map_err(|e| format!("reading {}: {e}", info.exe_name))?;
    if !release::is_usa_retail_exe(&bytes) {
        let sha1 = sha1_smol::Sha1::from(&bytes).digest().to_string();
        return Err(format!(
            "{name} is Guardian's Crusade (USA) with a changed {} (SHA-1 {sha1}). The remaster draws the retail game's code only; play a build with a changed EXE or a base on Living Tool's Play screen.",
            info.exe_name
        ));
    }
    Ok(Checked { title: info.title })
}

/// The disc the core reads, for Living Tool's readers: the same image and patches, read without
/// counting as the game's reads.
pub struct Quiet(pub Arc<Overlay>);

impl lt_core::SectorSource for Quiet {
    fn size(&self) -> u64 {
        self.0.byte_len()
    }

    fn read_at(&self, pos: u64, out: &mut [u8]) -> bool {
        self.0.read(pos, out)
    }
}
