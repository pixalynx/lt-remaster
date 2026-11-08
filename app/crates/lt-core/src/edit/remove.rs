//! Retail actor records taken out of the game (screen 10, research 06 §9
//! phase 0, the blank slate). Records are never deleted: scripts name actors
//! by character slot, type-10 records name directors, and added actors
//! follow the retail ones, so every slot keeps its number. A removed record
//! is pointed at a *removal block* in the relocation area instead:
//!
//! ```text
//! +0  A000 7FFF             a placement ladder: one unconditional branch, absent
//! +4  0000 0000 0000 0000   a walk that stops at once; +4 is also a Tier-2
//!                           program (0x00 END)
//! +12 "LR" FFFF             the mark, never read by the game
//! ```
//!
//! - An actor (any record the field gives an NPC structure) gets the block as
//!   its placement program. `WalkActor_SelectScript` then takes the absent
//!   body: the actor is off the map, drawn nowhere and can't be spoken to.
//!   A chest (record type 3) shows its opened shape at the words after an
//!   absent body, so a removed chest's type also becomes 1 ("not drawn").
//! - An object (record types 4/5: examine spots such as furniture text and
//!   beds) has no ladder; the spawn loop places it from its placement words.
//!   Its position can't be moved away safely (it is offset by whole chunks
//!   on the 36 × 18 torus), so its script is pointed at the block's END
//!   instead: examining it runs nothing.
//! - Chimney smoke (type 5) is an object too, and the field draws every
//!   type-5 object (`Field_DrawTorchFlames`) whatever its script, at the
//!   words its placement points at, read without a ladder: an absent body
//!   would only put it at x 0x7FFF, which the field's chunk shifts (±0x1000
//!   per border crossed within one pack's chunks) wrap back into view. So a
//!   removal can also *hide* it ([`Edits::hidden`]): its type becomes 4, an
//!   examine spot (never drawn; the ✕ check skips only type 5), whose script
//!   is the block's END. The blank slate leaves smoke drawn, since the
//!   chimneys stay baked into the mesh; the Pieces tool hides the smoke of a
//!   house it deletes ([`super::unbake`]).
//!
//! [`Edits::hidden`]: super::pack::Edits::hidden
//!
//! The party (slots 0 and 1) can't be removed. The talk program of a removed
//! actor stays, and a type-10 record that names it may still wake it
//! (inference: retail directors are present but unseen); remove those
//! records too ([`super::trigger::OFF_MAP`]). Removal is laid out with the
//! rest of the pack's edits ([`super::text::layout`]) and applied last, so
//! copies of a removed actor ([`super::actor`]) keep its retail record.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::container::le16;
use crate::npc::NpcPack;

use super::{EditError, Result};

/// The mark at the end of a removal block ("LR").
pub const MARK: u16 = 0x524C;
/// Bytes of the removal block.
pub const BLOCK_LEN: usize = 16;
/// Where the END program sits in the block.
pub const END_AT: u32 = 4;
/// Record type (byte 13) of a chest: an absent body shows it opened.
const CHEST: u8 = 3;
/// Record type of an examine spot: never drawn. Hidden smoke takes it.
const EXAMINE: u8 = 4;
/// Record type of chimney smoke: the field draws every object of this type.
pub const SMOKE: u8 = 5;
/// Record type that `Npc_DrawIfVisible` never draws.
const NOT_DRAWN: u8 = 1;

/// The removal block.
pub fn block() -> [u8; BLOCK_LEN] {
    let words: [u16; 8] = [0xA000, 0x7FFF, 0, 0, 0, 0, MARK, 0xFFFF];
    let mut out = [0u8; BLOCK_LEN];
    for (i, w) in words.iter().enumerate() {
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// Whether D-relative `at` of `d` holds a removal block past the retail
/// D-span `span` (where only the project writes).
pub fn is_block(d: &[u8], at: u32, span: u32) -> bool {
    let o = at as usize;
    at >= span && o + BLOCK_LEN <= d.len() && d[o..o + BLOCK_LEN] == block()
}

/// Whether a record's script word points at a removal block's END.
pub fn is_end(d: &[u8], script: u32, span: u32) -> bool {
    script >= END_AT && is_block(d, script - END_AT, span)
}

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

/// How a removed record is taken out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum How {
    /// An actor: its placement is the block (absent).
    Absent,
    /// An object (types 4/5): its script is the block's END.
    Silent,
}

/// How record `d` (the record's last word) is removed.
pub fn how(d: u32) -> How {
    if super::actor::is_object(d) {
        How::Silent
    } else {
        How::Absent
    }
}

/// The record type of `d` (the record's last word).
fn kind(d: u32) -> u8 {
    (d >> 8) as u8
}

/// Checks `removed` against `retail`: real records, not the party; and
/// `hidden`: smoke records.
pub fn check(retail: &[u8], removed: &BTreeSet<u32>, hidden: &BTreeSet<u32>) -> Result<()> {
    let r = header(retail, "retail")?;
    for &slot in removed {
        if slot < 2 {
            return Err(EditError::new("REMOVE_PARTY", "The Knight and the Baby can't be removed."));
        }
        if slot as usize >= r.actors.len() {
            return Err(EditError::new("REMOVE_NOT_RETAIL", format!("Actor {slot} isn't one of the pack's retail records; remove an added actor instead.")));
        }
    }
    for &slot in hidden {
        if r.actors.get(slot as usize).is_none_or(|a| kind(a.d) != SMOKE) {
            return Err(EditError::new("REMOVE_HIDE_NOT_SMOKE", format!("Record {slot} isn't chimney smoke; only smoke is hidden when removed.")));
        }
    }
    Ok(())
}

/// Writes the removal of `removed` into `b` (the laid-out pack, before the
/// relocation area is inserted): `at` is the block's D-relative offset in
/// the final pack. Records are the retail ones at this point. Smoke in
/// `hidden` (and removed) becomes an examine spot.
pub(crate) fn write(b: &mut [u8], r: &NpcPack, removed: &BTreeSet<u32>, hidden: &BTreeSet<u32>, at: u32) {
    let d = r.d as usize;
    for &slot in removed {
        let o = d + 4 + 16 * slot as usize;
        let rec_d = u32::from_le_bytes(b[o + 12..o + 16].try_into().expect("4 bytes"));
        match how(rec_d) {
            How::Absent => {
                b[o + 8..o + 12].copy_from_slice(&at.to_le_bytes());
                if b[o + 13] == CHEST {
                    b[o + 13] = NOT_DRAWN;
                }
            }
            How::Silent => {
                b[o..o + 4].copy_from_slice(&(at + END_AT).to_le_bytes());
                if hidden.contains(&slot) && b[o + 13] == SMOKE {
                    b[o + 13] = EXAMINE;
                }
            }
        }
    }
}

/// The retail records `current` removes, read back from their records.
pub fn recover(retail: &[u8], current: &[u8]) -> Result<BTreeSet<u32>> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let cd = c.d_block(current);
    let mut out = BTreeSet::new();
    for (slot, ra) in r.actors.iter().enumerate().skip(2) {
        let Some(ca) = c.actors.get(slot) else { break };
        let gone = match how(ra.d) {
            How::Absent => is_block(cd, ca.placement, r.d_span),
            How::Silent => is_end(cd, ca.script, r.d_span),
        };
        if gone {
            out.insert(slot as u32);
        }
    }
    Ok(out)
}

/// The removed smoke records of `current` that are hidden: retail type 5,
/// type 4 now ([`write`]).
pub fn recover_hidden(retail: &[u8], current: &[u8]) -> Result<BTreeSet<u32>> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let cd = c.d_block(current);
    let mut out = BTreeSet::new();
    for (slot, ra) in r.actors.iter().enumerate().skip(2) {
        let Some(ca) = c.actors.get(slot) else { break };
        if kind(ra.d) == SMOKE && kind(ca.d) == EXAMINE && is_end(cd, ca.script, r.d_span) {
            out.insert(slot as u32);
        }
    }
    Ok(out)
}

/// `current` with `take` taken out of the game, smoke hidden too, and `give`
/// put back (drawn again), the rest of its edits laid out again with it
/// ([`super::text::layout`]): what a deleted piece does to its records.
pub fn take_out(retail: &[u8], current: &[u8], take: &BTreeSet<u32>, give: &BTreeSet<u32>) -> Result<Vec<u8>> {
    let r = header(retail, "retail")?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    set.removed.retain(|s| !give.contains(s));
    set.hidden.retain(|s| !give.contains(s));
    set.removed.extend(take);
    set.hidden.extend(take.iter().filter(|&&s| r.actors.get(s as usize).is_some_and(|a| kind(a.d) == SMOKE)));
    Ok(super::text::layout(retail, current, &set)?.0)
}

/// The slots of `current` whose script is a removal block's END (objects
/// taken out): [`super::script`] leaves them out of its edited programs.
pub fn silenced(retail: &[u8], current: &[u8]) -> BTreeSet<u32> {
    let base = super::character::base(retail, current).unwrap_or(std::borrow::Cow::Borrowed(current));
    let current: &[u8] = &base;
    let (Some(r), Some(c)) = (NpcPack::parse(retail), NpcPack::parse(current)) else { return BTreeSet::new() };
    let cd = c.d_block(current);
    c.actors.iter().filter(|a| is_end(cd, a.script, r.d_span)).map(|a| a.slot).collect()
}

/// `current` with exactly `removed` taken out; its other edits are laid out
/// again with it ([`super::text::layout`]).
pub fn apply(retail: &[u8], current: &[u8], removed: &BTreeSet<u32>) -> Result<Vec<u8>> {
    let mut set = super::pack::Edits::recover(retail, current)?;
    set.removed = removed.clone();
    Ok(super::text::layout(retail, current, &set)?.0)
}

/// Every retail record that can be removed: slots 2 and up.
pub fn removable(retail: &[u8]) -> Result<Vec<(u32, How)>> {
    let r = header(retail, "retail")?;
    Ok(r.actors.iter().skip(2).map(|a| (a.slot, how(a.d))).collect())
}

/// Whether a placement word of `d` past the retail span `span` is a removal
/// block (the world editor reads such an actor as removed).
pub fn placement_removed(d: &[u8], placement: u32, span: u32) -> bool {
    is_block(d, placement, span) && le16(d, placement as usize + 2) == 0x7FFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_is_an_absent_ladder_and_an_end_program() {
        let b = block();
        let mut d = vec![0u8; 0x40];
        d[0x20..0x30].copy_from_slice(&b);
        let ladder = crate::placement::decode(&d, 0x20);
        assert_eq!(ladder.len(), 1);
        assert_eq!(ladder[0].cond, 0);
        assert_eq!(ladder[0].body, crate::placement::Body::Absent);
        let code = crate::tier2::disassemble(&d, 0x20 + END_AT).unwrap();
        assert_eq!(code.len(), 1);
        assert_eq!(code[0].op, 0x00);
        assert!(is_block(&d, 0x20, 0x20));
        assert!(!is_block(&d, 0x20, 0x21), "a block inside the retail span is retail data");
        assert!(is_end(&d, 0x24, 0x10));
        assert!(placement_removed(&d, 0x20, 0x10));
    }
}
