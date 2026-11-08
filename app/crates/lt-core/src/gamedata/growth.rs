//! Party growth (PRD §4.10, research/04 §2.6).
//!
//! - Base templates: EXE `D_801B552C`, 2 × 0x28 `Rec28` (Knight, Baby), copied to
//!   `D_801CD790` by `NewGame_InitPartyStats`.
//! - EXP curve: `ExpTable_Cumulative` 0x8017D808 (`main.c:2311`) is code: step
//!   15, `step += step/4` (cap 0x7FFF), running sum. `Exp(L)` is the total EXP
//!   that triggers the level-up from L to L + 1.
//! - Level-up: `PartyMember_LevelUp` 0x8017D854 (`main.c:2314`): 3 picks of
//!   `rand() % 7` through a switch (0/4 attack, 1/5 defense, 2/6 speed, 3 the
//!   PP stat), then +1 to each; `maxHP += def/(L−1)·L/2`, `maxPP += pp/(L/2)`.
//!
//! The step, the pick count and the switch's case → stat map are read back
//! from the EXE's machine code (checked against the retail instruction
//! patterns); when the code doesn't match, the decomp's values are used and
//! `verified` is false.

use serde::Serialize;

use crate::container::{le16, le32};

use super::{Result, Source};
use crate::text::Encoding;

pub const TEMPLATES: u32 = 0x801B_552C;
pub const TEMPLATE_SIZE: usize = 0x28;
pub const EXP_FN: u32 = 0x8017_D808;
pub const LEVEL_UP_FN: u32 = 0x8017_D854;
pub const MAX_LEVEL: u16 = 99;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyTemplate {
    pub name: String,
    pub hp: u16,
    pub pp: u16,
    pub max_hp: u16,
    pub max_pp: u16,
    pub level: u16,
    pub spd: u16,
    pub atk: u16,
    pub def: u16,
    /// +0x1C, the stat that drives PP growth.
    pub pp_stat: u16,
    /// +0x1E, 128 = neutral drop luck.
    pub luck: u8,
    pub element: u8,
    pub affinity: u32,
    pub exp: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelUpRule {
    /// Random picks per level-up (3).
    pub picks: u8,
    /// Stat raised by each `rand() % cases.len()` value: "atk", "def", "spd", "pp".
    pub cases: Vec<&'static str>,
    /// The rule was read from the EXE's code, not assumed.
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Growth {
    pub templates: Vec<PartyTemplate>,
    /// First step of the EXP curve (15).
    pub exp_step: u16,
    pub exp_verified: bool,
    /// `Exp(L)` for L = 1..=99 (`exp[L − 1]`).
    pub exp: Vec<u32>,
    pub level_up: LevelUpRule,
}

/// `ExpTable_Cumulative(level)`.
pub fn exp_cumulative(step0: u16, level: u16) -> u32 {
    let mut v = step0 as u32;
    let mut sum = v;
    for _ in 1..level.max(1) {
        v += v / 4;
        if v > 0x7FFF {
            v = 0x7FFF;
        }
        sum += v;
    }
    sum
}

/// A party template (0x28 bytes) as the EXE holds it.
pub fn template(r: &[u8]) -> PartyTemplate {
    template_in(r, Encoding::Ascii)
}

/// [`template`] in a release's encoding (the Japanese names are Shift-JIS).
pub fn template_in(r: &[u8], enc: Encoding) -> PartyTemplate {
    let end = r[..12].iter().position(|&c| c == 0).unwrap_or(12);
    PartyTemplate {
        name: match enc {
            Encoding::Ascii => super::latin1(&r[..end]),
            Encoding::ShiftJis => crate::text::sjis(&r[..end]),
        },
        hp: le16(r, 0x0C),
        pp: le16(r, 0x0E),
        max_hp: le16(r, 0x10),
        max_pp: le16(r, 0x12),
        level: le16(r, 0x14),
        spd: le16(r, 0x16),
        atk: le16(r, 0x18),
        def: le16(r, 0x1A),
        pp_stat: le16(r, 0x1C),
        luck: r[0x1E],
        element: r[0x1F],
        affinity: le32(r, 0x20),
        exp: le32(r, 0x24) as i32,
    }
}

/// `Growth_ClampStatValue`.
fn clamp(v: u32) -> u16 {
    v.min(0x7FFF) as u16
}

/// `t` raised to `level` by `PartyMember_LevelUp`'s rule, with the random picks spread evenly:
/// pick `k` (counted from the first level-up) raises `rule.cases[k % cases.len()]`, so every
/// `cases.len()` picks raise each stat by its share of the switch, as `rand() % n` does on
/// average. Then +1 to each stat, `maxHP += def / (L − 1) · L / 2` and `maxPP += pp / (L / 2)`
/// with L the new level. HP and PP are full, and the EXP is the total that reaches `level`
/// (`Party_InitStatsToLevel`). A `level` at or below the template's leaves its stats as they are.
pub fn grow(t: &PartyTemplate, level: u16, rule: &LevelUpRule, exp_step: u16) -> PartyTemplate {
    let mut g = t.clone();
    let mut k = 0usize;
    while g.level < level.min(MAX_LEVEL) {
        g.level += 1;
        for _ in 0..rule.picks {
            let stat = rule.cases.get(k % rule.cases.len().max(1)).copied().unwrap_or("atk");
            k += 1;
            let v = match stat {
                "atk" => &mut g.atk,
                "def" => &mut g.def,
                "spd" => &mut g.spd,
                _ => &mut g.pp_stat,
            };
            *v = clamp(*v as u32 + 1);
        }
        for v in [&mut g.atk, &mut g.def, &mut g.spd, &mut g.pp_stat] {
            *v = clamp(*v as u32 + 1);
        }
        let l = g.level as u32;
        g.max_hp = clamp(g.max_hp as u32 + g.def as u32 / (l - 1) * l / 2);
        g.max_pp = clamp(g.max_pp as u32 + g.pp_stat as u32 / (l >> 1));
    }
    if g.level != t.level {
        (g.hp, g.pp) = (g.max_hp, g.max_pp);
        g.exp = if g.level >= 2 { exp_cumulative(exp_step, g.level - 1) as i32 } else { 0 };
    }
    g
}

/// `addiu v1, zero, imm` at the top of `ExpTable_Cumulative`.
fn read_exp_step(src: Source<'_>) -> Option<u16> {
    let w = src.exe_u32(src.exe.va(EXP_FN)).ok()?;
    (w >> 16 == 0x2403).then_some(w as u16)
}

const DECOMP_CASES: [&str; 7] = ["atk", "def", "spd", "pp", "atk", "def", "spd"];

/// Decodes the switch in `PartyMember_LevelUp`: `slti v0, s0, picks` closes
/// the pick loop, `sltiu v0, v1, n` bounds the jump table, `lui/addiu` loads
/// its address, and each case starts with `lhu a0, off(s1)` naming the stat.
fn read_level_up(src: Source<'_>) -> Option<(u8, Vec<&'static str>)> {
    let code = src.exe_bytes(src.exe.va(LEVEL_UP_FN), 0x100).ok()?;
    let words: Vec<u32> = (0..code.len() / 4).map(|i| le32(code, i * 4)).collect();
    let imm = |w: u32| w as u16;
    let op = |w: u32| w >> 26;
    let mut table = None;
    let mut cases = None;
    let mut picks = None;
    for (i, &w) in words.iter().enumerate() {
        match op(w) {
            0x0F if table.is_none() => {
                // lui rt, hi; the next word adds the low half into some register.
                let rt = (w >> 16) & 31;
                if let Some(&n) = words.get(i + 1) {
                    if op(n) == 0x09 && (n >> 21) & 31 == rt {
                        table = Some(((imm(w) as u32) << 16).wrapping_add(imm(n) as i16 as i32 as u32));
                    }
                }
            }
            0x0B if cases.is_none() => cases = Some(imm(w) as usize),
            0x0A if picks.is_none() => picks = Some(imm(w) as u8),
            _ => {}
        }
    }
    let (table, n, picks) = (table?, cases?, picks?);
    if !(1..=16).contains(&n) || picks == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for k in 0..n as u32 {
        let target = src.exe_u32(table + k * 4).ok()?;
        let first = src.exe_u32(target).ok()?;
        // lhu (opcode 0x25) from the record pointer.
        if op(first) != 0x25 {
            return None;
        }
        out.push(match imm(first) {
            0x18 => "atk",
            0x1A => "def",
            0x16 => "spd",
            0x1C => "pp",
            _ => return None,
        });
    }
    Some((picks, out))
}

impl Growth {
    pub fn load(src: Source<'_>) -> Result<Self> {
        let b = src.exe_bytes(src.exe.va(TEMPLATES), 2 * TEMPLATE_SIZE)?;
        let templates = (0..2).map(|i| template_in(&b[i * TEMPLATE_SIZE..(i + 1) * TEMPLATE_SIZE], src.encoding())).collect();
        let step = read_exp_step(src);
        let exp_step = step.unwrap_or(15);
        let level_up = match read_level_up(src) {
            Some((picks, cases)) => LevelUpRule { picks, cases, verified: true },
            None => LevelUpRule { picks: 3, cases: DECOMP_CASES.to_vec(), verified: false },
        };
        Ok(Self {
            templates,
            exp_step,
            exp_verified: step.is_some(),
            exp: (1..=MAX_LEVEL).map(|l| exp_cumulative(exp_step, l)).collect(),
            level_up,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_curve_matches_research() {
        // research/04 §2.6: L2 33, L10 445, L20 4431, L30 41403, L50 616688.
        assert_eq!(exp_cumulative(15, 1), 15);
        assert_eq!(exp_cumulative(15, 2), 33);
        assert_eq!(exp_cumulative(15, 10), 445);
        assert_eq!(exp_cumulative(15, 20), 4431);
        assert_eq!(exp_cumulative(15, 30), 41403);
        assert_eq!(exp_cumulative(15, 50), 616688);
    }

    fn knight() -> PartyTemplate {
        // Retail's D_801B552C[0]: 24/5 (current), 18/5 (max), level 1, spd 5, atk 6, def 7, pp 3.
        PartyTemplate { name: "Knight".into(), hp: 24, pp: 5, max_hp: 18, max_pp: 5, level: 1, spd: 5, atk: 6, def: 7, pp_stat: 3, luck: 128, element: 0, affinity: 0, exp: 0 }
    }

    #[test]
    fn grows_a_template_by_the_average_level_up() {
        let rule = LevelUpRule { picks: 3, cases: DECOMP_CASES.to_vec(), verified: true };
        let k = knight();
        assert_eq!(grow(&k, 1, &rule, 15).max_hp, 18, "level 1 stays as it is");
        assert_eq!(grow(&k, 1, &rule, 15).hp, 24);
        // By hand: L2 +9 HP +4 PP, L3 +7 +6, L4 +8 +3, L5 +7 +4.
        let l5 = grow(&k, 5, &rule, 15);
        assert_eq!((l5.level, l5.max_hp, l5.max_pp, l5.hp, l5.pp), (5, 49, 22, 49, 22));
        assert_eq!((l5.atk, l5.def, l5.spd, l5.pp_stat), (14, 14, 12, 9));
        assert_eq!(l5.exp, exp_cumulative(15, 4) as i32);
        // Seven levels of three picks raise each stat by exactly its share of the switch.
        let l8 = grow(&k, 8, &rule, 15);
        assert_eq!((l8.atk - 6, l8.def - 7, l8.spd - 5, l8.pp_stat - 3), (6 + 7, 6 + 7, 6 + 7, 3 + 7));
        let l99 = grow(&k, 120, &rule, 15);
        assert_eq!(l99.level, MAX_LEVEL);
        assert!(l99.max_hp <= 0x7FFF && l99.max_hp > 1000);
        assert_eq!(grow(&l5, 3, &rule, 15).level, 5, "a lower level leaves the template alone");
    }
}
