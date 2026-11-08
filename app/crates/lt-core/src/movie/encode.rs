//! STR movies written (screen 14 §13, research 06 phase 1 "FMV Form 2"):
//! frames through the MDEC encoder ([`super::mdec::encode_fit`]), sound
//! through the XA encoder ([`super::xa::XaEncoder`]), laid out the way every
//! retail movie is:
//!
//! - each frame owns a 10-sector slot (15 fps from a 2× drive, 150 sectors
//!   a second) and fills its video sectors — 8 or 9, around the audio — at
//!   the finest quantisation that fits;
//! - one XA sector (4-bit stereo, 37.8 kHz, 2016 frames) every 8 sectors from
//!   sector 7, so sound keeps pace with the drive;
//! - video sectors Form 1 (submode 0x48), each a 32-byte chunk header and
//!   2016 bitstream bytes; audio Form 2 (0x64); the last sector EOF (0x80).
//!
//! The result is a run of 2336-byte Mode 2 payloads (see [`crate::cdrom::PAYLOAD`])
//! that the build writes with fresh sync, headers and checksums.

use serde::Serialize;

use super::demux::{SubHeader, CHUNK_DATA, CHUNK_HEADER, STR_MAGIC, STR_TYPE_MDEC};
use super::mdec::{self, FRAME_MAGIC};
use super::xa::{XaEncoder, SECTOR_DATA};
use super::{Movie, Sectors};
use crate::cdrom::PAYLOAD;

/// Sectors per frame slot (2× drive at 15 fps).
pub const SLOT: u32 = 10;
/// XA sectors: from sector 7, one every 8.
pub const AUDIO_FIRST: u32 = 7;
pub const AUDIO_STRIDE: u32 = 8;
/// PCM frames one 4-bit stereo XA sector carries, and its rate.
pub const AUDIO_FRAMES: usize = 2016;
pub const AUDIO_RATE: u32 = 37_800;

/// How a movie is laid out: its picture size and the subheaders it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub width: u16,
    pub height: u16,
    pub video: [u8; 4],
    pub audio: [u8; 4],
}

impl Layout {
    /// Every retail movie's: file 1, channel 1, video 0x48, audio 0x64 / coding 0x01.
    pub fn retail(width: u16, height: u16) -> Self {
        Self { width, height, video: [1, 1, 0x48, 0], audio: [1, 1, 0x64, 0x01] }
    }

    /// The layout of an existing movie (its size and subheaders).
    pub fn of(m: &Movie, src: &impl Sectors) -> Self {
        let mut l = Self::retail(m.info.width, m.info.height);
        let sub = |at: u32| src.raw(m.lba + at).ok().map(|r| SubHeader::parse(&r));
        if let Some(s) = m.frames.first().and_then(|f| sub(f.first_sector())) {
            l.video = [s.file, s.channel, s.submode & !0x81, 0];
        }
        if let Some(s) = m.audio_sectors.first().and_then(|&k| sub(k)) {
            l.audio = [s.file, s.channel, s.submode & !0x81, s.coding];
        }
        l
    }

    pub fn is_audio(sector: u32) -> bool {
        sector >= AUDIO_FIRST && (sector - AUDIO_FIRST).is_multiple_of(AUDIO_STRIDE)
    }

    /// The video sectors of frame `f`'s slot.
    pub fn video_sectors(f: u32) -> Vec<u32> {
        (f * SLOT..(f + 1) * SLOT).filter(|&k| !Self::is_audio(k)).collect()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Muxed {
    #[serde(skip)]
    pub payload: Vec<u8>,
    pub frames: u32,
    pub sectors: u32,
    pub audio_sectors: u32,
    pub qscale_min: u16,
    pub qscale_max: u16,
    /// Average bitstream bytes per frame.
    pub frame_bytes: u32,
    pub seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct EncodeError(pub String);

fn sector(sub: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; PAYLOAD];
    p[..4].copy_from_slice(&sub);
    p[4..8].copy_from_slice(&sub);
    p[8..8 + data.len()].copy_from_slice(data);
    p
}

/// Writes `count` frames (`frame(f)` gives frame f as RGBA at the layout's
/// size) and, when given, interleaved 37.8 kHz stereo PCM (padded with
/// silence or cut to the picture's length).
pub fn mux(layout: Layout, count: u32, mut frame: impl FnMut(u32) -> Result<Vec<u8>, EncodeError>, audio: Option<&[i16]>) -> Result<Muxed, EncodeError> {
    if count == 0 {
        return Err(EncodeError("A movie needs at least one frame.".into()));
    }
    let sectors = count * SLOT;
    let mut payload = vec![0u8; sectors as usize * PAYLOAD];
    let (mut qmin, mut qmax, mut bytes) = (u16::MAX, 0u16, 0u64);
    let (w, h) = (layout.width, layout.height);
    // Frames are gathered in batches and encoded on every core.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let batch = (threads * 4) as u32;
    let mut encoded: Vec<Option<mdec::Encoded>> = Vec::new();
    let mut first = 0;
    for f in 0..count {
        if f == first + encoded.len() as u32 {
            first = f;
            let n = batch.min(count - f);
            let pictures = (f..f + n).map(&mut frame).collect::<Result<Vec<_>, _>>()?;
            encoded = vec![None; n as usize];
            std::thread::scope(|s| {
                let per = (n as usize).div_ceil(threads);
                for (k, (pics, outs)) in pictures.chunks(per).zip(encoded.chunks_mut(per)).enumerate() {
                    s.spawn(move || {
                        for (i, (rgba, out)) in pics.iter().zip(outs.iter_mut()).enumerate() {
                            let g = f + (k * per + i) as u32;
                            *out = mdec::encode_fit(rgba, w, h, Layout::video_sectors(g).len() * CHUNK_DATA);
                        }
                    });
                }
            });
        }
        let slots = Layout::video_sectors(f);
        let e = encoded[(f - first) as usize]
            .take()
            .ok_or_else(|| EncodeError(format!("Frame {} doesn't fit its {} sectors even at the coarsest quantisation.", f + 1, slots.len())))?;
        qmin = qmin.min(e.qscale);
        qmax = qmax.max(e.qscale);
        bytes += e.data.len() as u64;
        for (i, &k) in slots.iter().enumerate() {
            let mut user = vec![0u8; CHUNK_HEADER + CHUNK_DATA];
            let put16 = |u: &mut Vec<u8>, at: usize, v: u16| u[at..at + 2].copy_from_slice(&v.to_le_bytes());
            put16(&mut user, 0, STR_MAGIC);
            put16(&mut user, 2, STR_TYPE_MDEC);
            put16(&mut user, 4, i as u16);
            put16(&mut user, 6, slots.len() as u16);
            user[8..12].copy_from_slice(&(f + 1).to_le_bytes());
            user[12..16].copy_from_slice(&(e.data.len() as u32).to_le_bytes());
            put16(&mut user, 16, w);
            put16(&mut user, 18, h);
            put16(&mut user, 20, e.codes_word);
            put16(&mut user, 22, FRAME_MAGIC);
            put16(&mut user, 24, e.qscale);
            put16(&mut user, 26, 2);
            let from = (i * CHUNK_DATA).min(e.data.len());
            let to = ((i + 1) * CHUNK_DATA).min(e.data.len());
            user[CHUNK_HEADER..CHUNK_HEADER + (to - from)].copy_from_slice(&e.data[from..to]);
            let at = k as usize * PAYLOAD;
            payload[at..at + PAYLOAD].copy_from_slice(&sector(layout.video, &user));
        }
    }
    let mut enc = XaEncoder::new();
    let mut audio_sectors = 0;
    for (n, k) in (AUDIO_FIRST..sectors).step_by(AUDIO_STRIDE as usize).enumerate() {
        let pcm = audio.map(|a| {
            let from = (n * AUDIO_FRAMES * 2).min(a.len());
            &a[from..(from + AUDIO_FRAMES * 2).min(a.len())]
        });
        let data = enc.encode_stereo(pcm.unwrap_or(&[]));
        let at = k as usize * PAYLOAD;
        payload[at..at + PAYLOAD].copy_from_slice(&sector(layout.audio, &data[..SECTOR_DATA]));
        audio_sectors += 1;
    }
    // The last sector carries EOF.
    let last = (sectors as usize - 1) * PAYLOAD;
    payload[last + 2] |= 0x80;
    payload[last + 6] |= 0x80;
    Ok(Muxed {
        payload,
        frames: count,
        sectors,
        audio_sectors,
        qscale_min: qmin,
        qscale_max: qmax,
        frame_bytes: (bytes / count as u64) as u32,
        seconds: count as f64 / 15.0,
    })
}

/// How a picture of another size is fitted into the movie's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Fit {
    /// Scaled to fit inside, keeping its shape (black bars).
    #[default]
    Contain,
    /// Scaled to fill, keeping its shape (edges cropped).
    Cover,
    /// Stretched to the movie's size.
    Stretch,
}

/// `src` (sw × sh RGBA) resampled into a `w × h` RGBA picture (bilinear).
pub fn fit(src: &[u8], sw: u32, sh: u32, w: u16, h: u16, how: Fit) -> Vec<u8> {
    let (w, h) = (w as f64, h as f64);
    let (sx, sy) = (sw as f64, sh as f64);
    let scale = match how {
        Fit::Contain => (w / sx).min(h / sy),
        Fit::Cover => (w / sx).max(h / sy),
        Fit::Stretch => 0.0,
    };
    let (dw, dh) = if how == Fit::Stretch { (w, h) } else { (sx * scale, sy * scale) };
    let (ox, oy) = ((w - dw) / 2.0, (h - dh) / 2.0);
    let mut out = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let o = (y * w as usize + x) * 4;
            out[o + 3] = 255;
            let (u, v) = ((x as f64 + 0.5 - ox) / dw * sx - 0.5, (y as f64 + 0.5 - oy) / dh * sy - 0.5);
            if u < -0.5 || v < -0.5 || u > sx - 0.5 || v > sy - 0.5 {
                continue;
            }
            let (x0, y0) = (u.floor().clamp(0.0, sx - 1.0) as usize, v.floor().clamp(0.0, sy - 1.0) as usize);
            let (x1, y1) = ((x0 + 1).min(sw as usize - 1), (y0 + 1).min(sh as usize - 1));
            let (fx, fy) = ((u - x0 as f64).clamp(0.0, 1.0), (v - y0 as f64).clamp(0.0, 1.0));
            for c in 0..3 {
                let p = |xx: usize, yy: usize| src[(yy * sw as usize + xx) * 4 + c] as f64;
                let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
                let bot = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
                out[o + c] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// A WAV's sound as 37.8 kHz interleaved stereo PCM.
pub fn audio_from_wav(bytes: &[u8]) -> Result<Vec<i16>, EncodeError> {
    let w = crate::audio::wav::read(bytes).map_err(|e| EncodeError(e.0))?;
    let l = crate::audio::wav::resample(&w.left, w.rate, AUDIO_RATE);
    let r = crate::audio::wav::resample(&w.right, w.rate, AUDIO_RATE);
    let (l, r) = (crate::audio::wav::to_i16(&l), crate::audio::wav::to_i16(&r));
    Ok(l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movie::{scan, Payload};

    #[test]
    fn muxes_frames_and_sound_the_reader_finds() {
        let (w, h) = (64u16, 32u16);
        let frame = |f: u32| -> Result<Vec<u8>, EncodeError> {
            Ok((0..w as usize * h as usize).flat_map(|i| [((i % 64) * 4) as u8, (f * 20) as u8, 128, 255]).collect())
        };
        let pcm: Vec<i16> = (0..AUDIO_FRAMES * 2 * 3).map(|i| ((i / 2) as f64 / 10.0).sin().mul_add(8000.0, 0.0) as i16).collect();
        let m = mux(Layout::retail(w, h), 4, frame, Some(&pcm)).unwrap();
        assert_eq!((m.frames, m.sectors, m.audio_sectors), (4, 40, 5));
        let p = Payload(&m.payload);
        let s = scan(&p, 0, p.sectors()).unwrap();
        assert_eq!((s.frames.len(), s.info.width, s.info.height), (4, w, h));
        assert_eq!(s.audio_sectors, [7, 15, 23, 31, 39]);
        assert_eq!(s.frames[1].first_sector(), 10);
        let (rgba, st) = s.decode_frame(&p, 2).unwrap();
        assert_eq!((st.version, rgba.len()), (2, w as usize * h as usize * 4));
        assert!((rgba[1] as i32 - 40).abs() <= 6, "green {}", rgba[1]);
        let a = s.decode_audio(&p).unwrap();
        assert_eq!((a.rate, a.channels), (37_800, 2));
        // The last sector says EOF.
        assert_eq!(m.payload[(m.sectors as usize - 1) * PAYLOAD + 2] & 0x80, 0x80);
    }

    #[test]
    fn fits_pictures() {
        let src = vec![255u8; 4 * 4 * 4];
        let out = fit(&src, 4, 4, 8, 4, Fit::Contain);
        // A square in a 2:1 picture: bars left and right.
        assert_eq!(out[0], 0);
        assert_eq!(out[(8 + 4) * 4], 255);
        let out = fit(&src, 4, 4, 8, 4, Fit::Stretch);
        assert_eq!(out[0], 255);
    }
}
