//! SPU ADSR envelopes. A TVB stores `ADSR1 | ADSR2 << 16` per slot (+0x400);
//! `SeqVoice_ApplySample` writes both halves to the voice on note-on.
//!
//! ADSR1: bit 15 attack exponential, 14–10 attack shift, 9–8 attack step
//! (7−n), 7–4 decay shift (step −8, always exponential), 3–0 sustain level
//! ((n+1)·0x800). ADSR2: bit 15 sustain exponential, 14 sustain decreases,
//! 12–8 sustain shift, 7–6 sustain step (7−n up, −8+n down), 5 release
//! exponential, 4–0 release shift (step −8). One envelope step lasts
//! `1 << max(0, shift−11)` samples at 44.1 kHz and moves `step << max(0, 11−shift)`.

use serde::Serialize;

pub const RATE: u32 = 44_100;
const MAX: i32 = 0x7FFF;

/// How long the sustain phase is held when drawing an envelope.
pub const HOLD_SECONDS: f32 = 0.45;
/// Points in a drawn envelope.
pub const ENV_POINTS: usize = 134;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AdsrView {
    /// Attack, decay, sustain (held 0.45 s), release in ms.
    pub ms: [f32; 4],
    /// Phase ends as fractions of the drawn envelope.
    pub marks: [f32; 3],
    /// 134 samples of the envelope level, 0–100.
    pub env: Vec<u8>,
    /// Sustain level, 0–100.
    pub sustain_level: u8,
    pub sustain_down: bool,
    pub sustain_exp: bool,
    /// When the sustain phase decreases: ms from the sustain level to silence
    /// (capped at 10 s); 0 when it holds or rises. For the approximate sampler.
    pub sustain_fade_ms: f32,
}

/// A run of `count` samples at one level.
struct Runs {
    runs: Vec<(u32, u16)>,
    total: u64,
}

impl Runs {
    fn push(&mut self, count: u32, level: i32) {
        self.runs.push((count, level as u16));
        self.total += count as u64;
    }

    /// Level at sample `at`.
    fn level_at(&self, at: u64) -> u16 {
        let mut acc = 0u64;
        for &(n, l) in &self.runs {
            acc += n as u64;
            if at < acc {
                return l;
            }
        }
        self.runs.last().map(|r| r.1).unwrap_or(0)
    }
}

struct Phase {
    exp: bool,
    down: bool,
    shift: i32,
    step: i32,
}

/// Runs one phase while `cond(level)` holds, up to `max` samples; returns the
/// samples spent.
fn run(level: &mut i32, p: &Phase, cond: impl Fn(i32) -> bool, max: u64, out: &mut Option<&mut Runs>) -> u64 {
    let mut n = 0u64;
    while cond(*level) && n < max {
        let mut cycles = 1u32 << (p.shift - 11).max(0);
        let mut step = p.step << (11 - p.shift).max(0);
        if p.exp && !p.down && *level > 0x6000 {
            cycles *= 4;
        }
        if p.exp && p.down {
            step = (step * *level) >> 15;
        }
        if n + cycles as u64 > max {
            // The phase is cut off mid-step (the drawn sustain hold).
            if let Some(r) = out.as_deref_mut() {
                r.push((max - n) as u32, *level);
            }
            n = max;
            break;
        }
        if let Some(r) = out.as_deref_mut() {
            r.push(cycles, *level);
        }
        n += cycles as u64;
        *level = (*level + step).clamp(0, MAX);
        // A decreasing exponential step can round to 0 and never finish.
        if step == 0 && p.down {
            break;
        }
    }
    n
}

fn phases(word: u32) -> (Phase, Phase, Phase, Phase, i32) {
    let a1 = (word & 0xFFFF) as i32;
    let a2 = (word >> 16) as i32;
    let attack = Phase { exp: a1 >> 15 != 0, down: false, shift: (a1 >> 10) & 0x1F, step: 7 - ((a1 >> 8) & 3) };
    let decay = Phase { exp: true, down: true, shift: (a1 >> 4) & 0xF, step: -8 };
    let sus_down = (a2 >> 14) & 1 != 0;
    let sustain = Phase {
        exp: a2 >> 15 != 0,
        down: sus_down,
        shift: (a2 >> 8) & 0x1F,
        step: if sus_down { -8 + ((a2 >> 6) & 3) } else { 7 - ((a2 >> 6) & 3) },
    };
    let release = Phase { exp: (a2 >> 5) & 1 != 0, down: true, shift: a2 & 0x1F, step: -8 };
    let level = ((a1 & 0xF) + 1) * 0x800;
    (attack, decay, sustain, release, level)
}

/// Simulates the envelope (attack → decay → sustain held 0.45 s → release)
/// and samples it for drawing.
pub fn view(word: u32) -> AdsrView {
    let (attack, decay, sustain, release, sl) = phases(word);
    let cap = RATE as u64 * 3;
    let mut runs = Runs { runs: Vec::new(), total: 0 };
    let mut level = 0;
    let ph = {
        let mut out = Some(&mut runs);
        let na = run(&mut level, &attack, |l| l < MAX, cap, &mut out);
        let nd = run(&mut level, &decay, |l| l > sl, cap, &mut out);
        let ns = run(&mut level, &sustain, |_| true, (RATE as f32 * HOLD_SECONDS) as u64, &mut out);
        let nr = run(&mut level, &release, |l| l > 0, cap, &mut out);
        [na, nd, ns, nr]
    };
    let total = runs.total.max(1);
    let env = (0..ENV_POINTS)
        .map(|x| {
            let at = (x as u64 * total / ENV_POINTS as u64).min(total - 1);
            ((runs.level_at(at) as f32 / MAX as f32) * 100.0).round() as u8
        })
        .collect();
    let ms = ph.map(|n| ((n as f64 / RATE as f64 * 1000.0) * 100.0).round() as f32 / 100.0);
    let mut acc = 0u64;
    let marks = [0, 1, 2].map(|k| {
        acc += ph[k];
        ((acc as f64 / total as f64) * 10_000.0).round() as f32 / 10_000.0
    });
    let sustain_fade_ms = if sustain.down {
        let mut l = sl.min(MAX);
        let n = run(&mut l, &sustain, |l| l > 0, RATE as u64 * 10, &mut None);
        (n as f64 / RATE as f64 * 1000.0).round() as f32
    } else {
        0.0
    };
    AdsrView {
        ms,
        marks,
        env,
        sustain_level: ((sl as f32 / MAX as f32) * 100.0).round().min(100.0) as u8,
        sustain_down: sustain.down,
        sustain_exp: sustain.exp,
        sustain_fade_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retail_default_word() {
        // 0x1FC080FF: exponential attack at shift 0 (6 samples), sustain
        // level 100% (no decay), sustain rises (holds at max), release shift 0.
        let v = view(0x1FC0_80FF);
        assert_eq!(v.sustain_level, 100);
        assert!(!v.sustain_down);
        assert!(v.ms[0] < 1.0);
        assert_eq!(v.env.len(), ENV_POINTS);
        assert_eq!(v.sustain_fade_ms, 0.0);
    }
}
