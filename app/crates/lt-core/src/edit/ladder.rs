//! Placement ladders the project edits (screen 10, LT-76): flag branches
//! added to a retail actor, removed, reordered, or given another flag, spot
//! or hidden body ([`crate::placement`] has the program format).
//!
//! Moving a branch rewrites its words in place ([`super::placement`]), but a
//! ladder that gains a branch grows, and retail packs its programs back to
//! back (Tier-2 `WALK` operands point into the same run of words). So an
//! edited ladder is a copy in the relocation area, laid out with the rest of
//! the pack's edits ([`super::text::layout`]), and only its actor's record
//! `c` points at it; actors that shared the retail program keep it.
//!
//! ```text
//! -8  "LP" slot n 0000      the mark, never read by the game
//! +0  A000|cond words…      n words: the rungs, each a condition word and
//!                           what follows it up to the next one
//! ```
//!
//! A rung copied from the retail ladder keeps every word after its
//! condition: the body and the walk, a chest's opened spot, an arrival's
//! camera words. A new placing rung walks nowhere (`x h z dir 0000`, like an
//! added actor); a new hidden rung is `7FFF`, the body retail hides actors
//! with (`WalkActor_SelectScript` clears the visible bit). A chest (record
//! type 3) reads the words after `7FFF` as its opened spot, so it gets no
//! new hidden rungs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::container::le16;
use crate::npc::NpcPack;

use super::{EditError, Result};

/// Marks the header before an edited ladder ("LP").
pub const MARK: u16 = 0x504C;
/// Bytes of the header before an edited ladder.
pub const HEADER_LEN: usize = 8;
/// Rungs an edited ladder can have (retail's longest has 8).
pub const MAX_RUNGS: usize = 16;
/// Words a walk is read for, at most, when copying a ladder's last rung.
const MAX_WALK: usize = 256;
/// Record type (byte 13) of a chest: an absent body shows it opened.
pub const CHEST: u8 = 3;
const ABSENT: u16 = 0x7FFF;
const ARRIVAL: u16 = 10000;

/// One rung: the condition (`0` always, 1–511 an event flag, ≥ 1000 an item
/// − 1000) and the words after the condition word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rung {
    pub cond: u16,
    pub words: Vec<u16>,
}

/// Edited ladders by retail actor slot.
pub type Ladders = BTreeMap<u32, Vec<Rung>>;

/// A new body for a rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum NewBody {
    /// Off the map in this state.
    Hidden,
    /// Stands here (local to the actor's home chunk), facing `dir` (0–4095).
    Place { x: i16, h: i16, z: i16, dir: u16 },
}

/// A rung of the ladder the editor asks for: rung `from` of the actor's
/// ladder as the project has it (its words kept unless `body` changes them),
/// or a new one (`from: None`, `body` required). The last rung's `cond` is 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RungEdit {
    #[serde(default)]
    pub from: Option<u32>,
    pub cond: u16,
    #[serde(default)]
    pub body: Option<NewBody>,
    /// A new walk after the spot (LT-95, [`super::walk`]); `None` keeps the
    /// rung's own (`0000` for a new spot). Only for a rung that places.
    #[serde(default)]
    pub walk: Option<Vec<u16>>,
}

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

fn word(d: &[u8], at: usize) -> Option<u16> {
    (at + 2 <= d.len()).then(|| le16(d, at))
}

const fn is_marker(w: u16) -> bool {
    w & 0xF000 == 0xA000
}

/// Record type (byte 13).
pub fn record_type(d: u32) -> u8 {
    (d >> 8) as u8
}

/// Walk words from `at` to the one that ends it (`0000` stops, `4000` loops,
/// `Bnnn` restarts), never past the next rung.
fn walk(d: &[u8], at: usize) -> Vec<u16> {
    let mut out = Vec::new();
    while out.len() < MAX_WALK {
        let Some(w) = word(d, at + out.len() * 2).filter(|&w| !is_marker(w)) else { break };
        out.push(w);
        if w == 0 || w == 0x4000 || w & 0xF000 == 0xB000 {
            break;
        }
    }
    out
}

/// The rungs of the program at D-relative `at`, as the game tries them, up
/// to the first unconditional one, for an actor record of type `typ`. Each
/// keeps the words up to the next condition word; the last one keeps what
/// the game reads after it (its body and walk).
pub fn rungs(d: &[u8], at: u32, typ: u8) -> Vec<Rung> {
    let branches = crate::placement::decode(d, at);
    let mut out = Vec::with_capacity(branches.len());
    for (i, br) in branches.iter().enumerate() {
        let body = br.at as usize + 2;
        let end = match branches.get(i + 1) {
            Some(next) => next.at as usize,
            None => match word(d, body) {
                Some(ABSENT) if typ == CHEST => {
                    let spot = body + 2;
                    spot + 8 + walk(d, spot + 8).len() * 2
                }
                Some(ABSENT) => body + 2,
                // 10000 x h z dir, then the arrival camera's words.
                Some(ARRIVAL) => body + 14,
                _ => body + 8 + walk(d, body + 8).len() * 2,
            },
        };
        let end = end.min(d.len());
        let words = (body..end).step_by(2).filter_map(|o| word(d, o)).collect();
        out.push(Rung { cond: br.cond, words });
    }
    out
}

/// The program bytes of an edited ladder for `slot`: the header, then the
/// rungs. The record points `HEADER_LEN` bytes in.
pub fn program(slot: u32, rungs: &[Rung]) -> Vec<u8> {
    let n: usize = rungs.iter().map(|r| 1 + r.words.len()).sum();
    let mut words = vec![MARK, slot as u16, n as u16, 0];
    for r in rungs {
        words.push(0xA000 | r.cond);
        words.extend_from_slice(&r.words);
    }
    let mut out: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out
}

/// The ladder at D-relative `at` (the record's `c`) if it is one this module
/// wrote for `slot`: the header before it and `n` words of rungs.
fn read(d: &[u8], at: u32, slot: u32, span: u32) -> Option<Vec<Rung>> {
    let at = at as usize;
    if at < span as usize + HEADER_LEN {
        return None;
    }
    let h = at - HEADER_LEN;
    if word(d, h)? != MARK || word(d, h + 2)? != slot as u16 {
        return None;
    }
    let n = word(d, h + 4)? as usize;
    let words: Vec<u16> = (0..n).map(|k| word(d, at + k * 2)).collect::<Option<_>>()?;
    // Split as the game scans: at each condition word until the unconditional one,
    // which keeps the rest (a chest's opened spot may run into the next program's words).
    let mut out: Vec<Rung> = Vec::new();
    for w in words {
        let open = out.last().is_none_or(|r| r.cond != 0);
        if is_marker(w) && open {
            out.push(Rung { cond: w & 0x0FFF, words: Vec::new() });
        } else {
            out.last_mut()?.words.push(w);
        }
    }
    Some(out)
}

/// The ladders `current` edits over `retail`: retail records whose `c`
/// points at a ladder this module wrote.
pub fn recover(retail: &[u8], current: &[u8]) -> Result<Ladders> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let cd = c.d_block(current);
    let mut out = Ladders::new();
    for (ra, ca) in r.actors.iter().zip(&c.actors) {
        if ca.placement == ra.placement {
            continue;
        }
        if let Some(rungs) = read(cd, ca.placement, ra.slot, r.d_span) {
            out.insert(ra.slot, rungs);
        }
    }
    Ok(out)
}

/// Slots whose ladder the project edited.
pub fn edited(retail: &[u8], current: &[u8]) -> Vec<u32> {
    let base = super::character::base(retail, current).unwrap_or(std::borrow::Cow::Borrowed(current));
    let mut out: Vec<u32> = recover(retail, &base).map(|l| l.into_keys().collect()).unwrap_or_default();
    // Copies with branches or hidden otherwise (LT-77), by their record slot.
    if let (Ok(added), Some(r)) = (super::actor::recover(retail, &base), NpcPack::parse(retail)) {
        let first = r.actors.len() as u32;
        out.extend(added.iter().enumerate().filter(|(_, a)| !a.branches.is_empty() || a.otherwise_hidden).map(|(i, _)| first + i as u32));
    }
    out
}

/// A copy's ladder as [`build`] takes it: its branches, then its own rung
/// (`x h z dir 0000`, or `7FFF` when it is hidden otherwise).
pub fn copy_rungs(a: &super::actor::AddedActor) -> Vec<Rung> {
    let own = match a.otherwise_hidden {
        true => vec![ABSENT],
        false => {
            let walk: &[u16] = if a.walk.is_empty() { &super::walk::STAND } else { &a.walk };
            [a.x as u16, a.h as u16, a.z as u16, a.dir].iter().chain(walk).copied().collect()
        }
    };
    a.branches.iter().cloned().chain([Rung { cond: 0, words: own }]).collect()
}

/// `a` with the ladder `rungs` (from [`build`]): the branches, and its own
/// rung as its spot and walk, or hidden.
fn set_copy(a: &mut super::actor::AddedActor, rungs: &[Rung], typ: u8) -> Result<()> {
    if rungs.is_empty() || rungs.len() > MAX_RUNGS {
        return Err(EditError::new("LADDER_RUNGS", format!("A ladder has 1 to {MAX_RUNGS} branches.")));
    }
    for (i, rung) in rungs.iter().enumerate() {
        check_rung(i, rung, i + 1 == rungs.len())?;
    }
    let (own, branches) = rungs.split_last().expect("checked");
    match own.words[..] {
        [ABSENT] if typ == CHEST => return Err(EditError::new("LADDER_CHEST", "A chest's hidden branch is its opened shape; a copy of one can't be hidden otherwise.")),
        [ABSENT] => {
            a.otherwise_hidden = true;
            a.walk.clear();
        }
        [x, h, z, dir, ref walk @ ..] if !walk.is_empty() => {
            super::walk::check(walk)?;
            (a.x, a.h, a.z, a.dir) = (x as i16, h as i16, z as i16, dir);
            a.otherwise_hidden = false;
            a.walk = if walk == super::walk::STAND { Vec::new() } else { walk.to_vec() };
        }
        _ => return Err(EditError::new("LADDER_WORDS", "A copy stands at its spot (walking from it or not) or is hidden when none of its flags is set.")),
    }
    a.branches = branches.to_vec();
    Ok(())
}

/// The retail ladder of `slot`.
pub fn retail_rungs(retail: &[u8], slot: u32) -> Result<Vec<Rung>> {
    let r = header(retail, "retail")?;
    let a = r.actors.get(slot as usize).ok_or_else(|| EditError::new("ACTOR_UNKNOWN", format!("The pack has no actor {slot}.")))?;
    Ok(rungs(r.d_block(retail), a.placement, record_type(a.d)))
}

fn check_cond(cond: u16, last: bool) -> Result<()> {
    if last {
        return match cond {
            0 => Ok(()),
            _ => Err(EditError::new("LADDER_NO_FALLBACK", "The last branch is where the actor stands otherwise; it can't have a condition.")),
        };
    }
    match cond {
        0 => Err(EditError::new("LADDER_EARLY_ALWAYS", "Only the last branch can be unconditional: the game stops at the first one that holds.")),
        1..=511 | 1000..=0xFFF => Ok(()),
        512..=999 => Err(EditError::new("LADDER_COND", format!("Condition {cond} never holds in game (flags are 1–511, items 1000 and up)."))),
        _ => Err(EditError::new("LADDER_COND", format!("Condition {cond} doesn't fit the 12-bit condition word."))),
    }
}

/// Checks a ladder for `slot` of `retail`.
pub fn check(retail: &[u8], slot: u32, rungs: &[Rung]) -> Result<()> {
    let r = header(retail, "retail")?;
    let Some(a) = r.actors.get(slot as usize) else {
        return Err(EditError::new("ACTOR_UNKNOWN", format!("The pack has no actor {slot}.")));
    };
    if slot < 2 {
        return Err(EditError::new("LADDER_PARTY", "The Knight and the Baby are placed by the game, not by a ladder."));
    }
    if super::actor::is_object(a.d) {
        return Err(EditError::new("LADDER_OBJECT", "An examine spot has one position in every story state; it has no ladder to edit."));
    }
    if rungs.is_empty() || rungs.len() > MAX_RUNGS {
        return Err(EditError::new("LADDER_RUNGS", format!("A ladder has 1 to {MAX_RUNGS} branches.")));
    }
    for (i, rung) in rungs.iter().enumerate() {
        check_rung(i, rung, i + 1 == rungs.len())?;
    }
    Ok(())
}

/// Checks rung `i` of a ladder (`last`: the unconditional one).
pub fn check_rung(i: usize, rung: &Rung, last: bool) -> Result<()> {
    check_cond(rung.cond, last)?;
    let ok = match rung.words.first() {
        Some(&ABSENT) => true,
        Some(&ARRIVAL) => rung.words.len() >= 5,
        Some(&w) => !is_marker(w) && rung.words.len() >= 4,
        None => false,
    };
    // The game ends a rung at the next condition word; after the last one it never looks.
    if !ok || (!last && rung.words.iter().any(|&w| is_marker(w))) {
        return Err(EditError::new("LADDER_WORDS", format!("Branch {} has no body the game reads.", i + 1)));
    }
    Ok(())
}

fn place_words(x: i16, h: i16, z: i16, dir: u16) -> Result<[u16; 4]> {
    if !(-0x800..0x800).contains(&x) || !(-0x800..0x800).contains(&z) {
        return Err(EditError::new("ACTOR_OUTSIDE", format!("({x}, {z}) is outside the chunk (−2048…2047).")));
    }
    if dir > 4095 {
        return Err(EditError::new("PLACE_DIR", format!("Facing {dir} is out of range (0–4095).")));
    }
    Ok([x as u16, h as u16, z as u16, dir])
}

/// Whether a rung's words place the actor (x h z dir, then its walk).
fn places(words: &[u16]) -> bool {
    words.len() >= 4 && words[0] != ABSENT && words[0] != ARRIVAL
}

/// The ladder `edits` asks for, from `now` (the actor's ladder as the project
/// has it) for an actor of record type `typ`.
pub fn build(now: &[Rung], typ: u8, edits: &[RungEdit]) -> Result<Vec<Rung>> {
    edits
        .iter()
        .map(|e| {
            let from = match e.from {
                Some(i) => Some(now.get(i as usize).ok_or_else(|| EditError::new("LADDER_FROM", format!("The ladder has no branch {}.", i + 1)))?),
                None => None,
            };
            let words = match (e.body, from) {
                (None, Some(f)) => f.words.clone(),
                (None, None) => return Err(EditError::new("LADDER_BODY", "A new branch needs a spot, or to hide the actor.")),
                (Some(NewBody::Place { x, h, z, dir }), f) => {
                    let mut w = place_words(x, h, z, dir)?.to_vec();
                    match f.filter(|f| places(&f.words)) {
                        // A placing rung keeps its walk.
                        Some(f) => w.extend_from_slice(&f.words[4..]),
                        None => w.push(0x0000),
                    }
                    w
                }
                (Some(NewBody::Hidden), Some(f)) if f.words.first() == Some(&ABSENT) => f.words.clone(),
                (Some(NewBody::Hidden), _) if typ == CHEST => {
                    return Err(EditError::new("LADDER_CHEST", "A chest's hidden branch is its opened shape; it can't get a new one."));
                }
                (Some(NewBody::Hidden), _) => vec![ABSENT],
            };
            let words = match &e.walk {
                None => words,
                Some(_) if !places(&words) => return Err(EditError::new("WALK_STANDS", "Only a branch that places the actor can walk from its spot.")),
                Some(walk) => {
                    super::walk::check(walk)?;
                    words[..4].iter().chain(walk).copied().collect()
                }
            };
            Ok(Rung { cond: e.cond, words })
        })
        .collect()
}

/// `current` with `slot`'s ladder as `edits` asks (`None`: back to retail),
/// laid out with the pack's other edits.
pub fn apply(retail: &[u8], current: &[u8], slot: u32, edits: Option<&[RungEdit]>) -> Result<(Vec<u8>, super::text::TextReport)> {
    let mut set = super::pack::Edits::recover(retail, current)?;
    let r = header(retail, "retail")?;
    // A copy the project added (LT-77): its branches live in its own program.
    if let Some(i) = (slot as usize).checked_sub(r.actors.len()) {
        let Some(a) = set.actors.get_mut(i) else {
            return Err(EditError::new("ACTOR_UNKNOWN", format!("The pack has no actor {slot}.")));
        };
        if a.spot {
            return Err(EditError::new("LADDER_OBJECT", "An examine spot has one position in every story state; it has no ladder to edit."));
        }
        let typ = r.actors.get(a.from as usize).map(|s| record_type(s.d)).unwrap_or(0);
        match edits {
            None => {
                a.branches.clear();
                a.otherwise_hidden = false;
            }
            Some(edits) => {
                let rungs = build(&copy_rungs(a), typ, edits)?;
                set_copy(a, &rungs, typ)?;
            }
        }
        let (b, report, _) = super::text::layout(retail, current, &set)?;
        return Ok((b, report));
    }
    match edits {
        None => {
            set.ladders.remove(&slot);
        }
        Some(edits) => {
            let a = r.actors.get(slot as usize).ok_or_else(|| EditError::new("ACTOR_UNKNOWN", format!("The pack has no actor {slot}.")))?;
            let typ = record_type(a.d);
            // The retail program where it is, with the project's in-place moves.
            let base = super::character::base(retail, current)?;
            let in_place = rungs(header(&base, "project's")?.d_block(&base), a.placement, typ);
            let now = set.ladders.get(&slot).cloned().unwrap_or_else(|| in_place.clone());
            let rungs = build(&now, typ, edits)?;
            check(retail, slot, &rungs)?;
            if set.removed.contains(&slot) {
                return Err(EditError::new("LADDER_REMOVED", "This actor was taken out of the game; put it back first."));
            }
            if rungs == in_place {
                set.ladders.remove(&slot);
            } else {
                set.ladders.insert(slot, rungs);
            }
        }
    }
    let (b, report, _) = super::text::layout(retail, current, &set)?;
    Ok((b, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn reads_rungs_with_their_words() {
        // if 0x20: placed, walks; if 0x81: absent; else placed, walk stops; then another program.
        let d = bytes(&[0xA020, 10, 20, 30, 0x400, 0x3005, 0x2010, 0x4000, 0xA081, 0x7FFF, 0xA000, 1, 2, 3, 0, 0, 0xA000, 9, 9, 9, 0, 0]);
        let r = rungs(&d, 0, 0);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], Rung { cond: 0x20, words: vec![10, 20, 30, 0x400, 0x3005, 0x2010, 0x4000] });
        assert_eq!(r[1], Rung { cond: 0x81, words: vec![0x7FFF] });
        assert_eq!(r[2], Rung { cond: 0, words: vec![1, 2, 3, 0, 0] });
        // A chest's absent body carries its opened spot and a walk.
        let d = bytes(&[0xA000, 0x7FFF, 5, 6, 7, 0, 0x001C, 0x0000, 0xA000]);
        assert_eq!(rungs(&d, 0, CHEST)[0].words, vec![0x7FFF, 5, 6, 7, 0, 0x001C, 0x0000]);
        assert_eq!(rungs(&d, 0, 0)[0].words, vec![0x7FFF]);
    }

    #[test]
    fn a_program_reads_back() {
        let ladder = vec![Rung { cond: 0x20, words: vec![1, 2, 3, 0x800, 0] }, Rung { cond: 0x81, words: vec![0x7FFF] }, Rung { cond: 0, words: vec![4, 5, 6, 0, 0x3002, 0] }];
        let mut d = vec![0u8; 0x40];
        let at = d.len() as u32 + HEADER_LEN as u32;
        d.extend(program(7, &ladder));
        assert!(d.len().is_multiple_of(4));
        assert_eq!(read(&d, at, 7, 0x40), Some(ladder.clone()));
        assert_eq!(read(&d, at, 8, 0x40), None, "another slot's mark");
        assert_eq!(read(&d, at, 7, 0x48), None, "inside the retail span");
        // The game sees the same ladder.
        let br = crate::placement::decode(&d, at);
        assert_eq!(br.iter().map(|b| b.cond).collect::<Vec<_>>(), vec![0x20, 0x81, 0]);
        assert_eq!(rungs(&d, at, 0), ladder);
    }

    #[test]
    fn builds_from_the_ladder_now() {
        let now = vec![Rung { cond: 0x20, words: vec![10, 20, 30, 0x400, 0x3005, 0x4000] }, Rung { cond: 0, words: vec![1, 2, 3, 0, 0] }];
        let place = |x, z| Some(NewBody::Place { x, h: -129, z, dir: 1024 });
        let got = build(
            &now,
            0,
            &[
                RungEdit { from: None, cond: 0x81, body: Some(NewBody::Hidden), walk: None },
                RungEdit { from: None, cond: 0x30, body: place(100, -200), walk: None },
                RungEdit { from: Some(0), cond: 0x21, body: place(11, 31), walk: None },
                RungEdit { from: Some(1), cond: 0, body: None, walk: None },
            ],
        )
        .unwrap();
        assert_eq!(got[0], Rung { cond: 0x81, words: vec![0x7FFF] });
        assert_eq!(got[1], Rung { cond: 0x30, words: vec![100, (-129i16) as u16, (-200i16) as u16, 1024, 0] });
        // Moved, its walk kept.
        assert_eq!(got[2], Rung { cond: 0x21, words: vec![11, (-129i16) as u16, 31, 1024, 0x3005, 0x4000] });
        assert_eq!(got[3], now[1]);
        let err = |e: &[RungEdit], typ| build(&now, typ, e).unwrap_err().code;
        assert_eq!(err(&[RungEdit { from: Some(5), cond: 0, body: None, walk: None }], 0), "LADDER_FROM");
        assert_eq!(err(&[RungEdit { from: None, cond: 0, body: None, walk: None }], 0), "LADDER_BODY");
        assert_eq!(err(&[RungEdit { from: None, cond: 0x20, body: Some(NewBody::Hidden), walk: None }], CHEST), "LADDER_CHEST");
        assert_eq!(err(&[RungEdit { from: None, cond: 0x20, body: place(4000, 0), walk: None }], 0), "ACTOR_OUTSIDE");
    }

    #[test]
    fn conditions_are_checked() {
        assert!(check_cond(0, true).is_ok());
        assert_eq!(check_cond(0x20, true).unwrap_err().code, "LADDER_NO_FALLBACK");
        assert_eq!(check_cond(0, false).unwrap_err().code, "LADDER_EARLY_ALWAYS");
        assert_eq!(check_cond(600, false).unwrap_err().code, "LADDER_COND");
        assert!(check_cond(0x1FF, false).is_ok());
        assert!(check_cond(1009, false).is_ok());
    }
}
