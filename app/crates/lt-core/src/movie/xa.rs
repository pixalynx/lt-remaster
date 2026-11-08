//! CD-XA ADPCM (Form 2 audio sectors), after psx-spx "CDROM XA Audio ADPCM
//! Compression".
//!
//! A sector's 2324 data bytes hold 18 sound groups of 128 bytes (+ 20 zero
//! bytes). A group is 16 header bytes (unit headers at 0x04–0x0B: bits 0–3
//! range, 4–5 filter) and 28 little-endian data words; word `j` packs sample
//! `j` of every unit — eight 4-bit units or four 8-bit units. In stereo the
//! even units are left and the odd units right.

use super::demux::XaCoding;

const POS: [i32; 4] = [0, 60, 115, 98];
const NEG: [i32; 4] = [0, 0, -52, -55];
const GROUPS: usize = 18;
const GROUP: usize = 128;

/// Decoder history, kept across sectors (one pair per channel).
#[derive(Debug, Default, Clone)]
pub struct XaDecoder {
    old: [i32; 2],
    older: [i32; 2],
}

impl XaDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one sector's samples to `out`, interleaved L/R when stereo.
    /// `data` is the sector's user data (from 0x18, at least 18 × 128 bytes).
    pub fn decode_sector(&mut self, data: &[u8], coding: XaCoding, out: &mut Vec<i16>) {
        let units = if coding.bits == 8 { 4 } else { 8 };
        let mut buf = [[0i16; 28]; 8];
        for g in 0..GROUPS {
            let Some(group) = data.get(g * GROUP..(g + 1) * GROUP) else { return };
            for (u, samples) in buf.iter_mut().enumerate().take(units) {
                let ch = if coding.stereo { u & 1 } else { 0 };
                self.unit(group, u, coding.bits, ch, samples);
            }
            if coding.stereo {
                for pair in buf[..units].as_chunks::<2>().0 {
                    for (l, r) in pair[0].iter().zip(&pair[1]) {
                        out.push(*l);
                        out.push(*r);
                    }
                }
            } else {
                for samples in buf.iter().take(units) {
                    out.extend_from_slice(samples);
                }
            }
        }
    }

    /// Decodes 28 samples of unit `u` of one sound group.
    fn unit(&mut self, group: &[u8], u: usize, bits: u8, ch: usize, out: &mut [i16; 28]) {
        let header = group[4 + u];
        // Ranges 13–15 are reserved and behave like 9.
        let range = match header & 0x0F {
            r @ 0..=12 => r as u32,
            _ => 9,
        };
        let filter = ((header >> 4) & 0x03) as usize;
        let (f0, f1) = (POS[filter], NEG[filter]);
        let (mut old, mut older) = (self.old[ch], self.older[ch]);
        for (j, s) in out.iter_mut().enumerate() {
            let word = &group[16 + j * 4..16 + j * 4 + 4];
            // The raw sample left-aligned in 16 bits, then shifted right by the range.
            let raw = if bits == 8 {
                (word[u] as i8 as i32) << 8
            } else {
                let nibble = (word[u / 2] >> ((u & 1) * 4)) & 0x0F;
                ((nibble as i32) << 28 >> 28) << 12
            };
            let v = (raw >> range) + ((old * f0 + older * f1 + 32) >> 6);
            let v = v.clamp(-0x8000, 0x7FFF);
            *s = v as i16;
            older = old;
            old = v;
        }
        self.old[ch] = old;
        self.older[ch] = older;
    }
}

/// Encoder state, kept across sectors (one pair per channel) — the
/// decoder's history, so each unit is predicted from what will be heard.
#[derive(Debug, Default, Clone)]
pub struct XaEncoder {
    old: [i32; 2],
    older: [i32; 2],
}

/// Data bytes of an audio sector the encoder fills (18 groups; the last 20 stay zero).
pub const SECTOR_DATA: usize = 2324;

impl XaEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// One 4-bit stereo sector (coding 0x01) from 2016 L/R sample pairs
    /// (`pcm` interleaved; short input is padded with silence).
    pub fn encode_stereo(&mut self, pcm: &[i16]) -> Vec<u8> {
        let mut out = vec![0u8; SECTOR_DATA];
        let at = |i: usize| pcm.get(i).copied().unwrap_or(0) as i32;
        for g in 0..GROUPS {
            let group = &mut out[g * GROUP..(g + 1) * GROUP];
            for u in 0..8 {
                // Unit u: channel u & 1, pairs (u / 2) of 28 frames.
                let ch = u & 1;
                let base = (g * 4 + u / 2) * 28;
                let samples: [i32; 28] = std::array::from_fn(|j| at((base + j) * 2 + ch));
                let (header, nibbles) = self.unit(&samples, ch);
                group[4 + u] = header;
                for (j, n) in nibbles.iter().enumerate() {
                    group[16 + j * 4 + u / 2] |= (*n as u8 & 0x0F) << ((u & 1) * 4);
                }
            }
            // The header copies: units 0–3 at 0x00, units 4–7 at 0x0C.
            for k in 0..4 {
                group[k] = group[4 + k];
                group[12 + k] = group[8 + k];
            }
        }
        out
    }

    /// The best filter and range for 28 samples, judged by the decoded
    /// error; returns the unit header and the nibbles, and keeps the history.
    fn unit(&mut self, s: &[i32; 28], ch: usize) -> (u8, [i8; 28]) {
        let mut best: Option<(i64, u8, [i8; 28], i32, i32)> = None;
        for filter in 0..4 {
            let (f0, f1) = (POS[filter], NEG[filter]);
            for range in 0..=12u32 {
                let step = 1i32 << (12 - range);
                let (mut old, mut older) = (self.old[ch], self.older[ch]);
                let mut err = 0i64;
                let mut nib = [0i8; 28];
                for j in 0..28 {
                    let pred = (old * f0 + older * f1 + 32) >> 6;
                    let want = s[j] - pred;
                    let n = ((want as f64 / step as f64).round() as i32).clamp(-8, 7);
                    let v = ((((n << 28) >> 28) << 12) >> range) + pred;
                    let v = v.clamp(-0x8000, 0x7FFF);
                    let d = (s[j] - v) as i64;
                    err += d * d;
                    nib[j] = n as i8;
                    older = old;
                    old = v;
                }
                if best.as_ref().is_none_or(|b| err < b.0) {
                    best = Some((err, (filter as u8) << 4 | range as u8, nib, old, older));
                }
            }
        }
        let (_, header, nib, old, older) = best.expect("16 candidates");
        self.old[ch] = old;
        self.older[ch] = older;
        (header, nib)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_tone_the_decoder_plays_back() {
        let c = XaCoding::from_byte(0x01);
        let pcm: Vec<i16> = (0..4032 * 3).map(|i| {
            let t = (i / 2) as f64 / 37_800.0;
            let f = if i % 2 == 0 { 440.0 } else { 660.0 };
            ((t * f * std::f64::consts::TAU).sin() * 12_000.0) as i16
        }).collect();
        let (mut enc, mut dec) = (XaEncoder::new(), XaDecoder::new());
        let mut back = Vec::new();
        for chunk in pcm.chunks(4032) {
            dec.decode_sector(&enc.encode_stereo(chunk), c, &mut back);
        }
        assert_eq!(back.len(), pcm.len());
        let (mut sig, mut err) = (0f64, 0f64);
        for (a, b) in pcm.iter().zip(&back) {
            sig += (*a as f64).powi(2);
            err += (*a as f64 - *b as f64).powi(2);
        }
        let snr = 10.0 * (sig / err.max(1.0)).log10();
        assert!(snr > 30.0, "XA round trip {snr:.1} dB");
    }

    #[test]
    fn silence_and_range() {
        let c = XaCoding::from_byte(0x01);
        let mut out = Vec::new();
        XaDecoder::new().decode_sector(&[0u8; 2324], c, &mut out);
        assert_eq!(out.len(), 2016 * 2);
        assert!(out.iter().all(|&s| s == 0));

        // One group: unit 0 (left) range 0, filter 0, nibble 7 → 7 << 12.
        let mut d = vec![0u8; 2324];
        d[4] = 0x00;
        d[5] = 0x0C; // right: range 12 → nibble value itself
        for j in 0..28 {
            d[16 + j * 4] = 0x17; // left nibble 7, right nibble 1
        }
        let mut out = Vec::new();
        XaDecoder::new().decode_sector(&d, c, &mut out);
        assert_eq!(out[0], 7 << 12);
        assert_eq!(out[1], 1);
    }
}
