//! Actor placement words → NPC pack bytes (screen 10, PRD 10 §5.4).
//!
//! A placement branch is `u16 0xA000 | cond` followed by its body; a placing
//! body is `s16 x, h, z, u16 dir` (see [`crate::placement`]). Moving an actor
//! rewrites those four words in place, so nothing else in the pack moves.
//! Placement programs are D-relative and outside the text pool and the
//! bytecode, so these writes compose with text edits ([`super::text`]).

use serde::{Deserialize, Serialize};

use crate::container::le16;
use crate::npc::NpcPack;

use super::{EditError, Result};

/// The body words of a placing branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub x: i16,
    pub h: i16,
    pub z: i16,
    /// 0–4095 (× 360/4096 degrees).
    pub dir: u16,
}

/// One branch to rewrite: `at` is the D-relative offset of its condition
/// word; `place: None` puts the retail words back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceWrite {
    pub at: u32,
    pub place: Option<Place>,
}

/// Body word values the game reads as "absent" and "only on arrival".
const ABSENT: i16 = 0x7FFF;
const ARRIVAL: i16 = 10000;

/// The placement of the branch at D-relative `at`, if it places the actor.
pub fn place_at(d: &[u8], at: u32) -> Option<Place> {
    let o = at as usize;
    if o + 10 > d.len() || le16(d, o) & 0xF000 != 0xA000 {
        return None;
    }
    let x = le16(d, o + 2) as i16;
    if x == ABSENT || x == ARRIVAL {
        return None;
    }
    Some(Place { x, h: le16(d, o + 4) as i16, z: le16(d, o + 6) as i16, dir: le16(d, o + 8) })
}

/// Applies `writes` to `current` (the pack as the project has it); `retail`
/// supplies the words for `place: None`.
pub fn apply(retail: &[u8], current: &[u8], writes: &[PlaceWrite]) -> Result<Vec<u8>> {
    // Characters from other packs come off for the write and go back on after it.
    let (base, chars, freed) = super::character::split(retail, current)?;
    let out = apply_base(retail, &base, writes)?;
    super::character::apply(retail, &out, &chars, &freed)
}

fn apply_base(retail: &[u8], current: &[u8], writes: &[PlaceWrite]) -> Result<Vec<u8>> {
    let c = NpcPack::parse(current).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The project's pack has no header."))?;
    let r = NpcPack::parse(retail).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The retail pack has no header."))?;
    if c.d != r.d {
        return Err(EditError::new("PLACE_LAYOUT_CHANGED", "This pack was replaced as a whole file; placement edits need the retail layout."));
    }
    let mut b = current.to_vec();
    let d = c.d as usize;
    for w in writes {
        let rd = r.d_block(retail);
        // A branch past the retail D-span is an added actor's (`edit::actor`); it has no retail words.
        let added = w.at >= r.d_span && w.place.is_some() && place_at(c.d_block(current), w.at).is_some();
        let retail_place = if added { w.place } else { place_at(rd, w.at) };
        if retail_place.is_none() {
            return Err(EditError::new(
                "PLACE_NOT_A_BRANCH",
                format!("D+0x{:04X} is not a branch that places the actor (absent and arrival-only branches have no position).", w.at),
            ));
        }
        let p = match w.place {
            Some(p) => p,
            None => retail_place.expect("checked"),
        };
        if p.x == ABSENT || p.x == ARRIVAL {
            return Err(EditError::new("PLACE_RESERVED_X", format!("x = {} reads as a branch marker in game; move one unit over.", p.x)));
        }
        if p.dir > 4095 {
            return Err(EditError::new("PLACE_DIR", format!("Facing {} is out of range (0–4095).", p.dir)));
        }
        let o = d + w.at as usize + 2;
        for (k, v) in [p.x as u16, p.h as u16, p.z as u16, p.dir].into_iter().enumerate() {
            b[o + k * 2..o + k * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
    }
    Ok(b)
}

/// A placing branch whose words differ from retail.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedBranch {
    pub at: u32,
    pub place: Place,
    pub retail: Place,
}

/// Every placing branch reachable from the actor records whose words differ
/// between `retail` and `current`, keyed by actor slot (shared programs list
/// the branch under each slot).
pub fn moved(retail: &[u8], current: &[u8]) -> Vec<(u32, MovedBranch)> {
    let base = super::character::base(retail, current).unwrap_or(std::borrow::Cow::Borrowed(current));
    let current: &[u8] = &base;
    let (Some(r), Some(c)) = (NpcPack::parse(retail), NpcPack::parse(current)) else { return Vec::new() };
    if r.d != c.d {
        return Vec::new();
    }
    let (rd, cd) = (r.d_block(retail), c.d_block(current));
    let mut out = Vec::new();
    for a in &r.actors {
        for br in crate::placement::decode(rd, a.placement) {
            if let (Some(rp), Some(cp)) = (place_at(rd, br.at), place_at(cd, br.at)) {
                if rp != cp {
                    out.push((a.slot, MovedBranch { at: br.at, place: cp, retail: rp }));
                }
            }
        }
    }
    out
}
