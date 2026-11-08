//! TSQ sequence banks: a 52-entry cue table, 24-track song tables and a
//! MIDI-like opcode stream. `docs/screens/14-audio.md` §4.2–4.3.
//!
//! Interpreter: `Sound_SeqTrackInterpreter` (0x8019A3B4,
//! `src/main/Sound_SeqTrackInterpreter.cdk-normalized.c`), ticked from the
//! VSync callback at 60 Hz. Songs start in `Sequencer_InitSongTracks`
//! (`main_801897ac.c:4551`), sound effects in `Sequencer_TriggerSfxCue` (:4490).

use crate::container::{le16, le_i16};

/// Cue table: 52 × `{u16 flag, u16 ofs}`.
pub const CUES: usize = 52;
pub const HEADER: usize = CUES * 4;
/// Unused cues point at the shared null stream `F0 FF FF 00` at 0xCC.
pub const NULL_STREAM: u16 = 0xCC;
/// Tracks per song table, `{u8 ch, u8 pad, u16 rel}` each.
pub const TRACKS: usize = 24;
/// Sequencer ticks per second (NTSC VSync).
pub const TICKS_PER_SECOND: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueKind {
    /// `flag == 0`: a 24-track table.
    Song,
    /// `flag != 0` (retail 0x0090, the voice-steal priority): one stream.
    Sfx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cue {
    pub index: usize,
    pub flag: u16,
    pub offset: u16,
    pub kind: CueKind,
}

/// The addressable cues 1–50 that are in use. Cue 0 is a dummy and cue 51
/// overlaps the null stream, so neither is offered.
pub fn cues(tsq: &[u8]) -> Vec<Cue> {
    if tsq.len() < HEADER {
        return Vec::new();
    }
    (1..CUES - 1)
        .filter_map(|index| {
            let flag = le16(tsq, index * 4);
            let offset = le16(tsq, index * 4 + 2);
            if offset == NULL_STREAM || offset as usize >= tsq.len() {
                return None;
            }
            let kind = if flag == 0 { CueKind::Song } else { CueKind::Sfx };
            Some(Cue { index, flag, offset, kind })
        })
        .collect()
}

/// Start positions of a song's 24 tracks (relative to the table, `ch` byte).
pub fn song_tracks(tsq: &[u8], table: usize) -> Vec<(u8, usize)> {
    (0..TRACKS)
        .filter_map(|k| {
            let at = table + k * 4;
            let rel = *tsq.get(at + 3)? as usize * 256 + tsq[at + 2] as usize;
            Some((tsq[at], table + rel))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// 0x00–0x7F: wait n ticks (ends this tick's burst). 0 waits 65,536 ticks.
    Delta(u8),
    /// 0x80–0xDF: note-on, note = byte & 0x7F, pitch from the table at 0x801B7518.
    Note(u8),
    /// E0: expression.
    Expression(u8),
    /// E1: volume left, right.
    Volume(u8, u8),
    /// E2: program = sample slot in this bank.
    Program(u8),
    /// E3: two bytes skipped.
    Skip(u16),
    /// E4: explicit pitch for the playing note (detune).
    Detune(u16),
    /// E5: note-on with an explicit 16-bit SPU pitch.
    NotePitch(u16),
    /// E6: reverb mode (and clear the work area).
    ReverbMode(u8),
    /// E7: reverb depth.
    ReverbDepth(u8),
    /// E8 / E9: reverb on / off for this voice.
    ReverbOn,
    ReverbOff,
    /// EA: pitch multiplier (/4096) for later notes; E2 resets it.
    PitchMult(u16),
    /// F0: key-off.
    NoteOff,
    /// F1: voice-steal priority.
    Priority(u8),
    /// F8: relative jump, `cursor (after the operand) += s16`.
    Jump(i16),
    /// F9: copy this track into a free voice and start it at cursor + s16.
    Spawn(i16),
    /// Any other byte ends the track.
    End(u8),
}

impl Op {
    /// Operand bytes after the opcode.
    pub fn operands(op: u8) -> usize {
        match op {
            0xE0 | 0xE2 | 0xE6 | 0xE7 | 0xF1 => 1,
            0xE1 | 0xE3 | 0xE4 | 0xE5 | 0xEA | 0xF8 | 0xF9 => 2,
            _ => 0,
        }
    }
}

/// Decodes the op at `pos`; `None` past the end of the data.
pub fn op_at(tsq: &[u8], pos: usize) -> Option<(Op, usize)> {
    let b = *tsq.get(pos)?;
    let n = Op::operands(b);
    if pos + 1 + n > tsq.len() {
        return None;
    }
    let u8a = || tsq[pos + 1];
    let u16a = || le16(tsq, pos + 1);
    let i16a = || le_i16(tsq, pos + 1);
    let op = match b {
        0x00..=0x7F => Op::Delta(b),
        0x80..=0xDF => Op::Note(b & 0x7F),
        0xE0 => Op::Expression(u8a()),
        0xE1 => Op::Volume(tsq[pos + 1], tsq[pos + 2]),
        0xE2 => Op::Program(u8a()),
        0xE3 => Op::Skip(u16a()),
        0xE4 => Op::Detune(u16a()),
        0xE5 => Op::NotePitch(u16a()),
        0xE6 => Op::ReverbMode(u8a()),
        0xE7 => Op::ReverbDepth(u8a()),
        0xE8 => Op::ReverbOn,
        0xE9 => Op::ReverbOff,
        0xEA => Op::PitchMult(u16a()),
        0xF0 => Op::NoteOff,
        0xF1 => Op::Priority(u8a()),
        0xF8 => Op::Jump(i16a()),
        0xF9 => Op::Spawn(i16a()),
        other => Op::End(other),
    };
    Some((op, pos + 1 + n))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub tick: u32,
    pub pos: usize,
    pub op: Op,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// An end opcode (the byte).
    End(u8),
    /// A backward (or already visited) F8 jump: the track loops to `pos`,
    /// first reached at `tick`.
    Loop { pos: usize, tick: u32 },
    /// Delta 0 (a 65,536-tick wait) or the tick limit.
    Stall,
    /// Ran past the end of the data.
    Truncated,
}

#[derive(Debug, Clone)]
pub struct Walk {
    pub start: usize,
    pub events: Vec<Event>,
    /// Tick of the last event (where the stream ends or loops).
    pub ticks: u32,
    pub stop: Stop,
}

/// Follows one stream linearly. Forward F8 jumps are followed; a jump to a
/// position already played ends the walk as a loop. Delta bytes advance the
/// tick; every other op is recorded with the tick it runs on.
pub fn walk(tsq: &[u8], start: usize, max_ticks: u32) -> Walk {
    let mut events = Vec::new();
    let mut tick = 0u32;
    let mut pos = start;
    // Tick at which each byte position was first reached (for loop targets).
    let mut seen: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
    let stop = loop {
        if tick > max_ticks {
            break Stop::Stall;
        }
        seen.entry(pos).or_insert(tick);
        let Some((op, next)) = op_at(tsq, pos) else { break Stop::Truncated };
        match op {
            Op::Delta(0) => break Stop::Stall,
            Op::Delta(n) => {
                tick += n as u32;
                pos = next;
                continue;
            }
            Op::End(b) => {
                events.push(Event { tick, pos, op });
                break Stop::End(b);
            }
            Op::Jump(rel) => {
                events.push(Event { tick, pos, op });
                let target = next as isize + rel as isize;
                if target < 0 || target as usize >= tsq.len() {
                    break Stop::Truncated;
                }
                let target = target as usize;
                if let Some(&t) = seen.get(&target) {
                    break Stop::Loop { pos: target, tick: t };
                }
                if target < pos {
                    // Backwards into bytes this walk skipped over: treat as a loop.
                    break Stop::Loop { pos: target, tick };
                }
                pos = target;
            }
            _ => {
                events.push(Event { tick, pos, op });
                pos = next;
            }
        }
    };
    Walk { start, events, ticks: tick, stop }
}

/* ───────────────────────── writing songs ───────────────────────── */

/// Bytes of one op (the inverse of [`op_at`]).
pub fn encode(op: Op) -> Vec<u8> {
    let w = |b: u8, v: u16| vec![b, v as u8, (v >> 8) as u8];
    match op {
        Op::Delta(n) => vec![n & 0x7F],
        Op::Note(n) => vec![0x80 | (n & 0x7F)],
        Op::Expression(x) => vec![0xE0, x],
        Op::Volume(l, r) => vec![0xE1, l, r],
        Op::Program(p) => vec![0xE2, p],
        Op::Skip(v) => w(0xE3, v),
        Op::Detune(v) => w(0xE4, v),
        Op::NotePitch(v) => w(0xE5, v),
        Op::ReverbMode(v) => vec![0xE6, v],
        Op::ReverbDepth(v) => vec![0xE7, v],
        Op::ReverbOn => vec![0xE8],
        Op::ReverbOff => vec![0xE9],
        Op::PitchMult(v) => w(0xEA, v),
        Op::NoteOff => vec![0xF0],
        Op::Priority(v) => vec![0xF1, v],
        Op::Jump(v) => w(0xF8, v as u16),
        Op::Spawn(v) => w(0xF9, v as u16),
        Op::End(b) => vec![b],
    }
}

/// Delta ops that wait `ticks` (each at most 127; none for 0).
pub fn waits(mut ticks: u32) -> Vec<Op> {
    let mut out = Vec::new();
    while ticks > 0 {
        let n = ticks.min(0x7F);
        out.push(Op::Delta(n as u8));
        ticks -= n;
    }
    out
}

/// One item of a song's code: an op, a label, or a jump/spawn to a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Op(Op),
    Label(usize),
    /// F8 to the label (loops).
    JumpTo(usize),
    /// F9: start a copy of this track at the label.
    SpawnAt(usize),
}

/// A song cue as code: 24 track starts (priority byte, label) and the code
/// they run, laid out in order. Tracks past `tracks.len()` point at a lone
/// end byte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Song {
    pub tracks: Vec<(u8, usize)>,
    pub code: Vec<Item>,
}

/// The song's table and code at `base` in a TSQ: 24 × `{prio, 0, u16 rel}`
/// then the code; `rel` is relative to the table. `None` when a label is
/// missing, a jump is out of range or there are more than 24 tracks.
pub fn assemble(song: &Song, base: usize) -> Option<Vec<u8>> {
    if song.tracks.len() > TRACKS {
        return None;
    }
    let size = |it: &Item| match it {
        Item::Op(op) => encode(*op).len(),
        Item::Label(_) => 0,
        Item::JumpTo(_) | Item::SpawnAt(_) => 3,
    };
    // Pass 1: label positions (relative to the table).
    let mut at = TRACKS * 4;
    let mut labels = std::collections::HashMap::new();
    for it in &song.code {
        if let Item::Label(l) = it {
            labels.insert(*l, at);
        }
        at += size(it);
    }
    let end_stub = at;
    let mut out = vec![0u8; TRACKS * 4];
    for k in 0..TRACKS {
        let (prio, rel) = match song.tracks.get(k) {
            Some(&(p, l)) => (p, *labels.get(&l)?),
            None => (0, end_stub),
        };
        let rel = u16::try_from(rel).ok()?;
        out[k * 4] = prio;
        out[k * 4 + 2..k * 4 + 4].copy_from_slice(&rel.to_le_bytes());
    }
    for it in &song.code {
        let pos = out.len();
        match it {
            Item::Op(op) => out.extend(encode(*op)),
            Item::Label(_) => {}
            Item::JumpTo(l) | Item::SpawnAt(l) => {
                let rel = *labels.get(l)? as isize - (pos as isize + 3);
                let rel = i16::try_from(rel).ok()?;
                out.extend(encode(if matches!(it, Item::JumpTo(_)) { Op::Jump(rel) } else { Op::Spawn(rel) }));
            }
        }
    }
    out.push(0xFF);
    let _ = base;
    Some(out)
}

/// Where a stream reference sits, so a re-laid TSQ can be fixed up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Site {
    /// Cue table entry `i` (its u16 offset).
    Cue(usize),
    /// Track `k` of the table at `table` (u16 rel to the table).
    Track { table: usize, k: usize },
    /// An F8/F9 at `op` (s16 rel to the op's end).
    Rel { op: usize },
}

/// Every stream position reachable from cue `i` (op starts) and the
/// references inside those streams, plus the cue's song table if it has one.
struct Reach {
    ops: std::collections::BTreeMap<usize, usize>,
    sites: Vec<(Site, usize)>,
    table: Option<usize>,
}

/// Cues the game can start: 1–50 (cue 0 is a dummy and entry 51's bytes
/// are the null stream).
const PLAYABLE: std::ops::Range<usize> = 1..CUES - 1;

fn reach(tsq: &[u8], i: usize) -> Reach {
    let mut r = Reach { ops: Default::default(), sites: Vec::new(), table: None };
    if !PLAYABLE.contains(&i) {
        return r;
    }
    let flag = le16(tsq, i * 4);
    let ofs = le16(tsq, i * 4 + 2) as usize;
    if ofs >= tsq.len() {
        return r;
    }
    r.sites.push((Site::Cue(i), ofs));
    // The shared null stream (or anything else in the header) stays where it is.
    if ofs < HEADER {
        return r;
    }
    let mut todo = Vec::new();
    if flag == 0 {
        r.table = Some(ofs);
        for k in 0..TRACKS {
            let at = ofs + k * 4;
            if at + 4 > tsq.len() {
                break;
            }
            let target = ofs + le16(tsq, at + 2) as usize;
            r.sites.push((Site::Track { table: ofs, k }, target));
            todo.push(target);
        }
    } else {
        todo.push(ofs);
    }
    let mut seen = std::collections::HashSet::new();
    while let Some(mut p) = todo.pop() {
        if !seen.insert(p) {
            continue;
        }
        loop {
            if r.ops.contains_key(&p) || p < HEADER {
                break;
            }
            let Some((op, next)) = op_at(tsq, p) else { break };
            r.ops.insert(p, next - p);
            match op {
                Op::End(_) => break,
                Op::Jump(rel) | Op::Spawn(rel) => {
                    let t = next as isize + rel as isize;
                    if t >= 0 && (t as usize) < tsq.len() {
                        r.sites.push((Site::Rel { op: p }, t as usize));
                        todo.push(t as usize);
                    }
                    if matches!(op, Op::Jump(_)) {
                        break;
                    }
                    p = next;
                }
                _ => p = next,
            }
        }
    }
    r
}

/// What [`replace_song`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Replaced {
    /// Bytes of the old song freed (only it reached them).
    pub freed: usize,
    /// Bytes the new song takes (table, code, end stub, alignment).
    pub added: usize,
    /// Where the new song's table starts.
    pub table: usize,
}

/// `tsq` with cue `cue` playing `song`: the bytes only the old cue reached
/// (its table, its streams and the dead bytes after them) are taken out,
/// every other cue's references are re-pointed, and the new song goes at
/// the end (4-aligned). `None` if the cue is out of range or the result
/// can't be addressed with 16-bit offsets.
pub fn replace_song(tsq: &[u8], cue: usize, song: &Song) -> Option<(Vec<u8>, Replaced)> {
    let (mut out, freed) = take_out(tsq, cue)?;
    let before = out.len();
    while out.len() % 4 != 0 {
        out.push(0);
    }
    let table = out.len();
    out.extend(assemble(song, table)?);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out[cue * 4..cue * 4 + 2].copy_from_slice(&0u16.to_le_bytes());
    out[cue * 4 + 2..cue * 4 + 4].copy_from_slice(&u16::try_from(table).ok()?.to_le_bytes());
    Some((out.clone(), Replaced { freed, added: out.len() - before, table }))
}

/// `tsq` without cue `cue`: the bytes only it reached taken out (as [`replace_song`] does), the
/// other cues re-pointed, and its entry the unused one (`0x0090`, the null stream). Streamed
/// music takes a song out this way (`crate::audio::stream`). `None` as for [`replace_song`].
pub fn remove_cue(tsq: &[u8], cue: usize) -> Option<Vec<u8>> {
    let (mut out, _) = take_out(tsq, cue)?;
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out[cue * 4..cue * 4 + 2].copy_from_slice(&0x0090u16.to_le_bytes());
    out[cue * 4 + 2..cue * 4 + 4].copy_from_slice(&NULL_STREAM.to_le_bytes());
    Some(out)
}

/// The highest cue the game can start (1–50) that holds nothing: a new cue goes there, as far as
/// possible from the low numbers code asks for by literal.
pub fn free_cue(tsq: &[u8]) -> Option<usize> {
    (PLAYABLE).rev().find(|&i| tsq.len() >= HEADER && le16(tsq, i * 4 + 2) == NULL_STREAM)
}

/// `tsq` with `stream` (one opcode stream, relative jumps inside it) appended 4-aligned as cue
/// `cue`, a sound effect of priority `flag` (retail 0x0090). `None` if the cue is out of range or
/// the result can't be addressed with 16-bit offsets.
pub fn add_cue(tsq: &[u8], cue: usize, flag: u16, stream: &[u8]) -> Option<Vec<u8>> {
    if !PLAYABLE.contains(&cue) || tsq.len() < HEADER || flag == 0 {
        return None;
    }
    // Past the last byte a cue reaches is padding (SE.TSQ is a whole sector): the new cue goes there.
    let mut out = tsq[..used_len(tsq).next_multiple_of(4).min(tsq.len())].to_vec();
    while out.len() % 4 != 0 {
        out.push(0);
    }
    let at = u16::try_from(out.len()).ok()?;
    out.extend_from_slice(stream);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    u16::try_from(out.len()).ok()?;
    out[cue * 4..cue * 4 + 2].copy_from_slice(&flag.to_le_bytes());
    out[cue * 4 + 2..cue * 4 + 4].copy_from_slice(&at.to_le_bytes());
    Some(out)
}

/// The end of the last byte any cue reaches (its ops, its song table), at least the header.
pub fn used_len(tsq: &[u8]) -> usize {
    (0..CUES)
        .map(|i| {
            let r = reach(tsq, i);
            let ops = r.ops.iter().map(|(&p, &l)| p + l).max().unwrap_or(0);
            ops.max(r.table.map_or(0, |t| t + TRACKS * 4)).max(r.sites.iter().map(|&(_, t)| t + 1).max().unwrap_or(0))
        })
        .max()
        .unwrap_or(0)
        .max(HEADER)
        .min(tsq.len())
}

/// The bytes of sound-effect cue `cue`'s stream when it is one straight run (as [`add_cue`]
/// writes them): from its start to its end byte, or to a jump back inside it. `None` for a song,
/// an empty cue, or a stream that jumps elsewhere or spawns.
pub fn sfx_stream(tsq: &[u8], cue: usize) -> Option<(u16, Vec<u8>)> {
    if !PLAYABLE.contains(&cue) || tsq.len() < HEADER {
        return None;
    }
    let (flag, at) = (le16(tsq, cue * 4), le16(tsq, cue * 4 + 2) as usize);
    if flag == 0 || at == NULL_STREAM as usize || at < HEADER {
        return None;
    }
    let mut p = at;
    loop {
        let (op, next) = op_at(tsq, p)?;
        match op {
            Op::End(_) => return Some((flag, tsq[at..next].to_vec())),
            Op::Jump(rel) => {
                let t = next as isize + rel as isize;
                return (t >= at as isize && (t as usize) < next).then(|| (flag, tsq[at..next].to_vec()));
            }
            Op::Spawn(_) => return None,
            _ => p = next,
        }
    }
}

/// The sample slots cue `cue` programs (its `E2` operands, every stream it reaches).
pub fn programs(tsq: &[u8], cue: usize) -> std::collections::BTreeSet<u8> {
    reach(tsq, cue)
        .ops
        .keys()
        .filter_map(|&p| match op_at(tsq, p) {
            Some((Op::Program(b), _)) => Some(b),
            _ => None,
        })
        .collect()
}

/// `tsq` with cue `cue`'s bytes taken out and every other cue re-pointed (the cue's own entry
/// left for the caller), and how many bytes went.
fn take_out(tsq: &[u8], cue: usize) -> Option<(Vec<u8>, usize)> {
    if !(1..CUES - 1).contains(&cue) || tsq.len() < HEADER {
        return None;
    }
    let all: Vec<Reach> = (0..CUES).map(|i| reach(tsq, i)).collect();
    // Bytes some other cue still uses (its ops and its song table).
    let mut kept = vec![false; tsq.len()];
    kept[..HEADER].iter_mut().for_each(|b| *b = true);
    for (i, r) in all.iter().enumerate().filter(|(i, _)| *i != cue) {
        let _ = i;
        for (&p, &l) in &r.ops {
            kept[p..(p + l).min(tsq.len())].iter_mut().for_each(|b| *b = true);
        }
        if let Some(t) = r.table {
            kept[t..(t + TRACKS * 4).min(tsq.len())].iter_mut().for_each(|b| *b = true);
        }
    }
    // The old cue's bytes, and the dead bytes that follow them up to the next kept byte.
    let mut gone = vec![false; tsq.len()];
    let mine = &all[cue];
    let mut spans: Vec<(usize, usize)> = mine.ops.iter().map(|(&p, &l)| (p, p + l)).collect();
    if let Some(t) = mine.table {
        spans.push((t, t + TRACKS * 4));
    }
    for (a, b) in spans {
        if a < HEADER {
            continue;
        }
        let mut e = b.min(tsq.len());
        while e < tsq.len() && !kept[e] && !mine.ops.contains_key(&e) && mine.table != Some(e) {
            e += 1;
        }
        for (k, g) in gone.iter_mut().enumerate().take(e).skip(a) {
            if !kept[k] {
                *g = true;
            }
        }
    }
    // The file's trailing padding stays at the end (the new song goes before nothing; we re-pad).
    let mut map = vec![0usize; tsq.len() + 1];
    let mut out = Vec::with_capacity(tsq.len());
    for (k, &b) in tsq.iter().enumerate() {
        map[k] = out.len();
        if !gone[k] {
            out.push(b);
        }
    }
    map[tsq.len()] = out.len();
    let freed = tsq.len() - out.len();
    // Re-point every other cue's references.
    for (i, r) in all.iter().enumerate().filter(|(i, _)| *i != cue) {
        let _ = i;
        for &(site, target) in &r.sites {
            let t = if gone[target] { NULL_STREAM as usize } else { map[target] };
            match site {
                Site::Cue(c) => {
                    let v = u16::try_from(t).ok()?;
                    out[c * 4 + 2..c * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                Site::Track { table, k } => {
                    let tb = map[table];
                    let v = u16::try_from(t.checked_sub(tb)?).ok()?;
                    out[tb + k * 4 + 2..tb + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                Site::Rel { op } => {
                    let o = map[op];
                    let v = i16::try_from(t as isize - (o as isize + 3)).ok()?;
                    out[o + 1..o + 3].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    Some((out, freed))
}

/// Cue `cue`'s song as code (its table's tracks and every stream they reach,
/// in byte order), for re-laying it unchanged. `None` for an SFX cue.
pub fn song_of(tsq: &[u8], cue: usize) -> Option<Song> {
    let r = reach(tsq, cue);
    let table = r.table?;
    // Ops decoded from two entry points that overlap can't be laid out one after the other.
    if r.ops.iter().zip(r.ops.iter().skip(1)).any(|((&a, &la), (&b, _))| a + la > b) {
        return None;
    }
    let mut labels: std::collections::BTreeMap<usize, usize> = Default::default();
    let label = |p: usize, labels: &mut std::collections::BTreeMap<usize, usize>| {
        let n = labels.len();
        *labels.entry(p).or_insert(n)
    };
    let mut tracks = Vec::new();
    for k in 0..TRACKS {
        let at = table + k * 4;
        let target = table + le16(tsq, at + 2) as usize;
        let l = label(target, &mut labels);
        tracks.push((tsq[at], l));
    }
    for &(site, target) in &r.sites {
        if matches!(site, Site::Rel { .. }) {
            label(target, &mut labels);
        }
    }
    let mut code = Vec::new();
    for &p in r.ops.keys() {
        if let Some(&l) = labels.get(&p) {
            code.push(Item::Label(l));
        }
        let (op, next) = op_at(tsq, p)?;
        code.push(match op {
            Op::Jump(rel) => Item::JumpTo(labels[&((next as isize + rel as isize) as usize)]),
            Op::Spawn(rel) => Item::SpawnAt(labels[&((next as isize + rel as isize) as usize)]),
            _ => Item::Op(op),
        });
    }
    // Targets that aren't op starts (a track aimed at padding, say) become a lone end byte.
    for (&p, &l) in &labels {
        if !r.ops.contains_key(&p) {
            code.push(Item::Label(l));
            code.push(Item::Op(Op::End(*tsq.get(p).unwrap_or(&0xFF))));
        }
    }
    Some(Song { tracks, code })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_notes_loops_and_ends() {
        // E2 05, note 0x30, wait 10, F0, wait 5, F8 → back to the note.
        let s = [0xE2, 0x05, 0xB0, 0x0A, 0xF0, 0x05, 0xF8, 0xF9, 0xFF];
        let w = walk(&s, 0, 10_000);
        assert_eq!(w.events[0].op, Op::Program(5));
        assert_eq!(w.events[1].op, Op::Note(0x30));
        assert_eq!(w.events[2], Event { tick: 10, pos: 4, op: Op::NoteOff });
        assert_eq!(w.stop, Stop::Loop { pos: 2, tick: 0 });
        assert_eq!(w.ticks, 15);

        let e = [0x80, 0x02, 0xFF];
        let w = walk(&e, 0, 10_000);
        assert_eq!(w.stop, Stop::End(0xFF));
        assert_eq!(w.ticks, 2);
    }

    #[test]
    fn cue_table() {
        let mut t = vec![0u8; HEADER + 8];
        for i in 0..CUES {
            t[i * 4..i * 4 + 4].copy_from_slice(&[0x90, 0, 0xCC, 0]);
        }
        t[4..8].copy_from_slice(&[0, 0, 0xD0, 0]);
        t[8..12].copy_from_slice(&[0x90, 0, 0xD4, 0]);
        let c = cues(&t);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].index, c[0].kind), (1, CueKind::Song));
        assert_eq!((c[1].index, c[1].kind), (2, CueKind::Sfx));
    }

    #[test]
    fn adding_a_sound_cue() {
        // Every cue on the null stream but cue 2, then sector padding.
        let mut t = vec![0u8; 0x800];
        for i in 0..CUES {
            t[i * 4..i * 4 + 4].copy_from_slice(&[0x90, 0, 0xCC, 0]);
        }
        t[0xCC..0xD0].copy_from_slice(&[0xF0, 0xFF, 0xFF, 0x00]);
        t[8..12].copy_from_slice(&[0x90, 0, 0xD0, 0]);
        t[0xD0..0xD6].copy_from_slice(&[0xE2, 0x01, 0xB0, 0x05, 0xF0, 0xFF]);
        assert_eq!(used_len(&t), 0xD6);
        assert_eq!(free_cue(&t), Some(50));
        // Plays once; then a loop that waits forever.
        let once = [0xF0, 0xE9, 0xE0, 0x80, 0xE2, 0x07, 0xE5, 0x00, 0x08, 0x10, 0xF0, 0xFF];
        let out = add_cue(&t, 50, 0x90, &once).unwrap();
        assert_eq!(out.len(), 0xD8 + 12, "the padding goes, the cue goes after the last byte used");
        assert_eq!(sfx_stream(&out, 50), Some((0x90, once.to_vec())));
        assert_eq!(sfx_stream(&out, 2), sfx_stream(&t, 2));
        assert_eq!(free_cue(&out), Some(49));
        let looping = [0xF0, 0xE9, 0xE0, 0x80, 0xE2, 0x07, 0xE5, 0x00, 0x08, 0x7F, 0xF8, 0xFC, 0xFF];
        let l = add_cue(&out, 49, 0x90, &looping).unwrap();
        assert_eq!(sfx_stream(&l, 49), Some((0x90, looping.to_vec())));
        assert!(add_cue(&t, 51, 0x90, &once).is_none() && add_cue(&t, 50, 0, &once).is_none());
        // Taken out, it's the null stream again.
        let gone = remove_cue(&out, 50).unwrap();
        assert!(!cues(&gone).iter().any(|c| c.index == 50));
        assert_eq!(sfx_stream(&gone, 2), sfx_stream(&t, 2));
    }
}
