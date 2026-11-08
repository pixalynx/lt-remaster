//! Streamed music on a HackerGC base (`gc_decomp/src/hgc/hgc_music.c`, screen 14 §14): which BGM
//! songs play from a stream file, as the EXE's `g_HgcMusic` table names them (the layout's
//! `music`), and the plan that streams a song or takes its stream out again.
//!
//! - `g_HgcMusic`: `{u8 bgm, u8 cue, u8 spare, u8 flags}` entries; cue 0 is an empty one. The game
//!   asks for a song as `0x40400000 | bank << 8 | cue`, and the player looks the cue up against the
//!   BGM file the bank holds.
//! - A stream file is spare file `new/N` ([`crate::audio::stream`]).
//! - Bank 2 (an area's or a battle's music, bgm/1–56 and 58–60) plays its stream from its own SPU
//!   region, so the BGM file is slimmed: the song's sequence out, the samples only it used out.
//! - The common bank (bgm/0: the overworld, the jingles) is slimmed the same way, but its songs
//!   borrow bank 2's region (`RING2`; the field reloads bank 2 after them), so the room its songs
//!   leave goes to sound effects every area can play.
//! - bgm/57, the flight's bank, isn't streamed: the flight uses both banks.

use serde::{Deserialize, Serialize};

use super::{EditError, Result};
use crate::audio::stream::{self, Ring};
use crate::audio::tsq;
use crate::container::le32;

/// `HgcMusicEntry.flags`: a common-bank song that plays from bank 2's region.
pub const RING2: u8 = 0x01;
/// BGM files: `bgm/0` to `bgm/60`.
pub const BGM_FILES: u8 = 61;
/// The flight's music bank, which can't be streamed.
pub const FLIGHT_BGM: u8 = 57;

fn bad(code: &'static str, message: impl Into<String>) -> EditError {
    EditError::new(code, message)
}

/// One streamed song.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub bgm: u8,
    pub cue: u8,
    /// The spare file (`new/N`) holding the stream.
    pub spare: u8,
    pub flags: u8,
}

fn offset(exe: &[u8], va: u32, len: usize) -> Option<usize> {
    let t_addr = le32(exe.get(..0x20)?, 0x18);
    let off = va.checked_sub(t_addr)? as usize + 0x800;
    (off + len <= exe.len()).then_some(off)
}

/// The table's place in the EXE and its entry count.
fn table(exe: &[u8]) -> Option<(usize, usize)> {
    let l = crate::layout::for_exe(exe)?;
    l.spare_files?;
    let (va, count) = l.music?;
    Some((offset(exe, va, count as usize * 4)?, count as usize))
}

/// Whether this EXE streams music (a HackerGC base with the music player).
pub fn supported(exe: &[u8]) -> bool {
    table(exe).is_some()
}

/// How many songs the table can name.
pub fn capacity(exe: &[u8]) -> u32 {
    table(exe).map_or(0, |(_, n)| n as u32)
}

/// The streamed songs, in table order.
pub fn entries(exe: &[u8]) -> Vec<Entry> {
    let Some((at, n)) = table(exe) else { return Vec::new() };
    (0..n)
        .map(|k| &exe[at + 4 * k..at + 4 * k + 4])
        .filter(|e| e[1] != 0)
        .map(|e| Entry { bgm: e[0], cue: e[1], spare: e[2], flags: e[3] })
        .collect()
}

/// `exe` with `bgm` `cue` streaming from `spare` (`None`: its entry taken out).
pub fn set(exe: &[u8], bgm: u8, cue: u8, spare: Option<(u8, u8)>) -> Result<Vec<u8>> {
    let (at, n) = table(exe).ok_or_else(needs_hackergc)?;
    if cue == 0 {
        return Err(bad("MUSIC_CUE", "Cue 0 is the sequence bank's dummy."));
    }
    let slot = |k: usize| &exe[at + 4 * k..at + 4 * k + 4];
    let mine = (0..n).find(|&k| slot(k)[1] == cue && slot(k)[0] == bgm);
    let k = match (mine, spare) {
        (Some(k), _) => k,
        (None, None) => return Ok(exe.to_vec()),
        (None, Some(_)) => (0..n).find(|&k| slot(k)[1] == 0).ok_or_else(|| bad("MUSIC_FULL", format!("The base streams at most {n} songs; take one out first.")))?,
    };
    let mut out = exe.to_vec();
    out[at + 4 * k..at + 4 * k + 4].copy_from_slice(&match spare {
        Some((s, f)) => [bgm, cue, s, f],
        None => [0; 4],
    });
    Ok(out)
}

pub fn needs_hackergc() -> EditError {
    bad("MUSIC_NEEDS_HACKERGC", "Streamed music needs a HackerGC base with the music player (hackergc-blank 1.1 or later). Put the project on one first.")
}

/// The sound bank a BGM file loads into: 1 for the common bank (bgm/0, and the flight's bgm/57),
/// 2 for the rest.
pub fn bank_of(bgm: u8) -> u8 {
    if bgm == 0 || bgm == FLIGHT_BGM { 1 } else { 2 }
}

/// The song cues of a BGM file (its TSQ's cues with a 24-track table).
pub fn songs(bgm_file: &[u8]) -> Vec<u8> {
    let Some(seq) = sequences(bgm_file) else { return Vec::new() };
    tsq::cues(seq).into_iter().filter(|c| c.kind == tsq::CueKind::Song).map(|c| c.index as u8).collect()
}

fn sequences(file: &[u8]) -> Option<&[u8]> {
    if file.len() < 8 {
        return None;
    }
    let (a, b) = (le32(file, 0) as usize, le32(file, 4) as usize);
    file.get(a..b)
}

/// Whether `bgm` `cue` can stream: a song of the base's file, in a bank the player serves.
pub fn check_song(bgm: u8, cue: u8, reference: &[u8]) -> Result<()> {
    if bgm >= BGM_FILES {
        return Err(bad("MUSIC_BGM", format!("bgm/{bgm} isn't a music file (bgm/0 to bgm/{}).", BGM_FILES - 1)));
    }
    if bgm == FLIGHT_BGM {
        return Err(bad("MUSIC_FLIGHT", "bgm/57 is the flight's music bank, which uses both of the game's music banks at once; it can't be streamed."));
    }
    if !songs(reference).contains(&cue) {
        return Err(bad("MUSIC_NOT_A_SONG", format!("Cue {cue} of bgm/{bgm} isn't a song (songs: {:?}).", songs(reference))));
    }
    Ok(())
}

/// The bank a set of streamed songs of one BGM file leaves, and the ring they play from: bank 2's
/// file slimmed from `reference` (the base's file, so a song taken back gets its sequence and
/// samples again), the common bank's left as it is.
///
/// The common bank is slimmed too, though its songs play from bank 2's region: what they leave is
/// room for sound effects every area can play (`crate::edit::sound::add_sound`).
pub fn bank_for(bgm: u8, reference: &[u8], cues: &[u8]) -> Result<(Option<Vec<u8>>, Ring, u8)> {
    let flags = if bank_of(bgm) == 1 { RING2 } else { 0 };
    if cues.is_empty() {
        return Ok((Some(reference.to_vec()), Ring::BANK2, flags));
    }
    let cues: Vec<usize> = cues.iter().map(|&c| c as usize).collect();
    if flags == RING2 {
        let (bank, _, _) = stream::slim(reference, &cues).map_err(|m| bad("MUSIC_BANK", m))?;
        return Ok((Some(bank), Ring::BANK2, RING2));
    }
    let (bank, ring) = stream::slim_bank(reference, &cues).map_err(|m| bad("MUSIC_BANK", m))?;
    Ok((Some(bank), ring, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banks_and_files() {
        assert_eq!(bank_of(0), 1);
        assert_eq!(bank_of(57), 1);
        assert_eq!(bank_of(12), 2);
        assert!(check_song(57, 1, &[]).is_err());
        assert!(check_song(61, 1, &[]).is_err());
        assert!(matches!(bank_for(0, &[1, 2], &[]), Ok((Some(f), r, RING2)) if r == Ring::BANK2 && f == [1, 2]));
    }
}
