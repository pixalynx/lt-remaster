//! Tier-1 trigger positions → NPC pack bytes (screen 10).
//!
//! Every trigger record starts with `s16 x, s16 z` (doors and 8-byte ops
//! alike, see [`crate::trigger`]), so moving one rewrites those two words in
//! place and nothing else in the pack moves. The records a write may touch
//! come from the retail tables, so writes can be applied in any order and on
//! top of placement ([`super::placement`]) and text ([`super::text`]) edits:
//! the tables sit between the actor records and the text pool, which neither
//! of those encoders rewrites. When [`super::door`] or [`super::actor`]
//! moved the tables, a record is written where it is now
//! ([`super::door::locate`]); retail offsets stay the records' ids.
//!
//! A record is *removed* by moving it off the map ([`OFF_MAP`] on both
//! axes): `ScanFieldEventTriggers` compares a record's x and z with the
//! Knight's chunk-local position (within 0x20), which never comes near
//! 0x7000, and the record keeps its type and length, so the tables still
//! parse the same and every other edit lays out unchanged.

use serde::{Deserialize, Serialize};

use crate::container::le16;
use crate::npc::NpcPack;
use crate::trigger::{self, TriggerGroup, TriggerRecord};

use super::{EditError, Result};

/// The word that ends a rung's record list; a record can't start with it.
const END: i16 = 0x7FFF;
/// x and z of a removed record: far outside every chunk (±0x800), and not
/// the end word.
pub const OFF_MAP: i16 = 0x7000;

/// Whether a record at (`x`, `z`) is removed.
pub fn is_off(x: i16, z: i16) -> bool {
    x == OFF_MAP && z == OFF_MAP
}

/// One record to move: `at` is the D-relative offset of the record (its x
/// word). `x` and `z` both `None` put the retail words back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerWrite {
    pub at: u32,
    #[serde(default)]
    pub x: Option<i16>,
    #[serde(default)]
    pub z: Option<i16>,
}

impl TriggerWrite {
    pub fn to(at: u32, x: i16, z: i16) -> Self {
        Self { at, x: Some(x), z: Some(z) }
    }

    pub fn retail(at: u32) -> Self {
        Self { at, x: None, z: None }
    }

    /// Takes the record out of the game ([`OFF_MAP`]).
    pub fn off(at: u32) -> Self {
        Self::to(at, OFF_MAP, OFF_MAP)
    }
}

/// D-relative offset, x and z of a record.
pub fn record_pos(r: &TriggerRecord) -> (u32, i16, i16) {
    match *r {
        TriggerRecord::Door { at, x, z, .. }
        | TriggerRecord::Set { at, x, z, .. }
        | TriggerRecord::Clear { at, x, z, .. }
        | TriggerRecord::Talk { at, x, z, .. }
        | TriggerRecord::Other { at, x, z, .. } => (at, x, z),
    }
}

/// Every record of both tables, in table order.
pub fn records(groups: &[TriggerGroup]) -> impl Iterator<Item = &TriggerRecord> {
    groups.iter().flat_map(|g| &g.rungs).flat_map(|r| &r.records)
}

fn tables(pack: &NpcPack, b: &[u8], which: &str) -> Result<Vec<TriggerGroup>> {
    trigger::parse_tables(pack.d_block(b), pack.actors.len())
        .ok_or_else(|| EditError::new("TRIGGER_NO_TABLE", format!("The {which} pack's trigger tables don't parse.")))
}

/// Applies `writes` to `current` (the pack as the project has it); `retail`
/// supplies the records that may move and the words for a restore.
pub fn apply(retail: &[u8], current: &[u8], writes: &[TriggerWrite]) -> Result<Vec<u8>> {
    // Characters from other packs come off for the write and go back on after it.
    let (base, chars, freed) = super::character::split(retail, current)?;
    let out = apply_base(retail, &base, writes)?;
    super::character::apply(retail, &out, &chars, &freed)
}

fn apply_base(retail: &[u8], current: &[u8], writes: &[TriggerWrite]) -> Result<Vec<u8>> {
    let c = NpcPack::parse(current).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The project's pack has no header."))?;
    let r = NpcPack::parse(retail).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The retail pack has no header."))?;
    if c.d != r.d || c.actors.len() < r.actors.len() {
        return Err(EditError::new(
            "TRIGGER_LAYOUT_CHANGED",
            "This pack was replaced as a whole file; trigger edits need the retail layout.",
        ));
    }
    let retail_groups = tables(&r, retail, "retail")?;
    let loc = super::door::locate(retail, current)?;
    let mut b = current.to_vec();
    let d = c.d as usize;
    for w in writes {
        let Some(rec) = records(&retail_groups).find(|rec| record_pos(rec).0 == w.at) else {
            return Err(EditError::new("TRIGGER_NOT_A_RECORD", format!("D+0x{:04X} is not the start of a trigger record.", w.at)));
        };
        let (_, rx, rz) = record_pos(rec);
        let (x, z) = match (w.x, w.z) {
            (Some(x), Some(z)) => (x, z),
            (None, None) => (rx, rz),
            _ => return Err(EditError::new("TRIGGER_HALF_WRITE", format!("D+0x{:04X}: give both x and z, or neither to restore retail.", w.at))),
        };
        if x == END {
            return Err(EditError::new("TRIGGER_RESERVED_X", format!("x = {x} ends the record list in game; move one unit over.")));
        }
        let o = d + loc.get(&w.at).copied().unwrap_or(w.at) as usize;
        if o + 4 > b.len() {
            return Err(EditError::new("TRIGGER_NOT_A_RECORD", format!("D+0x{:04X} is past the end of the pack.", w.at)));
        }
        b[o..o + 2].copy_from_slice(&x.to_le_bytes());
        b[o + 2..o + 4].copy_from_slice(&z.to_le_bytes());
    }
    Ok(b)
}

/// A record whose position differs from retail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedTrigger {
    pub at: u32,
    pub x: i16,
    pub z: i16,
    pub retail_x: i16,
    pub retail_z: i16,
}

/// Every retail record whose x/z words differ in `current` (same layout only).
pub fn moved(retail: &[u8], current: &[u8]) -> Vec<MovedTrigger> {
    let base = super::character::base(retail, current).unwrap_or(std::borrow::Cow::Borrowed(current));
    let current: &[u8] = &base;
    let (Some(r), Some(c)) = (NpcPack::parse(retail), NpcPack::parse(current)) else { return Vec::new() };
    if r.d != c.d || c.actors.len() < r.actors.len() {
        return Vec::new();
    }
    let Ok(groups) = tables(&r, retail, "retail") else { return Vec::new() };
    let Ok(loc) = super::door::locate(retail, current) else { return Vec::new() };
    let cd = c.d_block(current);
    records(&groups)
        .filter_map(|rec| {
            let (at, rx, rz) = record_pos(rec);
            let o = *loc.get(&at)? as usize;
            if o + 4 > cd.len() {
                return None;
            }
            let (x, z) = (le16(cd, o) as i16, le16(cd, o + 2) as i16);
            (x != rx || z != rz).then_some(MovedTrigger { at, x, z, retail_x: rx, retail_z: rz })
        })
        .collect()
}

/// The retail records `current` removes (by retail offset).
pub fn removed(retail: &[u8], current: &[u8]) -> Vec<u32> {
    let base = super::character::base(retail, current).unwrap_or(std::borrow::Cow::Borrowed(current));
    let current: &[u8] = &base;
    moved(retail, current).into_iter().filter(|m| is_off(m.x, m.z)).map(|m| m.at).collect()
}

/* ───────────────────────── door destinations ───────────────────────── */

/// Where a door sends the Knight: the words after its box in a type 0–7
/// record (`x z facing to tx tz flags pack|music`, [`crate::trigger`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoorDest {
    /// Destination chunk.
    pub to: u16,
    /// Arrival point, local to the destination chunk.
    pub tx: i16,
    pub tz: i16,
    /// Facing on arrival, 0–7 (× 0x200); it is also the record's type.
    pub facing: i16,
    /// NPC pack loaded when `to` isn't the chunk the door is on.
    pub pack: u8,
}

impl DoorDest {
    fn of(r: &TriggerRecord) -> Option<Self> {
        match *r {
            TriggerRecord::Door { facing, to, tx, tz, pack, .. } => Some(Self { to, tx, tz, facing, pack }),
            _ => None,
        }
    }

    /// The destination of the door record at `o` of a D-block.
    fn read(d: &[u8], o: usize) -> Option<Self> {
        let w = d.get(o..o + 16)?;
        let s16 = |i: usize| i16::from_le_bytes([w[i], w[i + 1]]);
        Some(Self { facing: s16(4), to: s16(6) as u16, tx: s16(8), tz: s16(10), pack: w[14] })
    }

    /// Writes the destination into a door record's 16 bytes; the box, the
    /// flags and the music byte keep theirs.
    fn write(&self, w: &mut [u8]) {
        w[4..6].copy_from_slice(&self.facing.to_le_bytes());
        w[6..8].copy_from_slice(&self.to.to_le_bytes());
        w[8..10].copy_from_slice(&self.tx.to_le_bytes());
        w[10..12].copy_from_slice(&self.tz.to_le_bytes());
        w[14] = self.pack;
    }
}

/// One retail door to send elsewhere: `at` is its retail D-relative offset
/// (as for [`TriggerWrite`]); `dest` None puts the retail destination back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorWrite {
    pub at: u32,
    #[serde(default)]
    pub dest: Option<DoorDest>,
}

/// A retail door whose destination differs from retail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetargetedDoor {
    pub at: u32,
    pub dest: DoorDest,
    pub retail: DoorDest,
}

/// Rewrites the destination words of retail doors in place (the record keeps
/// its type, length and box, so every other edit lays out unchanged, and
/// [`super::text::layout`] carries the words along as it does moved boxes).
pub fn retarget(retail: &[u8], current: &[u8], writes: &[DoorWrite]) -> Result<Vec<u8>> {
    let (base, chars, freed) = super::character::split(retail, current)?;
    let out = retarget_base(retail, &base, writes)?;
    let out = super::character::apply(retail, &out, &chars, &freed)?;
    // A cue rung repeats its ladder's always-rung records (`edit::door`);
    // lay table 2 out again so those copies send the Knight to the same place.
    let added = super::door::recover(retail, &out)?;
    if added.iter().any(|a| a.cue != 0) {
        return Ok(super::door::apply(retail, &out, &added)?.0);
    }
    Ok(out)
}

fn retarget_base(retail: &[u8], current: &[u8], writes: &[DoorWrite]) -> Result<Vec<u8>> {
    let c = NpcPack::parse(current).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The project's pack has no header."))?;
    let r = NpcPack::parse(retail).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The retail pack has no header."))?;
    if c.d != r.d || c.actors.len() < r.actors.len() {
        return Err(EditError::new(
            "TRIGGER_LAYOUT_CHANGED",
            "This pack was replaced as a whole file; door edits need the retail layout.",
        ));
    }
    let retail_groups = tables(&r, retail, "retail")?;
    let loc = super::door::locate(retail, current)?;
    let mut b = current.to_vec();
    let d = c.d as usize;
    for w in writes {
        let Some(rec) = records(&retail_groups).find(|rec| record_pos(rec).0 == w.at) else {
            return Err(EditError::new("TRIGGER_NOT_A_RECORD", format!("D+0x{:04X} is not the start of a trigger record.", w.at)));
        };
        let Some(own) = DoorDest::of(rec) else {
            return Err(EditError::new("DOOR_NOT_A_DOOR", format!("D+0x{:04X} is a trigger, not a door.", w.at)));
        };
        let dest = w.dest.unwrap_or(own);
        if !(0..8).contains(&dest.facing) {
            return Err(EditError::new("DOOR_FACING", format!("Facing {} isn't one of the 8 directions (0–7).", dest.facing)));
        }
        if !(-0x800..0x800).contains(&dest.tx) || !(-0x800..0x800).contains(&dest.tz) {
            return Err(EditError::new("DOOR_ARRIVAL_OUTSIDE", format!("The arrival ({}, {}) is outside the destination chunk.", dest.tx, dest.tz)));
        }
        let o = d + loc.get(&w.at).copied().unwrap_or(w.at) as usize;
        let slot = b.get_mut(o..o + 16).ok_or_else(|| EditError::new("TRIGGER_NOT_A_RECORD", format!("D+0x{:04X} is past the end of the pack.", w.at)))?;
        dest.write(slot);
        // The scanner skips word by word to 0x7FFF, so no word of the record may be it.
        if slot.chunks_exact(2).any(|p| i16::from_le_bytes([p[0], p[1]]) == END) {
            return Err(EditError::new("DOOR_END_WORD", format!("A word of the door at D+0x{:04X} would be 0x7FFF, which ends the record list in game.", w.at)));
        }
    }
    Ok(b)
}

/// Every retail door whose destination words differ in `current` (same layout only).
pub fn retargeted(retail: &[u8], current: &[u8]) -> Vec<RetargetedDoor> {
    let base = super::character::base(retail, current).unwrap_or(std::borrow::Cow::Borrowed(current));
    let current: &[u8] = &base;
    let (Some(r), Some(c)) = (NpcPack::parse(retail), NpcPack::parse(current)) else { return Vec::new() };
    if r.d != c.d || c.actors.len() < r.actors.len() {
        return Vec::new();
    }
    let Ok(groups) = tables(&r, retail, "retail") else { return Vec::new() };
    let Ok(loc) = super::door::locate(retail, current) else { return Vec::new() };
    let cd = c.d_block(current);
    records(&groups)
        .filter_map(|rec| {
            let own = DoorDest::of(rec)?;
            let at = record_pos(rec).0;
            let dest = DoorDest::read(cd, *loc.get(&at)? as usize)?;
            (dest != own).then_some(RetargetedDoor { at, dest, retail: own })
        })
        .collect()
}

/// Each retail door record: its retail offset (the id [`retarget`] takes)
/// and where it is in `current` (D-relative), which screen 11 reads its rows by.
pub fn door_ids(retail: &[u8], current: &[u8]) -> Result<Vec<(u32, u32)>> {
    let r = NpcPack::parse(retail).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The retail pack has no header."))?;
    let loc = super::door::locate(retail, current)?;
    Ok(records(&tables(&r, retail, "retail")?)
        .filter(|rec| matches!(rec, TriggerRecord::Door { .. }))
        .filter_map(|rec| {
            let at = record_pos(rec).0;
            Some((at, *loc.get(&at)?))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal pack: header, D at 0x20, two actors, pass 0 empty, pass 1
    /// with one group (chunk 78) holding a talk record and a door.
    fn pack() -> Vec<u8> {
        let mut b = vec![0u8; 0x20];
        b[0..4].copy_from_slice(&0x10u32.to_le_bytes()); // P
        b[4..8].copy_from_slice(&0x20u32.to_le_bytes()); // D
        let mut d: Vec<u8> = Vec::new();
        let w = |d: &mut Vec<u8>, v: i16| d.extend_from_slice(&v.to_le_bytes());
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend(std::iter::repeat_n(0u8, 32));
        let pass1_at = d.len();
        w(&mut d, 0); // pass1, patched below
        w(&mut d, 0); // pass 0: no groups
        let pass1 = d.len() as i16;
        d[pass1_at..pass1_at + 2].copy_from_slice(&pass1.to_le_bytes());
        w(&mut d, 1);
        w(&mut d, 78);
        w(&mut d, 1);
        w(&mut d, 0);
        for v in [-1170, -800, 10, 4] {
            w(&mut d, v);
        }
        for v in [100, 200, 2, 79, 10, 20, 0, 0x0102] {
            w(&mut d, v);
        }
        w(&mut d, 0x7FFF);
        b.extend_from_slice(&d);
        b.resize(0x800, 0);
        b
    }

    #[test]
    fn moves_and_restores_a_record() {
        let retail = pack();
        let p = NpcPack::parse(&retail).unwrap();
        let groups = trigger::parse_tables(p.d_block(&retail), p.actors.len()).unwrap();
        let ats: Vec<u32> = records(&groups).map(|r| record_pos(r).0).collect();
        assert_eq!(ats.len(), 2);

        let out = apply(&retail, &retail, &[TriggerWrite::to(ats[0], -1000, -700), TriggerWrite::to(ats[1], 96, 192)]).unwrap();
        let diff: Vec<usize> = out.iter().zip(&retail).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
        assert!(diff.iter().all(|&i| {
            let rel = i - 0x20;
            (ats[0] as usize..ats[0] as usize + 4).contains(&rel) || (ats[1] as usize..ats[1] as usize + 4).contains(&rel)
        }));
        let m = moved(&retail, &out);
        assert_eq!(m[0], MovedTrigger { at: ats[0], x: -1000, z: -700, retail_x: -1170, retail_z: -800 });
        assert_eq!(m.len(), 2);

        // Restore one, in either order.
        let back = apply(&retail, &out, &[TriggerWrite::retail(ats[1])]).unwrap();
        assert_eq!(moved(&retail, &back).len(), 1);
        let clean = apply(&retail, &back, &[TriggerWrite::retail(ats[0])]).unwrap();
        assert_eq!(clean, retail);
    }

    #[test]
    fn removes_and_restores_a_record() {
        let retail = pack();
        let p = NpcPack::parse(&retail).unwrap();
        let groups = trigger::parse_tables(p.d_block(&retail), p.actors.len()).unwrap();
        let ats: Vec<u32> = records(&groups).map(|r| record_pos(r).0).collect();
        let out = apply(&retail, &retail, &[TriggerWrite::off(ats[1])]).unwrap();
        assert_eq!(removed(&retail, &out), vec![ats[1]]);
        // Same types and lengths: the table parses to the same records.
        let after = trigger::parse_tables(p.d_block(&out), p.actors.len()).unwrap();
        assert_eq!(records(&after).count(), 2);
        assert!(matches!(records(&after).nth(1), Some(TriggerRecord::Door { x: OFF_MAP, z: OFF_MAP, to: 79, .. })));
        assert_eq!(apply(&retail, &out, &[TriggerWrite::retail(ats[1])]).unwrap(), retail);
    }

    #[test]
    fn refuses_non_records_and_the_end_marker() {
        let retail = pack();
        assert_eq!(apply(&retail, &retail, &[TriggerWrite::to(3, 0, 0)]).unwrap_err().code, "TRIGGER_NOT_A_RECORD");
        let p = NpcPack::parse(&retail).unwrap();
        let groups = trigger::parse_tables(p.d_block(&retail), p.actors.len()).unwrap();
        let at = record_pos(records(&groups).next().unwrap()).0;
        assert_eq!(apply(&retail, &retail, &[TriggerWrite::to(at, 0x7FFF, 0)]).unwrap_err().code, "TRIGGER_RESERVED_X");
        let half = TriggerWrite { at, x: Some(1), z: None };
        assert_eq!(apply(&retail, &retail, &[half]).unwrap_err().code, "TRIGGER_HALF_WRITE");
    }

    /// The test pack's door: retail offset, and its destination (79, (10, 20), facing 2, npc/2).
    fn door_at(b: &[u8]) -> u32 {
        let p = NpcPack::parse(b).unwrap();
        let groups = trigger::parse_tables(p.d_block(b), p.actors.len()).unwrap();
        let at = records(&groups).find(|r| matches!(r, TriggerRecord::Door { .. })).map(|r| record_pos(r).0);
        at.unwrap()
    }

    const RETAIL_DEST: DoorDest = DoorDest { to: 79, tx: 10, tz: 20, facing: 2, pack: 2 };
    const ELSEWHERE: DoorDest = DoorDest { to: 120, tx: -300, tz: 400, facing: 5, pack: 9 };

    /// Every door of the pack's tables as (x, to, tx, facing).
    fn doors_of(b: &[u8]) -> Vec<(i16, u16, i16, i16)> {
        let p = NpcPack::parse(b).unwrap();
        records(&p.triggers(b))
            .filter_map(|r| match *r {
                TriggerRecord::Door { x, to, tx, facing, .. } => Some((x, to, tx, facing)),
                _ => None,
            })
            .collect()
    }

    /// @LT-41/5: the write touches only the destination words, and restores.
    #[test]
    fn retargets_and_restores_a_door() {
        let retail = pack();
        let at = door_at(&retail);
        let out = retarget(&retail, &retail, &[DoorWrite { at, dest: Some(ELSEWHERE) }]).unwrap();
        // Facing, to, tx, tz and the pack byte; the box, flags and music keep theirs.
        let o = 0x20 + at as usize;
        let diff: Vec<usize> = out.iter().zip(&retail).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i - o).collect();
        assert!(!diff.is_empty() && diff.iter().all(|&i| (4..12).contains(&i) || i == 14), "{diff:?}");
        let p = NpcPack::parse(&out).unwrap();
        let after = trigger::parse_tables(p.d_block(&out), p.actors.len()).unwrap();
        assert!(matches!(
            records(&after).nth(1),
            Some(&TriggerRecord::Door { x: 100, z: 200, facing: 5, to: 120, tx: -300, tz: 400, flags: 0, pack: 9, music: 1, .. })
        ));
        assert_eq!(retargeted(&retail, &out), vec![RetargetedDoor { at, dest: ELSEWHERE, retail: RETAIL_DEST }]);
        assert_eq!(door_ids(&retail, &out).unwrap(), vec![(at, at)]);
        // Moving the box is a separate edit of the same record.
        let both = apply(&retail, &out, &[TriggerWrite::to(at, 96, 192)]).unwrap();
        assert_eq!(retargeted(&retail, &both).len(), 1);
        assert_eq!(moved(&retail, &both).len(), 1);
        // Putting retail back gives the retail bytes.
        let back = retarget(&retail, &out, &[DoorWrite { at, dest: None }]).unwrap();
        assert_eq!(back, retail);
        assert!(retargeted(&retail, &back).is_empty());
    }

    #[test]
    fn refuses_a_destination_the_game_cant_read() {
        let retail = pack();
        let at = door_at(&retail);
        let talk = record_pos(records(&trigger::parse_tables(NpcPack::parse(&retail).unwrap().d_block(&retail), 2).unwrap()).next().unwrap()).0;
        let code = |at: u32, dest: DoorDest| retarget(&retail, &retail, &[DoorWrite { at, dest: Some(dest) }]).unwrap_err().code;
        assert_eq!(code(talk, ELSEWHERE), "DOOR_NOT_A_DOOR");
        assert_eq!(code(3, ELSEWHERE), "TRIGGER_NOT_A_RECORD");
        assert_eq!(code(at, DoorDest { facing: 8, ..ELSEWHERE }), "DOOR_FACING");
        assert_eq!(code(at, DoorDest { tx: 0x800, ..ELSEWHERE }), "DOOR_ARRIVAL_OUTSIDE");
        assert_eq!(code(at, DoorDest { to: 0x7FFF, ..ELSEWHERE }), "DOOR_END_WORD");
    }

    /// @LT-41/5: a retarget in a table 2 the editor emitted (added and cued
    /// records): the cue rung's copy follows, and later layouts keep it.
    #[test]
    fn a_retarget_survives_added_doors_and_reaches_cue_rung_copies() {
        use super::super::door::{self, Added};
        let retail = pack();
        let at = door_at(&retail);
        let added_door = TriggerRecord::Door { at: 0, x: -64, z: 96, facing: 4, to: 1330, tx: 0, tz: 64, flags: 0x8000, pack: 6, music: 0 };
        let cued = Added { chunk: 78, record: TriggerRecord::Talk { at: 0, x: 300, z: 400, actor: 5 }, cue: 0x1A3 };
        let (out, _) = door::apply(&retail, &retail, &[Added::always(78, added_door), cued]).unwrap();
        // The retail door sits in its own rung and, copied, in the cue rung.
        assert_eq!(doors_of(&out).iter().filter(|d| d.0 == 100).count(), 2);

        let re = retarget(&retail, &out, &[DoorWrite { at, dest: Some(ELSEWHERE) }]).unwrap();
        let doors = doors_of(&re);
        assert!(doors.iter().filter(|d| d.0 == 100).all(|d| (d.1, d.2, d.3) == (120, -300, 5)), "{doors:?}");
        assert!(doors.contains(&(-64, 1330, 0, 4)), "the added door keeps its own destination");
        assert_eq!(retargeted(&retail, &re), vec![RetargetedDoor { at, dest: ELSEWHERE, retail: RETAIL_DEST }]);

        // Linking another door lays the table out again; the retarget stays.
        let mut more = door::recover(&retail, &re).unwrap();
        more.push(Added::always(1330, TriggerRecord::Door { at: 0, x: 0, z: -640, facing: 0, to: 78, tx: 0, tz: 0, flags: 0x8000, pack: 6, music: 0 }));
        let (again, _) = door::apply(&retail, &re, &more).unwrap();
        assert_eq!(retargeted(&retail, &again), retargeted(&retail, &re));

        // Putting it back reaches every copy.
        let back = retarget(&retail, &again, &[DoorWrite { at, dest: None }]).unwrap();
        assert!(retargeted(&retail, &back).is_empty());
        assert!(doors_of(&back).iter().filter(|d| d.0 == 100).all(|d| (d.1, d.2, d.3) == (79, 10, 2)));
    }
}
