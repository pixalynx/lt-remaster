//! Encounter and battle tables (PRD §4.9, research/04 §2.7).
//!
//! Table T lives in `ghost.npc`: SYS11 = `sys/9` (T at header word 8, file
//! 0x8FC8) with a byte-identical mirror in SYS12 = `sys/10` (0x46AC). The field
//! loader puts it at 0x80147FD0 and the game reads T through `*(u8**)0x80147FF0`.
//!
//! | T offset | records | layout |
//! |---|---|---|
//! | +0x000 | 128 × 5 B by species | [0..3] group sizes (unverified), [4] level for the ghost rating |
//! | +0x280 | 244 × 13 B by area 0x00–0xF3 | 5 × `{u8 a, u8 b}` (b 0 = solo), +0xA arena (`bf/n`), +0xB field BGM (`bgm/n`), +0xC battle area id |
//! | +0xF80 | 64 × 9 B, battle ids 0x100–0x13F | `{species, count}` × 3, arena, flags, area id |
//! | +0x11C0 | tail | unknown, preserved |

use serde::Serialize;

use crate::container::{le16, le32};

use super::modules::area_module;
use super::{bad, Result, Source};

pub const PRIMARY: &str = "sys/9";
pub const MIRROR: &str = "sys/10";
pub const SPECIES_COUNT: usize = 128;
pub const ZONE_COUNT: usize = 244;
pub const STORY_COUNT: usize = 64;
const ZONES_AT: usize = 0x280;
const STORY_AT: usize = 0xF80;
const TAIL_AT: usize = 0x11C0;
/// EXE `ScriptedBattleTable`: 2 × `{u16 map, u8 arena, u8 species, u32 area}`.
pub const SCRIPTED_TABLE: u32 = 0x801B_53A4;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeciesRec {
    pub id: u8,
    /// Bytes 0..3: solo min/max, paired min/max (**unverified**).
    pub group: [u8; 4],
    /// Byte 4: species level used to rate a ghost encounter.
    pub level: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Zone {
    /// Area (map) id 0x00–0xF3.
    pub id: u16,
    /// Five candidate pairs `[a, b]`; `b` 0 = solo, `a` 0 ends the list.
    pub pairs: [[u8; 2]; 5],
    /// Arena graphics = `bf/n`.
    pub arena: u8,
    /// +0xB: the zone's field music, `bgm/n` (`MapId_GetAreaId(id, 1)`; 0 = none).
    /// See `scene::area_bgm`.
    pub flags: u8,
    /// +0xC battle area id (0 → 7) → battle BGM.
    pub area: u8,
    /// Area module the battle loads (`OVRnn`).
    pub module: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryBattle {
    /// Battle id 0x100–0x13F.
    pub id: u16,
    /// `{species, count}` × 3.
    pub formation: [[u8; 2]; 3],
    pub arena: u8,
    pub flags: u8,
    pub area: u8,
    pub module: u8,
    /// On a HackerGC base: what the engine does differently in this battle (a u32 of
    /// [`BEHAVIOUR_BITS`] in the EXE's `g_HgcBattleFlags`); `None` on retail, where the code
    /// tests literal battle ids instead.
    pub behaviour: Option<u32>,
}

/// The story-battle behaviour bits of a HackerGC base (gc_decomp `include/config/config_hgc.h`):
/// `(mask, key, what)`. Bits 8–11 are a level floor (enemies at least the party level + n) and
/// bits 12–15 the actor slots 1–4 that cannot die. ALT_STAGE, SOLO's camera, the finale's menu,
/// the arena's statuses and HEAL_BOTH are read by the overlays (gc_decomp `tools/hgc_code_hooks.py`).
pub const BEHAVIOUR_BITS: &[(u32, &str, &str)] = &[
    (0x0001, "solo", "The formation's first enemy fights alone for the player, from the Baby's slot; the Knight sits out (retail 0x102–0x107)"),
    (0x0002, "babyArena", "The Baby alone in rounds: random formation, enemy level = round − 3, no rewards (0x124)"),
    (0x0004, "noSteal", "A toy's steal never yields an item (0x112)"),
    (0x0008, "lossResult", "A win ends like a loss (0x108, 0x114, 0x123)"),
    (0x0010, "bossSpu", "Enemy 0 takes the large sound-bank layout (0x103, 0x104, 0x116, 0x117)"),
    (0x0020, "finaleBaby", "The Baby obeys command 3 regardless (0x11D–0x11F)"),
    (0x0040, "noBabyB0", "The Baby's AI swaps action 0xB0 for 0xAA (0x11D, 0x11F)"),
    (0x0080, "altStage", "The arena's background variant 1 (0x103–0x107, 0x114)"),
    (0x1_0000, "healBoth", "The area module's heal covers both slots (0x104)"),
];
pub const BEHAVIOUR_LEVEL_SHIFT: u16 = 8;
pub const BEHAVIOUR_SLOTS_SHIFT: u16 = 12;

/// The SOLO bit ([`BEHAVIOUR_BITS`]). The battle puts the formation's first enemy in the Baby's slot
/// on the player's side (retail's 0x105 is Glor against Ramal), so the rest of the formation is all
/// it fights. With nothing left, a move at every enemy divides by zero in OVR03 and the game hangs
/// (LT-45), so a solo battle needs [`SOLO_MIN`] or more.
pub const SOLO: u32 = 0x0001;
pub const SOLO_MIN: u32 = 2;

/// How many enemies `formation` stages: the game counts entries up to the first empty species.
pub fn members(formation: &[[u8; 2]; 3]) -> u32 {
    formation.iter().take_while(|f| f[0] != 0).map(|f| f[1] as u32).sum()
}

impl StoryBattle {
    /// Whether this battle is a solo one: the base's SOLO bit, or on retail ids 0x102–0x107.
    pub fn solo(&self) -> bool {
        self.behaviour.map_or((0x102..0x108).contains(&self.id), |v| v & SOLO != 0)
    }
}

/// The base's behaviour word for story battle `k` (0-based), if its EXE has the table.
fn behaviour(exe: &crate::exe::PsxExe, k: usize) -> Option<u32> {
    let (at, count) = exe.layout.as_ref()?.battle_flags?;
    if k >= count as usize {
        return None;
    }
    exe.slice(at + 4 * k as u32, 4).ok().map(|b| crate::container::le32(b, 0))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptedBattle {
    pub map: u16,
    pub arena: u8,
    pub species: u8,
    pub area: u32,
    pub module: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Encounters {
    pub file: &'static str,
    /// File offset of T in `sys/9` (header word 8).
    pub table_offset: u32,
    pub mirror: &'static str,
    pub mirror_offset: u32,
    /// T in `sys/10` equals T in `sys/9` over T's length.
    pub mirror_identical: bool,
    /// Bytes of T after the story battles (unknown, preserved).
    pub tail_len: u32,
    pub species: Vec<SpeciesRec>,
    pub zones: Vec<Zone>,
    pub story: Vec<StoryBattle>,
    pub scripted: Vec<ScriptedBattle>,
}

/// Where a species is fought: a zone, a story battle or a scripted battle.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Home {
    pub area: u16,
    pub arena: u8,
    pub module: u8,
}

fn table_of(file: &[u8], name: &str) -> Result<(u32, usize)> {
    if file.len() < 0x24 {
        return Err(bad(format!("{name} is too short for a ghost.npc header")));
    }
    let at = le32(file, 0x20) as usize;
    if at + TAIL_AT > file.len() {
        return Err(bad(format!("{name}: table T at {at:#x} runs past the file")));
    }
    Ok((at as u32, at))
}

impl Encounters {
    pub fn load(src: Source<'_>) -> Result<Self> {
        let g = src.file(PRIMARY)?;
        let (table_offset, at) = table_of(&g, PRIMARY)?;
        let t = &g[at..];
        let m = src.file(MIRROR)?;
        let (mirror_offset, mat) = table_of(&m, MIRROR)?;
        let mt = &m[mat..];
        let mirror_identical = mt.len() >= t.len() && &mt[..t.len()] == t;

        let species = (0..SPECIES_COUNT)
            .map(|i| {
                let r = &t[i * 5..i * 5 + 5];
                SpeciesRec { id: i as u8, group: [r[0], r[1], r[2], r[3]], level: r[4] }
            })
            .collect();
        let zones = (0..ZONE_COUNT)
            .map(|z| {
                let r = &t[ZONES_AT + z * 13..ZONES_AT + z * 13 + 13];
                let mut pairs = [[0u8; 2]; 5];
                for (i, p) in pairs.iter_mut().enumerate() {
                    *p = [r[2 * i], r[2 * i + 1]];
                }
                Zone { id: z as u16, pairs, arena: r[10], flags: r[11], area: r[12], module: area_module(src.exe, z as u16) }
            })
            .collect();
        let story = (0..STORY_COUNT)
            .map(|k| {
                let r = &t[STORY_AT + k * 9..STORY_AT + k * 9 + 9];
                let id = 0x100 + k as u16;
                StoryBattle {
                    id,
                    formation: [[r[0], r[1]], [r[2], r[3]], [r[4], r[5]]],
                    arena: r[6],
                    flags: r[7],
                    area: r[8],
                    module: area_module(src.exe, id),
                    behaviour: behaviour(src.exe, k),
                }
            })
            .collect();
        let sb = src.exe_bytes(src.exe.va(SCRIPTED_TABLE), 16)?;
        let scripted = (0..2)
            .map(|i| {
                let r = &sb[i * 8..i * 8 + 8];
                let map = le16(r, 0);
                ScriptedBattle { map, arena: r[2], species: r[3], area: le32(r, 4), module: area_module(src.exe, map) }
            })
            .collect();
        Ok(Self {
            file: PRIMARY,
            table_offset,
            mirror: MIRROR,
            mirror_offset,
            mirror_identical,
            tail_len: (t.len() - TAIL_AT) as u32,
            species,
            zones,
            story,
            scripted,
        })
    }

    /// Every zone and battle that stages `species`, in id order (the scan
    /// `sim_catalog.py` does for its `home` table).
    pub fn homes(&self, species: u8) -> Vec<Home> {
        let mut out: Vec<Home> = Vec::new();
        for z in &self.zones {
            let hit = z
                .pairs
                .iter()
                .take_while(|p| p[0] != 0)
                .any(|p| p[0] == species || p[1] == species);
            if hit {
                out.push(Home { area: z.id, arena: z.arena, module: z.module });
            }
        }
        for b in &self.story {
            if b.formation.iter().any(|f| f[0] == species && f[1] != 0) {
                out.push(Home { area: b.id, arena: b.arena, module: b.module });
            }
        }
        for s in &self.scripted {
            if s.species == species {
                out.push(Home { area: s.map, arena: s.arena, module: s.module });
            }
        }
        out.sort_by_key(|h| h.area);
        out.dedup_by_key(|h| h.area);
        out
    }
}
