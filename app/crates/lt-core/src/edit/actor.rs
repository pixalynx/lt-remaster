//! Actors added to an NPC pack: copies of a retail actor (its talk script,
//! model and type) homed in another chunk, such as a new area (screen 10,
//! PRD 10 §5.5).
//!
//! The loader spawns every record of the pack (`NPC_UpdateActors` case 1)
//! and places each through its placement ladder, in coordinates local to the
//! record's home chunk (`b` low u16). `NpcBin_RelocateHeader` finds `pass1`
//! right after the last record, so records can only be added there: the
//! added ones follow the retail records, `pass1` and trigger table 1 move up
//! 16 bytes each, into the room trigger table 2 leaves when it is emitted in
//! the relocation area ([`super::door`]). That room bounds how many fit
//! ([`room`]).
//!
//! An added actor's placement program goes in the relocation area too:
//! `A000 x h z dir 0000`, one unconditional branch whose walk script stops at
//! once, then `"LT" from`, which the walker never reads and [`recover`] uses
//! to find the actor it copies.
//!
//! A copy can have flag branches (LT-77, [`AddedActor::branches`]): rungs the
//! game tries before the copy's own `A000`, written as [`super::ladder`]
//! writes them (`A000|flag`, then a spot and `0000`, or `7FFF`). Its own rung
//! can keep it off the map ([`AddedActor::otherwise_hidden`]): `A000 7FFF x h
//! z dir`, the spot kept for later and never read. Either way `"LT" from`
//! comes 6 words after the copy's `A000`, and a copy with no branches keeps
//! the 16-byte program.
//!
//! A copy that stands at its spot can walk (LT-95, [`AddedActor::walk`]): its
//! own rung is `A000 x h z dir`, then the walk ([`super::walk`], ending in a
//! stop, a loop or a re-place), then `"LT" from`. A copy that stands still
//! has the walk `0000`, so the mark stays 6 words in.
//!
//! A copy can be *hidden*: no model, no pose set, size and kind 0, the
//! shape of the invisible actors that direct nine retail walk-in scenes
//! (npc/1 actor 11, npc/9 actors 10–12…). It is drawn nowhere, can't be
//! spoken to and blocks nothing; a type-10 trigger record wakes it.
//!
//! A copy can look like a character brought in from another pack
//! ([`AddedActor::character`]): record `b` bytes 2–3 pick that character's
//! model and pose set (indices past the pack's own) and `d` bytes 0–1 take
//! its shadow and kind.
//!
//! A copy runs its source's talk program (and follows it when the program
//! is edited) until it gets one of its own ([`AddedActor::own`], edited in
//! screen 11): that program is assembled with the other edited programs
//! ([`super::script`]) and only the copy's record points at it.
//!
//! An added record can be an *examine spot* ([`AddedActor::spot`], a bed, a
//! sign, a chest: [`super::spot`]): record type 4, which the spawn loop
//! places from words 1–3 of the same placement program and which takes no
//! character slot.

use serde::{Deserialize, Serialize};

use crate::container::le16;
use crate::npc::NpcPack;

use super::{EditError, Result};

/// Marks the words after an added actor's walk script ("LT").
pub const MARK: u16 = 0x4C54;
/// Bytes of an added actor's placement program.
pub const PROGRAM_LEN: usize = 16;
/// Character slots a pack's actors can take: entries 0x00–0x4F of the 0x38-byte
/// structures at 0x80146600; objects (record type 4/5) don't take one.
/// `NPC_UpdateActors` clears 0x56 of them, but entry 0x50 (0x80147780) is the
/// field's special object (`Npc_DrawIfVisible(OBJ, 0x50, …)`) and 0x51–0x56
/// (0x801477B8) are the six wanderers. A pack character there shares its
/// structure with one of them: the wanderer loop draws it with a model and pose
/// set read from its own fields, and the pose blend writes past the end of RAM
/// over the exception vector (seen in PCSX-Redux, LT-49). Retail's busiest pack,
/// npc/22, has 77 characters.
pub const NPC_SLOTS: usize = 0x50;

/// An actor the project added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedActor {
    /// The retail slot it copies (talk script, model, type). An examine spot
    /// runs its program until it has one of its own; [`NEW_SPOT`] (0) is a
    /// spot copying nothing, which always has its own.
    pub from: u32,
    /// Home chunk: its placement is local to it, and it shows there.
    pub home: u16,
    pub x: i16,
    pub h: i16,
    pub z: i16,
    /// Facing, 0–4095.
    pub dir: u16,
    /// A scene director: no model, no pose set, size and kind 0 ([`record`]).
    #[serde(default)]
    pub hidden: bool,
    /// Looks like character `k` the project brought in ([`super::character`]):
    /// its model, pose set, shadow and kind instead of its source's.
    #[serde(default)]
    pub character: Option<u16>,
    /// Its own talk program; `None` runs its source's.
    #[serde(skip)]
    pub own: Option<Vec<crate::tier2::ast::Stmt>>,
    /// An examine spot ([`super::spot`]): record type 4, no model, no character
    /// slot; the Knight examines it and its program runs as his.
    #[serde(default)]
    pub spot: bool,
    /// Flag branches the game tries before the copy's own spot (conditional
    /// rungs, as [`super::ladder`] writes them). Empty: it stands at its spot
    /// in every story state.
    #[serde(default)]
    pub branches: Vec<super::ladder::Rung>,
    /// With none of its branches' flags set, the copy is off the map instead
    /// of at its spot (which it keeps).
    #[serde(default)]
    pub otherwise_hidden: bool,
    /// The walk from its own spot (LT-95, [`super::walk`]); empty: it stands
    /// still (`0000`). Not with `otherwise_hidden`, and not for a spot.
    #[serde(default)]
    pub walk: Vec<u16>,
    /// Added by the house whose inside's ground floor is this area (screen 23:
    /// residents, fires, a lock's spot), which takes it out again when it
    /// rebuilds. Written after the mark: `"LT" from|0x8000 area`.
    #[serde(default)]
    pub house: Option<u16>,
    /// With `spot`: a column of smoke (record type 5), drawn over its spot and
    /// never examined (`Field_DrawTorchFlames`, despite the name, draws only
    /// grey puffs; at most 3 a frame, the first in record order).
    #[serde(default)]
    pub smoke: bool,
}

/// Record `d` of added smoke: type 5, byte 12 = 2, as all 55 retail smoke records have it.
pub const SMOKE_D: u32 = 0x0000_0502;

/// Bit of the word after the mark that says a house's area follows it.
const HOUSE_BIT: u16 = 0x8000;

/// `from` of an examine spot that copies no record.
pub const NEW_SPOT: u32 = 0;

/// Record bytes a hidden copy changes: `b` bytes 2–3 (model, pose set) to
/// 0xFF, `d` bytes 0–1 (size, kind) to 0.
fn hide(out: &mut [u8; 16]) {
    out[6] = 0xFF;
    out[7] = 0xFF;
    out[12] = 0;
    out[13] = 0;
}

/// A director's looks: no model, no pose set, size and kind 0. Retail scene
/// directors have them, and so does a hidden copy of any actor.
pub(crate) fn looks_hidden(b: u32, d: u32) -> bool {
    b >> 16 == 0xFFFF && d & 0xFFFF == 0
}

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

/// Record type (byte 13): 4 and 5 are objects placed without a ladder.
pub fn is_object(d: u32) -> bool {
    matches!((d >> 8) & 0xFF, 4 | 5)
}

/// Bytes from the end of trigger table 1 to the end of table 2 in `retail`:
/// the room added records can take once table 2 moves.
pub fn room_bytes(retail: &[u8]) -> Result<usize> {
    let r = header(retail, "retail")?;
    let d = r.d_block(retail);
    let t1 = 4 + 16 * r.actors.len() + 2;
    let len1 = crate::trigger::table_len(d, t1).ok_or_else(|| EditError::new("TRIGGER_NO_TABLE", "The retail pack's trigger table 1 doesn't parse."))?;
    let len2 = if r.pass1 > 0 {
        crate::trigger::table_len(d, r.pass1 as usize).ok_or_else(|| EditError::new("TRIGGER_NO_TABLE", "The retail pack's trigger table 2 doesn't parse."))?
    } else {
        0
    };
    // Table 2 follows table 1 in every retail pack; anything else has no room.
    if r.pass1 as usize != t1 + len1 {
        return Ok(0);
    }
    Ok(len2)
}

/// How many actors `retail` can take.
pub fn room(retail: &[u8]) -> Result<usize> {
    Ok(room_bytes(retail)? / 16)
}

/// A pack's character budget (LT-55): of its [`NPC_SLOTS`] character slots,
/// how many its records take, how many of those are retail actors the project
/// took out (they keep their slot, [`super::remove`]), and what still fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    /// Characters the pack's records take: retail ones that aren't examine spots, and added actors.
    pub characters: usize,
    /// [`NPC_SLOTS`].
    pub limit: usize,
    /// Of `characters`, retail actors taken out of the game.
    pub taken_out: usize,
    /// Of `characters`, actors the project added.
    pub added: usize,
    /// Examine spots the project added (no character slot).
    pub spots: usize,
    /// Records the pack still takes (16 bytes each where trigger table 2 was, [`room`]).
    pub records_left: usize,
    /// Characters it still takes: the records left, up to the free slots.
    pub characters_left: usize,
}

/// The character budget of `current` (the project's pack) over `retail`.
pub fn budget(retail: &[u8], current: &[u8]) -> Result<Budget> {
    let r = header(retail, "retail")?;
    let set = super::pack::Edits::recover(retail, current)?;
    let retail_chars = r.actors.iter().filter(|a| !is_object(a.d)).count();
    let taken_out = set.removed.iter().filter(|&&s| r.actors.get(s as usize).is_some_and(|a| !is_object(a.d))).count();
    let added = set.actors.iter().filter(|a| !a.spot).count();
    let characters = retail_chars + added;
    let records_left = room(retail)?.saturating_sub(set.actors.len());
    Ok(Budget {
        characters,
        limit: NPC_SLOTS,
        taken_out,
        added,
        spots: set.actors.len() - added,
        records_left,
        characters_left: records_left.min(NPC_SLOTS.saturating_sub(characters)),
    })
}

/// Checks `added` against `retail`: real, non-object, non-party sources (an
/// examine spot may copy an object, or nothing with a program of its own),
/// the room and the NPC structures.
pub fn check(retail: &[u8], added: &[AddedActor]) -> Result<()> {
    let r = header(retail, "retail")?;
    for a in added {
        let Some(src) = r.actors.get(a.from as usize) else {
            return Err(EditError::new("ACTOR_UNKNOWN", format!("The pack has no actor {}.", a.from)));
        };
        if a.spot {
            if a.hidden || a.character.is_some() {
                return Err(EditError::new("SPOT_LOOKS", "An examine spot has no model, so it can't be hidden or look like a character."));
            }
            if a.from == NEW_SPOT && a.own.is_none() {
                return Err(EditError::new("SPOT_NO_PROGRAM", "This examine spot copies no actor, so it needs a program of its own."));
            }
            if a.from == 1 {
                return Err(EditError::new("ACTOR_PARTY", "The Knight and the Baby can't be copied."));
            }
        } else {
            if a.from < 2 {
                return Err(EditError::new("ACTOR_PARTY", "The Knight and the Baby can't be copied."));
            }
            if is_object(src.d) {
                return Err(EditError::new(
                    "ACTOR_OBJECT",
                    format!("Actor {} is an examine spot (type 4/5), which has no placement ladder to copy; add it as an examine spot.", a.from),
                ));
            }
        }
        if !(-0x800..0x800).contains(&a.x) || !(-0x800..0x800).contains(&a.z) {
            return Err(EditError::new("ACTOR_OUTSIDE", format!("({}, {}) is outside the chunk (−2048…2047).", a.x, a.z)));
        }
        if a.spot && (!a.branches.is_empty() || a.otherwise_hidden) {
            return Err(EditError::new("LADDER_OBJECT", "An examine spot has one position in every story state; it has no ladder to edit."));
        }
        if a.otherwise_hidden && super::ladder::record_type(src.d) == super::ladder::CHEST {
            return Err(EditError::new("LADDER_CHEST", "A chest's hidden branch is its opened shape; a copy of one can't be hidden otherwise."));
        }
        if a.branches.len() >= super::ladder::MAX_RUNGS {
            return Err(EditError::new("LADDER_RUNGS", format!("A ladder has 1 to {} branches.", super::ladder::MAX_RUNGS)));
        }
        for (i, r) in a.branches.iter().enumerate() {
            super::ladder::check_rung(i, r, false)?;
        }
        if !a.walk.is_empty() {
            if a.spot || a.otherwise_hidden {
                return Err(EditError::new("WALK_STANDS", "Only a copy that stands at its spot can walk from it; an examine spot or a copy hidden otherwise can't."));
            }
            super::walk::check(&a.walk)?;
        }
        if a.x == 0x7FFF || a.x == 10000 {
            return Err(EditError::new("PLACE_RESERVED_X", format!("x = {} reads as a branch marker in game; move one unit over.", a.x)));
        }
        if a.dir > 4095 {
            return Err(EditError::new("PLACE_DIR", format!("Facing {} is out of range (0–4095).", a.dir)));
        }
    }
    let fit = room(retail)?;
    if added.len() > fit {
        return Err(EditError::new(
            "ACTOR_ROOM",
            format!("This pack has room for {fit} added actor{} (16 bytes each, where its trigger table 2 was).", if fit == 1 { "" } else { "s" }),
        ));
    }
    let npcs = r.actors.iter().filter(|a| !is_object(a.d)).count() + added.iter().filter(|a| !a.spot).count();
    if npcs > NPC_SLOTS {
        return Err(EditError::new(
            "ACTOR_SLOTS",
            format!("A pack can have {NPC_SLOTS} characters (actors that aren't examine spots), taken-out retail ones included; this would make {npcs}. Use a pack with fewer retail actors, or add an examine spot (no character slot)."),
        ));
    }
    Ok(())
}

/// The placement program of an added actor: its branches, its own rung and
/// the mark, padded to 4 bytes ([`PROGRAM_LEN`] without branches).
pub fn program(a: &AddedActor) -> Vec<u8> {
    let mut words = Vec::new();
    for r in &a.branches {
        words.push(0xA000 | r.cond);
        words.extend_from_slice(&r.words);
    }
    let (x, h, z) = (a.x as u16, a.h as u16, a.z as u16);
    if a.otherwise_hidden {
        words.extend_from_slice(&[0xA000, 0x7FFF, x, h, z, a.dir]);
    } else {
        words.extend_from_slice(&[0xA000, x, h, z, a.dir]);
        words.extend_from_slice(if a.walk.is_empty() { &super::walk::STAND } else { &a.walk });
    }
    match a.house {
        Some(area) => words.extend_from_slice(&[MARK, a.from as u16 | HOUSE_BIT, area]),
        None => words.extend_from_slice(&[MARK, a.from as u16]),
    }
    if words.len() % 2 == 1 {
        words.push(0);
    }
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The branches before a copy's own `A000` at D-relative `at`, and where
/// that `A000` is; `None` when the words aren't a program [`program`] wrote.
fn branches_at(d: &[u8], at: usize) -> Option<(Vec<super::ladder::Rung>, usize)> {
    let word = |o: usize| (o + 2 <= d.len()).then(|| le16(d, o));
    let marker = |w: u16| w & 0xF000 == 0xA000;
    let mut out = Vec::new();
    let mut o = at;
    while out.len() < super::ladder::MAX_RUNGS {
        let w = word(o)?;
        if !marker(w) {
            return None;
        }
        if w == 0xA000 {
            return Some((out, o));
        }
        let mut rung = super::ladder::Rung { cond: w & 0x0FFF, words: Vec::new() };
        o += 2;
        while let Some(v) = word(o).filter(|&v| !marker(v)) {
            rung.words.push(v);
            o += 2;
            if rung.words.len() > 64 {
                return None;
            }
        }
        out.push(rung);
    }
    None
}

/// The record of an added actor: `source`'s script, model and type words
/// with `home` as the home chunk, placement at `at` (D-relative). `look`
/// is a brought-in character's (model, pose set, looks). An examine spot
/// keeps only the script word: no model or pose set, type 4.
pub fn record(source: [u8; 16], a: &AddedActor, at: u32, look: Option<(u8, u8, u16)>) -> [u8; 16] {
    let mut out = source;
    out[4..6].copy_from_slice(&a.home.to_le_bytes());
    out[8..12].copy_from_slice(&at.to_le_bytes());
    if a.spot {
        out[6] = 0xFF;
        out[7] = 0xFF;
        out[12..16].copy_from_slice(&if a.smoke { SMOKE_D } else { super::spot::RECORD_D }.to_le_bytes());
        return out;
    }
    if let Some((model, set, looks)) = look {
        out[6] = model;
        out[7] = set;
        out[12..14].copy_from_slice(&looks.to_le_bytes());
    }
    if a.hidden {
        hide(&mut out);
    }
    out
}

/// The actors `current` adds to `retail`, read back from their records and
/// programs.
pub fn recover(retail: &[u8], current: &[u8]) -> Result<Vec<AddedActor>> {
    let r = header(retail, "retail")?;
    let c = header(current, "project's")?;
    let cd = c.d_block(current);
    let script_of = |slot: usize| c.actors.get(slot).map(|a| a.script);
    c.actors
        .iter()
        .skip(r.actors.len())
        .map(|rec| {
            let found = branches_at(cd, rec.placement as usize);
            let (branches, at) = found.clone().unwrap_or_default();
            let w = |k: usize| cd.get(at + k * 2..at + k * 2 + 2).map(|_| le16(cd, at + k * 2));
            // Its own rung: `A000 x h z dir`, then its walk (`0000` standing still), or `A000 7FFF x h z
            // dir` (hidden otherwise); then the mark.
            let otherwise_hidden = w(1) == Some(0x7FFF);
            let walk: Vec<u16> = match otherwise_hidden {
                true => Vec::new(),
                false => {
                    let mut walk = Vec::new();
                    while let Some(v) = w(5 + walk.len()).filter(|_| walk.len() < super::walk::MAX_WORDS) {
                        walk.push(v);
                        if super::walk::ends(v) {
                            break;
                        }
                    }
                    walk
                }
            };
            let mark = if otherwise_hidden { 6 } else { 5 + walk.len() };
            let ended = otherwise_hidden || walk.last().is_some_and(|&v| super::walk::ends(v));
            let ours = found.is_some() && w(0) == Some(0xA000) && ended && w(mark) == Some(MARK);
            let walk = if walk == super::walk::STAND { Vec::new() } else { walk };
            let k = usize::from(otherwise_hidden);
            match (ours, w(1 + k), w(2 + k), w(3 + k), w(4 + k), w(mark + 1)) {
                (true, Some(x), Some(h), Some(z), Some(dir), Some(from)) => {
                    let house = (from & HOUSE_BIT != 0).then(|| w(mark + 2)).flatten();
                    let from = from & !HOUSE_BIT;
                    // A record pointing elsewhere than its source's program has one of its own. A copy
                    // of an examine spot the blank slate silenced runs the spot's retail program.
                    let silenced = |s: u32| super::remove::is_end(cd, s, r.d_span);
                    let retail_entry = r.actors.get(from as usize).map(|a| a.script);
                    let own = match script_of(from as usize) {
                        Some(s) if silenced(s) && retail_entry == Some(rec.script) => None,
                        Some(s) if s != rec.script => Some(super::script::lift_at(retail, current, rec.script)?),
                        _ => None,
                    };
                    // Hidden: the record has a director's looks. A copy of a retail director has
                    // them whether or not it was asked to hide, and is a director either way.
                    let hidden = looks_hidden(rec.b, rec.d);
                    // A model past the pack's own is a brought-in character.
                    let model = ((rec.b >> 16) & 0xFF) as usize;
                    let character = (!hidden && model != 0xFF && model >= r.group_a(retail)).then(|| (model - r.group_a(retail)) as u16);
                    // An examine spot: the record is an object (a copy of a retail one is, asked or not).
                    let spot = is_object(rec.d);
                    let smoke = spot && super::ladder::record_type(rec.d) == super::remove::SMOKE;
                    let (hidden, character) = if spot { (false, None) } else { (hidden, character) };
                    Ok(AddedActor { from: from as u32, home: rec.b as u16, x: x as i16, h: h as i16, z: z as i16, dir, hidden, own, character, spot, branches, otherwise_hidden, walk, house, smoke })
                }
                _ => Err(EditError::new("ACTOR_CHANGED", format!("Actor {} isn't one the editor added. Revert the pack.", rec.slot))),
            }
        })
        .collect()
}

/// `current` with exactly `added` actors; its text edits, edited programs and
/// added trigger records are laid out again with them ([`super::text::layout`]).
pub fn apply(retail: &[u8], current: &[u8], added: &[AddedActor]) -> Result<(Vec<u8>, super::text::TextReport)> {
    let mut set = super::pack::Edits::recover(retail, current)?;
    set.actors = added.to_vec();
    let (b, report, _) = super::text::layout(retail, current, &set)?;
    Ok((b, report))
}
