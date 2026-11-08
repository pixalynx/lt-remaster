//! Trigger records added to an NPC pack: doors into and out of new areas,
//! and any other Tier-1 record (screen 10, PRD 10 §5.6, Strategy A of §5.5).
//!
//! `ScanFieldEventTriggers` scans both tables of the loaded pack for the
//! Knight's chunk: table 1 (right after the actor records) and table 2 (at
//! `D + pass1`); in each, the chunk's group is a ladder whose first rung with
//! a set flag is the only one scanned. Table 1 can't move away from the actor
//! records, so added records go to table 2, which is emitted again in the
//! relocation area ([`super::text::layout`]) with `pass1` (s16) pointing at it:
//!
//! - every retail group in order, with the added records of its chunk at the
//!   end of every rung (so they hold in every story state), plus a closing
//!   always-rung when the ladder has none (so they also hold when no rung
//!   does);
//! - then one group per other chunk, with one always-rung;
//! - a record with a `cue` flag holds only while that flag is set: it goes in
//!   a rung of its own at the top of its chunk's ladder (one per cue), which
//!   also carries the ladder's always-rung records and the chunk's uncued
//!   added records, so doors keep working while the cue is set. This is how
//!   retail arms a scene (a rung on a flag the scene then clears); while the
//!   cue is set, the ladder's other story rungs are hidden, as they are under
//!   any retail scene rung.
//!
//! Each retail record appears once in the emitted table, so [`locate`] finds
//! it there (table 1 moves too, 16 bytes per actor [`super::actor`] adds);
//! [`super::trigger`] writes positions wherever a record is now. The project
//! keeps only bytes, so [`recover`] reads the added records back from the
//! emitted table.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::npc::NpcPack;
use crate::trigger::{self, TriggerGroup, TriggerRecord};

use super::{EditError, Result};

const END: i16 = 0x7FFF;

/// A record added to the group of `chunk`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Added {
    pub chunk: i16,
    /// The record; `at` is where its first copy is (D-relative), 0 when new.
    pub record: TriggerRecord,
    /// The flag it waits for (its own rung at the top of the ladder); 0 = it
    /// holds in every story state.
    pub cue: u16,
}

impl Added {
    /// A record that holds in every story state.
    pub fn always(chunk: i16, record: TriggerRecord) -> Self {
        Self { chunk, record, cue: 0 }
    }
}

/// The cues of `chunk`'s added records, in first-appearance order.
fn cues_of(added: &[Added], chunk: i16) -> Vec<u16> {
    let mut v: Vec<u16> = Vec::new();
    for a in added.iter().filter(|a| a.chunk == chunk && a.cue != 0) {
        if !v.contains(&a.cue) {
            v.push(a.cue);
        }
    }
    v
}

/// How many cue rungs [`emit`] put ahead of `retail`'s rungs in `emitted`
/// (after them may come a closing always-rung).
fn split(retail: &TriggerGroup, emitted: &TriggerGroup) -> Option<usize> {
    let (n, m) = (retail.rungs.len(), emitted.rungs.len());
    let flags: Vec<u16> = retail.rungs.iter().map(|r| r.flag).collect();
    let retail_always = retail.rungs.last().is_some_and(|r| r.flag == 0);
    for closing in [false, true] {
        if closing && retail_always {
            continue;
        }
        let Some(k) = m.checked_sub(n + usize::from(closing)) else { continue };
        let same = emitted.rungs[k..k + n].iter().map(|r| r.flag).eq(flags.iter().copied());
        let cues_ok = emitted.rungs[..k].iter().all(|r| r.flag != 0);
        let close_ok = !closing || emitted.rungs[m - 1].flag == 0;
        let grown = emitted.rungs[k..k + n].iter().zip(&retail.rungs).all(|(e, r)| e.records.len() >= r.records.len());
        if same && cues_ok && close_ok && grown {
            return Some(k);
        }
    }
    None
}

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

fn table2(p: &NpcPack, b: &[u8], what: &str) -> Result<Vec<TriggerGroup>> {
    if p.pass1 <= 0 {
        return Ok(Vec::new());
    }
    trigger::parse_table(p.d_block(b), p.pass1 as usize, 1).ok_or_else(|| EditError::new("TRIGGER_NO_TABLE", format!("The {what} pack's trigger table 2 doesn't parse.")))
}

/// The words of a record as the game reads them.
pub fn words(r: &TriggerRecord) -> Vec<i16> {
    match *r {
        TriggerRecord::Door { x, z, facing, to, tx, tz, flags, pack, music, .. } => {
            vec![x, z, facing, to as i16, tx, tz, flags as i16, (pack as u16 | (music as u16) << 8) as i16]
        }
        TriggerRecord::Set { x, z, flag, .. } => vec![x, z, 8, flag as i16],
        TriggerRecord::Clear { x, z, flag, .. } => vec![x, z, 9, flag as i16],
        TriggerRecord::Talk { x, z, actor, .. } => vec![x, z, 10, actor as i16],
        TriggerRecord::Other { x, z, typ, arg, .. } => vec![x, z, typ, arg],
    }
}

/// Checks a record the game can read back: no word is 0x7FFF (the scanner
/// skips word by word to it), a door's facing is 0–7, the box lies inside the
/// chunk (±0x800).
pub fn check(r: &TriggerRecord) -> Result<()> {
    let w = words(r);
    if w.contains(&END) {
        return Err(EditError::new("DOOR_END_WORD", "A word of the record is 0x7FFF, which ends the record list in game. Move it one unit."));
    }
    let (x, z) = (w[0], w[1]);
    if !(-0x800..0x800).contains(&x) || !(-0x800..0x800).contains(&z) {
        return Err(EditError::new("DOOR_OUTSIDE", format!("({x}, {z}) is outside the chunk (−2048…2047).")));
    }
    match *r {
        TriggerRecord::Door { facing, tx, tz, .. } => {
            if !(0..8).contains(&facing) {
                return Err(EditError::new("DOOR_FACING", format!("Facing {facing} isn't one of the 8 directions (0–7).")));
            }
            if !(-0x800..0x800).contains(&tx) || !(-0x800..0x800).contains(&tz) {
                return Err(EditError::new("DOOR_ARRIVAL_OUTSIDE", format!("The arrival ({tx}, {tz}) is outside the destination chunk.")));
            }
        }
        TriggerRecord::Other { typ, .. } if typ < 8 => {
            return Err(EditError::new("DOOR_TYPE", format!("Type {typ} reads as a door.")));
        }
        _ => {}
    }
    Ok(())
}

fn push(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn record_len(r: &TriggerRecord) -> usize {
    if matches!(r, TriggerRecord::Door { .. }) {
        16
    } else {
        8
    }
}

/// Each retail record's bytes as the project has them (moved positions), by retail offset.
pub type Positions = BTreeMap<u32, Vec<u8>>;

/// Where each retail trigger record is in `current` (D-relative), by its
/// retail offset. Table 1 follows the actor records, so it moves 16 bytes per
/// added actor; table 2 is retail's in place, or the emitted one.
pub fn locate(retail: &[u8], current: &[u8]) -> Result<BTreeMap<u32, u32>> {
    let base = super::character::base(retail, current)?;
    let current: &[u8] = &base;
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let changed = || EditError::new("DOOR_TABLE_CHANGED", "This pack's trigger tables aren't ones the editor wrote. Revert the pack.");
    let shift = 16 * c.actors.len().checked_sub(r.actors.len()).ok_or_else(changed)? as u32;
    let mut out = BTreeMap::new();
    let rgroups = r.triggers(retail);
    for rec in rgroups.iter().filter(|g| g.pass == 0).flat_map(|g| &g.rungs).flat_map(|r| &r.records) {
        let (at, _, _) = super::trigger::record_pos(rec);
        out.insert(at, at + shift);
    }
    let rg = table2(&r, retail, "retail")?;
    if c.pass1 == r.pass1 {
        if shift != 0 {
            return Err(changed());
        }
        for rec in rg.iter().flat_map(|g| &g.rungs).flat_map(|r| &r.records) {
            let (at, _, _) = super::trigger::record_pos(rec);
            out.insert(at, at);
        }
        return Ok(out);
    }
    let cg = table2(&c, current, "project's")?;
    for (i, g) in rg.iter().enumerate() {
        let cgi = cg.get(i).filter(|x| x.chunk == g.chunk && x.rungs.len() >= g.rungs.len()).ok_or_else(changed)?;
        let k = split(g, cgi).ok_or_else(changed)?;
        for (j, rung) in g.rungs.iter().enumerate() {
            let crung = &cgi.rungs[k + j];
            if crung.records.len() < rung.records.len() {
                return Err(changed());
            }
            for (k, rec) in rung.records.iter().enumerate() {
                let (at, _, _) = super::trigger::record_pos(rec);
                out.insert(at, super::trigger::record_pos(&crung.records[k]).0);
            }
        }
    }
    Ok(out)
}

/// The retail records' bytes as `current` has them ([`locate`]).
pub fn positions(retail: &[u8], current: &[u8]) -> Result<Positions> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let loc = locate(retail, current)?;
    let lens: BTreeMap<u32, usize> = r.triggers(retail).iter().flat_map(|g| &g.rungs).flat_map(|r| &r.records).map(|rec| (super::trigger::record_pos(rec).0, record_len(rec))).collect();
    let cd = c.d_block(current);
    loc.iter()
        .map(|(&at, &now)| {
            let n = lens[&at];
            let bytes = cd.get(now as usize..now as usize + n).ok_or_else(|| EditError::new("TRIGGER_NOT_A_RECORD", format!("D+0x{now:04X} is past the pack.")))?;
            Ok((at, bytes.to_vec()))
        })
        .collect()
}

/// Table 2 of `retail` with `added` merged in (module docs); retail records
/// take their bytes from `positions`.
pub fn emit(retail: &[u8], positions: &Positions, added: &[Added]) -> Result<Vec<u8>> {
    let r = header(retail, "retail")?;
    let groups = table2(&r, retail, "retail")?;
    for a in added {
        check(&a.record)?;
    }
    let mut out = Vec::new();
    let new_chunks: Vec<i16> = {
        let mut v: Vec<i16> = Vec::new();
        for a in added {
            if !groups.iter().any(|g| g.chunk == a.chunk) && !v.contains(&a.chunk) {
                v.push(a.chunk);
            }
        }
        v
    };
    push(&mut out, (groups.len() + new_chunks.len()) as i16);
    let mine = |chunk: i16| added.iter().filter(move |a| a.chunk == chunk && a.cue == 0).map(|a| &a.record);
    let cued = |chunk: i16, cue: u16| added.iter().filter(move |a| a.chunk == chunk && a.cue == cue).map(|a| &a.record);
    let retail_bytes = |rec: &TriggerRecord| -> Result<Vec<u8>> {
        let (at, _, _) = super::trigger::record_pos(rec);
        positions.get(&at).cloned().ok_or_else(|| EditError::new("TRIGGER_NOT_A_RECORD", format!("No position for D+0x{at:04X}.")))
    };
    for g in &groups {
        let extra: Vec<&TriggerRecord> = mine(g.chunk).collect();
        let cues = cues_of(added, g.chunk);
        let always = g.rungs.last().filter(|r| r.flag == 0);
        let closing = !extra.is_empty() && always.is_none();
        push(&mut out, g.chunk);
        push(&mut out, (cues.len() + g.rungs.len() + usize::from(closing)) as i16);
        for &cue in &cues {
            push(&mut out, cue as i16);
            for rec in cued(g.chunk, cue) {
                words(rec).into_iter().for_each(|w| push(&mut out, w));
            }
            for rec in always.map(|r| &r.records[..]).unwrap_or(&[]) {
                out.extend_from_slice(&retail_bytes(rec)?);
            }
            for rec in &extra {
                words(rec).into_iter().for_each(|w| push(&mut out, w));
            }
            push(&mut out, END);
        }
        for rung in &g.rungs {
            push(&mut out, rung.flag as i16);
            for rec in &rung.records {
                let (at, _, _) = super::trigger::record_pos(rec);
                out.extend_from_slice(positions.get(&at).ok_or_else(|| EditError::new("TRIGGER_NOT_A_RECORD", format!("No position for D+0x{at:04X}.")))?);
            }
            for rec in &extra {
                words(rec).into_iter().for_each(|w| push(&mut out, w));
            }
            push(&mut out, END);
        }
        if closing {
            push(&mut out, 0);
            for rec in &extra {
                words(rec).into_iter().for_each(|w| push(&mut out, w));
            }
            push(&mut out, END);
        }
    }
    for &chunk in &new_chunks {
        let extra: Vec<&TriggerRecord> = mine(chunk).collect();
        let cues = cues_of(added, chunk);
        push(&mut out, chunk);
        push(&mut out, (cues.len() + usize::from(!extra.is_empty())) as i16);
        for &cue in &cues {
            push(&mut out, cue as i16);
            for rec in cued(chunk, cue).chain(extra.iter().copied()) {
                words(rec).into_iter().for_each(|w| push(&mut out, w));
            }
            push(&mut out, END);
        }
        if !extra.is_empty() {
            push(&mut out, 0);
            for rec in &extra {
                words(rec).into_iter().for_each(|w| push(&mut out, w));
            }
            push(&mut out, END);
        }
    }
    Ok(out)
}

/// `added` in the order [`recover`] reads them back from what [`emit`]
/// writes: by group (retail groups, then new chunks), cued records rung by
/// rung, then the uncued ones.
pub fn canonical(retail: &[u8], added: &[Added]) -> Result<Vec<Added>> {
    let r = header(retail, "retail")?;
    let mut chunks: Vec<i16> = table2(&r, retail, "retail")?.iter().map(|g| g.chunk).collect();
    for a in added {
        if !chunks.contains(&a.chunk) {
            chunks.push(a.chunk);
        }
    }
    let mut out = Vec::new();
    for chunk in chunks {
        for cue in cues_of(added, chunk) {
            out.extend(added.iter().filter(|a| a.chunk == chunk && a.cue == cue).cloned());
        }
        out.extend(added.iter().filter(|a| a.chunk == chunk && a.cue == 0).cloned());
    }
    Ok(out)
}

/// The records `current` adds to retail's table 2, read back from the table
/// [`emit`] wrote (empty when table 2 is the retail one).
pub fn recover(retail: &[u8], current: &[u8]) -> Result<Vec<Added>> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    if c.pass1 == r.pass1 {
        return Ok(Vec::new());
    }
    let changed = || EditError::new("DOOR_TABLE_CHANGED", "This pack's trigger table 2 isn't one the editor wrote. Revert the pack.");
    let rg = table2(&r, retail, "retail")?;
    let cg = table2(&c, current, "project's")?;
    if cg.len() < rg.len() {
        return Err(changed());
    }
    Ok(walk(&rg, &cg).ok_or_else(changed)?.into_iter().map(|(_, a)| a).collect())
}

/// The pack serving a chunk the pack table leaves out (`world::pack_for` is
/// `None`): a new area, or a retail chunk with no entry for the flag state.
/// `Field_ChunkStreamTask` then keeps the pack the Knight came in with, the
/// one the door into the chunk named, so it is the first of `packs` (id, its
/// added records) with a record on `chunk`.
pub fn kept_pack(packs: impl IntoIterator<Item = (u32, Vec<Added>)>, chunk: u32) -> Option<u32> {
    packs.into_iter().find_map(|(n, added)| added.iter().any(|a| a.chunk as i32 == chunk as i32).then_some(n))
}

/// Every added record of the emitted table `cg` with the offset of its first
/// copy, in [`canonical`] order.
fn walk(rg: &[TriggerGroup], cg: &[TriggerGroup]) -> Option<Vec<(u32, Added)>> {
    if cg.len() < rg.len() {
        return None;
    }
    let mut out = Vec::new();
    let at = |rec: &TriggerRecord| super::trigger::record_pos(rec).0;
    for (i, g) in cg.iter().enumerate() {
        g.rungs.first()?;
        let (cues, uncued, copies): (usize, &[TriggerRecord], usize) = match rg.get(i) {
            Some(rgi) => {
                if rgi.chunk != g.chunk {
                    return None;
                }
                let cues = split(rgi, g)?;
                let first = &g.rungs[cues];
                let skip = rgi.rungs.first().map_or(0, |r| r.records.len());
                let copies = rgi.rungs.last().filter(|r| r.flag == 0).map_or(0, |r| r.records.len());
                (cues, first.records.get(skip..)?, copies)
            }
            None => {
                let last = g.rungs.last()?;
                let always = last.flag == 0;
                let cues = g.rungs.len() - usize::from(always);
                (cues, if always { &last.records[..] } else { &[] }, 0)
            }
        };
        for rung in &g.rungs[..cues] {
            let n = rung.records.len().checked_sub(copies + uncued.len())?;
            for rec in &rung.records[..n] {
                out.push((at(rec), Added { chunk: g.chunk, record: rec.clone(), cue: rung.flag }));
            }
        }
        for rec in uncued {
            out.push((at(rec), Added { chunk: g.chunk, record: rec.clone(), cue: 0 }));
        }
    }
    Some(out)
}

/// Both tables of `current` as the editor shows them: retail records keep
/// their retail offset (their stable id for [`super::trigger`]) with the
/// position the project has; an added record keeps the offset of its copy
/// and comes with its index in [`recover`]'s list.
pub fn view(retail: &[u8], current: &[u8]) -> Result<(Vec<TriggerGroup>, Vec<(u32, usize)>)> {
    let base = super::character::base(retail, current)?;
    let current: &[u8] = &base;
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let mut groups = c.triggers(current);
    let back: BTreeMap<u32, u32> = locate(retail, current)?.into_iter().map(|(at, now)| (now, at)).collect();
    let rg = table2(&r, retail, "retail")?;
    let firsts: Vec<(u32, Added)> = if c.pass1 == r.pass1 { Vec::new() } else { walk(&rg, &table2(&c, current, "project's")?).unwrap_or_default() };
    let marks: Vec<(u32, usize)> = firsts.iter().enumerate().map(|(k, (at, _))| (*at, k)).collect();
    let ours: BTreeMap<u32, usize> = marks.iter().copied().collect();
    for g in groups.iter_mut() {
        for rung in g.rungs.iter_mut() {
            let mut kept = Vec::new();
            for rec in rung.records.drain(..) {
                let (now, x, z) = super::trigger::record_pos(&rec);
                match back.get(&now) {
                    Some(&at) => kept.push(with_at(&rec, at, x, z)),
                    // An added record's first copy, or its copies in other rungs.
                    None if ours.contains_key(&now) || g.pass == 1 && firsts.iter().any(|(_, a)| a.chunk == g.chunk && words(&a.record) == words(&rec)) => kept.push(rec),
                    // A cue rung's copy of an always-rung retail record.
                    None => {}
                }
            }
            rung.records = kept;
        }
    }
    Ok((groups, marks))
}

fn with_at(r: &TriggerRecord, na: u32, nx: i16, nz: i16) -> TriggerRecord {
    let mut r = r.clone();
    match &mut r {
        TriggerRecord::Door { at, x, z, .. } | TriggerRecord::Set { at, x, z, .. } | TriggerRecord::Clear { at, x, z, .. } | TriggerRecord::Talk { at, x, z, .. } | TriggerRecord::Other { at, x, z, .. } => {
            *at = na;
            *x = nx;
            *z = nz;
        }
    }
    r
}

/// `current` with exactly `added` in table 2; its text edits, edited
/// programs and added actors are laid out again with them ([`super::text::layout`]).
pub fn apply(retail: &[u8], current: &[u8], added: &[Added]) -> Result<(Vec<u8>, super::text::TextReport)> {
    let mut set = super::pack::Edits::recover(retail, current)?;
    set.doors = canonical(retail, added)?;
    let (b, report, _) = super::text::layout(retail, current, &set)?;
    Ok((b, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn door(x: i16, z: i16, to: u16) -> TriggerRecord {
        TriggerRecord::Door { at: 0, x, z, facing: 4, to, tx: 0, tz: 64, flags: 0x8000, pack: 6, music: 0 }
    }

    /// The pack of `edit::trigger`'s tests: two actors, table 1 empty, table 2
    /// with one group (chunk 78) holding a talk record and a door.
    fn pack() -> Vec<u8> {
        let mut b = vec![0u8; 0x20];
        b[0..4].copy_from_slice(&0x10u32.to_le_bytes());
        b[4..8].copy_from_slice(&0x20u32.to_le_bytes());
        let mut d: Vec<u8> = Vec::new();
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend(std::iter::repeat_n(0u8, 32));
        let pass1_at = d.len();
        push(&mut d, 0);
        push(&mut d, 0);
        let pass1 = d.len() as i16;
        d[pass1_at..pass1_at + 2].copy_from_slice(&pass1.to_le_bytes());
        for v in [1, 78, 1, 0, -1170, -800, 10, 4, 100, 200, 2, 79, 10, 20, 0, 0x0102, END] {
            push(&mut d, v);
        }
        b.extend_from_slice(&d);
        b.resize(0x800, 0);
        b
    }

    fn groups(b: &[u8]) -> Vec<TriggerGroup> {
        let p = NpcPack::parse(b).unwrap();
        p.triggers(b)
    }

    #[test]
    fn added_records_join_every_rung_and_new_groups() {
        let retail = pack();
        let added = vec![Added::always(78, door(-64, 96, 1330)), Added::always(1330, door(0, -640, 78))];
        let (out, report) = apply(&retail, &retail, &added).unwrap();
        assert!(report.trigger_bytes > 0);
        let g = groups(&out);
        // Table 1 is empty; table 2: group 78 (talk, door, added door) and group 1330.
        let t2: Vec<_> = g.iter().filter(|g| g.pass == 1).collect();
        assert_eq!(t2.iter().map(|g| g.chunk).collect::<Vec<_>>(), vec![78, 1330]);
        assert_eq!(t2[0].rungs.len(), 1, "an always-rung needs no closing rung");
        assert_eq!(t2[0].rungs[0].records.len(), 3);
        assert_eq!(words(&t2[0].rungs[0].records[2]), words(&door(-64, 96, 1330)));
        assert_eq!(t2[1].rungs[0].records.len(), 1);
        let back = recover(&retail, &out).unwrap();
        assert_eq!(back.iter().map(|a| (a.chunk, words(&a.record))).collect::<Vec<_>>(), added.iter().map(|a| (a.chunk, words(&a.record))).collect::<Vec<_>>());
        // The old table 2 is cleared; pass1 points into the relocation area.
        let r = NpcPack::parse(&retail).unwrap();
        let o = NpcPack::parse(&out).unwrap();
        let at = 0x20 + r.pass1 as usize;
        assert!(out[at..at + 40].iter().all(|&x| x == 0));
        assert!(o.pass1 > r.pass1);
        // Removing them gives retail back.
        let (clean, _) = apply(&retail, &out, &[]).unwrap();
        assert_eq!(clean, retail);
    }

    #[test]
    fn a_ladder_without_an_always_rung_gets_one() {
        let mut retail = pack();
        // Make the rung conditional (flag 30).
        let r = NpcPack::parse(&retail).unwrap();
        let rung = 0x20 + r.pass1 as usize + 6;
        retail[rung..rung + 2].copy_from_slice(&30i16.to_le_bytes());
        let (out, _) = apply(&retail, &retail, &[Added::always(78, door(-64, 96, 1330))]).unwrap();
        let t2: Vec<_> = groups(&out).into_iter().filter(|g| g.pass == 1).collect();
        assert_eq!(t2[0].rungs.iter().map(|r| (r.flag, r.records.len())).collect::<Vec<_>>(), vec![(30, 3), (0, 1)]);
    }

    #[test]
    fn moved_retail_records_follow_into_the_emitted_table() {
        let retail = pack();
        let (out, _) = apply(&retail, &retail, &[Added::always(1330, door(0, -640, 78))]).unwrap();
        let talk = groups(&retail).into_iter().find(|g| g.pass == 1).unwrap().rungs[0].records[0].clone();
        let at = super::super::trigger::record_pos(&talk).0;
        let moved = super::super::trigger::apply(&retail, &out, &[super::super::trigger::TriggerWrite::to(at, -1000, -700)]).unwrap();
        let t2: Vec<_> = groups(&moved).into_iter().filter(|g| g.pass == 1).collect();
        let (_, x, z) = super::super::trigger::record_pos(&t2[0].rungs[0].records[0]);
        assert_eq!((x, z), (-1000, -700));
        assert_eq!(super::super::trigger::moved(&retail, &moved).len(), 1);
        assert_eq!(recover(&retail, &moved).unwrap().len(), 1);
        // A layout keeps the move.
        let (again, _) = apply(&retail, &moved, &recover(&retail, &moved).unwrap()).unwrap();
        assert_eq!(again, moved);
    }

    #[test]
    fn the_view_keeps_retail_ids_and_marks_added_records() {
        let retail = pack();
        let (out, _) = apply(&retail, &retail, &[Added::always(78, door(-64, 96, 1330))]).unwrap();
        let (groups, marks) = view(&retail, &out).unwrap();
        let rung = &groups.iter().find(|g| g.pass == 1).unwrap().rungs[0];
        let retail_rung = &groups_of(&retail)[0].rungs[0];
        assert_eq!(rung.records[..2], retail_rung.records[..]);
        let (at, _, _) = super::super::trigger::record_pos(&rung.records[2]);
        assert_eq!(marks, vec![(at, 0)]);
    }

    fn groups_of(b: &[u8]) -> Vec<TriggerGroup> {
        groups(b).into_iter().filter(|g| g.pass == 1).collect()
    }

    #[test]
    fn cued_records_get_a_rung_of_their_own_at_the_top() {
        let retail = pack();
        let talk = |x: i16, z: i16, actor: u16| TriggerRecord::Talk { at: 0, x, z, actor };
        let added = vec![
            Added { chunk: 78, record: talk(300, 400, 5), cue: 0x1A3 },
            Added::always(1330, door(0, -640, 78)),
            Added { chunk: 1330, record: talk(10, 20, 6), cue: 0x1A4 },
        ];
        let (out, _) = apply(&retail, &retail, &added).unwrap();
        let t2 = groups_of(&out);
        // Group 78: the cue rung (its talk, then copies of the always-rung's talk and door), then retail's rung.
        assert_eq!(t2[0].rungs.iter().map(|r| (r.flag, r.records.len())).collect::<Vec<_>>(), vec![(0x1A3, 3), (0, 2)]);
        assert_eq!(words(&t2[0].rungs[0].records[0]), words(&talk(300, 400, 5)));
        assert_eq!(words(&t2[0].rungs[0].records[1]), words(&t2[0].rungs[1].records[0]));
        // Group 1330: the cue rung carries the uncued door too, then an always-rung with the door.
        assert_eq!(t2[1].rungs.iter().map(|r| (r.flag, r.records.len())).collect::<Vec<_>>(), vec![(0x1A4, 2), (0, 1)]);
        // Read back in canonical order, cues kept.
        let back = recover(&retail, &out).unwrap();
        assert_eq!(back.iter().map(|a| (a.chunk, a.cue, words(&a.record))).collect::<Vec<_>>(), canonical(&retail, &added).unwrap().iter().map(|a| (a.chunk, a.cue, words(&a.record))).collect::<Vec<_>>());
        assert_eq!(back[0].cue, 0x1A3);
        // The view drops the cue rung's copies of retail records and marks the added ones.
        let (groups, marks) = view(&retail, &out).unwrap();
        let g78 = groups.iter().find(|g| g.pass == 1 && g.chunk == 78).unwrap();
        assert_eq!(g78.rungs[0].records.len(), 1);
        assert_eq!(marks.len(), 3);
        // A moved retail record moves in the cue rung's copy too (both come from its position).
        let talk78 = groups_of(&retail)[0].rungs[0].records[0].clone();
        let at = super::super::trigger::record_pos(&talk78).0;
        let moved = super::super::trigger::apply(&retail, &out, &[super::super::trigger::TriggerWrite::to(at, -1000, -700)]).unwrap();
        let (again, _) = apply(&retail, &moved, &recover(&retail, &moved).unwrap()).unwrap();
        let t2 = groups_of(&again);
        assert_eq!(super::super::trigger::record_pos(&t2[0].rungs[1].records[0]).1, -1000);
        assert_eq!(super::super::trigger::record_pos(&t2[0].rungs[0].records[1]).1, -1000);
        // Removing them all gives retail back.
        let (clean, _) = apply(&retail, &out, &[]).unwrap();
        assert_eq!(clean, retail);
    }

    #[test]
    fn checks_the_words_the_scanner_reads() {
        assert!(check(&door(10, 20, 1330)).is_ok());
        assert_eq!(check(&door(0x7FFF, 20, 1330)).unwrap_err().code, "DOOR_END_WORD");
        assert_eq!(check(&door(-3000, 20, 1330)).unwrap_err().code, "DOOR_OUTSIDE");
        let TriggerRecord::Door { at, x, z, to, tx, tz, flags, pack, music, .. } = door(1, 2, 3) else { unreachable!() };
        assert_eq!(check(&TriggerRecord::Door { at, x, z, facing: 8, to, tx, tz, flags, pack, music }).unwrap_err().code, "DOOR_FACING");
        let w = words(&door(1, 2, 1330));
        assert_eq!(w, vec![1, 2, 4, 1330, 0, 64, 0x8000u16 as i16, 6]);
    }
}
