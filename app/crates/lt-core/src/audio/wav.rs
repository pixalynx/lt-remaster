//! WAV in (screen 14 §5.5): RIFF/WAVE with PCM 8/16/24/32-bit integer or
//! 32-bit float samples, mixed down to mono (and kept as left/right for
//! movie audio), and a windowed-sinc resampler
//! to the rate a sample is replaced at.

use crate::container::{le16, le32};

#[derive(Debug, Clone, PartialEq)]
pub struct Wav {
    pub rate: u32,
    pub channels: u16,
    /// Mono, −1…1.
    pub samples: Vec<f32>,
    /// The first two channels, −1…1 (a mono file gives the same samples twice).
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavError(pub String);

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn bad(m: impl Into<String>) -> WavError {
    WavError(m.into())
}

/// Reads a WAV file's bytes.
pub fn read(b: &[u8]) -> Result<Wav, WavError> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad("Not a WAV file (no RIFF/WAVE header)."));
    }
    let mut at = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    while at + 8 <= b.len() {
        let (id, len) = (&b[at..at + 4], le32(b, at + 4) as usize);
        let body = &b[at + 8..(at + 8 + len).min(b.len())];
        match id {
            b"fmt " if body.len() >= 16 => {
                let mut format = le16(body, 0);
                // WAVE_FORMAT_EXTENSIBLE: the real format is the GUID's first u16.
                if format == 0xFFFE && body.len() >= 26 {
                    format = le16(body, 24);
                }
                fmt = Some((format, le16(body, 2), le32(body, 4), le16(body, 14)));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    let (format, channels, rate, bits) = fmt.ok_or_else(|| bad("The WAV has no fmt chunk."))?;
    let data = data.ok_or_else(|| bad("The WAV has no data chunk."))?;
    if channels == 0 || rate == 0 {
        return Err(bad("The WAV declares no channels or no rate."));
    }
    let width = (bits as usize).div_ceil(8);
    let frame = width * channels as usize;
    let sample = |s: &[u8]| -> Result<f32, WavError> {
        Ok(match (format, bits) {
            (1, 8) => (s[0] as f32 - 128.0) / 128.0,
            (1, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            _ => return Err(bad(format!("WAV format {format} at {bits} bits isn't supported; save it as 16-bit PCM."))),
        })
    };
    let mut samples = Vec::with_capacity(data.len() / frame.max(1));
    let mut lr = (Vec::new(), Vec::new());
    for f in data.chunks_exact(frame) {
        let mut sum = 0f32;
        for c in 0..channels as usize {
            let v = sample(&f[c * width..(c + 1) * width])?;
            sum += v;
            match c {
                0 => lr.0.push(v.clamp(-1.0, 1.0)),
                1 => lr.1.push(v.clamp(-1.0, 1.0)),
                _ => {}
            }
        }
        samples.push((sum / channels as f32).clamp(-1.0, 1.0));
    }
    if channels == 1 {
        lr.1 = lr.0.clone();
    }
    Ok(Wav { rate, channels, samples, left: lr.0, right: lr.1 })
}

/// `x` resampled from `from` Hz to `to` Hz (windowed sinc, 16 taps a side,
/// band-limited to the lower of the two rates).
pub fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let n = ((x.len() as f64) * ratio).round().max(1.0) as usize;
    let cutoff = ratio.min(1.0);
    const TAPS: isize = 16;
    let sinc = |t: f64| if t.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t) };
    (0..n)
        .map(|i| {
            let pos = i as f64 / ratio;
            let centre = pos.floor() as isize;
            let mut acc = 0f64;
            let mut norm = 0f64;
            for k in centre - TAPS + 1..=centre + TAPS {
                let t = pos - k as f64;
                let w = 0.5 + 0.5 * (std::f64::consts::PI * t / TAPS as f64).cos();
                let h = cutoff * sinc(t * cutoff) * if t.abs() < TAPS as f64 { w } else { 0.0 };
                let v = x.get(k.clamp(0, x.len() as isize - 1) as usize).copied().unwrap_or(0.0) as f64;
                acc += v * h;
                norm += h;
            }
            (if norm.abs() > 1e-9 { acc / norm } else { 0.0 }) as f32
        })
        .collect()
}

/// −1…1 to s16 with rounding and clipping.
pub fn to_i16(x: &[f32]) -> Vec<i16> {
    x.iter().map(|v| (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_exporter_writes() {
        let pcm: Vec<i16> = (0..64).map(|i| (i * 300 - 9000) as i16).collect();
        let w = read(&super::super::view::wav_mono16(&pcm, 22_050)).unwrap();
        assert_eq!((w.rate, w.channels, w.samples.len()), (22_050, 1, 64));
        assert_eq!(to_i16(&w.samples)[10], pcm[10]);
    }

    #[test]
    fn resampling_keeps_length_and_tone() {
        let x: Vec<f32> = (0..4410).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 441.0 / 44_100.0).sin() * 0.5).collect();
        let y = resample(&x, 44_100, 22_050);
        assert_eq!(y.len(), 2205);
        // Still a 441 Hz sine: one period every 50 samples at 22,050 Hz.
        let peak = y[100..2100].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.03, "{peak}");
        assert!((y[150] - (150.0f32 * 2.0 * std::f32::consts::PI / 50.0).sin() * 0.5).abs() < 0.05);
    }
}
