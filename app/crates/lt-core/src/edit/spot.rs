//! Examine spots the project adds (screen 10, LT-57): a bed to rest at, a
//! sign, a chest, anything the Knight examines with ✕. A spot is an added
//! record ([`super::actor::AddedActor`] with `spot`) of record type 4, after
//! the retail records like an added actor:
//!
//! - **Placed** by the spawn loop (OVR02 `0x801E8E3C`, case 1), which takes
//!   type 4/5 records out of the actor list and compacts them into an object
//!   table over the records: home chunk (`b` low u16), type, script, and x,
//!   h, z from words 1–3 of the placement, offset to the current chunk the
//!   way an actor's are (`WorldChunk_ComputeActorGridOffset`). An added
//!   actor's placement program `A000 x h z dir 0000 "LT" from` has them in
//!   the same words, as every retail spot's does, so a spot uses it
//!   unchanged. The game never reads `dir`; for a bed it is the side the
//!   Knight wakes on ([`wake`]).
//! - **No character slot.** Only records that aren't objects get one of the
//!   80 NPC structures ([`super::actor::NPC_SLOTS`]). A spot takes the 16
//!   bytes of a record where trigger table 2 was ([`super::actor::room`]),
//!   its 16-byte placement and its program.
//! - **Examined** in `CheckNpcAtPosition` (`main_801897ac.c`): on ✕, the
//!   point 48 units in front of the Knight within 64 of a spot whose home is
//!   the current chunk and whose type isn't 5 (a torch flame). Its script then
//!   runs as the Knight's (`KN->f28`), so it has no actor of its own to turn
//!   or animate. The Knight reaches a spot from walkable ground up to
//!   [`REACH`] units away.
//!
//! [`Preset`]s give a new spot its program: a bed (asks, then INN on the
//! spot's chunk: the rest, the heal and the memory-card save, waking on the
//! chosen ground), a sign (one line) and a chest (CHEST: the item once, with
//! a flag of its own; examined again, the game says it's empty).

use serde::{Deserialize, Serialize};

use crate::tier2::ast::Stmt;
use crate::world::walk::{cell_of, Walk};
use crate::tier2::dsl::{self, Severity};

use super::{EditError, Result};

/// Record `d` of an added spot: type 4 (byte 13), byte 12 = 2 as most of
/// retail's 2,481 spots have (the spawn loop doesn't copy it).
pub const RECORD_D: u32 = 0x0000_0402;
/// How far from a spot the Knight can stand and still examine it: his 48-unit
/// reach plus the 64-unit radius.
pub const REACH: i32 = 48 + 64;
/// How far in front of a bed (along its facing) the Knight wakes, before
/// snapping to a point he can stand on.
pub const WAKE_AHEAD: i32 = 64;
/// INN's last operand, as every retail bed has it.
const INN_F: u8 = 10;

/// What a new spot does when examined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Preset {
    /// Rest and save: asks, then INN on the spot's chunk (or `wake_chunk`), waking at (`wake_x`, `wake_z`).
    #[serde(rename_all = "camelCase")]
    Bed {
        wake_x: i16,
        wake_z: i16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake_chunk: Option<u16>,
    },
    /// One line (dialogue markup).
    Sign { line: String },
    /// `item` once: CHEST sets `flag` when it's taken, and says it's empty after.
    Chest { item: u16, flag: u16 },
    /// Says `line`, then opens shop `shop` (opcode 0x07 only sets the game mode, whoever runs it: research 13 §6).
    Shop { shop: u8, line: String },
}

impl Preset {
    /// What the editor calls a spot made from it.
    pub fn label(&self) -> &'static str {
        match self {
            Preset::Bed { .. } => "Bed",
            Preset::Sign { .. } => "Sign",
            Preset::Chest { .. } => "Chest",
            Preset::Shop { .. } => "Shop",
        }
    }
}

/// A DSL string literal holding `markup`.
fn quoted(markup: &str) -> String {
    let mut s = String::with_capacity(markup.len() + 2);
    s.push('"');
    for c in markup.chars() {
        if c == '"' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

/// The program of a spot made from `preset` on chunk `home`, as screen 11's DSL.
pub fn dsl(preset: &Preset, home: u16) -> String {
    match preset {
        Preset::Bed { wake_x, wake_z, wake_chunk } => format!(
            "say3 {}\nif yes {{\n  say {}\n  inn chunk/{} x:{wake_x} z:{wake_z} f:{INN_F}\n}}\n",
            quoted("A soft bed.{nl}Do you want to{nl}get some rest?"),
            quoted("Goodnight, $A."),
            wake_chunk.unwrap_or(home),
        ),
        Preset::Sign { line } => format!("say {}\n", quoted(line)),
        Preset::Chest { item, flag } => format!("say {}\nchest flag:0x{flag:03X} item:{item}\n", quoted("Searching the chest...")),
        Preset::Shop { shop, line } => format!("say {}\nshop {shop}\n", quoted(line)),
    }
}

/// [`dsl`] parsed: the statements a new spot's own program starts as.
pub fn program(preset: &Preset, home: u16) -> Result<Vec<Stmt>> {
    if let Preset::Sign { line } | Preset::Shop { line, .. } = preset {
        if line.trim().is_empty() {
            return Err(EditError::new("SPOT_EMPTY_LINE", "A sign or a shop needs a line to show."));
        }
        super::text::parse_markup(line)?;
    }
    if let Preset::Chest { flag, .. } = preset {
        if *flag == 0xF8 {
            return Err(EditError::new("SPOT_CHEST_FLAG", "Flag 0x0F8 never sets, so the chest would give its item every time. Pick a free flag."));
        }
    }
    let (stmts, diags) = dsl::parse(&dsl(preset, home));
    match diags.into_iter().find(|d| d.severity == Severity::Error) {
        Some(d) => Err(EditError::new("SPOT_PRESET", format!("The {} preset doesn't parse: {}", preset.label().to_lowercase(), d.message))),
        None => Ok(stmts),
    }
}

/// Points 16 units apart within [`REACH`] of (`x`, `z`) (strictly, as the
/// game's radius test is) where the Knight's centre can stand: floor he can walk
/// on, kept 32 units from walls by the game's probe ([`crate::world::walk`]). Wall
/// tops and gaps never count, though a spot itself may sit on furniture or a wall:
/// of retail's 2,424 type-4 spots only 183 are on floor cells (104 on wall tops,
/// 2,137 on blocked cells), and 2,422 have standing room within reach.
fn standing_near(walk: &Walk, x: i32, z: i32) -> impl Iterator<Item = (i32, i32)> + '_ {
    // The standing lattice's own points (multiples of 16), where its answers are exact.
    let span = move |c: i32| ((c - REACH).div_euclid(16) * 16..=c + REACH).step_by(16);
    span(z).flat_map(move |pz| span(x).map(move |px| (px, pz))).filter(move |&(px, pz)| (px - x).pow(2) + (pz - z).pow(2) < REACH * REACH && walk.can_stand(px, pz))
}

/// Where the Knight can examine a spot at (`x`, `z`) from: the nearest point he
/// can stand on within [`REACH`], and the floor height there (the height a spot
/// gets). `None` when there is none, so he couldn't examine it.
pub fn reach(walk: &Walk, x: i32, z: i32) -> Option<((i16, i16), i16)> {
    standing_near(walk, x, z).min_by_key(|&(px, pz)| (px - x).pow(2) + (pz - z).pow(2)).map(|(px, pz)| {
        let h = cell_of(px, pz).map_or(-1, |(c, r)| walk.height(c, r));
        ((px as i16, pz as i16), h)
    })
}

/// Where the Knight wakes after resting at a bed at (`x`, `z`) facing `dir`
/// (0–4095, x = sin, z = cos as the game turns): the point he can stand on,
/// within [`REACH`] of the bed, nearest to [`WAKE_AHEAD`] units in front of it.
pub fn wake(walk: &Walk, x: i32, z: i32, dir: u16) -> Option<(i16, i16)> {
    let a = dir as f64 * std::f64::consts::TAU / 4096.0;
    let (ax, az) = (x + (WAKE_AHEAD as f64 * a.sin()).round() as i32, z + (WAKE_AHEAD as f64 * a.cos()).round() as i32);
    standing_near(walk, x, z).min_by_key(|&(px, pz)| ((px - ax).pow(2) + (pz - az).pow(2), (px - x).pow(2) + (pz - z).pow(2))).map(|(px, pz)| (px as i16, pz as i16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier2::ast::Val;

    /// @LT-57/2: the bed, sign and chest presets parse into the programs they name.
    #[test]
    fn presets_parse() {
        let bed = program(&Preset::Bed { wake_x: -32, wake_z: -32, wake_chunk: None }, 1330).unwrap();
        let ops = |s: &[Stmt]| crate::tier2::ast::flatten(s).iter().filter_map(|f| if let crate::tier2::ast::Flat::Op { op, .. } = f { Some(*op) } else { None }).collect::<Vec<u8>>();
        let b = ops(&bed);
        assert!(b.contains(&0x03) && b.contains(&0x0A) && b.contains(&0x06), "{b:x?}");
        assert!(dsl(&Preset::Bed { wake_x: -32, wake_z: -32, wake_chunk: None }, 1330).contains("inn chunk/1330 x:-32 z:-32 f:10"));
        assert!(dsl(&Preset::Bed { wake_x: -448, wake_z: -944, wake_chunk: Some(78) }, 1330).contains("inn chunk/78 x:-448 z:-944 f:10"));
        let sign = program(&Preset::Sign { line: "North: the \"Shrine\"{nl}South: home".into() }, 78).unwrap();
        assert_eq!(ops(&sign)[0], 0x01);
        let chest = program(&Preset::Chest { item: 9, flag: 0x214 }, 78).unwrap();
        let flat = crate::tier2::ast::flatten(&chest);
        let args = flat.iter().find_map(|f| match f {
            crate::tier2::ast::Flat::Op { op: 0x28, args, .. } => Some(args.to_vec()),
            _ => None,
        });
        assert_eq!(args, Some(vec![Val::Num(0x214), Val::Num(9)]));
        assert_eq!(program(&Preset::Sign { line: "  ".into() }, 78).unwrap_err().code, "SPOT_EMPTY_LINE");
        assert_eq!(program(&Preset::Chest { item: 9, flag: 0xF8 }, 78).unwrap_err().code, "SPOT_CHEST_FLAG");
    }

    /// A room of floor (−33) walled by a thick block of wall tops (−225) and a gap
    /// of void, like a copied interior: walls and gaps don't count as standing room.
    fn room() -> Walk {
        let mut h = vec![-1i16; 128 * 128];
        for row in 40..88 {
            for col in 40..88 {
                h[row * 128 + col] = if (52..76).contains(&row) && (52..64).contains(&col) { -33 } else { -225 };
            }
        }
        Walk::new(&h, false, &[(at(58) as i16, at(64) as i16)])
    }

    const fn at(cell: i32) -> i32 {
        cell * 32 - 2048 + 16
    }

    /// The reach and wake helpers use the walk map (LT-58): a spot on the wall beside
    /// the floor is reachable, one deep in the wall block or the void isn't.
    #[test]
    fn reach_and_wake_follow_the_walk_map() {
        let w = room();
        // On the wall face one cell east of the floor (cells 52–63): reachable from the floor.
        let ((sx, _), h) = reach(&w, at(64), at(60)).expect("beside the floor");
        assert_eq!(h, -33);
        assert!((sx as i32) < at(64), "he stands on the floor side");
        // Ten cells into the wall block, and out in the void: nothing within 112.
        assert_eq!(reach(&w, at(74), at(60)), None);
        assert_eq!(reach(&w, at(100), at(60)), None);
        // A bed on the wall face facing west (3072) wakes the Knight on the floor in front of it.
        let (wx, wz) = wake(&w, at(64), at(60), 3072).unwrap();
        assert!(w.can_stand(wx as i32, wz as i32));
        assert!((wx as i32) < at(64) && (wz as i32 - at(60)).abs() <= 16, "({wx}, {wz})");
        assert_eq!(wake(&w, at(74), at(60), 3072), None);
    }
}
