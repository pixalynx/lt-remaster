//! Who stands on a chunk: an NPC pack's actor roster with each actor's
//! placement program decoded for the World editor (`docs/screens/10-world-editor.md`
//! §4.5, §5.4). Trigger tables come from [`crate::trigger`].
//!
//! Placement program (u16 words at D + record.c, `WalkActor_SelectScript`
//! OVR02 0x801EAABC):
//!
//! ```text
//! 0xA000 | cond     0 always · 1–511 CheckEventFlag · 512–999 never · ≥ 1000 item cond − 1000
//!   (false)         skip to the next word with (w & 0xF000) == 0xA000
//!   0x7FFF          absent in this state
//!   10000           present only when the arrival chunk matches
//!   x, h, z, dir    placed (s16 ×3, u16 0–4095); the cursor continues as the walk script
//! ```
//!
//! Walk words (`WalkActor_RunScript` 0x801EA540): 0nnn wait for flag n (0000 =
//! stop), 1nnn heading n·0x200, 2nnn walk n steps, 3nnn animation n, 4000 loop,
//! 6nnn/8nnn speed −n/n, 7nnn leave if flag n, 9000 auto-talk, Bnnn re-place
//! if flag n, C/D/Ennn turn; an 0xAxxx word is the next branch.

use serde::Serialize;

use crate::container::{le16, le32};
use crate::npc::{ActorLook, ActorRecord, NpcPack};
use crate::text;
use crate::tier2::{self, Operand};
use crate::trigger::{self, TriggerGroup, TriggerRecord};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Body {
    Absent,
    Arrival,
    Place { x: i16, h: i16, z: i16, dir: u16 },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Branch {
    /// D-relative offset of the condition word.
    pub at: u32,
    /// `w & 0xFFF` (0 = always).
    pub cond: u16,
    pub body: Body,
    /// Walk-script words after a placement (up to the next branch marker).
    pub walk: Vec<u16>,
}

const MAX_WORDS: usize = 512;
const MAX_WALK: usize = 64;

fn word(d: &[u8], at: usize) -> Option<u16> {
    (at + 2 <= d.len()).then(|| le16(d, at))
}

/// The branches of the program at D-relative `at`, in the order the game tries
/// them, ending with the first unconditional one (there is no terminator).
pub fn decode(d: &[u8], at: u32) -> Vec<Branch> {
    let mut out = Vec::new();
    let mut o = at as usize;
    let end = o + MAX_WORDS * 2;
    while o < end {
        let Some(w) = word(d, o) else { break };
        if w & 0xF000 != 0xA000 {
            if out.is_empty() {
                break; // not a program
            }
            o += 2;
            continue;
        }
        let cond = w & 0x0FFF;
        let b = o + 2;
        let (body, walk_at) = match word(d, b) {
            Some(0x7FFF) => (Body::Absent, None),
            Some(10000) => (Body::Arrival, None),
            Some(_) if b + 8 <= d.len() => (
                Body::Place { x: le16(d, b) as i16, h: le16(d, b + 2) as i16, z: le16(d, b + 4) as i16, dir: le16(d, b + 6) },
                Some(b + 8),
            ),
            _ => break,
        };
        let mut walk = Vec::new();
        if let Some(mut p) = walk_at {
            while walk.len() < MAX_WALK {
                let Some(v) = word(d, p) else { break };
                if v & 0xF000 == 0xA000 {
                    break;
                }
                walk.push(v);
                p += 2;
                if v == 0 || v == 0x4000 {
                    break;
                }
            }
        }
        out.push(Branch { at: o as u32, cond, body, walk });
        if cond == 0 {
            break;
        }
        o = b;
    }
    out
}

/// The first `0x3nnn` (set animation) word of a walk script.
pub fn walk_anim(walk: &[u16]) -> Option<u16> {
    walk.iter().find(|&&w| w & 0xF000 == 0x3000).map(|w| w & 0xFFF)
}

/// How a walk word reads an event flag (`WalkActor_RunScript`).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WalkRead {
    /// `0nnn`: wait on this word until flag n is set (a scene cue).
    Wait,
    /// `7nnn`: off the map while flag n is set.
    Hide,
    /// `Bnnn`: once flag n is set, run the ladder again (re-place); the walk restarts either way.
    Replace,
}

/// The flag a walk word reads. n = 0 is no read: `CheckEventFlag(0)` is always
/// true, so `0000` stops, `7000` leaves and `B000` re-places unconditionally.
pub fn walk_read(w: u16) -> Option<(WalkRead, u16)> {
    let read = match w & 0xF000 {
        0x0000 => WalkRead::Wait,
        0x7000 => WalkRead::Hide,
        0xB000 => WalkRead::Replace,
        _ => return None,
    };
    let n = w & 0x0FFF;
    (n != 0).then_some((read, n))
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WalkFlag {
    /// D-relative offset of the walk word.
    pub at: u32,
    /// Condition of the branch that walks (0 = always).
    pub cond: u16,
    pub read: WalkRead,
    pub flag: u16,
}

/// Flag reads in the walks of the placement program at `at`, for an actor record
/// of type `typ` (record `d` byte 1). A placed branch walks after its x, h, z, dir;
/// a type-3 actor's absent branch also walks (an exit) after x, h, z, dir of its
/// own. An arrival body (`10000`) carries x, h, z, dir too, but the words after it
/// feed the arrival camera, so it is skipped. A walk ends at `0000`, `4000`, a
/// `Bnnn` (it restarts) or the next branch marker.
pub fn walk_flags(d: &[u8], at: u32, typ: u8) -> Vec<WalkFlag> {
    let mut out = Vec::new();
    for br in decode(d, at) {
        let start = match br.body {
            Body::Place { .. } => br.at as usize + 10,
            Body::Absent if typ == 3 => br.at as usize + 12,
            _ => continue,
        };
        for p in (start..start + MAX_WALK * 2).step_by(2) {
            let Some(w) = word(d, p).filter(|w| w & 0xF000 != 0xA000) else { break };
            if let Some((read, flag)) = walk_read(w) {
                out.push(WalkFlag { at: p as u32, cond: br.cond, read, flag });
            }
            if w == 0 || w == 0x4000 || w & 0xF000 == 0xB000 {
                break;
            }
        }
    }
    out
}

/// Evaluates a ladder for a flag state: the first branch whose condition holds
/// (`cond 0` always; 512–999 never; ≥ 1000 needs an item, treated as not held).
pub fn active<'a>(branches: &'a [Branch], flag: &dyn Fn(u16) -> bool) -> Option<&'a Branch> {
    branches.iter().find(|b| match b.cond {
        0 => true,
        1..=511 => flag(b.cond),
        _ => false,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterActor {
    pub slot: u32,
    /// First `SPEAKER` (0x1F) name in the actor's script.
    pub speaker: Option<String>,
    /// Slots 0 and 1 (Knight, Baby).
    pub party: bool,
    /// Record `b` byte 3 = 0xFF: a static object (`WalkActor_SelectScript` +0x16 test).
    pub object: bool,
    /// Record `b` low u16: the home chunk (0 = follows the Knight).
    pub home: u16,
    /// Record `b` byte 2: the Group A model (`Character_Struct+0x16`).
    pub model: u8,
    /// Model, pose set and draw kind as the field reads them.
    pub look: ActorLook,
    /// NPC packet slots it takes while on screen ([`packet_slots`]).
    pub packet_slots: u8,
    /// D-relative talk script (record `a`).
    pub script: u32,
    /// D-relative placement program (record `c`).
    pub program: u32,
    /// Other slots whose record `c` points at the same program.
    pub shared_with: Vec<u32>,
    pub branches: Vec<Branch>,
    /// Animation of the first placed branch's walk script.
    pub walk_anim: Option<u16>,
    /// SAY lines (opcodes 0x01–0x03) in the script, to its END.
    pub lines: u32,
}

/// NPCs the field draws at once. `Npc_AllocPacketSlot` hands out seven model packet slots
/// (0x800F5000 + (i − 2)·0x5A00), the first two always the Knight's and the Baby's. A character
/// on screen that finds none stands there undrawn until a drawn one leaves the screen and frees
/// its slot; at a chunk's load the first ones by record order get them.
pub const DRAWN_AT_ONCE: u8 = 5;

/// Packet slots record `a` takes while on screen: none for the party (slots of their own), with
/// no model (`NPC_UpdateActors` skips the draw) or for small objects (kind 2 and up take one of
/// four slots at 0x80142600); else one, and a big model (kind bit 0x20) one more per 0x5A00 its
/// model builds. Kind 1, drawn only once a scene shows it, takes its slot while unseen.
pub fn packet_slots(pack: &NpcPack, b: &[u8], a: &ActorRecord) -> u8 {
    let look = ActorLook::of(a);
    let Some(model) = look.model else { return 0 };
    if a.slot < 2 || look.kind & 0xF >= 2 {
        return 0;
    }
    if look.kind & crate::edit::character::BIG_MODEL == 0 {
        return 1;
    }
    let built = pack.model(b, model as usize).and_then(|t| crate::edit::tmdcodec::Tmd::parse(t).ok()).map_or(0, |t| t.sizes().built);
    1 + (built / crate::edit::character::PACKET_SLOT) as u8
}

/// Every actor record of a pack, decoded.
pub fn roster(pack: &NpcPack, b: &[u8]) -> Vec<RosterActor> {
    let d = pack.d_block(b);
    pack.actors
        .iter()
        .map(|a| {
            let branches = decode(d, a.placement);
            let walk_anim = branches.iter().find(|br| matches!(br.body, Body::Place { .. })).and_then(|br| walk_anim(&br.walk));
            let (speaker, lines) = script_summary(d, a.script);
            RosterActor {
                slot: a.slot,
                speaker,
                party: a.slot < 2,
                object: a.b >> 24 == 0xFF,
                home: (a.b & 0xFFFF) as u16,
                model: (a.b >> 16) as u8,
                look: ActorLook::of(a),
                packet_slots: packet_slots(pack, b, a),
                script: a.script,
                program: a.placement,
                shared_with: pack.actors.iter().filter(|o| o.slot != a.slot && o.placement == a.placement).map(|o| o.slot).collect(),
                branches,
                walk_anim,
                lines,
            }
        })
        .collect()
}

/// First speaker name and SAY-line count of the script at `entry`.
fn script_summary(d: &[u8], entry: u32) -> (Option<String>, u32) {
    let Ok(code) = tier2::disassemble(d, entry) else { return (None, 0) };
    let mut speaker = None;
    let mut lines = 0;
    for i in &code {
        for o in &i.operands {
            match *o {
                Operand::Name(t) if speaker.is_none() => {
                    let s = text::plain(&text::decode(text::c_string(d, t as usize)));
                    if !s.trim().is_empty() {
                        speaker = Some(s.trim().to_string());
                    }
                }
                Operand::Text(_) if (0x01..=0x03).contains(&i.op) => lines += 1,
                _ => {}
            }
        }
    }
    (speaker, lines)
}

/* ───────────────────────── Triggers per chunk ───────────────────────── */

/// Trigger groups of a pack (both passes), or empty when they don't parse.
pub fn trigger_groups(pack: &NpcPack, b: &[u8]) -> Vec<TriggerGroup> {
    trigger::parse_tables(pack.d_block(b), pack.actors.len()).unwrap_or_default()
}

fn is_door(r: &TriggerRecord) -> bool {
    matches!(r, TriggerRecord::Door { .. })
}

/// Door records over every group and rung of a pack.
pub fn door_total(groups: &[TriggerGroup]) -> u32 {
    groups.iter().flat_map(|g| &g.rungs).flat_map(|r| &r.records).filter(|r| is_door(r)).count() as u32
}

/// Doors active on `chunk`: per table, the first rung whose flag holds
/// (`ScanFieldEventTriggers`: flag 0 = always). `None` if the pack has no group
/// for the chunk.
pub fn doors_on(groups: &[TriggerGroup], chunk: u32, flag: &dyn Fn(u16) -> bool) -> Option<u32> {
    let mut found = false;
    let mut n = 0;
    for g in groups.iter().filter(|g| g.chunk as i32 == chunk as i32) {
        found = true;
        if let Some(r) = g.rungs.iter().find(|r| r.flag == 0 || flag(r.flag)) {
            n += r.records.iter().filter(|r| is_door(r)).count() as u32;
        }
    }
    found.then_some(n)
}

/// Trigger records (doors, flag ops, talk starts) active on `chunk`.
pub fn records_on(groups: &[TriggerGroup], chunk: u32, flag: &dyn Fn(u16) -> bool) -> u32 {
    groups
        .iter()
        .filter(|g| g.chunk as i32 == chunk as i32)
        .filter_map(|g| g.rungs.iter().find(|r| r.flag == 0 || flag(r.flag)))
        .map(|r| r.records.len() as u32)
        .sum()
}

/// Raw D-record count without a full parse (`u32` at D).
pub fn record_count(b: &[u8]) -> Option<u32> {
    let d = le32(b.get(4..8)?, 0) as usize;
    (d + 4 <= b.len()).then(|| le32(b, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_ladder_with_walk_words() {
        let words: [u16; 14] = [0xA081, 0x7FFF, 0xA004, 10, 20, 30, 0x400, 0x3005, 0x8005, 0x206E, 0x4000, 0xA000, 0x7FFF, 0xBEEF];
        let d: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let b = decode(&d, 0);
        assert_eq!(b.iter().map(|b| b.cond).collect::<Vec<_>>(), vec![0x81, 4, 0]);
        assert_eq!(b[0].body, Body::Absent);
        assert_eq!(b[1].body, Body::Place { x: 10, h: 20, z: 30, dir: 0x400 });
        assert_eq!(b[1].walk, vec![0x3005, 0x8005, 0x206E, 0x4000]);
        assert_eq!(walk_anim(&b[1].walk), Some(5));
        assert_eq!(active(&b, &|f| f == 4).map(|b| b.cond), Some(4));
        assert_eq!(active(&b, &|_| false).map(|b| b.cond), Some(0));
    }

    #[test]
    fn finds_walk_flag_reads() {
        // if 0x16: placed; wait 0x18, walk, hide while 0x19, re-place on 0x1A (restart: 0x1B is dead).
        // if 0x81: absent, with an exit walk for type 3 waiting on 0x1C. Else arrival (camera words).
        let words: [u16; 26] = [
            0xA016, 1, 2, 3, 0, 0x0018, 0x2010, 0x7019, 0xB01A, 0x001B, //
            0xA081, 0x7FFF, 1, 2, 3, 0, 0x001C, 0x0000, //
            0xA000, 10000, 1, 2, 3, 0, 0x0038, 0x0000,
        ];
        let d: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let reads = |typ| walk_flags(&d, 0, typ).into_iter().map(|w| (w.at, w.cond, w.read, w.flag)).collect::<Vec<_>>();
        let placed = vec![(10, 0x16, WalkRead::Wait, 0x18), (14, 0x16, WalkRead::Hide, 0x19), (16, 0x16, WalkRead::Replace, 0x1A)];
        assert_eq!(reads(0), placed);
        assert_eq!(reads(3), [placed, vec![(32, 0x81, WalkRead::Wait, 0x1C)]].concat());
        assert_eq!(walk_read(0x0000), None);
        assert_eq!(walk_read(0xB000), None);
    }
}
