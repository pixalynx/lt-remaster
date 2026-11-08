//! FMV movies: the STR files of the movie table (`movie/0..13`, global
//! entries 0–13; 11 extents, as `movie/9` = `10` = `11` and `movie/12` = `13`).
//!
//! Every sector is MODE2/2352. Video chunks are Form 1 (submode 0x48): a
//! 32-byte STR chunk header, then 2016 bytes of the frame's bitstream, a
//! frame spread over 8–9 sectors. Audio is Form 2 XA-ADPCM (submode 0x64,
//! 2324 bytes), one sector in eight. Sector headers are in [`demux`], the
//! video bitstream and MDEC in [`mdec`], the audio in [`xa`]. Read-only:
//! nothing here writes sectors (Form 2 EDC writing does not exist yet).
//!
//! The game's player is OVR01 `Fmv_DecodeFrameTask` (0x80010F20); its
//! per-movie settings are the 7-word records at OVR01 `D_80013B60`
//! ([`PlayerSettings`]). It plays at 15 fps from a double-speed drive.

pub mod demux;
pub mod encode;
pub mod mdec;
pub mod xa;

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::disc::DiscImage;
use crate::error::LtError;
use demux::{ChunkHeader, SubHeader, XaCoding, CHUNK_DATA, CHUNK_HEADER, DATA};
pub use mdec::{FrameStats, MdecError};

/// Where a movie's raw sectors come from: the disc image (by LBA), or a
/// staged movie's payload ([`Payload`], sectors counted from 0).
pub trait Sectors {
    fn raw(&self, at: u32) -> std::result::Result<[u8; crate::disc::RAW_SECTOR], LtError>;
}

impl Sectors for DiscImage {
    fn raw(&self, at: u32) -> std::result::Result<[u8; crate::disc::RAW_SECTOR], LtError> {
        self.read_raw(at)
    }
}

/// A movie as the project stages it: runs of 2336-byte Mode 2 payloads
/// (subheaders and data, no sync or header).
pub struct Payload<'a>(pub &'a [u8]);

impl Payload<'_> {
    pub fn sectors(&self) -> u32 {
        (self.0.len() / crate::cdrom::PAYLOAD) as u32
    }
}

impl Sectors for Payload<'_> {
    fn raw(&self, at: u32) -> std::result::Result<[u8; crate::disc::RAW_SECTOR], LtError> {
        let p = crate::cdrom::PAYLOAD;
        let body = self.0.get(at as usize * p..(at as usize + 1) * p).ok_or(LtError::SectorOutOfRange { lba: at, sectors: self.sectors() })?;
        let mut raw = crate::cdrom::blank(at, [0; 4]);
        raw[crate::cdrom::SUBHEADER..crate::cdrom::SUBHEADER + p].copy_from_slice(body);
        Ok(raw)
    }
}

/// Sectors per second at single speed; STR movies stream at 2× (150/s).
pub const SECTORS_PER_SECOND_1X: u32 = 75;
/// OVR01 loads at 0x80010000 (the title overlay).
pub const OVR01_VADDR: u32 = 0x8001_0000;
/// `D_80013B60`: one 7-word record per movie-table entry.
pub const SETTINGS_VADDR: u32 = 0x8001_3B60;
pub const SETTINGS_COUNT: usize = 14;

/// Why a movie would not scan or decode. `code()` is the stable IPC code.
#[derive(Debug, thiserror::Error)]
pub enum MovieError {
    #[error(transparent)]
    Disc(#[from] LtError),
    #[error("the extent holds no STR video sectors")]
    NoVideo,
    #[error("the movie has no XA audio")]
    NoAudio,
    #[error("frame {0} is past the end of the movie ({1} frames)")]
    FrameRange(u32, u32),
    #[error("frame {frame} is missing chunk {chunk}")]
    MissingChunk { frame: u32, chunk: u16 },
    #[error("frame {frame} does not decode: {error}")]
    Frame { frame: u32, error: String },
}

impl MovieError {
    pub fn code(&self) -> &'static str {
        match self {
            MovieError::Disc(e) => e.code(),
            MovieError::NoVideo => "MOVIE_NO_VIDEO",
            MovieError::NoAudio => "MOVIE_NO_AUDIO",
            MovieError::FrameRange(..) => "MOVIE_FRAME_RANGE",
            MovieError::MissingChunk { .. } => "MOVIE_MISSING_CHUNK",
            MovieError::Frame { .. } => "MOVIE_BAD_FRAME",
        }
    }
}

pub type Result<T> = std::result::Result<T, MovieError>;

/// One video frame: where its chunks are and what its header says.
#[derive(Debug, Clone)]
pub struct FrameRef {
    /// Frame number from the chunk headers (starts at 1).
    pub number: u32,
    /// Sector offset (from the movie's first sector) of each chunk, in order.
    pub chunks: Vec<u32>,
    /// Demuxed bitstream bytes.
    pub bytes: u32,
    pub width: u16,
    pub height: u16,
    pub qscale: u16,
    pub version: u16,
}

impl FrameRef {
    pub fn first_sector(&self) -> u32 {
        self.chunks.iter().copied().min().unwrap_or(0)
    }

    pub fn last_sector(&self) -> u32 {
        self.chunks.iter().copied().max().unwrap_or(0)
    }
}

/// The XA stream picked for playback (the file/channel with most sectors).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInfo {
    pub file: u8,
    pub channel: u8,
    /// 37,800 or 18,900 Hz.
    pub rate: u32,
    pub channels: u8,
    /// 4 or 8.
    pub bits: u8,
    pub emphasis: bool,
    /// Audio sectors in this stream.
    pub sectors: u32,
    /// Sectors from one audio sector to the next (8 = one in eight).
    pub stride: u32,
    /// Sector offset of the first audio sector.
    pub first_sector: u32,
    /// PCM frames after decoding (gaps filled with silence).
    pub frames: u64,
    pub seconds: f64,
    /// When sample 0 plays, in seconds after frame 0 is shown (negative =
    /// before). Both are timed from when their sectors finish reading.
    pub offset: f64,
    /// Sectors of other XA streams, or of this one in another format.
    pub other_sectors: u32,
}

/// Facts about a movie extent, for the list and the inspector.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieInfo {
    pub lba: u32,
    pub sectors: u32,
    pub width: u16,
    pub height: u16,
    pub frames: u32,
    /// Frame numbers of the first and last frame.
    pub first_frame: u32,
    pub last_frame: u32,
    /// Sectors between frame starts (average).
    pub sectors_per_frame: f64,
    /// Drive speed, 1 or 2 (×75 sectors/s). Derived from the audio interleave.
    pub speed: u8,
    /// No audio to derive the speed from, so 2× is assumed.
    pub speed_assumed: bool,
    pub fps: f64,
    /// Video length, frames ÷ fps.
    pub duration: f64,
    /// Bitstream versions seen (2 for every retail movie).
    pub versions: Vec<u16>,
    pub qscale_min: u16,
    pub qscale_max: u16,
    /// Sectors per frame (chunk counts), smallest and largest.
    pub chunks_min: u16,
    pub chunks_max: u16,
    pub max_frame_bytes: u32,
    pub video_sectors: u32,
    pub audio_sectors: u32,
    /// Zero-filled padding sectors (submode 0).
    pub empty_sectors: u32,
    /// Anything else (not video, audio or padding).
    pub other_sectors: u32,
    /// Frames with a missing chunk (they will not decode).
    pub incomplete_frames: u32,
    pub audio: Option<AudioInfo>,
}

/// A scanned movie: the index of its frames and audio sectors.
#[derive(Debug, Clone)]
pub struct Movie {
    pub lba: u32,
    pub sectors: u32,
    pub frames: Vec<FrameRef>,
    /// Sector offsets of the chosen audio stream.
    pub audio_sectors: Vec<u32>,
    pub info: MovieInfo,
}

/// Decoded movie audio: interleaved when stereo.
#[derive(Debug, Clone)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u8,
    pub samples: Vec<i16>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

/// Reads every sector header of the extent and indexes frames and audio.
/// Reads `sectors` raw sectors (about 21 MB per minute of movie).
pub fn scan(image: &impl Sectors, lba: u32, sectors: u32) -> Result<Movie> {
    struct Building {
        head: ChunkHeader,
        chunks: Vec<Option<u32>>,
    }
    let mut frames: BTreeMap<u32, Building> = BTreeMap::new();
    let mut streams: HashMap<(u8, u8, u8), Vec<u32>> = HashMap::new();
    let (mut video, mut xa, mut empty, mut other) = (0u32, 0u32, 0u32, 0u32);
    for k in 0..sectors {
        let raw = image.raw(lba + k)?;
        let sh = SubHeader::parse(&raw);
        if sh.is_audio() {
            xa += 1;
            streams.entry((sh.file, sh.channel, sh.coding)).or_default().push(k);
        } else if let Some(h) = (!sh.is_form2()).then(|| ChunkHeader::parse(&raw[DATA..])).flatten() {
            video += 1;
            let b = frames.entry(h.frame).or_insert_with(|| Building { head: h, chunks: vec![None; h.chunks as usize] });
            if let Some(slot) = b.chunks.get_mut(h.chunk as usize) {
                slot.get_or_insert(k);
            }
            if h.chunk == 0 {
                b.head = h;
            }
        } else if sh.submode == 0 && raw[DATA..DATA + 2048].iter().all(|&b| b == 0) {
            empty += 1;
        } else {
            other += 1;
        }
    }
    if frames.is_empty() {
        return Err(MovieError::NoVideo);
    }

    let mut incomplete = 0;
    let list: Vec<FrameRef> = frames
        .into_iter()
        .map(|(number, b)| {
            if b.chunks.iter().any(Option::is_none) {
                incomplete += 1;
            }
            FrameRef {
                number,
                chunks: b.chunks.iter().map(|c| c.unwrap_or(u32::MAX)).collect(),
                bytes: b.head.bytes,
                width: b.head.width,
                height: b.head.height,
                qscale: b.head.qscale,
                version: b.head.version,
            }
        })
        .collect();

    // The audio stream: the file/channel/format with the most sectors.
    let best = streams.iter().max_by_key(|(key, v)| (v.len(), std::cmp::Reverse(**key))).map(|(k, v)| (*k, v.clone()));
    let (audio_sectors, audio_key) = match best {
        Some((key, v)) => (v, Some(key)),
        None => (Vec::new(), None),
    };

    let n = list.len() as u32;
    let first = &list[0];
    let last = &list[list.len() - 1];
    let spf = if n > 1 {
        (last.first_sector().saturating_sub(first.first_sector())) as f64 / (n - 1) as f64
    } else {
        sectors as f64
    };
    let stride = most_common_gap(&audio_sectors);
    let audio_info = audio_key.map(|(file, channel, coding)| {
        let c = XaCoding::from_byte(coding);
        let per = c.frames_per_sector();
        let filled = filled_sector_count(&audio_sectors, stride);
        (file, channel, c, per, filled)
    });
    // Speed from the interleave: stride × rate ÷ samples per sector = sectors/s.
    let (speed, assumed) = match &audio_info {
        Some((_, _, c, per, _)) if stride > 0 => {
            let sps = stride as f64 * c.rate as f64 / *per as f64;
            (if sps < 112.5 { 1u8 } else { 2u8 }, false)
        }
        _ => (2u8, true),
    };
    let sps = (SECTORS_PER_SECOND_1X * speed as u32) as f64;
    let fps = if spf > 0.0 { sps / spf } else { 0.0 };

    let mut versions: Vec<u16> = list.iter().map(|f| f.version).collect();
    versions.sort_unstable();
    versions.dedup();
    let audio = audio_info.map(|(file, channel, c, per, filled)| {
        let frames = filled as u64 * per as u64;
        let first_sector = audio_sectors[0];
        AudioInfo {
            file,
            channel,
            rate: c.rate,
            channels: c.channels(),
            bits: c.bits,
            emphasis: c.emphasis,
            sectors: audio_sectors.len() as u32,
            stride,
            first_sector,
            frames,
            seconds: frames as f64 / c.rate as f64,
            offset: (first_sector as f64 - first.last_sector() as f64) / sps,
            other_sectors: xa - audio_sectors.len() as u32,
        }
    });
    let info = MovieInfo {
        lba,
        sectors,
        width: first.width,
        height: first.height,
        frames: n,
        first_frame: first.number,
        last_frame: last.number,
        sectors_per_frame: spf,
        speed,
        speed_assumed: assumed,
        fps,
        duration: if fps > 0.0 { n as f64 / fps } else { 0.0 },
        versions,
        qscale_min: list.iter().map(|f| f.qscale).min().unwrap_or(0),
        qscale_max: list.iter().map(|f| f.qscale).max().unwrap_or(0),
        chunks_min: list.iter().map(|f| f.chunks.len() as u16).min().unwrap_or(0),
        chunks_max: list.iter().map(|f| f.chunks.len() as u16).max().unwrap_or(0),
        max_frame_bytes: list.iter().map(|f| f.bytes).max().unwrap_or(0),
        video_sectors: video,
        audio_sectors: xa,
        empty_sectors: empty,
        other_sectors: other,
        incomplete_frames: incomplete,
        audio,
    };
    Ok(Movie { lba, sectors, frames: list, audio_sectors, info })
}

/// The most common distance between consecutive sectors (0 if fewer than 2).
fn most_common_gap(s: &[u32]) -> u32 {
    let mut count: HashMap<u32, u32> = HashMap::new();
    for w in s.windows(2) {
        *count.entry(w[1] - w[0]).or_default() += 1;
    }
    count.into_iter().max_by_key(|&(gap, n)| (n, std::cmp::Reverse(gap))).map(|(g, _)| g).unwrap_or(0)
}

/// Sector count once gaps longer than the stride are filled with silence.
fn filled_sector_count(s: &[u32], stride: u32) -> u32 {
    if s.is_empty() {
        return 0;
    }
    let mut n = 1;
    for w in s.windows(2) {
        n += gap_sectors(w[1] - w[0], stride);
    }
    n
}

/// How many sector slots a gap covers: 1, or more when sectors are missing.
fn gap_sectors(gap: u32, stride: u32) -> u32 {
    if stride == 0 || gap <= stride {
        1
    } else {
        (gap + stride / 2) / stride
    }
}

impl Movie {
    /// The bitstream of frame `n` (0-based), demuxed from its chunks.
    pub fn demux(&self, image: &impl Sectors, n: u32) -> Result<Vec<u8>> {
        let f = self.frames.get(n as usize).ok_or(MovieError::FrameRange(n, self.frames.len() as u32))?;
        let mut data = Vec::with_capacity(f.chunks.len() * CHUNK_DATA);
        for (i, &k) in f.chunks.iter().enumerate() {
            if k == u32::MAX {
                return Err(MovieError::MissingChunk { frame: f.number, chunk: i as u16 });
            }
            let raw = image.raw(self.lba + k)?;
            data.extend_from_slice(&raw[DATA + CHUNK_HEADER..DATA + CHUNK_HEADER + CHUNK_DATA]);
        }
        data.truncate(f.bytes as usize);
        Ok(data)
    }

    /// Frame `n` (0-based) as RGBA8, `width × height × 4` bytes.
    pub fn decode_frame(&self, image: &impl Sectors, n: u32) -> Result<(Vec<u8>, FrameStats)> {
        let data = self.demux(image, n)?;
        let f = &self.frames[n as usize];
        let mut rgba = vec![0u8; f.width as usize * f.height as usize * 4];
        let stats = mdec::decode(&data, f.width, f.height, &mut rgba)
            .map_err(|e| MovieError::Frame { frame: f.number, error: e.to_string() })?;
        Ok((rgba, stats))
    }

    /// The whole XA stream as 16-bit PCM. A gap longer than the interleave
    /// (a missing audio sector) becomes silence, so audio stays in sync.
    pub fn decode_audio(&self, image: &impl Sectors) -> Result<Pcm> {
        let a = self.info.audio.as_ref().ok_or(MovieError::NoAudio)?;
        let coding = image.raw(self.lba + self.audio_sectors[0]).map(|r| XaCoding::from_byte(SubHeader::parse(&r).coding))?;
        let per = coding.frames_per_sector() as usize * a.channels as usize;
        let mut samples = Vec::with_capacity(a.frames as usize * a.channels as usize);
        let mut dec = xa::XaDecoder::new();
        let mut prev: Option<u32> = None;
        for &k in &self.audio_sectors {
            if let Some(p) = prev {
                let missing = gap_sectors(k - p, a.stride) - 1;
                samples.resize(samples.len() + missing as usize * per, 0);
            }
            prev = Some(k);
            let raw = image.raw(self.lba + k)?;
            dec.decode_sector(&raw[DATA..DATA + demux::FORM2_DATA], coding, &mut samples);
        }
        Ok(Pcm { rate: a.rate, channels: a.channels, samples })
    }
}

/// One record of OVR01 `D_80013B60`: how the game plays a movie-table entry.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSettings {
    /// +0x00: 1 = 24-bit MDEC output (every retail entry).
    pub rgb24: bool,
    /// +0x08: the player stops once this frame number is reached.
    pub last_frame: u32,
    /// +0x0C, +0x10: VRAM position.
    pub x: i32,
    pub y: i32,
    /// +0x14, +0x18: display size.
    pub width: i32,
    pub height: i32,
}

/// The 14 player records, read from OVR01's bytes (`ovr/1`).
pub fn player_settings(ovr01: &[u8]) -> Vec<PlayerSettings> {
    settings_at(ovr01, (SETTINGS_VADDR - OVR01_VADDR) as usize, SETTINGS_COUNT)
}

/// [`player_settings`] for the release `exe` belongs to: one record per movie in its table
/// (11 on the Japanese release), where its OVR01 keeps them.
pub fn player_settings_in(exe: &crate::PsxExe, ovr01: &[u8]) -> Vec<PlayerSettings> {
    let movies = exe.release.info().sub_table(crate::fs::Kind::Movie).count as usize;
    settings_at(ovr01, (exe.ovr_va(1, SETTINGS_VADDR) - OVR01_VADDR) as usize, movies)
}

fn settings_at(ovr01: &[u8], base: usize, count: usize) -> Vec<PlayerSettings> {
    (0..count)
        .map_while(|i| {
            let r = ovr01.get(base + i * 28..base + i * 28 + 28)?;
            let w = |o: usize| i32::from_le_bytes([r[o], r[o + 1], r[o + 2], r[o + 3]]);
            Some(PlayerSettings { rgb24: w(0) != 0, last_frame: w(8).max(0) as u32, x: w(12), y: w(16), width: w(20), height: w(24) })
        })
        .collect()
}

/// A 16-bit PCM WAV.
pub fn wav(pcm: &Pcm) -> Vec<u8> {
    let ch = pcm.channels.max(1) as u32;
    let data = pcm.samples.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&(ch as u16).to_le_bytes());
    w.extend_from_slice(&pcm.rate.to_le_bytes());
    w.extend_from_slice(&(pcm.rate * ch * 2).to_le_bytes());
    w.extend_from_slice(&((ch * 2) as u16).to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for s in &pcm.samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// An 8-bit RGB PNG of an RGBA8 image (stored deflate blocks: no
/// compression, but valid everywhere).
pub fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let row = width as usize * 3 + 1;
    let mut raw = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        raw.push(0); // filter: none
        for p in rgba[y * width as usize * 4..(y + 1) * width as usize * 4].as_chunks::<4>().0 {
            raw.extend_from_slice(&p[..3]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(body);
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps() {
        assert_eq!(most_common_gap(&[7, 15, 23, 31, 47]), 8);
        assert_eq!(filled_sector_count(&[7, 15, 23, 31, 47], 8), 6);
        assert_eq!(gap_sectors(8, 8), 1);
        assert_eq!(gap_sectors(16, 8), 2);
    }

    #[test]
    fn settings_record() {
        let mut ovr = vec![0u8; 0x3B60 + 28 * 14];
        let r = &mut ovr[0x3B60 + 28 * 8..];
        r[0] = 1;
        r[8..12].copy_from_slice(&144i32.to_le_bytes());
        r[20..24].copy_from_slice(&320i32.to_le_bytes());
        r[24..28].copy_from_slice(&224i32.to_le_bytes());
        let s = player_settings(&ovr);
        assert_eq!(s.len(), 14);
        assert_eq!(s[8], PlayerSettings { rgb24: true, last_frame: 144, x: 0, y: 0, width: 320, height: 224 });
    }

    #[test]
    fn png_and_wav_headers() {
        let p = png(2, 2, &[255; 16]);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        let w = wav(&Pcm { rate: 37_800, channels: 2, samples: vec![0; 8] });
        assert_eq!((&w[..4], w.len()), (&b"RIFF"[..], 44 + 16));
        assert_eq!(u16::from_le_bytes([w[22], w[23]]), 2);
    }
}
