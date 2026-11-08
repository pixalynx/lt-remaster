//! Movies replaced (screen 14 §13, research 06 phase 1 "FMV Form 2"): a
//! movie-table file re-made from PNG frames and a WAV
//! ([`crate::movie::encode::mux`]), or taken from an STR file someone else
//! made, staged as 2336-byte Mode 2 payloads the build writes as they are
//! (Form 1 video, Form 2 XA audio).
//!
//! The player stops at the frame number OVR01 `D_80013B60` +0x08 names
//! ([`crate::movie::PlayerSettings::last_frame`]), so every movie-table
//! entry reading the file (aliases included) gets the new last frame.

use serde::{Deserialize, Serialize};

use crate::cdrom::{self, PAYLOAD};
use crate::disc::RAW_SECTOR;
use crate::movie::encode::{self, Fit, Layout};
use crate::movie::{self, Movie, Payload, Sectors, OVR01_VADDR, SETTINGS_VADDR};

use super::{EditError, Result};

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieOptions {
    /// How frames of another size go into the movie's picture.
    #[serde(default)]
    pub fit: Fit,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieReplaced {
    /// "frames" (encoded here) or "str" (taken as it is).
    pub source: &'static str,
    pub width: u16,
    pub height: u16,
    pub frames: u32,
    pub sectors: u32,
    pub sectors_before: u32,
    pub seconds: f64,
    pub audio_seconds: Option<f64>,
    pub qscale_min: u16,
    pub qscale_max: u16,
    /// Average bitstream bytes per frame.
    pub frame_bytes: u32,
    /// The middle frame decoded against its source picture (frames only).
    pub psnr: Option<f64>,
    /// Movie-table entries whose player record gets the new last frame.
    pub entries: Vec<u16>,
    pub last_frame_before: u32,
    /// The extent grows past the old one: the build moves the file to the end of the disc.
    pub moves: bool,
}

/// PNG frames (in order) and an optional WAV, encoded at the old movie's
/// picture size and layout. Returns the payload and what it holds.
pub fn from_frames(old: &Movie, src: &impl Sectors, pngs: &[Vec<u8>], wav: Option<&[u8]>, opt: &MovieOptions) -> Result<(Vec<u8>, MovieReplaced)> {
    if pngs.is_empty() {
        return Err(bad("MOVIE_NO_FRAMES", "Pick a folder with PNG frames (frame0001.png, …)."));
    }
    let layout = Layout::of(old, src);
    let (w, h) = (layout.width, layout.height);
    let picture = |i: usize| -> Result<Vec<u8>> {
        let p = super::teximport::decode_png(&pngs[i]).map_err(|e| bad("MOVIE_FRAME_BAD", format!("Frame {}: {}", i + 1, e.message)))?;
        Ok(if (p.width, p.height) == (w as u32, h as u32) { p.rgba } else { encode::fit(&p.rgba, p.width, p.height, w, h, opt.fit) })
    };
    let audio = wav.map(encode::audio_from_wav).transpose().map_err(|e| bad("WAV_BAD", e.0))?;
    let mid = pngs.len() / 2;
    let mut mid_src = None;
    let m = encode::mux(
        layout,
        pngs.len() as u32,
        |f| {
            let rgba = picture(f as usize).map_err(|e| encode::EncodeError(e.message))?;
            if f as usize == mid {
                mid_src = Some(rgba.clone());
            }
            Ok(rgba)
        },
        audio.as_deref(),
    )
    .map_err(|e| bad("MOVIE_ENCODE", e.0))?;
    // Read the result back: every frame decodes, and the middle one is compared with its picture.
    let p = Payload(&m.payload);
    let back = movie::scan(&p, 0, p.sectors()).map_err(|e| bad("MOVIE_VERIFY", e.to_string()))?;
    if back.frames.len() != pngs.len() {
        return Err(bad("MOVIE_VERIFY", format!("{} frames went in, {} read back.", pngs.len(), back.frames.len())));
    }
    let (rgba, _) = back.decode_frame(&p, mid as u32).map_err(|e| bad("MOVIE_VERIFY", e.to_string()))?;
    let psnr = mid_src.map(|s| psnr(&s, &rgba));
    let report = MovieReplaced {
        source: "frames",
        width: w,
        height: h,
        frames: m.frames,
        sectors: m.sectors,
        sectors_before: old.sectors,
        seconds: m.seconds,
        audio_seconds: audio.map(|a| a.len() as f64 / 2.0 / encode::AUDIO_RATE as f64),
        qscale_min: m.qscale_min,
        qscale_max: m.qscale_max,
        frame_bytes: m.frame_bytes,
        psnr,
        entries: Vec::new(),
        last_frame_before: 0,
        moves: m.sectors > old.sectors,
    };
    Ok((m.payload, report))
}

/// An STR file made elsewhere: raw 2352-byte sectors (a `.bin` rip or
/// `.str` with sync) or 2336-byte Mode 2 sectors. It must be a movie of the
/// old one's picture size.
pub fn from_str(old: &Movie, bytes: &[u8]) -> Result<(Vec<u8>, MovieReplaced)> {
    let payload: Vec<u8> = if bytes.len().is_multiple_of(RAW_SECTOR) && bytes.starts_with(&cdrom::SYNC) {
        bytes.chunks(RAW_SECTOR).flat_map(|r| r[cdrom::SUBHEADER..cdrom::SUBHEADER + PAYLOAD].to_vec()).collect()
    } else if bytes.len().is_multiple_of(PAYLOAD) {
        bytes.to_vec()
    } else {
        return Err(bad(
            "MOVIE_STR_SECTORS",
            "The STR file isn't whole CD sectors (2352 bytes with sync, or 2336 without). Save it as raw Mode 2 sectors, not 2048-byte user data.",
        ));
    };
    let p = Payload(&payload);
    let m = movie::scan(&p, 0, p.sectors()).map_err(|e| bad("MOVIE_STR_BAD", format!("The STR file doesn't read as a movie: {e}.")))?;
    if (m.info.width, m.info.height) != (old.info.width, old.info.height) {
        return Err(bad(
            "MOVIE_SIZE",
            format!("The STR is {}×{}; this movie plays at {}×{}.", m.info.width, m.info.height, old.info.width, old.info.height),
        ));
    }
    m.decode_frame(&p, 0).map_err(|e| bad("MOVIE_STR_BAD", format!("Its first frame doesn't decode: {e}.")))?;
    let report = MovieReplaced {
        source: "str",
        width: m.info.width,
        height: m.info.height,
        frames: m.frames.len() as u32,
        sectors: m.sectors,
        sectors_before: old.sectors,
        seconds: m.info.duration,
        audio_seconds: m.info.audio.as_ref().map(|a| a.seconds),
        qscale_min: m.info.qscale_min,
        qscale_max: m.info.qscale_max,
        frame_bytes: (m.frames.iter().map(|f| f.bytes as u64).sum::<u64>() / m.frames.len().max(1) as u64) as u32,
        psnr: None,
        entries: Vec::new(),
        last_frame_before: 0,
        moves: m.sectors > old.sectors,
    };
    Ok((payload, report))
}

/// OVR01 with the player records of `entries` stopping at `last_frame`.
pub fn set_last_frame(ovr01: &[u8], entries: &[u16], last_frame: u32) -> Result<Vec<u8>> {
    let base = (SETTINGS_VADDR - OVR01_VADDR) as usize;
    let mut out = ovr01.to_vec();
    for &e in entries {
        let at = base + e as usize * 28 + 8;
        out.get_mut(at..at + 4)
            .ok_or_else(|| bad("MOVIE_SETTINGS", format!("OVR01 has no player record {e}.")))?
            .copy_from_slice(&(last_frame as i32).to_le_bytes());
    }
    Ok(out)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    let mse = (0..n).filter(|i| i % 4 != 3).map(|i| (a[i] as f64 - b[i] as f64).powi(2)).sum::<f64>() / (n * 3 / 4).max(1) as f64;
    10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
}
