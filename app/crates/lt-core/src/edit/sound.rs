//! Samples replaced in a sound bank (screen 14 §5.5, research 06 phase 1):
//! a WAV mixed to mono, resampled to the rate the slot is replaced at,
//! encoded to SPU-ADPCM like the retail samples ([`crate::audio::adpcm::encode`]),
//! spliced into the bank's TVB ([`crate::audio::tvb::replace_slot`]) and the
//! file rebuilt around it:
//!
//! | Bank | File rebuilt |
//! |---|---|
//! | BGM, NPC special sound, field SE (`sys/8`) | `[tsq, tvb]` header and TSQ kept, the TVB replaced |
//! | System SE | `sys/1` (SE.TVB) replaced |
//! | toy, enemy, Baby form | the TVB section, the container re-laid ([`super::model::replace_section_index`]) |
//!
//! [`import_midi`] puts a MIDI file into a song cue: notes dealt out to up
//! to 24 TSQ tracks ([`crate::audio::midi::to_song`]), the old song's bytes
//! taken out and the new one appended ([`crate::audio::tsq::replace_song`]),
//! the file rebuilt the same way with the new TSQ.
//!
//! TVBs carry no sample rate. A slot is replaced at 22,050 Hz by default,
//! the rate the design-time tools assume; cues pitch it through the game's
//! pitch table either way. Every slot sharing the offset changes with it.
//!
//! [`add_sound`] makes a new sound effect (screen 14 §15): an audio file
//! becomes a sample in a free slot ([`crate::audio::tvb::add_slot`]) and a
//! cue in a free cue ([`crate::audio::tsq::add_cue`], [`sound_stream`]), which
//! a script plays with `sfx 0x<sound slot><cue>`. [`carry`] and [`put_back`]
//! keep a project's sounds when the bank is rebuilt from its base.

use serde::{Deserialize, Serialize};

use crate::audio::{adpcm, bank, midi, tsq, tvb, wav, BankKind, BankRef};

use super::{EditError, Result};

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

/// The rate a slot is replaced at when none is given.
pub const DEFAULT_RATE: u32 = 22_050;

/// How the new sample loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "at")]
pub enum Looping {
    /// Loop like the old sample: from the same point in time, if it looped.
    #[default]
    Keep,
    /// Play once.
    Off,
    /// Loop from this sample (at the new rate; snapped down to a 28-sample block).
    From(usize),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceOptions {
    /// Hz the WAV is resampled to (default 22,050).
    #[serde(default)]
    pub rate: Option<u32>,
    #[serde(default)]
    pub looping: Looping,
    /// Linear gain applied before encoding (1.0 = as is).
    #[serde(default)]
    pub gain: Option<f32>,
}

impl Default for ReplaceOptions {
    fn default() -> Self {
        Self { rate: None, looping: Looping::Keep, gain: None }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleReplaced {
    pub slot: usize,
    /// Other slots at the same offset: they play the new sample too.
    pub aliases: Vec<usize>,
    pub rate: u32,
    pub seconds: f32,
    pub bytes_before: u32,
    pub bytes_after: u32,
    /// Loop start in samples from the sample's first, if it loops.
    pub loop_start: Option<usize>,
    pub was_looped: bool,
    /// The decoded ADPCM against the resampled WAV.
    pub snr_db: f32,
    /// The WAV clipped somewhere (after gain).
    pub clipped: bool,
    /// ADPCM bytes in the bank after the change, and the file's bytes.
    pub bank_adpcm: u32,
    pub file_bytes: u32,
    pub budgets: Vec<bank::Budget>,
}

/// Files that change: (index into the bank's `files`, new bytes).
pub type Changed = Vec<(usize, Vec<u8>)>;

/// The files of bank `r` (in `r.files` order) with `slot` replaced by the
/// WAV in `wav_bytes`; only the files that change are returned.
pub fn replace_sample(r: &BankRef, files: &[Vec<u8>], slot: usize, wav_bytes: &[u8], opt: &ReplaceOptions) -> Result<(Changed, SampleReplaced)> {
    let b = bank::split(r, files).ok_or_else(|| bad("BANK_BAD", format!("{}'s sound bank doesn't parse.", r.id)))?;
    let t = tvb::Tvb::parse(&b.tvb).ok_or_else(|| bad("BANK_BAD", "The voice bank doesn't parse."))?;
    let span = t.spans().into_iter().flatten().find(|s| s.slot == slot).ok_or_else(|| bad("SAMPLE_UNUSED", format!("Slot {slot} holds no sample.")))?;
    let old = adpcm::decode(t.slot_bytes(slot).unwrap_or_default());
    let w = wav::read(wav_bytes).map_err(|e| bad("WAV_BAD", e.0))?;
    if w.samples.is_empty() {
        return Err(bad("WAV_EMPTY", "The WAV has no samples."));
    }
    let rate = opt.rate.unwrap_or(DEFAULT_RATE).clamp(1_000, 48_000);
    let gain = opt.gain.unwrap_or(1.0);
    let mut x = wav::resample(&w.samples, w.rate, rate);
    let clipped = x.iter().any(|v| (v * gain).abs() > 1.0);
    for v in &mut x {
        *v *= gain;
    }
    let pcm = wav::to_i16(&x);
    // The old loop point in samples from the data start (the silent block before it isn't counted).
    let lead = adpcm::SAMPLES_PER_BLOCK;
    let old_loop = old.loop_start.filter(|_| old.looped()).map(|l| l.saturating_sub(lead));
    let loop_start = match opt.looping {
        Looping::Off => None,
        Looping::From(at) => Some(at.min(pcm.len().saturating_sub(1))),
        Looping::Keep => old_loop.map(|l| ((l as f64) * rate as f64 / DEFAULT_RATE as f64) as usize).map(|l| l.min(pcm.len().saturating_sub(1))),
    };
    let encoded = adpcm::encode(&pcm, loop_start);
    let back = adpcm::decode(&encoded);
    let snr = adpcm::snr_db(&pcm, back.pcm.get(lead..).unwrap_or_default()) as f32;
    let new_tvb = tvb::replace_slot(&b.tvb[..tvb::HEADER + t.size as usize], slot, &encoded).ok_or_else(|| bad("BANK_BAD", "The sample couldn't be spliced into the bank."))?;
    let nt = tvb::Tvb::parse(&new_tvb).ok_or_else(|| bad("BANK_BAD", "The new bank doesn't parse."))?;
    let out = rebuild(r, files, &b, &new_tvb)?;
    // The rebuilt bank must read back: the new sample where the slot points, every other sample unchanged.
    let mut after: Vec<Vec<u8>> = files.to_vec();
    for (i, f) in &out {
        after[*i] = f.clone();
    }
    let ab = bank::split(r, &after).ok_or_else(|| bad("SAMPLE_VERIFY", "The rebuilt bank doesn't parse."))?;
    let at = tvb::Tvb::parse(&ab.tvb).ok_or_else(|| bad("SAMPLE_VERIFY", "The rebuilt voice bank doesn't parse."))?;
    if at.slot_bytes(slot).map(|s| &s[..encoded.len().min(s.len())]) != Some(&encoded[..]) {
        return Err(bad("SAMPLE_VERIFY", "The new sample doesn't read back from the rebuilt bank."));
    }
    for i in (0..tvb::SLOTS).filter(|&i| i != slot && !span.shares.contains(&i)) {
        if t.slot_bytes(i) != at.slot_bytes(i) {
            return Err(bad("SAMPLE_VERIFY", format!("Slot {i} changed in the rebuilt bank.")));
        }
    }
    let file_bytes = ab.file_bytes;
    let budgets = bank::placement(&ab, nt.size).budgets;
    if let Some(over) = budgets.iter().find(|x| x.value > x.max && !x.soft) {
        return Err(bad("SAMPLE_BUDGET", format!("{}: {} B, the game stops at {} B. Shorten the sample or lower its rate.", over.label, over.value, over.max)));
    }
    let report = SampleReplaced {
        slot,
        aliases: span.shares.clone(),
        rate,
        seconds: pcm.len() as f32 / rate as f32,
        bytes_before: span.bytes,
        bytes_after: encoded.len().next_multiple_of(16) as u32,
        loop_start: loop_start.map(|l| (l / adpcm::SAMPLES_PER_BLOCK) * adpcm::SAMPLES_PER_BLOCK),
        was_looped: old.looped(),
        snr_db: snr,
        clipped,
        bank_adpcm: nt.size,
        file_bytes,
        budgets,
    };
    Ok((out, report))
}

/// The bank's files with `new_tvb` in place of its voice bank.
fn rebuild(r: &BankRef, files: &[Vec<u8>], b: &bank::Bank, new_tvb: &[u8]) -> Result<Changed> {
    let pad = |mut v: Vec<u8>| {
        v.resize(v.len().div_ceil(2048) * 2048, 0);
        v
    };
    let f = &files[0];
    Ok(match r.kind {
        BankKind::Music | BankKind::NpcSound | BankKind::FieldSe => {
            let mut out = f[..b.tvb_offset as usize].to_vec();
            out.extend_from_slice(new_tvb);
            vec![(0, pad(out))]
        }
        BankKind::SystemSe => vec![(1, pad(new_tvb.to_vec()))],
        BankKind::Toy | BankKind::Enemy | BankKind::BabyForm => {
            let (kind, index) = match r.kind {
                BankKind::Toy => (super::model::Container::Toy, 5),
                BankKind::Enemy => (super::model::Container::Enemy, 4),
                _ => (super::model::Container::Pb, 3),
            };
            vec![(0, super::model::replace_section_index(kind, f, index, new_tvb.to_vec())?)]
        }
    })
}

/* ───────────────────────── MIDI into a song cue ───────────────────────── */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CueImported {
    pub cue: usize,
    #[serde(flatten)]
    pub song: midi::SongReport,
    /// Sample slots the song plays that hold no sample in this bank.
    pub missing_slots: Vec<u8>,
    pub seconds: f32,
    /// TSQ bytes before and after, and what the old song gave back.
    pub tsq_before: u32,
    pub tsq_after: u32,
    pub freed: u32,
    pub file_bytes: u32,
    pub budgets: Vec<bank::Budget>,
}

/// The files of bank `r` with song cue `cue` replaced by the MIDI file in
/// `midi_bytes` (an empty cue takes a new song); only changed files come back.
pub fn import_midi(r: &BankRef, files: &[Vec<u8>], cue: usize, midi_bytes: &[u8], opt: &midi::ImportOptions) -> Result<(Changed, CueImported)> {
    let b = bank::split(r, files).ok_or_else(|| bad("BANK_BAD", format!("{}'s sound bank doesn't parse.", r.id)))?;
    if !(1..tsq::CUES - 1).contains(&cue) {
        return Err(bad("CUE_RANGE", format!("Cue {cue} can't be played (1–50).")));
    }
    if let Some(c) = tsq::cues(&b.tsq).into_iter().find(|c| c.index == cue) {
        if c.kind == tsq::CueKind::Sfx {
            return Err(bad("CUE_SFX", format!("Cue {cue} is a sound effect (one stream); MIDI goes into song cues.")));
        }
    }
    let smf = midi::read(midi_bytes).map_err(|e| bad("MIDI_BAD", e.0))?;
    let (song, rep) = midi::to_song(&smf, opt).map_err(|e| bad("MIDI_EMPTY", e.0))?;
    let t = tvb::Tvb::parse(&b.tvb).ok_or_else(|| bad("BANK_BAD", "The voice bank doesn't parse."))?;
    let used: Vec<usize> = t.spans().into_iter().flatten().map(|s| s.slot).collect();
    let missing_slots = rep.programs.iter().map(|p| p.slot).filter(|s| !used.contains(&(*s as usize))).collect::<Vec<_>>();
    let (new_tsq, laid) = tsq::replace_song(&b.tsq, cue, &song).ok_or_else(|| bad("SEQ_LAYOUT", "The song doesn't fit the TSQ's 16-bit offsets."))?;
    let out = rebuild_tsq(r, files, &b, &new_tsq)?;
    let mut after: Vec<Vec<u8>> = files.to_vec();
    for (i, f) in &out {
        after[*i] = f.clone();
    }
    let ab = bank::split(r, &after).ok_or_else(|| bad("SEQ_VERIFY", "The rebuilt bank doesn't parse."))?;
    if ab.tsq[..new_tsq.len().min(ab.tsq.len())] != new_tsq[..] || ab.tvb[..tvb::HEADER] != b.tvb[..tvb::HEADER] {
        return Err(bad("SEQ_VERIFY", "The new sequence doesn't read back from the rebuilt bank."));
    }
    let placed = bank::placement(&ab, t.size);
    if let Some(over) = placed.budgets.iter().find(|x| x.value > x.max && !x.soft) {
        return Err(bad("SEQ_BUDGET", format!("{}: {} B, the game stops at {} B. Shorten the song or use fewer tracks.", over.label, over.value, over.max)));
    }
    let report = CueImported {
        cue,
        seconds: rep.ticks as f32 / tsq::TICKS_PER_SECOND as f32,
        song: rep,
        missing_slots,
        tsq_before: b.tsq.len() as u32,
        tsq_after: new_tsq.len() as u32,
        freed: laid.freed as u32,
        file_bytes: ab.file_bytes,
        budgets: placed.budgets,
    };
    Ok((out, report))
}

/// The bank's files with `new_tsq` in place of its sequence bank.
fn rebuild_tsq(r: &BankRef, files: &[Vec<u8>], b: &bank::Bank, new_tsq: &[u8]) -> Result<Changed> {
    let pad = |mut v: Vec<u8>| {
        v.resize(v.len().div_ceil(2048) * 2048, 0);
        v
    };
    let f = &files[0];
    Ok(match r.kind {
        BankKind::Music | BankKind::NpcSound => {
            // `u32 tsqOfs, u32 tvbOfs`, then the TSQ and the voice bank as it was.
            let tvb_ofs = 8 + new_tsq.len();
            let mut out = Vec::with_capacity(tvb_ofs + b.tvb.len());
            out.extend(8u32.to_le_bytes());
            out.extend((tvb_ofs as u32).to_le_bytes());
            out.extend_from_slice(new_tsq);
            let t = tvb::Tvb::parse(&b.tvb).ok_or_else(|| bad("BANK_BAD", "The voice bank doesn't parse."))?;
            out.extend_from_slice(&b.tvb[..t.total_bytes().min(b.tvb.len())]);
            vec![(0, pad(out))]
        }
        BankKind::FieldSe => {
            // SE2.DAT's voice bank stays at +0x408: the TSQ keeps its 0x400 bytes.
            if new_tsq.len() > b.tsq.len() {
                return Err(bad("SEQ_BUDGET", format!("SE2's sequence holds {} B; the new one is {} B.", b.tsq.len(), new_tsq.len())));
            }
            let mut out = f.clone();
            let at = b.tsq_offset as usize;
            out[at..at + b.tsq.len()].fill(0);
            out[at..at + new_tsq.len()].copy_from_slice(new_tsq);
            vec![(0, out)]
        }
        BankKind::SystemSe => {
            if new_tsq.len() > 0x1000 {
                return Err(bad("SEQ_BUDGET", format!("SE.TSQ is loaded into 0x1000 B; the new one is {} B.", new_tsq.len())));
            }
            vec![(0, pad(new_tsq.to_vec()))]
        }
        BankKind::Toy | BankKind::Enemy | BankKind::BabyForm => {
            let (kind, index) = match r.kind {
                BankKind::Toy => (super::model::Container::Toy, 3),
                BankKind::Enemy => (super::model::Container::Enemy, 2),
                _ => (super::model::Container::Pb, 2),
            };
            vec![(0, super::model::replace_section_index(kind, f, index, new_tsq.to_vec())?)]
        }
    })
}

/* ───────────────────────── New sound effects ───────────────────────── */

/// How an audio file becomes a sound effect.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundOptions {
    /// Hz the sound is resampled to; it plays at that rate (an explicit SPU pitch).
    #[serde(default)]
    pub rate: Option<u32>,
    /// 0–1 of the loudest a cue plays (expression 255); retail effects sit at 0.23–0.47.
    #[serde(default)]
    pub volume: Option<f32>,
    /// Loop the sound until a stop (`sfx 0x8000 | id`): ambience, engines, rain.
    #[serde(default)]
    pub looping: bool,
    /// Cut the silence at either end (default on).
    #[serde(default)]
    pub keep_silence: bool,
    /// Bring the peak to −1 dBFS (default on).
    #[serde(default)]
    pub no_normalize: bool,
    /// The bank's reverb on the sound.
    #[serde(default)]
    pub reverb: bool,
}

/// The priority retail sound-effect cues carry (`Sequencer_TriggerSfxCue` takes a voice at or
/// below it).
pub const SFX_PRIORITY: u16 = 0x0090;
/// The default volume: retail effects' expressions run 59–119 of 255, songs' up to 252.
pub const SFX_VOLUME: f32 = 0.5;
/// The longest sound accepted, in seconds (the banks hold far less anyway).
pub const SFX_MAX_SECONDS: f64 = 30.0;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundAdded {
    pub cue: usize,
    /// The sample slot it plays.
    pub slot: usize,
    /// What a script plays it with: `sfx 0x<request>` (sound slot << 8 | cue).
    pub request: u16,
    pub sound_slot: u8,
    pub rate: f64,
    pub seconds: f64,
    pub source_seconds: f64,
    pub looping: bool,
    /// ADPCM bytes of the new sample.
    pub bytes: u32,
    pub snr_db: f32,
    pub clipped: bool,
    pub gain_db: f32,
    /// ADPCM bytes the bank holds after it, and its SPU region.
    pub bank_adpcm: u32,
    pub spu_cap: u32,
    pub tsq_bytes: u32,
    pub file_bytes: u32,
    pub budgets: Vec<bank::Budget>,
}

/// A sound effect lifted out of one bank to put into another (the same bank rebuilt from its base
/// when a song is streamed or put back): its cue stream and the samples it programs.
#[derive(Debug, Clone, PartialEq)]
pub struct Carried {
    pub cue: usize,
    pub flag: u16,
    pub stream: Vec<u8>,
    /// (slot, ADPCM, ADSR word)
    pub samples: Vec<(usize, Vec<u8>, u32)>,
}

/// The kinds of bank a sound effect can go into: banks the field or a scene keeps loaded.
fn takes_sounds(kind: BankKind) -> bool {
    matches!(kind, BankKind::Music | BankKind::SystemSe | BankKind::FieldSe | BankKind::NpcSound)
}

/// The bank's files with `new_tsq` and `new_tvb` (header + body) in place of its own.
fn rebuild_both(r: &BankRef, files: &[Vec<u8>], b: &bank::Bank, new_tsq: &[u8], new_tvb: &[u8]) -> Result<Changed> {
    let pad = |mut v: Vec<u8>| {
        v.resize(v.len().div_ceil(2048) * 2048, 0);
        v
    };
    Ok(match r.kind {
        BankKind::Music | BankKind::NpcSound => {
            let tvb_ofs = 8 + new_tsq.len();
            let mut out = Vec::with_capacity(tvb_ofs + new_tvb.len());
            out.extend(8u32.to_le_bytes());
            out.extend((tvb_ofs as u32).to_le_bytes());
            out.extend_from_slice(new_tsq);
            out.extend_from_slice(new_tvb);
            vec![(0, pad(out))]
        }
        BankKind::FieldSe => {
            if new_tsq.len() > b.tvb_offset as usize - b.tsq_offset as usize {
                return Err(bad("SEQ_BUDGET", format!("SE2's sequence holds {} B; the new one is {} B.", b.tvb_offset - b.tsq_offset, new_tsq.len())));
            }
            let mut out = files[0][..b.tvb_offset as usize].to_vec();
            let at = b.tsq_offset as usize;
            out[at..b.tvb_offset as usize].fill(0);
            out[at..at + new_tsq.len()].copy_from_slice(new_tsq);
            out.extend_from_slice(new_tvb);
            vec![(0, pad(out))]
        }
        BankKind::SystemSe => {
            if new_tsq.len() > 0x1000 {
                return Err(bad("SEQ_BUDGET", format!("SE.TSQ is loaded into 0x1000 B; the new one is {} B.", new_tsq.len())));
            }
            vec![(0, pad(new_tsq.to_vec())), (1, pad(new_tvb.to_vec()))]
        }
        _ => return Err(bad("SOUND_BANK_KIND", format!("{} is a battle bank: new sounds go into music, system, field or NPC sound banks.", r.id))),
    })
}

/// The cue stream of a sound: key off, reverb, expression, program, the note at an explicit
/// pitch, then a wait as long as the sample and key off, or (looping) a wait that never ends.
pub fn sound_stream(slot: u8, pitch: u16, expression: u8, ticks: u32, looping: bool, reverb: bool) -> Vec<u8> {
    let mut s = vec![0xF0, if reverb { 0xE8 } else { 0xE9 }, 0xE0, expression, 0xE2, slot, 0xE5, pitch as u8, (pitch >> 8) as u8];
    if looping {
        // 7F F8 FC FF: wait 127 ticks, jump back to the wait.
        s.extend_from_slice(&[0x7F, 0xF8, 0xFC, 0xFF]);
    } else {
        let mut t = ticks.max(1);
        while t > 0 {
            let d = t.min(0x7F);
            s.push(d as u8);
            t -= d;
        }
        s.extend_from_slice(&[0xF0, 0xFF]);
    }
    s
}

/// The cues a bank has that its base doesn't: sound effects a project added.
pub fn added_cues(r: &BankRef, files: &[Vec<u8>], base: &[Vec<u8>]) -> Vec<usize> {
    let (Some(b), Some(o)) = (bank::split(r, files), bank::split(r, base)) else { return Vec::new() };
    let old: Vec<usize> = tsq::cues(&o.tsq).into_iter().map(|c| c.index).collect();
    tsq::cues(&b.tsq).into_iter().filter(|c| c.kind == tsq::CueKind::Sfx && !old.contains(&c.index)).map(|c| c.index).collect()
}

/// Lifts sound-effect cue `cue` out of the bank (see [`Carried`]).
pub fn carry(r: &BankRef, files: &[Vec<u8>], cue: usize) -> Option<Carried> {
    let b = bank::split(r, files)?;
    let t = tvb::Tvb::parse(&b.tvb)?;
    let (flag, stream) = tsq::sfx_stream(&b.tsq, cue)?;
    let samples = tsq::programs(&b.tsq, cue)
        .into_iter()
        .filter(|&p| (p as usize) < tvb::SLOTS && t.used(p as usize))
        .map(|p| (p as usize, t.slot_bytes(p as usize).unwrap_or_default().to_vec(), t.adsr[p as usize]))
        .collect();
    Some(Carried { cue, flag, stream, samples })
}

/// The bank's files with `sounds` put back: each at its cue and its slots when those are free,
/// else at a free cue and free slots (the stream's programs re-pointed).
pub fn put_back(r: &BankRef, files: &[Vec<u8>], sounds: &[Carried]) -> Result<Changed> {
    if sounds.is_empty() {
        return Ok(Vec::new());
    }
    let b = bank::split(r, files).ok_or_else(|| bad("BANK_BAD", format!("{}'s sound bank doesn't parse.", r.id)))?;
    let mut seq = b.tsq.clone();
    let t0 = tvb::Tvb::parse(&b.tvb).ok_or_else(|| bad("BANK_BAD", "The voice bank doesn't parse."))?;
    let mut voices = b.tvb[..t0.total_bytes().min(b.tvb.len())].to_vec();
    for s in sounds {
        let mut stream = s.stream.clone();
        for (slot, adpcm, adsr) in &s.samples {
            let t = tvb::Tvb::parse(&voices).ok_or_else(|| bad("BANK_BAD", "The voice bank doesn't parse."))?;
            let at = if !t.used(*slot) { *slot } else { (1..tvb::SLOTS).find(|&i| !t.used(i)).ok_or_else(|| bad("SOUND_NO_SLOT", "The bank has no free sample slot."))? };
            voices = tvb::add_slot(&voices, at, adpcm, *adsr).ok_or_else(|| bad("BANK_BAD", "The sample couldn't be added."))?;
            if at != *slot {
                // Re-point the stream's E2 operands at the new slot.
                let mut p = 0;
                while let Some((op, next)) = tsq::op_at(&stream, p) {
                    if op == tsq::Op::Program(*slot as u8) {
                        stream[p + 1] = at as u8;
                    }
                    p = next;
                }
            }
        }
        let cue = if tsq::cues(&seq).iter().any(|c| c.index == s.cue) { tsq::free_cue(&seq).ok_or_else(|| bad("SOUND_NO_CUE", "The bank has no free cue."))? } else { s.cue };
        seq = tsq::add_cue(&seq, cue, s.flag, &stream).ok_or_else(|| bad("SEQ_LAYOUT", "The sequence bank's offsets don't reach that far."))?;
    }
    rebuild_both(r, files, &b, &seq, &voices)
}

/// The bank's files with the audio file `audio` (MP3, OGG, FLAC, WAV, AIFF, M4A) added as a new
/// sound effect: a sample in a free slot and a cue in a free cue, which a script plays with
/// `sfx 0x<sound slot><cue>`. Only the files that change come back.
pub fn add_sound(r: &BankRef, files: &[Vec<u8>], audio: &[u8], ext: Option<&str>, opt: &SoundOptions) -> Result<(Changed, SoundAdded)> {
    if !takes_sounds(r.kind) {
        return Err(bad("SOUND_BANK_KIND", format!("{} is a battle bank: new sounds go into music, system, field or NPC sound banks.", r.id)));
    }
    let b = bank::split(r, files).ok_or_else(|| bad("BANK_BAD", format!("{}'s sound bank doesn't parse.", r.id)))?;
    let t = tvb::Tvb::parse(&b.tvb).ok_or_else(|| bad("BANK_BAD", "The voice bank doesn't parse."))?;
    let pcm = crate::audio::decode::decode(audio, ext).map_err(|e| bad("AUDIO_BAD", e.0))?;
    let mut x: Vec<f32> = pcm.left.iter().zip(&pcm.right).map(|(l, r)| (l + r) * 0.5).collect();
    let source_seconds = x.len() as f64 / pcm.rate as f64;
    if !opt.keep_silence {
        let quiet = 0.003f32;
        let first = x.iter().position(|v| v.abs() > quiet).unwrap_or(0);
        let last = x.iter().rposition(|v| v.abs() > quiet).map_or(x.len(), |p| p + 1);
        let margin = (pcm.rate / 100) as usize;
        x = x[first.saturating_sub(margin)..(last + margin).min(x.len())].to_vec();
    }
    if x.is_empty() {
        return Err(bad("AUDIO_SILENT", "The file is silent."));
    }
    if x.len() as f64 / pcm.rate as f64 > SFX_MAX_SECONDS {
        return Err(bad("SOUND_LONG", format!("The sound is {:.1} s; sound effects stop at {SFX_MAX_SECONDS} s. Stream it as music instead.", x.len() as f64 / pcm.rate as f64)));
    }
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    let gain = if !opt.no_normalize && peak > 1e-4 { (0.891 / peak).min(8.0) } else { 1.0 };
    let pitch = ((opt.rate.unwrap_or(DEFAULT_RATE).clamp(4_000, 44_100) as f64) * 4096.0 / 44_100.0).round().clamp(1.0, 0x3FFF as f64) as u16;
    let rate = pitch as f64 * 44_100.0 / 4096.0;
    let y = crate::audio::stream::resample(&x, rate / pcm.rate as f64);
    let clipped = y.iter().any(|v| (v * gain).abs() > 1.0);
    let lane: Vec<i16> = y.iter().map(|v| (v * gain * 32767.0).round().clamp(-32768.0, 32767.0) as i16).collect();
    let encoded = adpcm::encode(&lane, opt.looping.then_some(0));
    let back = adpcm::decode(&encoded);
    let lead = adpcm::SAMPLES_PER_BLOCK;
    let snr = adpcm::snr_db(&lane, back.pcm.get(lead..).unwrap_or_default()) as f32;
    // A free slot (slot 0 means "no sample" to the system bank's voices) and a free cue.
    let slot = (1..tvb::SLOTS).find(|&i| !t.used(i)).ok_or_else(|| bad("SOUND_NO_SLOT", format!("{} has no free sample slot (128 used).", r.id)))?;
    let cue = tsq::free_cue(&b.tsq).ok_or_else(|| bad("SOUND_NO_CUE", format!("{} has no free cue (1–50 all used).", r.id)))?;
    let seconds = lane.len() as f64 / rate;
    let ticks = (seconds * tsq::TICKS_PER_SECOND as f64).ceil() as u32 + 1;
    let expression = (opt.volume.unwrap_or(SFX_VOLUME).clamp(0.0, 1.0) * 255.0).round() as u8;
    let stream = sound_stream(slot as u8, pitch, expression, ticks, opt.looping, opt.reverb);
    let new_tsq = tsq::add_cue(&b.tsq, cue, SFX_PRIORITY, &stream).ok_or_else(|| bad("SEQ_LAYOUT", "The sequence bank's offsets don't reach that far."))?;
    let new_tvb = tvb::add_slot(&b.tvb[..t.total_bytes().min(b.tvb.len())], slot, &encoded, tvb::DEFAULT_ADSR).ok_or_else(|| bad("BANK_BAD", "The sample couldn't be added."))?;
    let out = rebuild_both(r, files, &b, &new_tsq, &new_tvb)?;
    let mut after: Vec<Vec<u8>> = files.to_vec();
    for (i, f) in &out {
        after[*i] = f.clone();
    }
    let ab = bank::split(r, &after).ok_or_else(|| bad("SOUND_VERIFY", "The rebuilt bank doesn't parse."))?;
    let at = tvb::Tvb::parse(&ab.tvb).ok_or_else(|| bad("SOUND_VERIFY", "The rebuilt voice bank doesn't parse."))?;
    if at.slot_bytes(slot).map(|s| &s[..encoded.len().min(s.len())]) != Some(&encoded[..]) || tsq::sfx_stream(&ab.tsq, cue).map(|(_, s)| s) != Some(stream.clone()) {
        return Err(bad("SOUND_VERIFY", "The new sound doesn't read back from the rebuilt bank."));
    }
    for i in (0..tvb::SLOTS).filter(|&i| i != slot) {
        if t.slot_bytes(i) != at.slot_bytes(i) {
            return Err(bad("SOUND_VERIFY", format!("Slot {i} changed in the rebuilt bank.")));
        }
    }
    let placed = bank::placement(&ab, at.size);
    if let Some(over) = placed.budgets.iter().find(|x| x.value > x.max && !x.soft) {
        let room = placed.spu_cap.saturating_sub(t.size);
        return Err(bad(
            "SOUND_BUDGET",
            format!(
                "{}: {} B, the game stops at {} B ({} B were free; this sound needs {} B). Shorten it, lower its rate, or pick a bank with more room.",
                over.label,
                over.value,
                over.max,
                room,
                encoded.len()
            ),
        ));
    }
    let report = SoundAdded {
        cue,
        slot,
        request: ((placed.sound_slot as u16) << 8) | cue as u16,
        sound_slot: placed.sound_slot,
        rate,
        seconds,
        source_seconds,
        looping: opt.looping,
        bytes: encoded.len() as u32,
        snr_db: snr,
        clipped,
        gain_db: 20.0 * gain.log10(),
        bank_adpcm: at.size,
        spu_cap: placed.spu_cap,
        tsq_bytes: new_tsq.len() as u32,
        file_bytes: ab.file_bytes,
        budgets: placed.budgets,
    };
    Ok((out, report))
}

/// The bank's files without sound-effect cue `cue` and the samples only it played.
pub fn remove_sound(r: &BankRef, files: &[Vec<u8>], cue: usize) -> Result<Changed> {
    let b = bank::split(r, files).ok_or_else(|| bad("BANK_BAD", format!("{}'s sound bank doesn't parse.", r.id)))?;
    if !tsq::cues(&b.tsq).iter().any(|c| c.index == cue && c.kind == tsq::CueKind::Sfx) {
        return Err(bad("SOUND_NOT_FOUND", format!("{} has no sound effect at cue {cue}.", r.id)));
    }
    let new_tsq = tsq::remove_cue(&b.tsq, cue).ok_or_else(|| bad("SEQ_LAYOUT", "The cue can't be taken out."))?;
    let t = tvb::Tvb::parse(&b.tvb).ok_or_else(|| bad("BANK_BAD", "The voice bank doesn't parse."))?;
    let mut voices = b.tvb[..t.total_bytes().min(b.tvb.len())].to_vec();
    let still: std::collections::BTreeSet<u8> = (1..tsq::CUES - 1).flat_map(|c| tsq::programs(&new_tsq, c)).collect();
    for p in tsq::programs(&b.tsq, cue) {
        if !still.contains(&p) && (p as usize) < tvb::SLOTS && t.used(p as usize) {
            voices = tvb::remove_slot(&voices, p as usize).ok_or_else(|| bad("BANK_BAD", "The sample couldn't be taken out."))?;
        }
    }
    rebuild_both(r, files, &b, &new_tsq, &voices)
}
