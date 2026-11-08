//! What a house adds to an NPC pack besides its doors and examine spots (screen 23 phase 6, spec §12.2): its
//! residents, the smoke over the chimneys of its lit fires, and the stand-ins of its locked doors. Every record
//! carries the house's mark ([`AddedActor::house`]: its inside's ground-floor area), so [`take_out`] removes exactly
//! the house's own when it rebuilds, and nothing the project placed by hand.
//!
//! - A **resident** is a copy of a retail character homed on a storey's area ([`super::actor`]), with a talk program
//!   of its own (it turns to the Knight and says its line) and a walk ([`super::inside::resident_walk`]). One from
//!   another pack copies a character of this one (its *carrier*) and looks like the one brought in
//!   ([`super::character`]).
//! - **Smoke** is a type-5 object ([`AddedActor::smoke`]) over the chimney: the field draws a column of grey puffs
//!   rising from its spot, the first three on screen in record order, so retail smoke in view comes first.
//! - A **locked door**'s way in is a door record in a box the Knight can't walk into; a type-10 record in the box he
//!   does step into wakes a hidden director there ([`super::scene`] builds new scenes the same way), whose program
//!   ([`lock_dsl`]) walks him through once the lock's flag is set, unlocks it when he holds the key, and otherwise
//!   says so and turns him back. The ladder can't do it: a flag rung sits above the chunk's story rungs in trigger
//!   table 2 (70 of retail's 83 groups there have them, the village's among them) and would hide them for good.

use crate::npc::NpcPack;
use crate::tier2::dsl::{self, Severity};
use crate::trigger::TriggerRecord;

use super::actor::{self, AddedActor};
use super::character::Character;
use super::door::{self, Added};
use super::inside::ResidentPlan;
use super::scene::{char_slot, record_of};
use super::{EditError, Result};

/// One record a house adds.
#[derive(Debug, Clone)]
pub enum Piece {
    /// A resident: a copy of actor `from` of the pack, looking like `look` (a character from another pack) if any.
    Resident { plan: ResidentPlan, from: u32, look: Option<Box<Character>> },
    /// Smoke over a chimney, homed on the chunk the house stands in.
    Smoke { home: u16, x: i16, h: i16, z: i16 },
    /// A locked door's stand-in: a hidden director at the door box (`home` chunk units), running `dsl` when the
    /// Knight steps in.
    Lock { home: u16, x: i16, h: i16, z: i16, dsl: String },
}

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

fn parsed(src: &str, what: &str) -> Result<Vec<crate::tier2::ast::Stmt>> {
    let (stmts, diags) = dsl::parse(src);
    match diags.into_iter().find(|d| d.severity == Severity::Error) {
        Some(d) => Err(EditError::new("LIFE_SCRIPT", format!("{what}'s program doesn't parse: {}", d.message))),
        None => Ok(stmts),
    }
}

/// A character of `retail` that a copy can be made from and dressed as another: not the party, not an object or a
/// director, with a program.
pub fn carrier(retail: &[u8]) -> Option<u32> {
    let r = NpcPack::parse(retail)?;
    r.actors.iter().find(|a| a.slot >= 2 && !actor::is_object(a.d) && !actor::looks_hidden(a.b, a.d) && a.script != 0 && super::ladder::record_type(a.d) == 0).map(|a| a.slot)
}

/// `current` without the records house `house` added (and the talk records that wake its directors), or `None`
/// when it has none. Later added characters move down; their wake-up records follow.
pub fn take_out(retail: &[u8], current: &[u8], house: u16) -> Result<Option<Vec<u8>>> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    let gone: Vec<usize> = set.actors.iter().enumerate().filter(|(_, a)| a.house == Some(house)).map(|(k, _)| k).collect();
    if gone.is_empty() {
        return Ok(None);
    }
    let chars: Vec<u16> = gone.iter().filter(|&&k| !set.actors[k].spot).filter_map(|&k| char_slot(&c.actors, r.actors.len() + k)).collect();
    for &k in gone.iter().rev() {
        set.actors.remove(k);
    }
    set.doors.retain(|d| !matches!(d.record, TriggerRecord::Talk { actor, .. } if chars.contains(&actor)));
    for d in set.doors.iter_mut() {
        if let TriggerRecord::Talk { actor, .. } = &mut d.record {
            if record_of(&c.actors, *actor).is_some_and(|rec| rec >= r.actors.len()) {
                *actor -= chars.iter().filter(|&&g| g < *actor).count() as u16;
            }
        }
    }
    set.doors = door::canonical(retail, &set.doors)?;
    Ok(Some(super::text::layout(retail, current, &set)?.0))
}

/// `current` with `pieces` added for house `house`, in one layout. `areas` are the project's new areas, which a
/// lock's warp may name. Returns the pack and each piece's record slot.
pub fn add(retail: &[u8], current: &[u8], house: u16, pieces: &[Piece], areas: &[u32]) -> Result<(Vec<u8>, Vec<u32>)> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let mut set = super::pack::Edits::recover(retail, current)?;
    // Character slots: the pack's characters so far, then each added one in turn.
    let mut next_char = c.actors.iter().enumerate().filter(|(i, a)| *i < 2 || !actor::is_object(a.d)).count() as u16;
    let mut slots = Vec::new();
    let mut programs: Vec<(u32, String)> = Vec::new();
    for p in pieces {
        let base = AddedActor {
            from: actor::NEW_SPOT,
            home: 0,
            x: 0,
            h: 0,
            z: 0,
            dir: 0,
            hidden: false,
            own: None,
            character: None,
            spot: false,
            branches: Vec::new(),
            otherwise_hidden: false,
            walk: Vec::new(),
            house: Some(house),
            smoke: false,
        };
        let a = match p {
            Piece::Resident { plan, from, look } => {
                let character = match look {
                    Some(ch) => Some(match set.characters.iter().position(|x| x.label == ch.label) {
                        Some(k) => k as u16,
                        None => {
                            set.characters.push((**ch).clone());
                            (set.characters.len() - 1) as u16
                        }
                    }),
                    None => None,
                };
                let who = if plan.name.is_empty() { "A resident".to_string() } else { plan.name.clone() };
                let dsl = plan.dsl();
                AddedActor { from: *from, home: plan.area as u16, x: plan.x, h: plan.h, z: plan.z, dir: plan.dir, own: Some(parsed(&dsl, &who)?), character, walk: plan.walk.clone(), ..base }
            }
            Piece::Smoke { home, x, h, z } => AddedActor { home: *home, x: *x, h: *h, z: *z, own: Some(Vec::new()), spot: true, smoke: true, ..base },
            Piece::Lock { home, x, h, z, dsl } => {
                let from = director_source(&r).ok_or_else(|| EditError::new("LOCK_NO_SOURCE", "This pack has no actor a locked door's director can copy."))?;
                AddedActor { from, home: *home, x: *x, h: *h, z: *z, hidden: true, own: Some(parsed(dsl, "A locked door")?), ..base }
            }
        };
        let slot = (r.actors.len() + set.actors.len()) as u32;
        if let Piece::Lock { home, x, z, dsl, .. } = p {
            set.doors.push(Added::always(*home as i16, TriggerRecord::Talk { at: 0, x: *x, z: *z, actor: next_char }));
            programs.push((slot, dsl.clone()));
        }
        if let Piece::Resident { plan, .. } = p {
            programs.push((slot, plan.dsl()));
        }
        if !a.spot {
            next_char += 1;
        }
        set.actors.push(a);
        slots.push(slot);
    }
    set.doors = door::canonical(retail, &set.doors)?;
    let (laid, _, _) = super::text::layout(retail, current, &set)?;
    // Checked as screen 11 checks a program: text that fits, flags in range, chunk operands naming real areas.
    for (slot, src) in &programs {
        let check = super::script::check(retail, &laid, *slot, Some(src), areas)?;
        if let Some(d) = check.diagnostics.iter().find(|d| d.severity == Severity::Error) {
            return Err(EditError::new("LIFE_SCRIPT", format!("Actor {slot}'s program: {}", d.message)));
        }
    }
    Ok((laid, slots))
}

/// A retail actor a director can copy (as [`super::scene`] picks one): not the party, not an object, with a
/// program, a hidden retail director first.
fn director_source(r: &NpcPack) -> Option<u32> {
    let ok = |a: &&crate::npc::ActorRecord| a.slot >= 2 && !actor::is_object(a.d) && a.script != 0;
    r.actors.iter().filter(ok).find(|a| actor::looks_hidden(a.b, a.d)).or_else(|| r.actors.iter().find(ok)).map(|a| a.slot)
}

/// The program of a locked door's stand-in (§12.2). The way in is an ordinary door record in a box just inside the
/// doorway, where the Knight can't stand; the stand-in sits in the box he steps into. Once `flag` is set, or when he
/// holds `key` (it unlocks the door and is used up), it walks him into the door's box (`walk to(…)` moves him
/// straight there, walls or not), and the door takes him through with its pack, its music and its sound, as any
/// door does. Locked, it says so and turns him back out of the box. A script's own warp would not do: it names no
/// pack, so a new area would come up without its people and spots.
pub fn lock_dsl(flag: u16, key: Option<u16>, into: [i16; 4]) -> String {
    let walk = format!("walk to({} {} {} {})", into[0], into[1], into[2], into[3]);
    let locked = "say \"It's locked.\"\n  knight_away";
    match key {
        Some(k) => format!("if flag(0x{flag:03X}) {{\n  {walk}\n}} else if item({k}) {{\n  set 0x{flag:03X}\n  ret2 {k}\n  say \"You unlock the door.\"\n  {walk}\n}} else {{\n  {locked}\n}}\n"),
        None => format!("if flag(0x{flag:03X}) {{\n  {walk}\n}} else {{\n  {locked}\n}}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_programs_parse() {
        for key in [None, Some(504)] {
            let src = lock_dsl(0x1A0, key, [-48, -129, 120, 0]);
            let (stmts, diags) = dsl::parse(&src);
            assert!(diags.iter().all(|d| d.severity != Severity::Error), "{src}\n{diags:?}");
            assert!(!stmts.is_empty());
            assert!(src.contains("walk to(-48 -129 120 0)"), "{src}");
        }
        assert!(lock_dsl(0x1A0, Some(504), [0, 0, 0, 0]).contains("ret2 504"));
    }
}
