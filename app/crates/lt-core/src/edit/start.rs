//! What a new game starts with (LT-56), on a HackerGC base with a boot map: the Baby in or out,
//! the Knight's and the Baby's levels, the rubies, a starting item list and story flags. All of
//! it is data the published base already reads at a new game, so no decomp change is needed:
//!
//! - **Levels** are the party templates in the EXE (`D_801B552C`, [`super::tables`]).
//!   `NewGame_InitPartyStats` copies them, and `Party_InitStatsToLevel(1)` leaves a template that
//!   is already past level 1 as it is. A level is written as the base's level-1 template grown by
//!   the average level-up ([`crate::gamedata::growth::grow`]): HP and PP full, and the EXP the
//!   total that reaches the level.
//! - **The rest** is the head of the starter scene's program ([`super::blank::starter_at`]). That
//!   scene runs once, at a new game, before its warp:
//!   - **Baby.** `CLEAR 0xFE` ("no Baby") and `SET 0xF9` ("the Baby is in the party"). The warp's
//!     chunk load reads 0xFE into `D_801B53ED`, which `Battle_IsActorActive(1)` asks. The battle
//!     HUD, the turn queue, the item menus and two dozen other sites ask 0xF9 as well. Retail's
//!     opening clears 0xFE in npc/7 and sets 0xF9 later, in npc/1.
//!   - **Rubies.** `ADD_RUBIES −999999` (`Rubies_Add` stops at 0), then `ADD_RUBIES n`. This makes
//!     the amount exact whatever the base starts with (HackerGC's `NEWGAME_RUBIES`, 100).
//!   - **Items.** One `GIVE` each. GIVE is silent while there is room (it opens the discard menu
//!     when the bag is full), so the list is held to the room a new game has.
//!   - **Flags.** One `SET` each.
//!
//! The ops after the head are kept as they are: the warp, and anything an author added in
//! screen 11 or 20. All of it is ordinary game state at runtime, so it is in every save.

use serde::{Deserialize, Serialize};

use super::script::{self, Arg, Node};
use super::{EditError, Result};
use crate::gamedata::growth::{self, LevelUpRule, PartyTemplate, MAX_LEVEL};
use crate::tier2::dsl::Severity;

/// Set at a new game: no Baby yet. The warp's chunk load reads it into `D_801B53ED`.
pub const NO_BABY: u16 = 0xFE;
/// The Baby is in the party (the battle HUD, the turn queue, the item menus).
pub const BABY_IN_PARTY: u16 = 0xF9;
/// `Rubies_Add`'s cap.
pub const RUBIES_MAX: u32 = 999_999;
/// What HackerGC's `NewGame_InitInventoryState` gives (`config_boot.h` `NEWGAME_RUBIES`).
pub const BASE_RUBIES: u32 = 100;
/// GIVE operands: key item records 198–214 are 500–516, a Living Toy's roster row r is 1000 + r.
pub const KEY_FIRST_RECORD: u16 = 198;
pub const KEY_OPERAND: u16 = 500;
pub const TOY_OPERAND: u16 = 1000;
const ITEM_RECORDS: u16 = crate::gamedata::items::ITEM_COUNT as u16;
const TOY_ROWS: u16 = crate::gamedata::toys::ROSTER_ROWS as u16;
/// What the base gives at a new game (`config_boot.h`): the Knight's four equipped items, which
/// take four of the 20 item slots, and two key items. As GIVE operands.
pub const BASE_EQUIPPED: [u16; 4] = [0x13, 0x49, 0x79, 0x7F];
pub const BASE_KEY_ITEMS: [u16; 2] = [0x202, 0x203];
/// Room a new game's bag has: 20 item slots less the equipped items, 20 key-item slots less the
/// base's key items.
pub const ITEM_ROOM: usize = 20 - BASE_EQUIPPED.len();
pub const KEY_ROOM: usize = 20 - BASE_KEY_ITEMS.len();
/// Flags a start may set at most (each is a 3-byte SET in the starter scene).
pub const FLAGS_MAX: usize = 64;

const OP_SET: u8 = 0x04;
const OP_CLEAR: u8 = 0x05;
const OP_GIVE: u8 = 0x16;
const OP_ADD_RUBIES: u8 = 0x33;

/// The parts of a new game's start that live in the starter scene.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Start {
    /// The Baby is in the party from the start.
    pub baby: bool,
    /// The rubies a new game has; `None` keeps the base's (100).
    pub rubies: Option<u32>,
    /// GIVE operands, in order (see [`ItemKind`]).
    pub items: Vec<u16>,
    /// Story flags set at a new game.
    pub flags: Vec<u16>,
}

/// What a GIVE operand gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// An item or a piece of equipment (records 1–197): one of the 20 item slots.
    Item,
    /// A key item (operand 500–516, records 198–214): one of the 20 key-item slots.
    Key,
    /// A Living Toy (1000 + its roster row).
    Toy,
}

/// What `id` gives, if it gives anything.
pub fn item_kind(id: u16) -> Option<ItemKind> {
    match id {
        1..KEY_FIRST_RECORD => Some(ItemKind::Item),
        _ if (KEY_OPERAND..KEY_OPERAND + ITEM_RECORDS - KEY_FIRST_RECORD).contains(&id) => Some(ItemKind::Key),
        _ if (TOY_OPERAND..TOY_OPERAND + TOY_ROWS).contains(&id) => Some(ItemKind::Toy),
        _ => None,
    }
}

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

/// Refuses a start the starter scene can't give: rubies past the cap, items that don't exist or
/// don't fit a new game's bag, flags the card sets itself or the engine doesn't have.
pub fn check(s: &Start, flag_count: u32) -> Result<()> {
    if let Some(r) = s.rubies.filter(|&r| r > RUBIES_MAX) {
        return Err(bad("START_RUBIES", format!("{r} rubies is past the game's cap of {RUBIES_MAX}.")));
    }
    let (mut items, mut keys) = (0, 0);
    let mut toys = std::collections::BTreeSet::new();
    for &id in &s.items {
        match item_kind(id) {
            None => {
                return Err(bad(
                    "START_ITEM",
                    format!("Item {id} doesn't exist. Items are 1–197, key items 500–516 and Living Toys 1000–{}.", TOY_OPERAND + TOY_ROWS - 1),
                ))
            }
            Some(ItemKind::Item) => items += 1,
            Some(ItemKind::Key) => keys += 1,
            Some(ItemKind::Toy) => {
                if !toys.insert(id) {
                    return Err(bad("START_ITEM", format!("Living Toy {} is in the list twice; a toy is owned once.", id - TOY_OPERAND)));
                }
            }
        }
    }
    if items > ITEM_ROOM {
        return Err(bad("START_ITEMS_FULL", format!("{items} items don't fit: a new game has room for {ITEM_ROOM} (20 slots, 4 hold the Knight's equipment).")));
    }
    if keys > KEY_ROOM {
        return Err(bad("START_ITEMS_FULL", format!("{keys} key items don't fit: a new game has room for {KEY_ROOM}.")));
    }
    if s.flags.len() > FLAGS_MAX {
        return Err(bad("START_FLAGS", format!("{} flags is more than a start sets ({FLAGS_MAX}).", s.flags.len())));
    }
    for &f in &s.flags {
        let why = refused(f).or((f as u32 >= flag_count).then_some("is past the flags this engine has"));
        if let Some(why) = why {
            return Err(bad("START_FLAG", format!("Flag 0x{f:03X} {why}.")));
        }
    }
    Ok(())
}

/// Why flag `f` isn't one a start sets, if it isn't (whatever the engine's flag count).
fn refused(f: u16) -> Option<&'static str> {
    match f {
        0 => Some("is always set"),
        BABY_IN_PARTY | NO_BABY => Some("goes with the Baby switch"),
        super::blank::KNIGHT_HIDDEN | super::blank::SCENE_MUSIC => Some("is the starter scene's own (it clears it)"),
        0xFD => Some("isn't a flag to SET: SET 0xFD wipes flags 0–63"),
        _ => None,
    }
}

fn settable(f: u16) -> bool {
    refused(f).is_none()
}

fn num(n: &Node) -> Option<(u8, i64)> {
    match n {
        Node::Op { op, args, .. } => match args.as_slice() {
            [Arg::Num { value }] => Some((*op, *value)),
            _ => None,
        },
        _ => None,
    }
}

fn op(op: u8, v: i64) -> Node {
    Node::Op { op, args: vec![Arg::Num { value: v }], at: None }
}

/// Reads the start back from a starter scene's program: the head it writes, then the ops after it
/// (`rest`, from the warp on), which a new head goes in front of.
pub fn read(tree: &[Node]) -> (Start, Vec<Node>) {
    let mut s = Start::default();
    let mut k = 0;
    while let Some((o, v)) = tree.get(k).and_then(num) {
        match (o, v) {
            (OP_CLEAR, v) if v == super::blank::KNIGHT_HIDDEN as i64 || v == super::blank::SCENE_MUSIC as i64 => {}
            (OP_CLEAR, v) if v == NO_BABY as i64 => s.baby = true,
            (OP_SET, v) if v == BABY_IN_PARTY as i64 && s.baby => {}
            (OP_ADD_RUBIES, v) if v == -(RUBIES_MAX as i64) => match tree.get(k + 1).and_then(num) {
                Some((OP_ADD_RUBIES, n)) if (0..=RUBIES_MAX as i64).contains(&n) => {
                    s.rubies = Some(n as u32);
                    k += 1;
                }
                _ => break,
            },
            (OP_GIVE, v) if (0..=0xFFFF).contains(&v) => s.items.push(v as u16),
            // Flags a start can't set (the Baby's without the Baby, the starter's own) are an author's.
            (OP_SET, v) if (1..0xFFFF).contains(&v) && settable(v as u16) => s.flags.push(v as u16),
            _ => break,
        }
        k += 1;
    }
    (s, tree[k..].to_vec())
}

/// The starter scene's program for `s`, in front of `rest` (the warp and anything after it).
pub fn program(s: &Start, rest: &[Node]) -> Vec<Node> {
    let mut out = vec![op(OP_CLEAR, super::blank::KNIGHT_HIDDEN as i64), op(OP_CLEAR, super::blank::SCENE_MUSIC as i64)];
    if s.baby {
        out.push(op(OP_CLEAR, NO_BABY as i64));
        out.push(op(OP_SET, BABY_IN_PARTY as i64));
    }
    if let Some(r) = s.rubies {
        out.push(op(OP_ADD_RUBIES, -(RUBIES_MAX as i64)));
        out.push(op(OP_ADD_RUBIES, r as i64));
    }
    out.extend(s.items.iter().map(|&i| op(OP_GIVE, i as i64)));
    out.extend(s.flags.iter().map(|&f| op(OP_SET, f as i64)));
    out.extend(rest.iter().cloned());
    out
}

/// Ops of the starter's own after its head, besides the warp and the END: what an author added.
pub fn extra_ops(rest: &[Node]) -> u32 {
    rest.iter().filter(|n| !matches!(n, Node::Op { op: 0x00 | 0x19, .. })).count() as u32
}

/// The start the starter scene `slot` of `current` gives, and how many ops of its own it keeps.
pub fn of_scene(reference: &[u8], current: &[u8], slot: u32) -> Result<(Start, u32)> {
    let tree = script::source(reference, current, slot, "starter")?.tree;
    let (s, rest) = read(&tree);
    Ok((s, extra_ops(&rest)))
}

/// `current` with the starter scene `slot` giving `s` (checked against `flag_count` flags).
/// `areas` are the project's new areas, which the warp may name.
pub fn set_scene(reference: &[u8], current: &[u8], slot: u32, s: &Start, flag_count: u32, areas: &[u32]) -> Result<Vec<u8>> {
    check(s, flag_count)?;
    let tree = script::source(reference, current, slot, "starter")?.tree;
    let (_, rest) = read(&tree);
    let check = script::check_tree(reference, current, slot, Some(&program(s, &rest)), areas)?;
    match check.bytes {
        Some(b) => Ok(b),
        None => {
            let first = check.diagnostics.iter().find(|d| d.severity == Severity::Error);
            Err(bad("START_SCRIPT", first.map_or_else(|| "The starter scene's program doesn't assemble.".to_string(), |d| d.message.clone())))
        }
    }
}

/// The template `current` (the project's) at `level`: `base` (the base's own, at its first
/// level) grown to it, with the project's name, luck, element and affinity. `None` when
/// `current` is at that level already, so screen 13's fine-tuning of its stats stays.
pub fn at_level(base: &PartyTemplate, current: &PartyTemplate, level: u16, rule: &LevelUpRule, exp_step: u16) -> Result<Option<PartyTemplate>> {
    if !(1..=MAX_LEVEL).contains(&level) {
        return Err(bad("START_LEVEL", format!("Level {level} is outside 1–{MAX_LEVEL}.")));
    }
    if level == current.level {
        return Ok(None);
    }
    if level < base.level {
        return Err(bad("START_LEVEL", format!("The base's {} starts at level {}; a start can't go below it.", base.name, base.level)));
    }
    let mut g = growth::grow(base, level, rule, exp_step);
    if level == base.level {
        g = base.clone();
    }
    g.name = current.name.clone();
    (g.luck, g.element, g.affinity) = (current.luck, current.element, current.affinity);
    Ok(Some(g))
}

/// Max HP and PP of `base` grown to each level 1–99 (index 0 = level 1), as [`at_level`] writes
/// them; levels below the base's own read as its own.
pub fn level_table(base: &PartyTemplate, rule: &LevelUpRule, exp_step: u16) -> Vec<(u16, u16)> {
    (1..=MAX_LEVEL)
        .map(|l| {
            let g = growth::grow(base, l, rule, exp_step);
            (g.max_hp, g.max_pp)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warp() -> Node {
        Node::Op { op: 0x19, args: [5, 326, 0x1FC, 0x4C, 0].into_iter().map(|v| Arg::Num { value: v }).collect(), at: None }
    }

    /// @LT-56/1: the Baby, rubies, items and flags the card sets are the starter program's head,
    /// and read back from it.
    #[test]
    fn a_start_reads_back_from_the_program_it_writes() {
        let plain = super::super::blank::starter_program();
        let (s, rest) = read(&plain);
        assert_eq!(s, Start::default(), "the base's starter gives nothing");
        assert_eq!(rest, vec![warp()]);
        assert_eq!(program(&s, &rest), plain, "an empty start writes the starter program as it was");
        let full = Start { baby: true, rubies: Some(1234), items: vec![5, 500, 1003, 5], flags: vec![0x210, 0x81] };
        let p = program(&full, &rest);
        assert_eq!(p.len(), 2 + 2 + 2 + 4 + 2 + 1);
        assert_eq!(read(&p), (full.clone(), vec![warp()]));
        let none = Start { rubies: Some(0), ..Default::default() };
        assert_eq!(read(&program(&none, &rest)).0, none, "zero rubies is a start of its own");
    }

    /// @LT-56/2: rewriting the start keeps the warp (and an author's own ops) after it.
    #[test]
    fn an_authors_own_ops_stay_after_the_head() {
        let say = Node::Op { op: 0x01, args: vec![Arg::Text { markup: "Hello".into() }], at: None };
        let mut tree = super::super::blank::starter_program();
        tree.insert(2, op(OP_SET, 0x99));
        tree.insert(3, say.clone());
        let (s, rest) = read(&tree);
        assert_eq!(s.flags, [0x99]);
        assert_eq!(rest, vec![say.clone(), warp()]);
        assert_eq!(extra_ops(&rest), 1);
        let again = program(&Start { baby: true, ..s }, &rest);
        assert_eq!(&again[again.len() - 2..], [say, warp()]);
        // SET 0xF9 without the Baby is the author's, so it stays in the rest.
        let (odd, rest) = read(&[op(OP_SET, BABY_IN_PARTY as i64), warp()]);
        assert_eq!((odd.baby, odd.flags.len(), rest.len()), (false, 0, 2));
        // A lone ADD_RUBIES isn't the card's: it stays in the rest.
        let (r, rest) = read(&[op(OP_ADD_RUBIES, 50), warp()]);
        assert_eq!(r.rubies, None);
        assert_eq!(rest.len(), 2);
    }

    /// @LT-56/1: a start is held to what a new game's bag and the engine's flags take.
    #[test]
    fn checks_what_a_new_game_can_hold() {
        assert!(check(&Start { baby: true, rubies: Some(RUBIES_MAX), items: vec![1, 197, 500, 516, 1000, 1071], flags: vec![1, 0x210] }, 0x2A0).is_ok());
        let code = |s: Start| check(&s, 0x200).unwrap_err().code;
        assert_eq!(code(Start { rubies: Some(RUBIES_MAX + 1), ..Default::default() }), "START_RUBIES");
        for bad in [0, 198, 499, 517, 999, 1072] {
            assert_eq!(code(Start { items: vec![bad], ..Default::default() }), "START_ITEM", "item {bad}");
        }
        assert_eq!(code(Start { items: vec![1003, 1003], ..Default::default() }), "START_ITEM");
        assert!(check(&Start { items: vec![5; ITEM_ROOM], ..Default::default() }, 0x200).is_ok());
        assert_eq!(code(Start { items: vec![5; ITEM_ROOM + 1], ..Default::default() }), "START_ITEMS_FULL");
        assert_eq!(code(Start { items: vec![500; KEY_ROOM + 1], ..Default::default() }), "START_ITEMS_FULL");
        for f in [0, 0xF9, 0xFC, 0xFD, 0xFE, 0xFF, 0x200] {
            assert_eq!(code(Start { flags: vec![f], ..Default::default() }), "START_FLAG", "flag {f:#x}");
        }
        assert_eq!(item_kind(500), Some(ItemKind::Key));
        assert_eq!(item_kind(BASE_KEY_ITEMS[0]), Some(ItemKind::Key));
        assert_eq!(item_kind(BASE_EQUIPPED[0]), Some(ItemKind::Item));
    }

    /// @LT-56/1: the Knight's level is the base's template grown, with the HP and PP that follow.
    #[test]
    fn a_level_grows_from_the_bases_template_and_keeps_the_projects_name() {
        let rule = LevelUpRule { picks: 3, cases: vec!["atk", "def", "spd", "pp", "atk", "def", "spd"], verified: true };
        let base = PartyTemplate { name: "Knight".into(), hp: 24, pp: 5, max_hp: 18, max_pp: 5, level: 1, spd: 5, atk: 6, def: 7, pp_stat: 3, luck: 128, element: 0, affinity: 0, exp: 0 };
        let mine = PartyTemplate { name: "Sir Ada".into(), luck: 200, ..base.clone() };
        let l5 = at_level(&base, &mine, 5, &rule, 15).unwrap().unwrap();
        assert_eq!((l5.name.as_str(), l5.luck, l5.level, l5.max_hp, l5.max_pp), ("Sir Ada", 200, 5, 49, 22));
        assert!(at_level(&base, &l5, 5, &rule, 15).unwrap().is_none(), "the same level leaves the template alone");
        let back = at_level(&base, &l5, 1, &rule, 15).unwrap().unwrap();
        assert_eq!((back.level, back.max_hp, back.hp, back.name.as_str()), (1, 18, 24, "Sir Ada"), "level 1 is the base's own template");
        assert_eq!(at_level(&base, &mine, 0, &rule, 15).unwrap_err().code, "START_LEVEL");
        let t = level_table(&base, &rule, 15);
        assert_eq!((t.len(), t[0], t[4]), (99, (18, 5), (49, 22)));
    }
}
