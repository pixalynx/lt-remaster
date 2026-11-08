//! Standard MIDI files to and from TSQ songs (screen 14 §12, research 06
//! phase 1 "TSQ/MIDI").
//!
//! The mapping is the one the screen's MIDI export uses (`14-audio/midi.ts`):
//! one MIDI tick per sequencer tick (60 per second), TSQ note n = MIDI n + 12,
//! program = sample slot, CC11 = expression (E0), velocity = the voice volume
//! (expression × E1, 0–0x3FFF scaled to 127). Imports read any tempo map and
//! division; loops come from `loopStart` / `loopEnd` markers (or CC111 for
//! the loop start).
//!
//! A TSQ track plays one note at a time, so imported notes are dealt out to
//! up to 24 tracks: a note goes to the track that last played its MIDI part
//! when that one is free, else to any free track; with all 24 busy it is
//! dropped and counted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::tsq::{self, Item, Op, Song};
use super::view::CueView;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MidiError(pub String);

fn bad(m: impl Into<String>) -> MidiError {
    MidiError(m.into())
}

/* ───────────────────────── reading ───────────────────────── */

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    NoteOn { ch: u8, note: u8, vel: u8 },
    NoteOff { ch: u8, note: u8 },
    Program { ch: u8, program: u8 },
    Control { ch: u8, cc: u8, value: u8 },
    /// Microseconds per quarter note.
    Tempo(u32),
    /// Marker (FF 06), cue point (FF 07) or text (FF 01).
    Marker(String),
    TrackName(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Absolute MIDI tick.
    pub tick: u32,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Smf {
    pub format: u16,
    /// Ticks per quarter note.
    pub division: u16,
    pub tracks: Vec<Vec<Event>>,
}

fn vlq(b: &[u8], at: &mut usize) -> Result<u32, MidiError> {
    let mut v = 0u32;
    for _ in 0..4 {
        let c = *b.get(*at).ok_or_else(|| bad("The MIDI file ends inside a number."))?;
        *at += 1;
        v = (v << 7) | (c & 0x7F) as u32;
        if c & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(bad("A MIDI variable-length number is too long."))
}

/// Parses a Standard MIDI File (format 0 or 1, ticks-per-quarter division).
pub fn read(b: &[u8]) -> Result<Smf, MidiError> {
    // RIFF-wrapped MIDI (.rmi): the SMF sits in the "data" chunk.
    let b = if b.len() > 20 && &b[..4] == b"RIFF" && &b[8..12] == b"RMID" { &b[20..] } else { b };
    if b.len() < 14 || &b[..4] != b"MThd" {
        return Err(bad("Not a Standard MIDI File (no MThd header)."));
    }
    let be16 = |at: usize| u16::from_be_bytes([b[at], b[at + 1]]);
    let be32 = |at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let hlen = be32(4) as usize;
    let format = be16(8);
    let ntracks = be16(10) as usize;
    let division = be16(12);
    if division & 0x8000 != 0 {
        return Err(bad("SMPTE-timed MIDI files aren't supported; save it with ticks per quarter note."));
    }
    if format > 1 {
        return Err(bad(format!("MIDI format {format} isn't supported (0 or 1).")));
    }
    let mut at = 8 + hlen;
    let mut tracks = Vec::new();
    while tracks.len() < ntracks && at + 8 <= b.len() {
        let len = be32(at + 4) as usize;
        let body = b.get(at + 8..at + 8 + len).ok_or_else(|| bad("A MIDI track runs past the end of the file."))?;
        if &b[at..at + 4] == b"MTrk" {
            tracks.push(read_track(body)?);
        }
        at += 8 + len;
    }
    if tracks.is_empty() {
        return Err(bad("The MIDI file has no tracks."));
    }
    Ok(Smf { format, division: division.max(1), tracks })
}

fn read_track(b: &[u8]) -> Result<Vec<Event>, MidiError> {
    let mut out = Vec::new();
    let (mut at, mut tick, mut status) = (0usize, 0u32, 0u8);
    let text = |d: &[u8]| d.iter().map(|&c| c as char).collect::<String>();
    while at < b.len() {
        tick = tick.saturating_add(vlq(b, &mut at)?);
        let mut s = *b.get(at).ok_or_else(|| bad("A MIDI track ends after a delta time."))?;
        if s & 0x80 != 0 {
            at += 1;
        } else {
            s = status; // running status
        }
        let byte = |at: &mut usize| -> Result<u8, MidiError> {
            let v = *b.get(*at).ok_or_else(|| bad("A MIDI event is cut off."))?;
            *at += 1;
            Ok(v)
        };
        match s {
            0xFF => {
                let ty = byte(&mut at)?;
                let len = vlq(b, &mut at)? as usize;
                let d = b.get(at..at + len).ok_or_else(|| bad("A MIDI meta event is cut off."))?;
                at += len;
                match ty {
                    0x2F => break,
                    0x51 if len == 3 => out.push(Event { tick, kind: Kind::Tempo(u32::from_be_bytes([0, d[0], d[1], d[2]])) }),
                    0x01 | 0x06 | 0x07 => out.push(Event { tick, kind: Kind::Marker(text(d)) }),
                    0x03 => out.push(Event { tick, kind: Kind::TrackName(text(d)) }),
                    _ => {}
                }
            }
            0xF0 | 0xF7 => {
                let len = vlq(b, &mut at)? as usize;
                at += len;
            }
            0x80..=0xEF => {
                status = s;
                let ch = s & 0x0F;
                let a = byte(&mut at)?;
                match s & 0xF0 {
                    0xC0 => out.push(Event { tick, kind: Kind::Program { ch, program: a } }),
                    0xD0 => {}
                    hi => {
                        let v = byte(&mut at)?;
                        out.push(Event {
                            tick,
                            kind: match hi {
                                0x90 if v > 0 => Kind::NoteOn { ch, note: a, vel: v },
                                0x90 | 0x80 => Kind::NoteOff { ch, note: a },
                                0xB0 => Kind::Control { ch, cc: a, value: v },
                                _ => continue, // aftertouch, pitch bend
                            },
                        });
                    }
                }
            }
            _ => return Err(bad(format!("Unknown MIDI status byte {s:#04x}."))),
        }
    }
    Ok(out)
}

/* ───────────────────────── writing ───────────────────────── */

fn put_vlq(out: &mut Vec<u8>, mut n: u32) {
    let mut buf = vec![(n & 0x7F) as u8];
    n >>= 7;
    while n > 0 {
        buf.push((n & 0x7F) as u8 | 0x80);
        n >>= 7;
    }
    buf.reverse();
    out.extend(buf);
}

fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.extend((body.len() as u32).to_be_bytes());
    v.extend(body);
    v
}

fn meta_text(ty: u8, s: &str) -> Vec<u8> {
    let d: Vec<u8> = s.bytes().map(|c| c & 0x7F).collect();
    let mut v = vec![0xFF, ty];
    put_vlq(&mut v, d.len() as u32);
    v.extend(d);
    v
}

/// The screen's MIDI export of a decoded cue (`cueToMidi` in `midi.ts`):
/// format 1, 60 ticks per quarter at 60 BPM, one track per TSQ track, plus
/// `loopStart` / `loopEnd` markers when the cue loops.
pub fn write_cue(cue: &CueView, title: &str) -> Vec<u8> {
    let mut tracks: Vec<Vec<u8>> = Vec::new();
    let mut t0 = vec![0];
    t0.extend(meta_text(0x03, title));
    t0.extend([0, 0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40]);
    if let Some(l) = cue.loop_tick.filter(|_| cue.r#loop) {
        put_vlq(&mut t0, l);
        t0.extend(meta_text(0x06, "loopStart"));
        put_vlq(&mut t0, cue.ticks.saturating_sub(l));
        t0.extend(meta_text(0x06, "loopEnd"));
    }
    t0.extend([0, 0xFF, 0x2F, 0]);
    tracks.push(t0);
    for (k, tr) in cue.tracks.iter().enumerate() {
        if tr.notes.is_empty() {
            continue;
        }
        let ch = (k % 16) as u8;
        let mut ev: Vec<(u32, u8, Vec<u8>)> = Vec::new();
        for e in tr.exprs.chunks(2) {
            ev.push((e[0], 1, vec![0xB0 | ch, 11, e[1].min(127) as u8]));
        }
        let mut prog = -2i32;
        for (i, n) in tr.notes.chunks(4).enumerate() {
            let (a, b, note, slot) = (n[0] as u32, n[1] as u32, n[2], n[3]);
            let midi_note = (note + 12).clamp(0, 127) as u8;
            if slot != prog && slot >= 0 {
                ev.push((a, 2, vec![0xC0 | ch, (slot & 0x7F) as u8]));
            }
            prog = slot;
            let l = tr.vol.get(i * 2).copied().unwrap_or(12800) as f64;
            let r = tr.vol.get(i * 2 + 1).copied().unwrap_or(12800) as f64;
            let vel = ((l.max(r) / 0x3FFF as f64) * 127.0).round().clamp(1.0, 127.0) as u8;
            ev.push((a, 3, vec![0x90 | ch, midi_note, vel]));
            ev.push((b, 0, vec![0x80 | ch, midi_note, 0]));
        }
        // Note-offs first at a tick, then everything else in the order it was written, so a
        // note's program change stays with its note-on even when two notes start on one tick.
        ev.sort_by_key(|e| (e.0, e.1 != 0));
        let mut body = vec![0];
        body.extend(meta_text(0x03, &format!("{k:02} {}", tr.label)));
        let mut last = 0;
        for (t, _, bytes) in ev {
            put_vlq(&mut body, t.saturating_sub(last));
            body.extend(bytes);
            last = last.max(t);
        }
        body.extend([0, 0xFF, 0x2F, 0]);
        tracks.push(body);
    }
    let mut out = chunk(b"MThd", &[0, 1, (tracks.len() >> 8) as u8, tracks.len() as u8, 0, 60]);
    for t in tracks {
        out.extend(chunk(b"MTrk", &t));
    }
    out
}

/* ───────────────────────── MIDI → song ───────────────────────── */

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// MIDI program → sample slot (programs not listed play their own number).
    #[serde(default)]
    pub programs: BTreeMap<u8, u8>,
    /// Semitones added to every note.
    #[serde(default)]
    pub transpose: i8,
    /// Ignore the file's loop markers (play once).
    #[serde(default)]
    pub no_loop: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramUse {
    pub program: u8,
    pub slot: u8,
    pub notes: u32,
    /// MIDI channels it was used on (10 = drums).
    pub channels: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SongReport {
    pub midi_tracks: u32,
    /// TSQ tracks the notes were dealt to (≤ 24).
    pub tracks: u32,
    pub notes: u32,
    /// Notes with all 24 tracks busy.
    pub dropped: u32,
    /// Notes moved by octaves into the playable 0–95 range.
    pub transposed: u32,
    /// Most notes sounding at once.
    pub polyphony: u32,
    pub programs: Vec<ProgramUse>,
    /// Sequencer ticks (60 per second).
    pub ticks: u32,
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
}

/// A note-on waiting for its note-off: (tick, velocity, program, expression, volume, pan).
type Sounding = (u32, u8, u8, u8, u8, u8);

#[derive(Debug, Clone, Copy)]
struct Note {
    start: u32,
    end: u32,
    note: u8,
    slot: u8,
    expr: u8,
    vol: (u8, u8),
    part: u32,
}

/// MIDI ticks → sequencer ticks (60 per second) through the tempo map.
struct Clock {
    division: f64,
    /// (MIDI tick, µs at that tick, µs per quarter from it).
    map: Vec<(u32, f64, f64)>,
}

impl Clock {
    fn new(smf: &Smf) -> Self {
        let mut tempos: Vec<(u32, u32)> = smf
            .tracks
            .iter()
            .flatten()
            .filter_map(|e| match e.kind {
                Kind::Tempo(t) if t > 0 => Some((e.tick, t)),
                _ => None,
            })
            .collect();
        tempos.sort_by_key(|t| t.0);
        let division = smf.division as f64;
        let mut map = vec![(0u32, 0f64, 500_000f64)];
        for (tick, us) in tempos {
            let &(t0, us0, q) = map.last().unwrap();
            let at = us0 + (tick - t0) as f64 * q / division;
            if tick == t0 {
                map.last_mut().unwrap().2 = us as f64;
            } else {
                map.push((tick, at, us as f64));
            }
        }
        Self { division, map }
    }

    fn ticks(&self, midi: u32) -> u32 {
        let i = self.map.partition_point(|m| m.0 <= midi) - 1;
        let (t0, us0, q) = self.map[i];
        let us = us0 + (midi - t0) as f64 * q / self.division;
        (us * tsq::TICKS_PER_SECOND as f64 / 1e6).round() as u32
    }
}

fn is_marker(s: &str, names: &[&str]) -> bool {
    let k: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    names.contains(&k.as_str())
}

fn emit_wait(code: &mut Vec<Item>, t: &mut u32, to: u32) {
    code.extend(tsq::waits(to.saturating_sub(*t)).into_iter().map(Item::Op));
    *t = (*t).max(to);
}

/// The notes of a MIDI file as a TSQ song (see the module notes), and what
/// happened to them.
pub fn to_song(smf: &Smf, opt: &ImportOptions) -> Result<(Song, SongReport), MidiError> {
    let clock = Clock::new(smf);
    let mut notes: Vec<Note> = Vec::new();
    let (mut loop_start, mut loop_end) = (None, None);
    let mut transposed = 0u32;
    let mut uses: BTreeMap<u8, ProgramUse> = BTreeMap::new();
    for (ti, track) in smf.tracks.iter().enumerate() {
        // Channel state for this track's events (format 1 keeps channels per track as well).
        let mut program = [0u8; 16];
        let mut expr = [127u8; 16];
        let mut volume = [127u8; 16];
        let mut pan = [64u8; 16];
        // Notes sounding, by (channel, note): the channel as it was at note-on.
        let mut open: BTreeMap<(u8, u8), Vec<Sounding>> = BTreeMap::new();
        // A track's events are in time order already; within a tick the file's order counts
        // (a program change belongs to the note-on after it).
        for e in track {
            match &e.kind {
                Kind::Marker(m) if is_marker(m, &["loopstart", "loop"]) => loop_start = Some(clock.ticks(e.tick)),
                Kind::Marker(m) if is_marker(m, &["loopend"]) => loop_end = Some(clock.ticks(e.tick)),
                Kind::Control { cc: 111, .. } => loop_start = Some(clock.ticks(e.tick)),
                Kind::Control { ch, cc, value } => match cc {
                    7 => volume[*ch as usize] = *value,
                    10 => pan[*ch as usize] = *value,
                    11 => expr[*ch as usize] = *value,
                    _ => {}
                },
                Kind::Program { ch, program: p } => program[*ch as usize] = *p,
                Kind::NoteOn { ch, note, vel } => {
                    let c = *ch as usize;
                    open.entry((*ch, *note)).or_default().push((e.tick, *vel, program[c], expr[c], volume[c], pan[c]));
                }
                Kind::NoteOff { ch, note } => {
                    let Some((t0, vel, p, ex, volume, pan)) = open.get_mut(&(*ch, *note)).and_then(|v| (!v.is_empty()).then(|| v.remove(0))) else { continue };
                    let mut n = *note as i32 - 12 + opt.transpose as i32;
                    if !(0..=0x5F).contains(&n) {
                        transposed += 1;
                        while n < 0 {
                            n += 12;
                        }
                        while n > 0x5F {
                            n -= 12;
                        }
                    }
                    let slot = opt.programs.get(&p).copied().unwrap_or(p).min(127);
                    let u = uses.entry(p).or_insert(ProgramUse { program: p, slot, notes: 0, channels: Vec::new() });
                    u.notes += 1;
                    if !u.channels.contains(&(*ch + 1)) {
                        u.channels.push(*ch + 1);
                    }
                    // Loudness: velocity × channel volume, compensated for the expression the voice carries.
                    let ex = ex.max(1);
                    let loud = vel as f64 * volume as f64 / 127.0;
                    let v = ((loud * 0x3FFF as f64) / (127.0 * ex as f64)).round().clamp(0.0, 127.0);
                    let (l, r) = match pan {
                        0..=63 => (v, v * pan as f64 / 64.0),
                        64 => (v, v),
                        _ => (v * (127 - pan) as f64 / 63.0, v),
                    };
                    let (start, end) = (clock.ticks(t0), clock.ticks(e.tick));
                    if end > start {
                        notes.push(Note { start, end, note: n as u8, slot, expr: ex, vol: (l.round() as u8, r.round() as u8), part: (ti as u32) << 4 | *ch as u32 });
                    }
                }
                _ => {}
            }
        }
    }
    if notes.is_empty() {
        return Err(bad("The MIDI file has no notes."));
    }
    notes.sort_by_key(|n| (n.start, n.part, n.note));
    let song_end = notes.iter().map(|n| n.end).max().unwrap_or(0);
    let looping = !opt.no_loop && loop_start.is_some_and(|l| l < loop_end.unwrap_or(song_end));
    let (ls, le) = if looping { (loop_start, Some(loop_end.unwrap_or(song_end))) } else { (None, None) };
    if let Some(le) = le {
        notes.retain(|n| n.start < le);
        for n in notes.iter_mut() {
            n.end = n.end.min(le);
        }
    }
    // Polyphony (for the report).
    let mut edges: Vec<(u32, i32)> = notes.iter().flat_map(|n| [(n.start, 1), (n.end, -1)]).collect();
    edges.sort_by_key(|e| (e.0, e.1));
    let polyphony = edges.iter().scan(0i32, |s, e| {
        *s += e.1;
        Some(*s)
    }).max().unwrap_or(0) as u32;
    // Deal the notes out to tracks.
    let mut lanes: Vec<(u32, u32, Vec<Note>)> = Vec::new(); // (free from, last part, notes)
    let mut dropped = 0u32;
    for n in &notes {
        // A free track of the same part, else a new track, else the free track whose voice
        // setup (sample, expression, volume) is closest, so as few set-up bytes as possible go out.
        let same = lanes.iter().position(|l| l.0 <= n.start && l.1 == n.part);
        let setup = |l: &(u32, u32, Vec<Note>)| {
            l.2.last().map_or(3, |m| (m.slot != n.slot) as u32 + (m.expr != n.expr) as u32 + (m.vol.0.abs_diff(n.vol.0) > 1 || m.vol.1.abs_diff(n.vol.1) > 1) as u32)
        };
        let i = match same {
            Some(i) => i,
            None if lanes.len() < tsq::TRACKS => {
                lanes.push((0, n.part, Vec::new()));
                lanes.len() - 1
            }
            None => match lanes.iter().enumerate().filter(|(_, l)| l.0 <= n.start).min_by_key(|(_, l)| (setup(l), l.0)).map(|(i, _)| i) {
                Some(i) => i,
                None => {
                    dropped += 1;
                    continue;
                }
            },
        };
        lanes[i].0 = n.end;
        lanes[i].1 = n.part;
        lanes[i].2.push(*n);
    }
    // Each lane → one stream.
    let mut song = Song::default();
    let mut label = 0usize;
    for lane in &lanes {
        let start = label;
        let loop_label = label + 1;
        label += 2;
        song.tracks.push((0x80, start));
        song.code.push(Item::Label(start));
        let (mut t, mut slot, mut expr, mut vol) = (0u32, None, None, None);
        let mut playing = false;
        let mut looped = false;
        for (k, n) in lane.2.iter().enumerate() {
            if let Some(l) = ls.filter(|&l| !looped && n.start >= l) {
                // The loop starts between notes: mark it, and set the voice up again after the jump back.
                emit_wait(&mut song.code, &mut t, l);
                song.code.push(Item::Label(loop_label));
                looped = true;
                (slot, expr, vol) = (None, None, None);
            }
            emit_wait(&mut song.code, &mut t, n.start);
            if slot != Some(n.slot) {
                song.code.push(Item::Op(Op::Program(n.slot)));
                slot = Some(n.slot);
            }
            if expr != Some(n.expr) {
                song.code.push(Item::Op(Op::Expression(n.expr)));
                expr = Some(n.expr);
            }
            // Velocity is rounded to 0–127 on its way through MIDI: a volume within a step of the
            // current one is the same volume (and saves the 3-byte E1).
            let near = |a: u8, b: u8| a.abs_diff(b) <= 1;
            if !vol.is_some_and(|(l, r): (u8, u8)| near(l, n.vol.0) && near(r, n.vol.1)) {
                song.code.push(Item::Op(Op::Volume(n.vol.0, n.vol.1)));
                vol = Some(n.vol);
            }
            song.code.push(Item::Op(Op::Note(n.note)));
            playing = true;
            let next_start = lane.2.get(k + 1).map(|m| m.start);
            if next_start != Some(n.end) {
                emit_wait(&mut song.code, &mut t, n.end);
                song.code.push(Item::Op(Op::NoteOff));
                playing = false;
            }
        }
        match (ls, le) {
            (Some(l), Some(e)) => {
                if !looped {
                    emit_wait(&mut song.code, &mut t, l);
                    song.code.push(Item::Label(loop_label));
                }
                emit_wait(&mut song.code, &mut t, e);
                if playing {
                    song.code.push(Item::Op(Op::NoteOff));
                }
                song.code.push(Item::JumpTo(loop_label));
            }
            _ => {
                if playing {
                    song.code.push(Item::Op(Op::NoteOff));
                }
                song.code.push(Item::Op(Op::End(0xFF)));
            }
        }
    }
    let report = SongReport {
        midi_tracks: smf.tracks.len() as u32,
        tracks: lanes.len() as u32,
        notes: notes.len() as u32 - dropped,
        dropped,
        transposed,
        polyphony,
        programs: uses.into_values().collect(),
        ticks: le.unwrap_or(song_end),
        loop_start: ls,
        loop_end: le,
    };
    Ok((song, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smf(events: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut last = 0;
        for (t, e) in events {
            put_vlq(&mut body, t - last);
            body.extend(e);
            last = *t;
        }
        body.extend([0, 0xFF, 0x2F, 0]);
        let mut out = chunk(b"MThd", &[0, 0, 0, 1, 0, 60]);
        out.extend(chunk(b"MTrk", &body));
        out
    }

    #[test]
    fn reads_running_status_and_tempo() {
        // 60 BPM (1 s per quarter, 60 ticks per quarter → 1 MIDI tick = 1 sequencer tick).
        let f = smf(&[(0, vec![0xFF, 0x51, 3, 0x0F, 0x42, 0x40]), (0, vec![0xC0, 5]), (0, vec![0x90, 60, 100]), (30, vec![62, 90]), (60, vec![60, 0]), (90, vec![0x80, 62, 0])]);
        let s = read(&f).unwrap();
        assert_eq!(s.tracks[0].len(), 6);
        assert_eq!(s.tracks[0][3], Event { tick: 30, kind: Kind::NoteOn { ch: 0, note: 62, vel: 90 } });
        assert_eq!(s.tracks[0][4], Event { tick: 60, kind: Kind::NoteOff { ch: 0, note: 60 } });
        let (song, rep) = to_song(&s, &ImportOptions::default()).unwrap();
        assert_eq!((rep.tracks, rep.notes, rep.polyphony, rep.ticks), (2, 2, 2, 90));
        assert_eq!(rep.programs[0].slot, 5);
        // Note 60 → TSQ 48, on track 0 from tick 0 to 60.
        let bytes = tsq::assemble(&song, 0).unwrap();
        let w = tsq::walk(&bytes, (bytes[2] as usize) | (bytes[3] as usize) << 8, 10_000);
        assert!(w.events.iter().any(|e| e.op == Op::Note(48) && e.tick == 0));
        assert!(w.events.iter().any(|e| e.op == Op::NoteOff && e.tick == 60));
        assert_eq!(w.stop, tsq::Stop::End(0xFF));
    }

    #[test]
    fn loops_and_double_tempo() {
        // 120 BPM: 60 MIDI ticks = half a second = 30 sequencer ticks.
        let f = smf(&[
            (0, vec![0xFF, 0x51, 3, 0x07, 0xA1, 0x20]),
            (0, vec![0x90, 60, 100]),
            (60, vec![0x80, 60, 0]),
            (60, vec![0xFF, 0x06, 9, b'l', b'o', b'o', b'p', b'S', b't', b'a', b'r', b't']),
            (60, vec![0x90, 64, 100]),
            (120, vec![0x80, 64, 0]),
            (120, vec![0xFF, 0x06, 7, b'l', b'o', b'o', b'p', b'E', b'n', b'd']),
        ]);
        let (song, rep) = to_song(&read(&f).unwrap(), &ImportOptions::default()).unwrap();
        assert_eq!((rep.loop_start, rep.loop_end, rep.ticks), (Some(30), Some(60), 60));
        let bytes = tsq::assemble(&song, 0).unwrap();
        let w = tsq::walk(&bytes, (bytes[2] as usize) | (bytes[3] as usize) << 8, 10_000);
        assert!(matches!(w.stop, tsq::Stop::Loop { tick: 30, .. }), "{:?}", w.stop);
        assert_eq!(w.ticks, 60);
    }
}
