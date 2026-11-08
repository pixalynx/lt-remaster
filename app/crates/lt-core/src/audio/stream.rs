//! Streamed music: an audio file as a song the game streams from the CD (HackerGC
//! `src/hgc/hgc_music.c`, `docs/screens/14-audio.md` §14).
//!
//! A song cue of a BGM file plays from a spare file (`new/N`) instead of its sequence: SPU-ADPCM
//! on reserved voices (0, and 1 for stereo), from a ring in the bank's own SPU region that the
//! player refills from the CD in bursts. This module writes the stream file and the slimmed bank
//! the ring needs: the bank keeps its other cues, the samples only the streamed songs used go,
//! and the ring takes the room they leave.
//!
//! Stream file: sector 0 is the header ([`Head`], `HgcStreamHead` in `config_hgc.h`), then data
//! sectors. A data sector is a *frame*: stereo, 1 KB of left-channel ADPCM then 1 KB of right;
//! mono, 2 KB. Each 1 KB / 2 KB unit starts with a filter-0 block, so a voice can start on it, or
//! the loop jump to it, without the previous block's history. Every block's flags are 0x02; the
//! player sets the ring's own (loop start, ring end, stream end) as it copies frames in.
//!
//! The loop goes back to a frame boundary, so the song is resampled a hair off its rate (well under
//! a percent, reported as `tempoChange`) to make the looped part a whole number of frames, and the
//! intro is padded at the front to a frame boundary.

use serde::{Deserialize, Serialize};

use super::{adpcm, decode::Pcm, tsq, tvb};
use crate::container::le32;

pub const MAGIC: u32 = 0x534D_544C; // "LTMS"
pub const VERSION: u8 = 1;
pub const SECTOR: usize = 0x800;
/// [`Head::flags`]: at the end, go on from `loop_sector`.
pub const LOOP: u8 = 0x01;
/// Frames a segment (the player tracks the voice by segments).
pub const SEG: u32 = 8;
/// The ring's least frames: the voice's segment, a guard segment and one being filled.
pub const MIN_FRAMES: u32 = 3 * SEG;
/// Sectors a burst reads at most (`HGC_MUS_BOUNCE_MAX`).
pub const BOUNCE_MAX: u8 = 5;
/// A BGM bank's SPU region (0x21710 bank 1, 0x3D710 bank 2).
pub const BANK_BYTES: u32 = 0x1C000;
/// The bank's TSQ buffer (0x801CEEE8 bank 1, 0x801D16E8 bank 2): the TSQ, then the bounce buffer
/// at its end. The player reads the header into its last sector.
pub const TSQ_BUFFER: usize = 0x2800;
/// SPU sample rate the pitch register is relative to.
pub const SPU_RATE: f64 = 44_100.0;
/// NTSC fields a second at 240 lines (263 a field, 3,413 GPU clocks a line).
pub const VBLANK_HZ: f64 = 53_693_175.0 / (3413.0 * 263.0);
/// The rate an import is resampled to unless it says otherwise.
pub const DEFAULT_RATE: u32 = 22_050;

/// `HgcStreamHead`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Head {
    pub channels: u8,
    pub flags: u8,
    pub bounce: u8,
    pub pitch: u16,
    pub volume: u16,
    pub sectors: u32,
    pub loop_sector: u32,
    pub ring_offset: u32,
    pub ring_frames: u16,
    /// 1/256 vblanks a segment plays for (the player's clock when an SPU IRQ doesn't come).
    pub seg_vbl: u16,
}

pub const HEAD_BYTES: usize = 28;

impl Head {
    pub fn to_bytes(&self) -> [u8; HEAD_BYTES] {
        let mut b = [0u8; HEAD_BYTES];
        b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        b[4] = VERSION;
        b[5] = self.channels;
        b[6] = self.flags;
        b[7] = self.bounce;
        b[8..10].copy_from_slice(&self.pitch.to_le_bytes());
        b[10..12].copy_from_slice(&self.volume.to_le_bytes());
        b[12..16].copy_from_slice(&self.sectors.to_le_bytes());
        b[16..20].copy_from_slice(&self.loop_sector.to_le_bytes());
        b[20..24].copy_from_slice(&self.ring_offset.to_le_bytes());
        b[24..26].copy_from_slice(&self.ring_frames.to_le_bytes());
        b[26..28].copy_from_slice(&self.seg_vbl.to_le_bytes());
        b
    }

    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < HEAD_BYTES || le32(b, 0) != MAGIC || b[4] != VERSION {
            return None;
        }
        let u16a = |k: usize| u16::from_le_bytes([b[k], b[k + 1]]);
        Some(Self {
            channels: b[5],
            flags: b[6],
            bounce: b[7],
            pitch: u16a(8),
            volume: u16a(10),
            sectors: le32(b, 12),
            loop_sector: le32(b, 16),
            ring_offset: le32(b, 20),
            ring_frames: u16a(24),
            seg_vbl: u16a(26),
        })
    }

    pub fn looping(&self) -> bool {
        self.flags & LOOP != 0
    }

    /// The rate the pitch plays at.
    pub fn rate(&self) -> f64 {
        self.pitch as f64 * SPU_RATE / 4096.0
    }

    /// PCM samples a frame holds, per channel.
    pub fn frame_samples(&self) -> usize {
        frame_samples(self.channels)
    }

    /// Everything the player takes on trust (it only checks the magic).
    pub fn check(&self) -> Result<(), String> {
        let ok = matches!(self.channels, 1 | 2)
            && (1..=BOUNCE_MAX).contains(&self.bounce)
            && self.pitch > 0
            && self.pitch <= 0x3FFF
            && self.volume <= 0x3FFF
            && self.sectors > 0
            && self.loop_sector < self.sectors
            && self.ring_offset.is_multiple_of(64)
            && self.ring_frames as u32 >= MIN_FRAMES
            && self.ring_offset + self.ring_frames as u32 * SECTOR as u32 <= BANK_BYTES
            && self.seg_vbl > 0;
        if ok { Ok(()) } else { Err(format!("The stream header doesn't hold together: {self:?}")) }
    }
}

pub fn frame_samples(channels: u8) -> usize {
    if channels == 2 { 1792 } else { 3584 }
}

/// Where the streams of one bank play from: past the samples the bank keeps (`offset` bytes from
/// the bank's SPU base), `frames` long, read `bounce` sectors a burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ring {
    pub offset: u32,
    pub frames: u16,
    pub bounce: u8,
}

impl Ring {
    /// Bank 2's whole region and TSQ buffer, which the common bank's songs borrow (the jingles
    /// sharing their bank keep too much of it): `HGC_MUS_RING2`.
    pub const BANK2: Ring = Ring { offset: 0, frames: (BANK_BYTES / SECTOR as u32) as u16, bounce: BOUNCE_MAX };

    /// The ring for a bank whose voice bank holds `adpcm` bytes and whose TSQ is `tsq` bytes.
    pub fn fit(adpcm: u32, tsq: usize) -> Result<Self, String> {
        let offset = adpcm.max(64).next_multiple_of(64);
        let frames = BANK_BYTES.saturating_sub(offset) / SECTOR as u32;
        if frames < MIN_FRAMES {
            return Err(format!(
                "The bank's other cues keep {} KB of samples, which leaves {} KB of its 112 KB for the stream; it needs {} KB. Stream more of its songs, or replace samples with shorter ones.",
                adpcm / 1024,
                BANK_BYTES.saturating_sub(offset) / 1024,
                MIN_FRAMES as usize * SECTOR / 1024
            ));
        }
        let bounce = ((TSQ_BUFFER.saturating_sub(tsq)) / SECTOR).min(BOUNCE_MAX as usize) as u8;
        if bounce == 0 {
            return Err(format!(
                "The bank's sequences take {tsq} of the {TSQ_BUFFER} bytes the game keeps for them; the stream needs the last 2 KB to read through. Stream more of its songs."
            ));
        }
        Ok(Self { offset, frames: frames as u16, bounce })
    }

    /// Seconds of music the ring holds ahead of the voice (past the guard and the voice's own
    /// segment), at `channels` and `rate`.
    pub fn ahead_seconds(&self, channels: u8, rate: f64) -> f64 {
        (self.frames as f64 - 2.0 * SEG as f64) * frame_samples(channels) as f64 / rate
    }
}

/// A BGM file (`u32 tsqOfs, u32 tvbOfs` + TSQ + TVB) without the songs `cues`: their TSQ bytes out
/// ([`tsq::remove_cue`]), and only the samples the other cues program, packed
/// ([`tvb::keep_slots`]). Returns the new file (sector padded) and the ring it leaves.
pub fn slim_bank(file: &[u8], cues: &[usize]) -> Result<(Vec<u8>, Ring), String> {
    let (out, adpcm, tsq) = slim(file, cues)?;
    Ok((out, Ring::fit(adpcm, tsq)?))
}

/// A BGM file without the songs `cues`, as [`slim_bank`] lays it out, with the ADPCM bytes and the
/// TSQ bytes left: for a bank whose streams borrow another's region (the common bank), and the
/// room it leaves for sound effects.
pub fn slim(file: &[u8], cues: &[usize]) -> Result<(Vec<u8>, u32, usize), String> {
    if file.len() < 8 {
        return Err("The music file is too short to be a bank.".into());
    }
    let (a, b) = (le32(file, 0) as usize, le32(file, 4) as usize);
    if a != 8 || b <= a || b + tvb::HEADER > file.len() {
        return Err("The music file isn't a TSQ + TVB pair.".into());
    }
    let mut seq = file[a..b].to_vec();
    for &c in cues {
        seq = tsq::remove_cue(&seq, c).ok_or_else(|| format!("Cue {c} can't be taken out of the sequence bank."))?;
    }
    let mut keep = [false; tvb::SLOTS];
    for c in 1..tsq::CUES - 1 {
        for p in tsq::programs(&seq, c) {
            if (p as usize) < tvb::SLOTS {
                keep[p as usize] = true;
            }
        }
    }
    let t = tvb::Tvb::parse(&file[b..]).ok_or("The music file's voice bank doesn't parse.")?;
    let voices = tvb::keep_slots(&file[b..b + tvb::HEADER + t.size as usize], &keep).ok_or("The voice bank couldn't be packed.")?;
    let size = le32(&voices, tvb::SIZE_AT);
    let mut out = Vec::with_capacity(8 + seq.len() + voices.len());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&((8 + seq.len()) as u32).to_le_bytes());
    out.extend_from_slice(&seq);
    out.extend_from_slice(&voices);
    out.resize(out.len().next_multiple_of(SECTOR), 0);
    Ok((out, size, seq.len()))
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "at")]
pub enum StreamLoop {
    /// The whole song loops.
    #[default]
    Whole,
    /// Plays once and stops.
    Off,
    /// Loops back to this many seconds in (an intro that plays once).
    From(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamOptions {
    /// Hz the song is resampled to (it plays at the SPU pitch nearest it).
    #[serde(default = "default_rate")]
    pub rate: u32,
    #[serde(default = "yes")]
    pub stereo: bool,
    #[serde(default)]
    pub looping: StreamLoop,
    /// 0–1 of the voice's full volume.
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// Bring the song's peak to −1 dBFS first.
    #[serde(default = "yes")]
    pub normalize: bool,
}

/// What a missing "loop from" means to a caller that takes `null` as "plays once": the whole
/// song loops.
pub fn loop_default() -> Option<f64> {
    Some(0.0)
}

fn default_rate() -> u32 {
    DEFAULT_RATE
}
fn yes() -> bool {
    true
}
fn default_volume() -> f32 {
    0.8
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self { rate: DEFAULT_RATE, stereo: true, looping: StreamLoop::Whole, volume: default_volume(), normalize: true }
    }
}

/// What [`encode`] made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfo {
    pub head: Head,
    /// Seconds of data (intro padding included).
    pub seconds: f64,
    pub source_seconds: f64,
    pub source_rate: u32,
    pub source_channels: u16,
    /// The rate the pitch plays at.
    pub rate: f64,
    /// Where the loop goes back to, in seconds of data (`None`: plays once).
    pub loop_seconds: Option<f64>,
    /// How much faster (+) or slower the song plays so its loop is whole frames, in percent.
    pub tempo_change: f64,
    pub bytes: u32,
    pub snr_db: f64,
    /// Gain applied by normalising, in dB.
    pub gain_db: f64,
    pub clipped: bool,
    /// Seconds the ring holds ahead of the voice: how long a load may keep the CD.
    pub ahead_seconds: f64,
    pub title: Option<String>,
}

/// A stream file and what went into it.
#[derive(Debug, Clone)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub info: StreamInfo,
}

/// The title and source facts the header sector carries after [`Head`] (the player ignores them).
const NOTE_AT: usize = 0x40;

/// `pcm` as a stream file for a bank whose streams play from `ring`.
pub fn encode(pcm: &Pcm, opt: &StreamOptions, ring: Ring, title: Option<&str>) -> Result<Encoded, String> {
    if pcm.frames() == 0 {
        return Err("The file has no audio in it.".into());
    }
    let channels: u8 = if opt.stereo { 2 } else { 1 };
    let fs = frame_samples(channels);
    let pitch = ((opt.rate.clamp(8_000, 44_100) as f64) * 4096.0 / SPU_RATE).round().clamp(1.0, 0x3FFF as f64) as u16;
    let rate = pitch as f64 * SPU_RATE / 4096.0;
    let src = pcm.rate as f64;
    let n = pcm.frames();
    // Channels to encode, normalised.
    let mut chans: Vec<Vec<f32>> = if channels == 2 {
        vec![pcm.left.clone(), pcm.right.clone()]
    } else {
        vec![pcm.left.iter().zip(&pcm.right).map(|(l, r)| (l + r) * 0.5).collect()]
    };
    let peak = chans.iter().flatten().fold(0f32, |m, v| m.max(v.abs()));
    let gain = if opt.normalize && peak > 1e-4 { (0.891 / peak).min(8.0) } else { 1.0 };
    for c in &mut chans {
        for v in c.iter_mut() {
            *v *= gain;
        }
    }
    // Lay out the loop: the looped part a whole number of frames, its start on a frame boundary.
    let (ratio, intro_src, body_frames) = match opt.looping {
        StreamLoop::Off => (rate / src, n, 0usize),
        StreamLoop::Whole | StreamLoop::From(_) => {
            let at = match opt.looping {
                StreamLoop::From(s) => ((s.max(0.0) * src).round() as usize).min(n.saturating_sub(1)),
                _ => 0,
            };
            let body = n - at;
            let frames = ((body as f64 * rate / src / fs as f64).round() as usize).max(1);
            (frames as f64 * fs as f64 / body as f64, at, frames)
        }
    };
    let looping = !matches!(opt.looping, StreamLoop::Off);
    let tempo_change = (ratio / (rate / src) - 1.0) * 100.0;
    let intro = (intro_src as f64 * ratio).round() as usize;
    let pad = if looping { (fs - intro % fs) % fs } else { 0 };
    let total = if looping { pad + intro + body_frames * fs } else { (intro).next_multiple_of(fs).max(fs) };
    let mut clipped = false;
    let lanes: Vec<Vec<i16>> = chans
        .iter()
        .map(|c| {
            // The looped part's end sees its own start next, as it will when it plays.
            let mut x = c.clone();
            if looping {
                x.extend_from_slice(&c[intro_src..(intro_src + 64).min(n)]);
            }
            let y = resample(&x, ratio);
            let mut lane = vec![0i16; total];
            let keep = if looping { intro + body_frames * fs } else { intro };
            for (k, v) in y.iter().take(keep).enumerate() {
                clipped |= v.abs() > 1.0;
                lane[pad + k] = (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
            }
            lane
        })
        .collect();
    let frames = total / fs;
    let loop_sector = if looping { ((pad + intro) / fs) as u32 } else { 0 };
    // ADPCM, a unit (one channel's share of a frame) at a time.
    let unit = fs / adpcm::SAMPLES_PER_BLOCK;
    let mut data = vec![0u8; frames * SECTOR];
    let mut snr_num = 0f64;
    let mut snr_den = 0f64;
    for (c, lane) in lanes.iter().enumerate() {
        let (mut h1, mut h2) = (0i32, 0i32);
        let mut back = Vec::with_capacity(SECTOR / channels as usize);
        for f in 0..frames {
            let at = f * SECTOR + c * (SECTOR / channels as usize);
            back.clear();
            for k in 0..unit {
                let s = &lane[f * fs + k * adpcm::SAMPLES_PER_BLOCK..f * fs + (k + 1) * adpcm::SAMPLES_PER_BLOCK];
                let (head, body, a, b) = adpcm::encode_stream_block(s, h1, h2, k == 0);
                (h1, h2) = (a, b);
                let o = at + k * 16;
                data[o..o + 2].copy_from_slice(&head);
                data[o + 2..o + 16].copy_from_slice(&body);
            }
            back.extend(adpcm::decode(&data[at..at + unit * 16]).pcm);
            for (k, &v) in back.iter().enumerate() {
                let w = lane[f * fs + k] as f64;
                snr_num += w * w;
                snr_den += (w - v as f64).powi(2);
            }
        }
    }
    let seg_vbl = (SEG as f64 * fs as f64 / rate * VBLANK_HZ * 256.0).round().clamp(1.0, 65535.0) as u16;
    let head = Head {
        channels,
        flags: if looping { LOOP } else { 0 },
        bounce: ring.bounce,
        pitch,
        volume: (opt.volume.clamp(0.0, 1.0) * 0x3FFF as f32).round() as u16,
        sectors: frames as u32,
        loop_sector,
        ring_offset: ring.offset,
        ring_frames: ring.frames,
        seg_vbl,
    };
    head.check()?;
    let mut bytes = vec![0u8; SECTOR];
    bytes[..HEAD_BYTES].copy_from_slice(&head.to_bytes());
    let note = serde_json::json!({ "title": title, "sourceSeconds": n as f64 / src, "sourceRate": pcm.rate });
    let note = serde_json::to_vec(&note).unwrap_or_default();
    bytes[NOTE_AT..NOTE_AT + note.len().min(SECTOR - NOTE_AT)].copy_from_slice(&note[..note.len().min(SECTOR - NOTE_AT)]);
    bytes.extend_from_slice(&data);
    let info = StreamInfo {
        head,
        seconds: total as f64 / rate,
        source_seconds: n as f64 / src,
        source_rate: pcm.rate,
        source_channels: pcm.channels,
        rate,
        loop_seconds: looping.then(|| (pad + intro) as f64 / rate),
        tempo_change,
        bytes: bytes.len() as u32,
        snr_db: if snr_den == 0.0 { 99.0 } else { 10.0 * (snr_num.max(1.0) / snr_den).log10() },
        gain_db: 20.0 * (gain as f64).log10(),
        clipped,
        ahead_seconds: ring.ahead_seconds(channels, rate),
        title: title.map(str::to_string),
    };
    Ok(Encoded { bytes, info })
}

/// The header and the note of a stream file.
pub fn read(bytes: &[u8]) -> Option<(Head, Option<String>)> {
    let head = Head::parse(bytes)?;
    let note = bytes.get(NOTE_AT..SECTOR)?;
    let end = note.iter().position(|&b| b == 0).unwrap_or(note.len());
    let title = serde_json::from_slice::<serde_json::Value>(&note[..end]).ok().and_then(|v| v.get("title")?.as_str().map(str::to_string));
    Some((head, title))
}

/// The stream file `bytes` with its ring and bounce set to `ring` (the bank around it changed).
pub fn with_ring(bytes: &[u8], ring: Ring) -> Option<Vec<u8>> {
    let mut h = Head::parse(bytes)?;
    h.ring_offset = ring.offset;
    h.ring_frames = ring.frames;
    h.bounce = ring.bounce;
    h.check().ok()?;
    let mut out = bytes.to_vec();
    out[..HEAD_BYTES].copy_from_slice(&h.to_bytes());
    Some(out)
}

/// The stream decoded back, as the game plays it: per channel, the data once, then
/// `again` seconds more from the loop (so the loop's seam can be heard).
pub fn decode_back(bytes: &[u8], again: f64) -> Option<(Head, Vec<Vec<i16>>)> {
    let h = Head::parse(bytes)?;
    let fs = h.frame_samples();
    let unit = SECTOR / h.channels as usize;
    let mut lanes = vec![Vec::with_capacity(h.sectors as usize * fs); h.channels as usize];
    for f in 0..h.sectors as usize {
        let at = SECTOR + f * SECTOR;
        let frame = bytes.get(at..at + SECTOR)?;
        for (c, lane) in lanes.iter_mut().enumerate() {
            lane.extend(adpcm::decode(&frame[c * unit..(c + 1) * unit]).pcm);
        }
    }
    if h.looping() {
        let from = h.loop_sector as usize * fs;
        let more = ((again * h.rate()) as usize).min(lanes[0].len() - from);
        for lane in &mut lanes {
            let tail: Vec<i16> = lane[from..from + more].to_vec();
            lane.extend(tail);
        }
    }
    Some((h, lanes))
}

/// [`decode_back`] as a 16-bit WAV (stereo when the stream is).
pub fn preview_wav(bytes: &[u8], again: f64) -> Option<Vec<u8>> {
    let (h, lanes) = decode_back(bytes, again)?;
    let ch = lanes.len() as u16;
    let rate = h.rate().round() as u32;
    let frames = lanes[0].len();
    let data = frames * ch as usize * 2;
    let mut w = Vec::with_capacity(44 + data);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&ch.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * ch as u32 * 2).to_le_bytes());
    w.extend_from_slice(&(ch * 2).to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data as u32).to_le_bytes());
    for k in 0..frames {
        for lane in &lanes {
            w.extend_from_slice(&lane[k].to_le_bytes());
        }
    }
    Some(w)
}

/// `x` resampled by `ratio` (output samples per input sample): a 32-tap windowed sinc from a
/// 512-phase table, band-limited to the lower rate. Outside `x` is silence.
pub fn resample(x: &[f32], ratio: f64) -> Vec<f32> {
    const TAPS: usize = 16;
    const PHASES: usize = 512;
    if x.is_empty() || ratio <= 0.0 {
        return Vec::new();
    }
    let cutoff = ratio.min(1.0) * 0.94;
    let mut table = vec![0f32; (PHASES + 1) * 2 * TAPS];
    for p in 0..=PHASES {
        let frac = p as f64 / PHASES as f64;
        let row = &mut table[p * 2 * TAPS..(p + 1) * 2 * TAPS];
        let mut sum = 0f64;
        let mut h = [0f64; 2 * TAPS];
        for (k, hk) in h.iter_mut().enumerate() {
            // Tap k reads x[centre + k - TAPS + 1]; its distance from the output point:
            let t = frac - (k as f64 - TAPS as f64 + 1.0);
            let w = if t.abs() >= TAPS as f64 { 0.0 } else { 0.42 + 0.5 * (std::f64::consts::PI * t / TAPS as f64).cos() + 0.08 * (2.0 * std::f64::consts::PI * t / TAPS as f64).cos() };
            let s = if t.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * t * cutoff).sin() / (std::f64::consts::PI * t * cutoff) };
            *hk = s * w;
            sum += *hk;
        }
        for (r, hk) in row.iter_mut().zip(h) {
            *r = (hk / sum) as f32;
        }
    }
    let n = ((x.len() as f64) * ratio).round().max(1.0) as usize;
    let step = 1.0 / ratio;
    (0..n)
        .map(|i| {
            let pos = i as f64 * step;
            let centre = pos.floor() as isize;
            let p = ((pos - centre as f64) * PHASES as f64).round() as usize;
            let row = &table[p * 2 * TAPS..(p + 1) * 2 * TAPS];
            let first = centre - TAPS as isize + 1;
            let mut acc = 0f32;
            if first >= 0 && (first as usize + 2 * TAPS) <= x.len() {
                let s = &x[first as usize..first as usize + 2 * TAPS];
                for k in 0..2 * TAPS {
                    acc += s[k] * row[k];
                }
            } else {
                for (k, r) in row.iter().enumerate() {
                    let j = first + k as isize;
                    if j >= 0 && (j as usize) < x.len() {
                        acc += x[j as usize] * r;
                    }
                }
            }
            acc
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f64, rate: u32) -> Pcm {
        let n = (seconds * rate as f64) as usize;
        let left: Vec<f32> = (0..n).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / rate as f32).sin() * 0.5).collect();
        let right: Vec<f32> = (0..n).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 660.0 / rate as f32).sin() * 0.25).collect();
        Pcm { rate, channels: 2, left, right, title: None }
    }

    const RING: Ring = Ring { offset: 0x40, frames: 48, bounce: 4 };

    #[test]
    fn a_looped_stereo_stream_is_whole_frames_with_a_clean_header() {
        let e = encode(&tone(3.3, 44_100), &StreamOptions::default(), RING, Some("Test")).unwrap();
        let h = e.info.head;
        assert_eq!((h.channels, h.flags, h.pitch), (2, LOOP, 0x0800));
        assert_eq!(h.loop_sector, 0);
        assert_eq!(e.bytes.len(), SECTOR * (1 + h.sectors as usize));
        assert!(e.info.tempo_change.abs() < 3.0, "{}", e.info.tempo_change);
        assert!(e.info.snr_db > 25.0, "{}", e.info.snr_db);
        assert_eq!(read(&e.bytes).unwrap().1.as_deref(), Some("Test"));
        // Every block's flags are 0x02, and each unit's first block uses filter 0.
        for f in 0..h.sectors as usize {
            let frame = &e.bytes[SECTOR * (1 + f)..SECTOR * (2 + f)];
            assert!(frame.chunks(16).all(|b| b[1] == 0x02));
            assert_eq!(frame[0] >> 4, 0);
            assert_eq!(frame[0x400] >> 4, 0);
        }
    }

    #[test]
    fn an_intro_pads_to_a_frame_and_the_loop_lands_on_it() {
        let opt = StreamOptions { looping: StreamLoop::From(1.0), stereo: false, ..Default::default() };
        let e = encode(&tone(4.0, 32_000), &opt, RING, None).unwrap();
        let h = e.info.head;
        assert_eq!(h.channels, 1);
        let fs = frame_samples(1) as f64;
        // The intro (1 s, trimmed with the rest to fit the loop) padded at the front to whole
        // frames: the loop starts within a frame after it.
        let at = e.info.loop_seconds.unwrap();
        let intro = 1.0 + e.info.tempo_change / 100.0;
        assert!((intro - 0.001..intro + fs / e.info.rate).contains(&at), "{at} {intro}");
        assert_eq!(h.loop_sector as f64 * fs / e.info.rate, at);
        let (_, lanes) = decode_back(&e.bytes, 1.0).unwrap();
        assert_eq!(lanes.len(), 1);
        assert!(lanes[0].len() > h.sectors as usize * frame_samples(1));
    }

    #[test]
    fn once_through_has_no_loop() {
        let opt = StreamOptions { looping: StreamLoop::Off, ..Default::default() };
        let e = encode(&tone(1.0, 22_050), &opt, RING, None).unwrap();
        assert!(!e.info.head.looping());
        assert_eq!(e.info.loop_seconds, None);
        assert_eq!(e.info.head.sectors as usize, 22_050usize.div_ceil(1792));
        assert!(preview_wav(&e.bytes, 0.0).unwrap().starts_with(b"RIFF"));
    }

    #[test]
    fn rings_fit_the_room_the_bank_leaves() {
        let r = Ring::fit(40_000, 0x1000).unwrap();
        assert_eq!(r.offset, 40_000u32.next_multiple_of(64));
        assert_eq!(r.frames, ((BANK_BYTES - r.offset) / 2048) as u16);
        assert_eq!(r.bounce, 3);
        assert!(Ring::fit(80_000, 0x1000).is_err());
        assert!(Ring::fit(0, 0x2100).is_err());
        assert_eq!(Ring::fit(0, 0).unwrap(), Ring { offset: 64, frames: 55, bounce: 5 });
    }

    #[test]
    fn resampling_keeps_a_tone() {
        let x: Vec<f32> = (0..44_100).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 44_100.0).sin() * 0.5).collect();
        let y = resample(&x, 0.5);
        assert_eq!(y.len(), 22_050);
        let peak = y[1000..21_000].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
        // 1 kHz at 22,050: a period of 22.05 samples.
        let want = (5000.0f32 * 2.0 * std::f32::consts::PI * 1000.0 / 22_050.0).sin() * 0.5;
        assert!((y[5000] - want).abs() < 0.02, "{} vs {want}", y[5000]);
    }
}
