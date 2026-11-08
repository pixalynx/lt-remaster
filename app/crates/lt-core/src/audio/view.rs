//! Screen-ready views of a bank: cue lanes (notes with real lengths, E0
//! expression, E2 markers, F8 loops, F9 spawned voices), the 128-slot sample
//! grid with peaks, and ADSR envelopes. Payloads stay small: peaks, not PCM.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use super::adpcm;
use super::adsr::{self, AdsrView};
use super::bank::{self, Bank, BankKind, Placement};
use super::tsq::{self, CueKind, Op, Stop};
use super::tvb::{Tvb, SLOTS};

/// Columns of the grid thumbnail and the inspector waveform.
pub const MINI_COLS: usize = 22;
pub const WAVE_COLS: usize = 134;
/// Longest stretch of a cue that is walked (10 minutes).
pub const MAX_TICKS: u32 = 60 * 600;
/// Extra voices started by F9 that a cue may show.
const MAX_SPAWNED: usize = 48;
/// Display length of a note whose sample is unknown.
const UNKNOWN_NOTE_TICKS: u32 = 18;

/// The pitch for note `n` when no EXE table is given: 12-TET, note 48 = 0x1000
/// (44.1 kHz), as in the table at 0x801B7518.
pub fn default_pitch(n: usize) -> u16 {
    (4096.0 * 2f64.powf((n as f64 - 48.0) / 12.0)).round().min(0x3FFF as f64) as u16
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BankRow {
    pub id: String,
    pub kind: BankKind,
    pub index: u16,
    pub name: Option<String>,
    pub sound_slot: u8,
    pub role: String,
    /// Bytes the file really uses, and its sector-padded size.
    pub file_bytes: u32,
    pub file_sectors: u32,
    pub tsq_bytes: u32,
    pub adpcm_bytes: u32,
    pub songs: u16,
    pub sfx: u16,
    /// Used slots and distinct sample offsets.
    pub used_slots: u16,
    pub offsets: u16,
    /// The bank's SPU region size (for the list's percentage).
    pub spu_cap: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackView {
    /// Sample slots played ("s5·43"), or the end byte ("FF end").
    pub label: String,
    /// Stream start in the TSQ.
    pub start: u32,
    /// Started by an F9 in another stream (at `notes[0]`'s tick or later).
    pub spawned: bool,
    /// Flat `[startTick, endTick, note, sampleSlot]`; slot −1 = no E2 yet.
    pub notes: Vec<i32>,
    /// SPU pitch per note (0x1000 = 44.1 kHz), after the EA multiplier.
    pub pitch: Vec<u16>,
    /// Voice volume per note, flat `[left, right]` (expression × E1, 0–0x3FFF).
    pub vol: Vec<u16>,
    /// Flat `[tick, value]` E0 expression changes.
    pub exprs: Vec<u32>,
    /// Ticks of E2 program changes after the first two ticks.
    pub progs: Vec<u32>,
    /// "end", "loop", "stall" or "truncated".
    pub stop: &'static str,
    pub loop_tick: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CueView {
    pub cue: u16,
    /// "song" (24-track table) or "sfx" (one stream).
    pub kind: &'static str,
    pub flag: u16,
    pub offset: u16,
    /// Length: the tick where the last stream ends or loops.
    pub ticks: u32,
    pub r#loop: bool,
    /// Where F8 jumps back to (the earliest target among looping tracks).
    pub loop_tick: Option<u32>,
    /// Tracks with at least one note.
    pub active: u16,
    pub note_ons: u32,
    pub note_offs: u32,
    pub programs: u32,
    pub spawns: u32,
    pub tracks: Vec<TrackView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleView {
    /// Offset from TVB +0x800.
    pub offset: u32,
    pub bytes: u32,
    /// PCM samples decoded (to the end flag).
    pub pcm: u32,
    pub looped: bool,
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
    /// Decodes to silence (peak ≤ 1).
    pub silent: bool,
    /// First slot of its alias group.
    pub first: bool,
    pub shares: Vec<u16>,
    /// Every alias has a different ADSR word.
    pub own_adsr: bool,
    /// ADSR1 | ADSR2 << 16, hex.
    pub adsr: String,
    pub mini: Vec<i8>,
    pub wave: Vec<i8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BankView {
    #[serde(flatten)]
    pub row: BankRow,
    pub tsq_offset: u32,
    pub tvb_offset: u32,
    pub placement: Placement,
    pub cues: Vec<CueView>,
    /// Cues 1–50 that point at the null stream.
    pub empty_cues: u16,
    /// 128 entries, `null` = unused slot.
    pub samples: Vec<Option<SampleView>>,
    pub adsr: BTreeMap<String, AdsrView>,
    pub silent: u16,
    /// Note → SPU pitch (128 entries from the EXE at 0x801B7518).
    pub pitch: Vec<u16>,
}

/// Header facts for the bank list.
pub fn row(b: &Bank) -> BankRow {
    let tvb = Tvb::parse(&b.tvb);
    let cues = tsq::cues(&b.tsq);
    let adpcm = tvb.as_ref().map(|t| t.size).unwrap_or(0);
    let p = bank::placement(b, adpcm);
    BankRow {
        id: b.id.clone(),
        kind: b.kind,
        index: b.index,
        name: b.name.clone(),
        sound_slot: p.sound_slot,
        role: p.role,
        file_bytes: b.file_bytes,
        file_sectors: b.file_sectors,
        tsq_bytes: b.tsq.len() as u32,
        adpcm_bytes: adpcm,
        songs: cues.iter().filter(|c| c.kind == CueKind::Song).count() as u16,
        sfx: cues.iter().filter(|c| c.kind == CueKind::Sfx).count() as u16,
        used_slots: tvb.as_ref().map(|t| t.used_count()).unwrap_or(0) as u16,
        offsets: tvb.as_ref().map(|t| t.distinct_offsets().len()).unwrap_or(0) as u16,
        spu_cap: p.spu_cap,
    }
}

/// A decoded slot: PCM plus loop facts.
pub struct SlotPcm {
    pub decoded: adpcm::Decoded,
    pub span: super::tvb::SlotSpan,
}

/// Decodes every distinct sample once.
pub fn decode_slots(tvb: &Tvb) -> Vec<Option<SlotPcm>> {
    let spans = tvb.spans();
    let mut cache: BTreeMap<u32, adpcm::Decoded> = BTreeMap::new();
    spans
        .into_iter()
        .map(|s| {
            let s = s?;
            let d = cache
                .entry(s.offset)
                .or_insert_with(|| adpcm::decode(tvb.slot_bytes(s.slot).unwrap_or(&[])))
                .clone();
            Some(SlotPcm { decoded: d, span: s })
        })
        .collect()
}

/// The whole screen payload for one bank. `pitch_table` is the EXE's
/// 0x801B7518 (128 × u16); `None` falls back to 12-TET.
pub fn bank_view(b: &Bank, pitch_table: Option<&[u16]>) -> Option<BankView> {
    let tvb = Tvb::parse(&b.tvb)?;
    let slots = decode_slots(&tvb);
    let pitch: Vec<u16> = match pitch_table {
        Some(t) if t.len() >= 128 => t[..128].to_vec(),
        _ => (0..128).map(default_pitch).collect(),
    };

    let mut adsr_map = BTreeMap::new();
    let samples: Vec<Option<SampleView>> = slots
        .iter()
        .enumerate()
        .map(|(slot, s)| {
            let s = s.as_ref()?;
            let d = &s.decoded;
            let word = tvb.adsr[slot];
            let key = format!("{word:08X}");
            adsr_map.entry(key.clone()).or_insert_with(|| adsr::view(word));
            let silent = d.peak <= 1;
            Some(SampleView {
                offset: s.span.offset,
                bytes: s.span.bytes,
                pcm: d.pcm.len() as u32,
                looped: d.looped(),
                loop_start: d.loop_start.map(|v| v as u32),
                loop_end: d.end.map(|v| v as u32),
                silent,
                first: s.span.first == slot,
                shares: s.span.shares.iter().map(|&v| v as u16).collect(),
                own_adsr: s.span.shares.iter().all(|&g| tvb.adsr[g] != word),
                adsr: key,
                mini: if silent { Vec::new() } else { adpcm::peaks(&d.pcm, MINI_COLS) },
                wave: if silent { Vec::new() } else { adpcm::peaks(&d.pcm, WAVE_COLS) },
            })
        })
        .collect();

    // Length in ticks of each slot at a given pitch (one-shots end by themselves).
    let lengths: Vec<Option<(u32, bool)>> =
        slots.iter().map(|s| s.as_ref().map(|s| (s.decoded.pcm.len() as u32, s.decoded.looped()))).collect();
    let cue_list = tsq::cues(&b.tsq);
    let cues = cue_list.iter().map(|c| cue_view(&b.tsq, c, &pitch, &lengths)).collect();
    let silent = samples.iter().flatten().filter(|s| s.silent).count() as u16;
    let row = row(b);
    let placement = bank::placement(b, tvb.size);
    Some(BankView {
        empty_cues: (tsq::CUES as u16 - 2).saturating_sub(cue_list.len() as u16),
        row,
        tsq_offset: b.tsq_offset,
        tvb_offset: b.tvb_offset,
        placement,
        cues,
        samples,
        adsr: adsr_map,
        silent,
        pitch,
    })
}

/// Ticks a sample plays at `pitch` before it ends by itself.
fn sample_ticks(len: u32, pitch: u16) -> u32 {
    if pitch == 0 {
        return u32::MAX;
    }
    let seconds = len as f64 * 4096.0 / pitch as f64 / adsr::RATE as f64;
    (seconds * tsq::TICKS_PER_SECOND as f64).ceil().max(1.0) as u32
}

/// The nearest note for an explicit SPU pitch (E5).
fn note_for_pitch(p: u16) -> i32 {
    if p == 0 {
        return 0;
    }
    (48.0 + 12.0 * (p as f64 / 4096.0).log2()).round().clamp(0.0, 95.0) as i32
}

/// Per-track state that a spawned voice inherits (`*e = *c` in the F9 case).
#[derive(Clone, Copy)]
struct Voice {
    prog: Option<u8>,
    mult: u16,
    vl: u32,
    vr: u32,
    expr: u32,
}

/// Song and sfx defaults: `Sequencer_InitSongTracks` / `TriggerSfxCue` set
/// volume 0x40/0x40 and expression 200; the program is left as it was.
const VOICE: Voice = Voice { prog: None, mult: 0, vl: 0x40, vr: 0x40, expr: 200 };

struct Stream {
    start: usize,
    /// Tick offset of the stream's first op (spawned voices start late).
    at: u32,
    spawned: bool,
    voice: Voice,
}

fn cue_view(tsq_bytes: &[u8], c: &tsq::Cue, pitch: &[u16], lengths: &[Option<(u32, bool)>]) -> CueView {
    let mut queue: VecDeque<Stream> = VecDeque::new();
    match c.kind {
        CueKind::Song => {
            for (_, start) in tsq::song_tracks(tsq_bytes, c.offset as usize) {
                queue.push_back(Stream { start, at: 0, spawned: false, voice: VOICE });
            }
        }
        CueKind::Sfx => queue.push_back(Stream { start: c.offset as usize, at: 0, spawned: false, voice: VOICE }),
    }
    let mut seen_spawns: BTreeSet<(usize, u32)> = BTreeSet::new();
    let (mut ticks, mut note_ons, mut note_offs, mut programs, mut spawns) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut loop_tick: Option<u32> = None;
    let mut tracks = Vec::new();
    let mut spawned_count = 0;
    while let Some(s) = queue.pop_front() {
        let w = tsq::walk(tsq_bytes, s.start, MAX_TICKS);
        let base = s.at;
        let Voice { mut prog, mut mult, mut vl, mut vr, mut expr } = s.voice;
        let mut cur: Option<(u32, i32, i32, u16, [u16; 2])> = None;
        let mut tv = TrackView {
            label: String::new(),
            start: s.start as u32,
            spawned: s.spawned,
            notes: Vec::new(),
            pitch: Vec::new(),
            vol: Vec::new(),
            exprs: Vec::new(),
            progs: Vec::new(),
            stop: "end",
            loop_tick: None,
        };
        let close = |tv: &mut TrackView, cur: &mut Option<(u32, i32, i32, u16, [u16; 2])>, at: u32| {
            if let Some((t0, note, slot, p, v)) = cur.take() {
                let cap = match usize::try_from(slot).ok().and_then(|i| lengths.get(i).copied().flatten()) {
                    Some((_, true)) => u32::MAX,
                    Some((len, false)) => sample_ticks(len, p),
                    None => UNKNOWN_NOTE_TICKS,
                };
                let end = at.min(t0.saturating_add(cap)).max(t0 + 1);
                tv.notes.extend([t0 as i32, end as i32, note, slot]);
                tv.pitch.push(p);
                tv.vol.extend(v);
            }
        };
        for e in &w.events {
            let t = base + e.tick;
            match e.op {
                Op::Note(_) | Op::NotePitch(_) => {
                    close(&mut tv, &mut cur, t);
                    let (note, mut p) = match e.op {
                        Op::Note(n) => (n as i32, pitch.get(n as usize).copied().unwrap_or(0)),
                        Op::NotePitch(p) => (note_for_pitch(p), p),
                        _ => unreachable!(),
                    };
                    if mult != 0 {
                        p = (p as u32 * mult as u32 / 4096) as u16;
                    }
                    let v = [(expr * vl).min(0x7FFF) as u16, (expr * vr).min(0x7FFF) as u16];
                    cur = Some((t, note, prog.map(|p| p as i32).unwrap_or(-1), p, v));
                    note_ons += 1;
                }
                Op::NoteOff => {
                    close(&mut tv, &mut cur, t);
                    note_offs += 1;
                }
                Op::Program(p) => {
                    prog = Some(p);
                    mult = 0;
                    programs += 1;
                    if e.tick > 2 {
                        tv.progs.push(t);
                    }
                }
                Op::PitchMult(m) => mult = m,
                Op::Expression(x) => {
                    expr = x as u32;
                    tv.exprs.extend([t, x as u32]);
                }
                Op::Volume(l, r) => {
                    vl = l as u32;
                    vr = r as u32;
                }
                Op::Spawn(rel) => {
                    spawns += 1;
                    let target = e.pos as isize + 3 + rel as isize;
                    if target >= 0
                        && (target as usize) < tsq_bytes.len()
                        && spawned_count < MAX_SPAWNED
                        && seen_spawns.insert((target as usize, t))
                    {
                        spawned_count += 1;
                        let voice = Voice { prog, mult, vl, vr, expr };
                        queue.push_back(Stream { start: target as usize, at: t, spawned: true, voice });
                    }
                }
                _ => {}
            }
        }
        let end_tick = base + w.ticks;
        close(&mut tv, &mut cur, end_tick);
        ticks = ticks.max(end_tick);
        tv.stop = match w.stop {
            Stop::End(_) => "end",
            Stop::Loop { tick, .. } => {
                let lt = base + tick;
                tv.loop_tick = Some(lt);
                loop_tick = Some(loop_tick.map_or(lt, |v: u32| v.min(lt)));
                "loop"
            }
            Stop::Stall => "stall",
            Stop::Truncated => "truncated",
        };
        let mut used: Vec<i32> = tv.notes.chunks(4).map(|n| n[3]).collect();
        used.sort_unstable();
        used.dedup();
        tv.label = if !used.is_empty() {
            let list: Vec<String> = used.iter().map(|s| if *s < 0 { "?".into() } else { s.to_string() }).collect();
            format!("{}s{}", if s.spawned { "↳" } else { "" }, list.join("·"))
        } else {
            match w.stop {
                Stop::End(b) => format!("{b:02X} end"),
                Stop::Loop { .. } => "loop".into(),
                Stop::Stall => "stall".into(),
                Stop::Truncated => "cut".into(),
            }
        };
        tracks.push(tv);
    }
    CueView {
        cue: c.index as u16,
        kind: if c.kind == CueKind::Song { "song" } else { "sfx" },
        flag: c.flag,
        offset: c.offset,
        ticks,
        r#loop: loop_tick.is_some(),
        loop_tick,
        active: tracks.iter().filter(|t| !t.notes.is_empty()).count() as u16,
        note_ons,
        note_offs,
        programs,
        spawns,
        tracks,
    }
}

/// One slot as `[u32 pcmLen][i32 loopStart (−1 = none)][u32 flags (bit 0
/// looped, bit 1 silent)][u32 adsr word]` then PCM i16 LE.
pub fn slot_pcm_bytes(tvb: &Tvb, slot: usize) -> Option<Vec<u8>> {
    let raw = tvb.slot_bytes(slot)?;
    let d = adpcm::decode(raw);
    let mut out = Vec::with_capacity(16 + d.pcm.len() * 2);
    out.extend_from_slice(&(d.pcm.len() as u32).to_le_bytes());
    let ls = if d.looped() { d.loop_start.map(|v| v as i32).unwrap_or(-1) } else { -1 };
    out.extend_from_slice(&ls.to_le_bytes());
    let flags = d.looped() as u32 | ((d.peak <= 1) as u32) << 1;
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&tvb.adsr[slot].to_le_bytes());
    for s in &d.pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    Some(out)
}

/// Mono 16-bit WAV of one slot at `rate` Hz (the loop is not unrolled).
pub fn slot_wav(tvb: &Tvb, slot: usize, rate: u32) -> Option<Vec<u8>> {
    let d = adpcm::decode(tvb.slot_bytes(slot)?);
    Some(wav_mono16(&d.pcm, rate))
}

pub fn wav_mono16(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data = (pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// Used slots of a TVB (convenience for callers that only need the count).
pub fn used_slots(tvb: &Tvb) -> Vec<usize> {
    (0..SLOTS).filter(|&i| tvb.used(i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_fallback_matches_the_exe_shape() {
        assert_eq!(default_pitch(48), 0x1000);
        assert_eq!(default_pitch(36), 0x800);
        assert_eq!(default_pitch(60), 0x2000);
        assert_eq!(note_for_pitch(0x1000), 48);
        assert_eq!(sample_ticks(44_100, 0x1000), 60);
    }

    #[test]
    fn wav_header() {
        let w = wav_mono16(&[0, 1, -1], 22_050);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
    }
}
