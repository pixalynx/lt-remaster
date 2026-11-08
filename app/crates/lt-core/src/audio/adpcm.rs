//! SPU-ADPCM (the headerless body of a VAG). See `docs/screens/14-audio.md` §4.6.
//!
//! A 16-byte block is `b0 = shift | filter << 4`, `b1 = flags` (bit 0 end →
//! jump to the loop address, bit 1 repeat (else mute at the end), bit 2 loop
//! start), then 14 bytes = 28 nibbles, low nibble first.

/// Bytes per block and PCM samples per block.
pub const BLOCK: usize = 16;
pub const SAMPLES_PER_BLOCK: usize = 28;

/// The SPU's two-tap prediction filters (×1/64).
const POS: [i32; 5] = [0, 60, 115, 98, 122];
const NEG: [i32; 5] = [0, 0, -52, -55, -60];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decoded {
    pub pcm: Vec<i16>,
    /// PCM index of the first block flagged "loop start" (bit 2).
    pub loop_start: Option<usize>,
    /// PCM length up to and including the block flagged "end" (bit 0).
    pub end: Option<usize>,
    /// The end block also has "repeat" (bit 1): the voice jumps back to the
    /// loop start instead of muting. Retail looped samples use 06…02…03.
    pub repeat: bool,
    /// Whole blocks decoded.
    pub blocks: usize,
    /// Peak absolute sample value.
    pub peak: u16,
}

impl Decoded {
    pub fn looped(&self) -> bool {
        self.repeat && self.loop_start.is_some()
    }
}

/// Decodes blocks until one carries the end flag (or the data runs out).
/// Prediction is `(h1·F+ + h2·F−) >> 6` (floor) like the design-time decoder;
/// reserved shifts 13–15 behave as 9 on the SPU.
pub fn decode(adpcm: &[u8]) -> Decoded {
    let mut out = Decoded::default();
    out.pcm.reserve(adpcm.len() / BLOCK * SAMPLES_PER_BLOCK);
    let (mut h1, mut h2) = (0i32, 0i32);
    for blk in adpcm.as_chunks::<BLOCK>().0 {
        let shift = match blk[0] & 0x0F {
            s @ 0..=12 => s as u32,
            _ => 9,
        };
        let filter = ((blk[0] >> 4) as usize).min(4);
        let flags = blk[1];
        if flags & 4 != 0 && out.loop_start.is_none() {
            out.loop_start = Some(out.pcm.len());
        }
        for k in 0..SAMPLES_PER_BLOCK {
            let nib = (blk[2 + k / 2] >> ((k & 1) * 4)) & 0x0F;
            // Sign-extend the nibble into bits 12..15, then shift right.
            let mut s = (((nib as i32) << 28) >> 16) >> shift;
            s += (h1 * POS[filter] + h2 * NEG[filter]) >> 6;
            let s = s.clamp(-32768, 32767);
            out.peak = out.peak.max(s.unsigned_abs() as u16);
            out.pcm.push(s as i16);
            h2 = h1;
            h1 = s;
        }
        out.blocks += 1;
        if flags & 1 != 0 {
            out.end = Some(out.pcm.len());
            out.repeat = flags & 2 != 0;
            break;
        }
    }
    out
}

/* ───────────────────────── Encoder ───────────────────────── */

/// Block flags the retail banks use.
pub mod flag {
    pub const END: u8 = 0x01;
    pub const REPEAT: u8 = 0x02;
    pub const LOOP_START: u8 = 0x04;
}

/// The nibble the decoder turns closest to `residual` at `shift`, and the
/// sample it decodes to after `pred`.
fn quantise(residual: i32, pred: i32, shift: u32) -> (i32, i32) {
    let step = 1i32 << (12 - shift);
    let nib = ((residual as f64) / step as f64).round().clamp(-8.0, 7.0) as i32;
    let s = (((nib << 28) >> 16) >> shift) + pred;
    (nib, s.clamp(-32768, 32767))
}

/// One block of up to 28 samples at state (h1, h2): the header byte, the 14
/// data bytes and the state after it, picking the filter and shift with the
/// least squared error the decoder would make.
fn encode_block(samples: &[i16], h1: i32, h2: i32) -> (u8, [u8; 14], i32, i32) {
    let mut best: Option<(i64, usize, u32)> = None;
    for f in 0..5 {
        for shift in 0..=12u32 {
            let (mut a, mut b, mut err) = (h1, h2, 0i64);
            for k in 0..SAMPLES_PER_BLOCK {
                let x = samples.get(k).copied().unwrap_or(0) as i32;
                let pred = (a * POS[f] + b * NEG[f]) >> 6;
                let (_, s) = quantise(x - pred, pred, shift);
                err += ((x - s) as i64).pow(2);
                b = a;
                a = s;
                if best.is_some_and(|(e, _, _)| err >= e) {
                    break;
                }
            }
            if best.is_none_or(|(e, _, _)| err < e) {
                best = Some((err, f, shift));
            }
        }
    }
    let (_, f, shift) = best.expect("at least one candidate");
    let (mut a, mut b) = (h1, h2);
    let mut data = [0u8; 14];
    for k in 0..SAMPLES_PER_BLOCK {
        let x = samples.get(k).copied().unwrap_or(0) as i32;
        let pred = (a * POS[f] + b * NEG[f]) >> 6;
        let (nib, s) = quantise(x - pred, pred, shift);
        data[k / 2] |= ((nib as u8) & 0x0F) << ((k & 1) * 4);
        b = a;
        a = s;
    }
    (shift as u8 | (f as u8) << 4, data, a, b)
}

/// One block for a long stream ([`crate::audio::stream`]): like [`encode_block`], but the shift
/// is worked out from the residual's peak for each filter (and the next one up tried too, as
/// error feedback can overshoot), ten candidates rather than 65. `filter0` keeps the block
/// independent of the history: a block a voice may start on, or jump to, decodes the same.
pub fn encode_stream_block(samples: &[i16], h1: i32, h2: i32, filter0: bool) -> ([u8; 2], [u8; 14], i32, i32) {
    let mut best: Option<(i64, usize, u32)> = None;
    let filters = if filter0 { 0..1 } else { 0..5 };
    for f in filters {
        // The residual's peak with the input as its own history: the shift that covers it.
        let (mut a, mut b, mut peak) = (h1, h2, 0i32);
        for k in 0..SAMPLES_PER_BLOCK {
            let x = samples.get(k).copied().unwrap_or(0) as i32;
            peak = peak.max((x - ((a * POS[f] + b * NEG[f]) >> 6)).abs());
            b = a;
            a = x;
        }
        let mut need = 0u32;
        while need < 12 && (7i32 << need) < peak {
            need += 1;
        }
        let first = 12 - need;
        for shift in [first, first.saturating_sub(1)] {
            let (mut a, mut b, mut err) = (h1, h2, 0i64);
            for k in 0..SAMPLES_PER_BLOCK {
                let x = samples.get(k).copied().unwrap_or(0) as i32;
                let pred = (a * POS[f] + b * NEG[f]) >> 6;
                let (_, s) = quantise(x - pred, pred, shift);
                err += ((x - s) as i64).pow(2);
                b = a;
                a = s;
            }
            if best.is_none_or(|(e, _, _)| err < e) {
                best = Some((err, f, shift));
            }
        }
    }
    let (_, f, shift) = best.expect("at least one candidate");
    let (mut a, mut b) = (h1, h2);
    let mut data = [0u8; 14];
    for k in 0..SAMPLES_PER_BLOCK {
        let x = samples.get(k).copied().unwrap_or(0) as i32;
        let pred = (a * POS[f] + b * NEG[f]) >> 6;
        let (nib, s) = quantise(x - pred, pred, shift);
        data[k / 2] |= ((nib as u8) & 0x0F) << ((k & 1) * 4);
        b = a;
        a = s;
    }
    ([shift as u8 | (f as u8) << 4, flag::REPEAT], data, a, b)
}

/// SPU-ADPCM for `pcm`, laid out like the retail banks: a silent block,
/// then the data — one-shot `04 00… 01` plus a silent `07` block, or looped
/// from `loop_start` (snapped down to a block) `02… 06 02… 03` with the
/// loop's end trimmed to a whole block.
pub fn encode(pcm: &[i16], loop_start: Option<usize>) -> Vec<u8> {
    let n = SAMPLES_PER_BLOCK;
    let (pcm, ls) = match loop_start {
        Some(l) if pcm.len() >= n => {
            let ls = (l.min(pcm.len() - n) / n) * n;
            let end = ls + ((pcm.len() - ls) / n).max(1) * n;
            (&pcm[..end.min(pcm.len())], Some(ls / n))
        }
        _ => (pcm, None),
    };
    let blocks = pcm.len().div_ceil(n).max(1);
    let mut out = vec![0u8; BLOCK];
    let (mut h1, mut h2) = (0, 0);
    for i in 0..blocks {
        let chunk = &pcm[(i * n).min(pcm.len())..((i + 1) * n).min(pcm.len())];
        let (head, data, a, b) = encode_block(chunk, h1, h2);
        (h1, h2) = (a, b);
        let last = i + 1 == blocks;
        let flags = match ls {
            // A one-block loop (retail `… 02 07`) starts and ends on the same block.
            Some(l) if i == l && last => flag::LOOP_START | flag::END | flag::REPEAT,
            Some(_) if last => flag::END | flag::REPEAT,
            Some(l) if i == l => flag::LOOP_START | flag::REPEAT,
            Some(_) => flag::REPEAT,
            None if last => flag::END,
            None if i == 0 => flag::LOOP_START,
            None => 0,
        };
        // A one-block one-shot carries both its start and its end.
        let flags = if ls.is_none() && blocks == 1 { flag::LOOP_START | flag::END } else { flags };
        out.push(head);
        out.push(flags);
        out.extend_from_slice(&data);
    }
    if ls.is_none() {
        let mut silent = [0u8; BLOCK];
        silent[1] = flag::END | flag::REPEAT | flag::LOOP_START;
        out.extend_from_slice(&silent);
    }
    out
}

/// Signal-to-noise ratio of `got` against `want`, in dB (equal lengths compared).
pub fn snr_db(want: &[i16], got: &[i16]) -> f64 {
    let (mut sig, mut noise) = (0f64, 0f64);
    for (a, b) in want.iter().zip(got) {
        sig += (*a as f64).powi(2);
        noise += (*a as f64 - *b as f64).powi(2);
    }
    if noise == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (sig.max(1.0) / noise).log10()
}

/// `cols` columns of `[max, min]`, scaled to −100…100 against the sample's own
/// peak (flat pairs, ready for a waveform path).
pub fn peaks(pcm: &[i16], cols: usize) -> Vec<i8> {
    let n = pcm.len();
    if n == 0 || cols == 0 {
        return Vec::new();
    }
    let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0).max(1) as f32;
    let mut out = Vec::with_capacity(cols * 2);
    for x in 0..cols {
        let a = x * n / cols;
        let b = ((x + 1) * n / cols).max(a + 1).min(n);
        let seg = &pcm[a.min(n - 1)..b];
        let hi = *seg.iter().max().unwrap() as f32;
        let lo = *seg.iter().min().unwrap() as f32;
        out.push((hi / peak * 100.0).round() as i8);
        out.push((lo / peak * 100.0).round() as i8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_filters_and_flags() {
        // Block 0: shift 12, filter 0, loop start; nibbles 1 and −1 alternate.
        let mut a = vec![0x0C, 0x06];
        a.extend([0xF1u8; 14]);
        // Block 1: shift 0, filter 1 (prediction only), end + repeat.
        a.extend([0x10, 0x03]);
        a.extend([0u8; 14]);
        let d = decode(&a);
        assert_eq!(d.pcm.len(), 56);
        assert_eq!(&d.pcm[..2], &[1, -1]);
        assert_eq!(d.loop_start, Some(0));
        assert_eq!(d.end, Some(56));
        assert!(d.looped());
        // Filter 1 decays the last sample: −1·60 >> 6 = −1.
        assert_eq!(d.pcm[28], -1);
    }

    #[test]
    fn stops_at_the_end_flag() {
        let mut a = vec![0u8; 16];
        a[1] = 0x01;
        a.extend([0x0Cu8, 0, 0x77, 0x77, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let d = decode(&a);
        assert_eq!(d.blocks, 1);
        assert_eq!(d.peak, 0);
        assert!(!d.looped());
    }

    #[test]
    fn encodes_a_one_shot_and_a_loop_like_retail() {
        let pcm: Vec<i16> = (0..300).map(|i| ((i as f64 * 0.21).sin() * 12000.0) as i16).collect();
        let one = encode(&pcm, None);
        let flags: Vec<u8> = one.chunks(16).map(|b| b[1]).collect();
        assert_eq!(flags.first(), Some(&0));
        assert_eq!(flags[1], flag::LOOP_START);
        assert_eq!(&flags[flags.len() - 2..], &[flag::END, 0x07]);
        let d = decode(&one);
        assert_eq!(d.pcm.len(), 28 + 11 * 28);
        assert!(snr_db(&pcm, &d.pcm[28..]) > 30.0, "{}", snr_db(&pcm, &d.pcm[28..]));
        let looped = encode(&pcm, Some(100));
        let flags: Vec<u8> = looped.chunks(16).map(|b| b[1]).collect();
        // 100 snaps to 84 (block 3 of the data, block 4 with the silent one).
        assert_eq!(&flags[..6], &[0, 2, 2, 2, 6, 2]);
        assert_eq!(flags.last(), Some(&3));
        let d = decode(&looped);
        assert!(d.looped());
        assert_eq!(d.loop_start, Some(28 + 84));
        assert_eq!((d.pcm.len() - 28 - 84) % 28, 0);
    }

    #[test]
    fn peaks_are_scaled_pairs() {
        let pcm: Vec<i16> = (0..100).map(|i| if i % 2 == 0 { 1000 } else { -500 }).collect();
        let p = peaks(&pcm, 10);
        assert_eq!(p.len(), 20);
        assert_eq!((p[0], p[1]), (100, -50));
    }
}
