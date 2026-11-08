//! Audio files in for streamed music: MP3, OGG Vorbis, FLAC, WAV, AIFF and
//! M4A/AAC, decoded with Symphonia to left/right floats at the file's rate.
//! A mono file gives the same samples twice; channels past the second are
//! dropped.

use std::io::Cursor;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymErr;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// A decoded file: −1…1 per channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub rate: u32,
    /// Channels in the file (before the mix to two).
    pub channels: u16,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Title from the file's tags, if any.
    pub title: Option<String>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.rate.max(1) as f64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn bad(m: impl Into<String>) -> DecodeError {
    DecodeError(m.into())
}

/// The longest file accepted, in seconds (a CD's worth of music is far more
/// than a disc image can spare; this only stops runaway decodes).
pub const MAX_SECONDS: u32 = 20 * 60;

/// Decodes `bytes`; `ext` (e.g. "mp3") helps the probe, but the content decides.
pub fn decode(bytes: &[u8], ext: Option<&str>) -> Result<Pcm, DecodeError> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext {
        hint.with_extension(&e.to_ascii_lowercase());
    }
    let mut probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .map_err(|e| bad(format!("Not an audio file Living Tool can read ({e}). Use MP3, OGG, FLAC, WAV, AIFF or M4A.")))?;
    let mut title = None;
    let mut take_title = |m: Option<&symphonia::core::meta::MetadataRevision>| {
        if let Some(rev) = m {
            for t in rev.tags() {
                if t.std_key == Some(symphonia::core::meta::StandardTagKey::TrackTitle) {
                    let v = t.value.to_string();
                    if !v.trim().is_empty() {
                        title = Some(v.trim().to_string());
                    }
                }
            }
        }
    };
    if let Some(m) = probed.metadata.get() {
        take_title(m.current());
    }
    let mut format = probed.format;
    take_title(format.metadata().current());
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| bad("The file has no audio track."))?
        .clone();
    let rate = track.codec_params.sample_rate.ok_or_else(|| bad("The file doesn't say its sample rate."))?;
    let mut dec = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| bad(format!("Living Tool can't decode this file's audio ({e}).")))?;
    let mut channels = track.codec_params.channels.map(|c| c.count() as u16).unwrap_or(0);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut buf: Option<SampleBuffer<f32>> = None;
    let cap = MAX_SECONDS as usize * rate as usize;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymErr::ResetRequired) => break,
            Err(e) => return Err(bad(format!("The file stopped decoding part way ({e})."))),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let audio = match dec.decode(&packet) {
            Ok(a) => a,
            // A damaged frame: skip it, as players do.
            Err(SymErr::DecodeError(_)) => continue,
            Err(SymErr::IoError(_)) => continue,
            Err(e) => return Err(bad(format!("The file stopped decoding part way ({e})."))),
        };
        let spec = *audio.spec();
        let n = spec.channels.count();
        if n == 0 {
            continue;
        }
        channels = n as u16;
        let b = buf.get_or_insert_with(|| SampleBuffer::new(audio.capacity() as u64, spec));
        if b.capacity() < audio.capacity() * n {
            *b = SampleBuffer::new(audio.capacity() as u64, spec);
        }
        b.copy_interleaved_ref(audio);
        for f in b.samples().chunks_exact(n) {
            left.push(f[0].clamp(-1.0, 1.0));
            right.push(f[if n > 1 { 1 } else { 0 }].clamp(-1.0, 1.0));
        }
        if left.len() > cap {
            return Err(bad(format!("The file is longer than {} minutes.", MAX_SECONDS / 60)));
        }
    }
    if left.is_empty() {
        return Err(bad("The file has no audio in it."));
    }
    Ok(Pcm { rate, channels, left, right, title })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_stereo_wav() {
        // 16-bit stereo, 8 frames: left rises, right falls.
        let frames: Vec<(i16, i16)> = (0..8).map(|i| (i * 1000, -i * 1000)).collect();
        let mut b = Vec::new();
        let data_len = frames.len() as u32 * 4;
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&32_000u32.to_le_bytes());
        b.extend_from_slice(&(32_000u32 * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for (l, r) in &frames {
            b.extend_from_slice(&l.to_le_bytes());
            b.extend_from_slice(&r.to_le_bytes());
        }
        let p = decode(&b, Some("wav")).unwrap();
        assert_eq!((p.rate, p.channels, p.frames()), (32_000, 2, 8));
        assert!((p.left[3] - 3000.0 / 32768.0).abs() < 1e-4);
        assert!((p.right[3] + 3000.0 / 32768.0).abs() < 1e-4);
    }

    #[test]
    fn refuses_what_is_not_audio() {
        assert!(decode(b"definitely not audio at all", Some("mp3")).is_err());
    }
}
