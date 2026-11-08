//! Tier-1 proximity triggers and placement ladders, both inside an NPC pack's
//! D-block. Layout: `docs/research/03-world-maps-events.md` §2.2–2.3.
//!
//! Trigger tables (`ScanFieldEventTriggers`, main 0x8018FFF8). Two of them: pass 0
//! right after the actor records' `s16 pass1`, pass 1 at `D + pass1`. The group
//! id is the chunk the Knight stands in; in a group only the first trigger whose
//! flag is set is active ("first true wins").
//!
//! ```text
//! s16 nGroups
//!   s16 chunk, s16 nTriggers
//!     u16 flag (0 = always), then records until s16 0x7FFF:
//!       type < 8   16-byte door  {x, z, facing, u16 to, ax, az, u16 flags, u8 pack, u8 music}
//!       type 8/9/10 8-byte op    {x, z, type, id}   8 set flag · 9 clear flag · 10 make actor `id` talk
//! ```
//!
//! Placement ladders (`WalkActor_SelectScript`, OVR02 0x801EAABC): a list of
//! `u16 0xA000 | cond` words, each followed by its body. The first condition
//! that holds wins: `CheckEventFlag(cond)` (0 = always), or for `cond >= 1000`
//! the inventory holds item `cond − 1000`. A failed condition skips words to the
//! next `0xAxxx` marker. Bodies: `0x7FFF` absent, `10000` present only on a
//! matching arrival, else `s16 x, h, z, u16 dir` (then the walk script).

use serde::Serialize;

/// D-relative little-endian reads that fail instead of panicking.
fn rd_u16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(at)?, *d.get(at + 1)?]))
}

fn rd_i16(d: &[u8], at: usize) -> Option<i16> {
    rd_u16(d, at).map(|v| v as i16)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TriggerRecord {
    /// Warp box: fires within 0x20 of (x, z).
    Door {
        at: u32,
        x: i16,
        z: i16,
        /// Facing on arrival (0–7, × 0x200).
        facing: i16,
        /// Destination chunk.
        to: u16,
        tx: i16,
        tz: i16,
        /// 0x8000 door SFX · 0x4000 remember as return point · 0x2000 forget it · low 12 bits area sound.
        flags: u16,
        /// NPC pack to load when the destination differs.
        pack: u8,
        music: u8,
    },
    /// Type 8: `SetEventFlag(flag)` (0xFD wipes flags 0–63).
    Set { at: u32, x: i16, z: i16, flag: u16 },
    /// Type 9: `ClearEventFlag(flag)`.
    Clear { at: u32, x: i16, z: i16, flag: u16 },
    /// Type 10: actor `actor` runs its Tier-2 script as if spoken to (scene start).
    Talk { at: u32, x: i16, z: i16, actor: u16 },
    /// Any other 8-byte type (none in retail).
    Other { at: u32, x: i16, z: i16, typ: i16, arg: i16 },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rung {
    /// D-relative offset of the flag word.
    pub at: u32,
    /// Condition flag; 0 = always.
    pub flag: u16,
    pub records: Vec<TriggerRecord>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerGroup {
    /// 0 = the table after the actor records, 1 = the table at `D + pass1`.
    pub pass: u8,
    /// D-relative offset of the group header.
    pub at: u32,
    /// Group id = the chunk the Knight is in.
    pub chunk: i16,
    pub rungs: Vec<Rung>,
}

const MAX_GROUPS: usize = 1024;
const MAX_RUNGS: usize = 1024;
const MAX_RECORDS: usize = 4096;

/// Parses one trigger table at D-relative `at`.
pub fn parse_table(d: &[u8], at: usize, pass: u8) -> Option<Vec<TriggerGroup>> {
    let mut o = at;
    let ngroups = rd_i16(d, o)?;
    if !(0..=MAX_GROUPS as i16).contains(&ngroups) {
        return None;
    }
    o += 2;
    let mut groups = Vec::with_capacity(ngroups as usize);
    for _ in 0..ngroups {
        let g_at = o as u32;
        let chunk = rd_i16(d, o)?;
        let nt = rd_i16(d, o + 2)?;
        if !(0..=MAX_RUNGS as i16).contains(&nt) {
            return None;
        }
        o += 4;
        let mut rungs = Vec::with_capacity(nt as usize);
        for _ in 0..nt {
            let r_at = o as u32;
            let flag = rd_u16(d, o)?;
            o += 2;
            let mut records = Vec::new();
            while rd_i16(d, o)? != 0x7FFF {
                if records.len() >= MAX_RECORDS {
                    return None;
                }
                let at = o as u32;
                let (x, z, typ) = (rd_i16(d, o)?, rd_i16(d, o + 2)?, rd_i16(d, o + 4)?);
                if typ < 8 {
                    let w = d.get(o..o + 16)?;
                    records.push(TriggerRecord::Door {
                        at,
                        x,
                        z,
                        facing: typ,
                        to: u16::from_le_bytes([w[6], w[7]]),
                        tx: i16::from_le_bytes([w[8], w[9]]),
                        tz: i16::from_le_bytes([w[10], w[11]]),
                        flags: u16::from_le_bytes([w[12], w[13]]),
                        pack: w[14],
                        music: w[15],
                    });
                    o += 16;
                } else {
                    let arg = rd_i16(d, o + 6)?;
                    records.push(match typ {
                        8 => TriggerRecord::Set { at, x, z, flag: arg as u16 },
                        9 => TriggerRecord::Clear { at, x, z, flag: arg as u16 },
                        10 => TriggerRecord::Talk { at, x, z, actor: arg as u16 },
                        _ => TriggerRecord::Other { at, x, z, typ, arg },
                    });
                    o += 8;
                }
            }
            o += 2;
            rungs.push(Rung { at: r_at, flag, records });
        }
        groups.push(TriggerGroup { pass, at: g_at, chunk, rungs });
    }
    Some(groups)
}

/// Bytes one trigger table at D-relative `at` takes (to its last 0x7FFF).
pub fn table_len(d: &[u8], at: usize) -> Option<usize> {
    let groups = parse_table(d, at, 0)?;
    let mut o = at + 2;
    for g in &groups {
        o += 4;
        for r in &g.rungs {
            o += 2 + r.records.iter().map(|x| if matches!(x, TriggerRecord::Door { .. }) { 16 } else { 8 }).sum::<usize>() + 2;
        }
    }
    Some(o - at)
}

/// Both trigger tables of a D-block with `actors` records (pass 0, then pass 1).
pub fn parse_tables(d: &[u8], actors: usize) -> Option<Vec<TriggerGroup>> {
    let tail = 4 + actors * 16;
    let pass1 = rd_i16(d, tail)?;
    let mut out = parse_table(d, tail + 2, 0)?;
    if pass1 > 0 {
        out.extend(parse_table(d, pass1 as usize, 1)?);
    }
    Some(out)
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Outcome {
    /// `0x7FFF`: the actor is not on the map in this state.
    Absent,
    /// `10000`: present only when the arrival matches.
    Arrival,
    /// Placed at (x, h, z) facing `dir` (0–4095).
    Place { x: i16, h: i16, z: i16, dir: u16 },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacementBranch {
    /// D-relative offset of the condition word.
    pub at: u32,
    /// `w & 0xFFF`: 0 always · 1–511 flag · 512–999 never · ≥ 1000 item `cond − 1000`.
    pub cond: u16,
    pub outcome: Outcome,
}

/// Words a ladder may span before the scan gives up (retail max is far below).
const LADDER_WORDS: usize = 256;

/// The condition ladder of a placement program at D-relative `at`, in the
/// order the game tries it, up to and including the first `always` branch.
pub fn placement_ladder(d: &[u8], at: u32) -> Vec<PlacementBranch> {
    let mut out = Vec::new();
    let start = at as usize;
    let mut o = start;
    let mut first = true;
    while o < start + LADDER_WORDS * 2 {
        let Some(w) = rd_u16(d, o) else { break };
        if first || w & 0xF000 == 0xA000 {
            first = false;
            let cond = w & 0x0FFF;
            let outcome = match rd_u16(d, o + 2) {
                Some(0x7FFF) => Outcome::Absent,
                Some(10000) => Outcome::Arrival,
                _ => Outcome::Place {
                    x: rd_i16(d, o + 2).unwrap_or(0),
                    h: rd_i16(d, o + 4).unwrap_or(0),
                    z: rd_i16(d, o + 6).unwrap_or(0),
                    dir: rd_u16(d, o + 8).unwrap_or(0),
                },
            };
            out.push(PlacementBranch { at: o as u32, cond, outcome });
            if cond == 0 {
                break;
            }
        }
        o += 2;
    }
    out
}

/* ───────────────────────── Code-side hotspots (read-only) ───────────────────────── */

/// A hard-coded proximity box in the main EXE
/// (`docs/research/03-world-maps-events.md` §2.4). Both kinds fire within
/// ±0x40 of (x, z) and are not part of any NPC pack.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Hotspot {
    /// `FUN_801906bc`: code `0x3nnnn` spawns OVR02 nested script `nnnn`
    /// (`D_801E7FF4`); `0x2nnnn` sets the field task state.
    Script { addr: u32, x: i16, z: i16, code: u32 },
    /// `Field_TryTriggerProximityDialogue` (`FUN_8019040c`): warp record
    /// `index` of `D_801B7208` ({facing, to, x, z, flags}).
    Warp { addr: u32, x: i16, z: i16, index: u32, facing: i16, to: u16, tx: i16, tz: i16, flags: u16 },
}

/// Half-width of a hotspot's box (|Δx|, |Δz| ≤ 0x40).
pub const HOTSPOT_REACH: i16 = 0x40;

/// `{s16 x, s16 z, u32 code}` tables by chunk (`FUN_801906bc`).
const SCRIPT_TABLES: [(u32, u32); 4] = [(0x27, 0x801B_7188), (0x2B, 0x801B_7178), (0x10C, 0x801B_71E8), (0x56, 0x801B_71F8)];
/// `{s16 x, s16 z, u32 index}` tables by chunk (`FUN_8019040c`).
const WARP_TABLES: [(u32, u32); 4] = [(0x50, 0x801B_7198), (0x141, 0x801B_71A8), (0xBB, 0x801B_71B8), (0xBC, 0x801B_71D0)];
/// 10-byte warp records `{s16 facing, u16 to, s16 x, s16 z, u16 flags}`.
const WARP_RECORDS: u32 = 0x801B_7208;
const MAX_HOTSPOTS: u32 = 16;

/// The EXE's hard-coded hotspots on `chunk` (empty for most chunks, or when
/// the EXE doesn't cover the tables).
pub fn hotspots(exe: &crate::PsxExe, chunk: u32) -> Vec<Hotspot> {
    let mut out = Vec::new();
    let entries = |base: u32| -> Vec<(u32, i16, i16, u32)> {
        (0..MAX_HOTSPOTS)
            .map(|i| base + i * 8)
            .map_while(|a| {
                let w = exe.slice(a, 8).ok()?;
                let x = i16::from_le_bytes([w[0], w[1]]);
                (x != 0x7FFF).then(|| (a, x, i16::from_le_bytes([w[2], w[3]]), u32::from_le_bytes([w[4], w[5], w[6], w[7]])))
            })
            .collect()
    };
    for (_, base) in SCRIPT_TABLES.iter().filter(|(c, _)| *c == chunk) {
        out.extend(entries(exe.va(*base)).into_iter().map(|(addr, x, z, code)| Hotspot::Script { addr, x, z, code }));
    }
    for (_, base) in WARP_TABLES.iter().filter(|(c, _)| *c == chunk) {
        for (addr, x, z, index) in entries(exe.va(*base)) {
            let Ok(w) = exe.slice(exe.va(WARP_RECORDS) + index * 10, 10) else { continue };
            let h = |o: usize| u16::from_le_bytes([w[o], w[o + 1]]);
            out.push(Hotspot::Warp { addr, x, z, index, facing: h(0) as i16, to: h(2), tx: h(4) as i16, tz: h(6) as i16, flags: h(8) });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_ladder_and_a_door() {
        // 1 group (chunk 326), 2 rungs: flag 0x1A → talk actor 3; always → door to chunk 324.
        let mut d = vec![0u8; 0];
        let w = |d: &mut Vec<u8>, v: i16| d.extend_from_slice(&v.to_le_bytes());
        w(&mut d, 1);
        w(&mut d, 326);
        w(&mut d, 2);
        w(&mut d, 0x1A);
        for v in [-31, -33, 10, 3] {
            w(&mut d, v);
        }
        w(&mut d, 0x7FFF);
        w(&mut d, 0);
        for v in [-800, -304, 0, 324, -816, 432, 0x4000, 0x0807] {
            w(&mut d, v);
        }
        w(&mut d, 0x7FFF);
        let g = parse_table(&d, 0, 0).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].chunk, 326);
        assert_eq!(g[0].rungs[0].flag, 0x1A);
        assert_eq!(g[0].rungs[0].records[0], TriggerRecord::Talk { at: 8, x: -31, z: -33, actor: 3 });
        match &g[0].rungs[1].records[0] {
            TriggerRecord::Door { to, tx, tz, flags, pack, music, .. } => {
                assert_eq!((*to, *tx, *tz, *flags, *pack, *music), (324, -816, 432, 0x4000, 7, 8));
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn ladder_stops_at_always() {
        let words: [u16; 9] = [0xA081, 0x7FFF, 0xA004, 10, 20, 30, 0x400, 0xA000, 5];
        let d: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let l = placement_ladder(&d, 0);
        assert_eq!(l.iter().map(|b| b.cond).collect::<Vec<_>>(), vec![0x81, 4, 0]);
        assert_eq!(l[0].outcome, Outcome::Absent);
        assert_eq!(l[1].outcome, Outcome::Place { x: 10, h: 20, z: 30, dir: 0x400 });
    }
}
