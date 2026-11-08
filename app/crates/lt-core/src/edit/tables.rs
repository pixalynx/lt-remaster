//! Game-data tables written back (screen 13 §5.3, research 06 phase 1
//! "game-data encoders"). Every table keeps its size and record count; a
//! record is rewritten in place over the bytes the project has, so fields the
//! decoders don't model survive:
//!
//! | Table | File | Record |
//! |---|---|---|
//! | items | `exe` 0x801B5614 | 215 × 20 B; name and description are `char*` |
//! | drop codes | `ovr/4` 0x801FDAFC | s16[206] |
//! | shops | `ovr/2` 0x801FCE00 | 23 pointers into a 252-byte pool, repacked |
//! | Living Toy roster | `exe` 0x801B69E0 | 72 × 24 B; name and description are `char*` |
//! | toy abilities | `toy/n` section 1 | 3 × 0x2C phases, then the actor block |
//! | species, zones, story battles | `sys/9` table T, mirrored into `sys/10` | 5, 13 and 9 B |
//! | scripted battles | `exe` 0x801B53A4 | 2 × 8 B |
//! | party templates | `exe` 0x801B552C | 2 × 0x28 B |
//!
//! Strings behind `char*` stay where they are: a new text is written over
//! the old one when it fits the old one's 4-byte-aligned slot, or the
//! pointer moves to a table string that already reads the same. Several
//! records share some strings ("Weapon", "Shield"): writing over one changes
//! them all, and the result lists them.

use serde::{Deserialize, Serialize};

use crate::container::le32;
use crate::gamedata::encounters::{self, SPECIES_COUNT, STORY_COUNT, ZONE_COUNT};
use crate::gamedata::growth::{PartyTemplate, TEMPLATES, TEMPLATE_SIZE};
use crate::gamedata::items::{Item, DROP_REMAP, DROP_REMAP_LEN, ITEM_COUNT, ITEM_SIZE, ITEM_TABLE};
use crate::gamedata::shops::{MAX_ENTRIES, POOL_END, POOL_START, SHOP_COUNT, TABLE as SHOP_TABLE};
use crate::gamedata::toys::{Phase, Toy, ToyFile, ROSTER, ROSTER_ROWS, ROW_SIZE, TOY_FILES};
use crate::gamedata::{fixed_str, GameData, OVR_LOAD};

use super::{EditError, Result};

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

/// Where a table edit reads the bytes it changes: the project's copy of a
/// file (`current`) and the disc's (`retail`), by canonical id.
pub trait Files {
    fn current(&self, id: &str) -> Result<Vec<u8>>;
    fn retail(&self, id: &str) -> Result<Vec<u8>>;
}

/* ───────────────────────── record fields ───────────────────────── */

/// An item's editable fields (`ItemRec` minus the zero word at +0x0E).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemFields {
    pub name: String,
    pub price: u16,
    pub cls: u16,
    pub stat: u8,
    pub val: u8,
    pub stat2: u8,
    pub val2: i8,
    pub elem: u16,
    pub desc: String,
}

impl ItemFields {
    pub fn of(i: &Item) -> Self {
        Self { name: i.name.clone(), price: i.price, cls: i.cls, stat: i.stat, val: i.val, stat2: i.stat2, val2: i.val2, elem: i.elem, desc: i.desc.clone() }
    }
}

/// A roster row's editable fields. The row byte (+0x0A, must equal the row)
/// and the visual-task code pointer (+0x10) stay as they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToyFields {
    pub name: String,
    pub pp: u16,
    pub hp: u16,
    pub max_hp: u16,
    pub category: u8,
    pub flags: u16,
    pub target: u16,
    pub desc: String,
}

impl ToyFields {
    pub fn of(t: &Toy) -> Self {
        Self { name: t.name.clone(), pp: t.pp, hp: t.hp, max_hp: t.max_hp, category: t.category, flags: t.flags, target: t.target, desc: t.desc.clone() }
    }
}

/// One ability (phase) record of a toy file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseFields {
    pub name: String,
    pub flag: u8,
    pub attr: u8,
    pub resist: u32,
    pub status: u16,
    pub target: u16,
    pub min: i32,
    pub max: i32,
    pub timing: u8,
    pub kind: u8,
    pub effect: u16,
}

impl PhaseFields {
    pub fn of(p: &Phase) -> Self {
        Self {
            name: p.name.clone(),
            flag: p.flag,
            attr: p.attr,
            resist: p.resist,
            status: p.status,
            target: p.target,
            min: p.min,
            max: p.max,
            timing: p.timing,
            kind: p.kind,
            effect: p.effect,
        }
    }
}

/// A toy file's ability block: the three phases and the actor size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbilityFields {
    pub phases: Vec<PhaseFields>,
    /// Actor size, height, width (+0x8A…+0x8E).
    pub dims: [u16; 3],
}

impl AbilityFields {
    pub fn of(f: &ToyFile) -> Self {
        Self { phases: f.phases.iter().map(PhaseFields::of).collect(), dims: f.dims }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeciesFields {
    pub group: [u8; 4],
    pub level: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneFields {
    pub pairs: [[u8; 2]; 5],
    pub arena: u8,
    /// +0xB, the field music `bgm/n`.
    pub flags: u8,
    pub area: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryFields {
    pub formation: [[u8; 2]; 3],
    pub arena: u8,
    pub flags: u8,
    pub area: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptedFields {
    pub map: u16,
    pub arena: u8,
    pub species: u8,
    pub area: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateFields {
    pub name: String,
    pub hp: u16,
    pub pp: u16,
    pub max_hp: u16,
    pub max_pp: u16,
    pub level: u16,
    pub spd: u16,
    pub atk: u16,
    pub def: u16,
    pub pp_stat: u16,
    pub luck: u8,
    pub element: u8,
    pub affinity: u32,
    pub exp: i32,
}

impl TemplateFields {
    pub fn of(t: &PartyTemplate) -> Self {
        Self {
            name: t.name.clone(),
            hp: t.hp,
            pp: t.pp,
            max_hp: t.max_hp,
            max_pp: t.max_pp,
            level: t.level,
            spd: t.spd,
            atk: t.atk,
            def: t.def,
            pp_stat: t.pp_stat,
            luck: t.luck,
            element: t.element,
            affinity: t.affinity,
            exp: t.exp,
        }
    }
}

/// One record of one table, with its new values (the whole record).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "table")]
pub enum Edit {
    Item { id: u16, fields: ItemFields },
    /// Drop code → item id (0–214, or 500–516 for key items 198–214).
    Drop { code: u16, item: i16 },
    /// A shop's list; `split` gives a shop that shares its list its own.
    Shop {
        id: u8,
        items: Vec<u8>,
        #[serde(default)]
        split: bool,
    },
    Toy { row: u8, fields: ToyFields },
    Abilities { toy: u16, fields: AbilityFields },
    Species { id: u8, fields: SpeciesFields },
    Zone { id: u16, fields: ZoneFields },
    Story { id: u16, fields: StoryFields },
    /// A HackerGC base's behaviour word for a story battle (`g_HgcBattleFlags`).
    Behaviour { id: u16, value: u32 },
    Scripted { index: u8, fields: ScriptedFields },
    Template { index: u8, fields: TemplateFields },
}

/// What an edit changed.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    /// Files with new bytes, by canonical id (only those that differ).
    #[serde(skip)]
    pub files: Vec<(String, Vec<u8>)>,
    /// Other records that show the change: shops sharing the list, items or
    /// toys sharing a string ("item 74", "shop 6").
    pub also: Vec<String>,
    /// Things worth knowing that didn't block the edit.
    pub notes: Vec<String>,
}

/// Applies `edit` to the project's files. `data` is the tables as the
/// project has them (enemy module masks, zone modules).
pub fn apply(files: &impl Files, data: &GameData, edit: &Edit) -> Result<Applied> {
    let mut out = Applied::default();
    match edit {
        Edit::Item { id, fields } => {
            let (exe, also) = set_item(&files.current("exe")?, &files.retail("exe")?, *id, fields)?;
            out.also = also;
            push(&mut out, files, "exe", exe)?;
        }
        Edit::Drop { code, item } => {
            let b = set_drop(&files.current("ovr/4")?, *code, *item)?;
            push(&mut out, files, "ovr/4", b)?;
        }
        Edit::Shop { id, items, split } => {
            let (b, also) = set_shop(&files.current("ovr/2")?, Some(&files.retail("ovr/2")?), *id, items, *split)?;
            out.also = also.iter().map(|s| format!("shop {s}")).collect();
            push(&mut out, files, "ovr/2", b)?;
        }
        Edit::Toy { row, fields } => {
            let (exe, also) = set_toy(&files.current("exe")?, &files.retail("exe")?, *row, fields)?;
            out.also = also;
            push(&mut out, files, "exe", exe)?;
        }
        Edit::Abilities { toy, fields } => {
            if *toy >= TOY_FILES {
                return Err(bad("TABLE_UNKNOWN_ENTRY", format!("There is no toy/{toy} (0–{}).", TOY_FILES - 1)));
            }
            let id = format!("toy/{toy}");
            let (b, notes) = set_abilities(&files.current(&id)?, fields)?;
            out.notes = notes;
            push(&mut out, files, &id, b)?;
        }
        Edit::Species { id, fields } => {
            if *id as usize >= SPECIES_COUNT {
                return Err(bad("TABLE_UNKNOWN_ENTRY", format!("There is no species record {id} (0–{}).", SPECIES_COUNT - 1)));
            }
            let mut r = [0u8; 5];
            r[..4].copy_from_slice(&fields.group);
            r[4] = fields.level;
            set_t(&mut out, files, *id as usize * 5, &r)?;
        }
        Edit::Zone { id, fields } => {
            let zone = data.encounters.zones.get(*id as usize).filter(|_| (*id as usize) < ZONE_COUNT);
            let zone = zone.ok_or_else(|| bad("TABLE_UNKNOWN_ENTRY", format!("There is no zone {id} (0–{}).", ZONE_COUNT - 1)))?;
            check_pairs(&fields.pairs)?;
            let had: Vec<u8> = zone.pairs.iter().flatten().copied().collect();
            let new: Vec<u8> = fields.pairs.iter().take_while(|p| p[0] != 0).flatten().copied().filter(|s| *s != 0 && !had.contains(s)).collect();
            check_modules(data, &new, zone.module, &format!("zone {id}"))?;
            let mut r = [0u8; 13];
            for (i, p) in fields.pairs.iter().enumerate() {
                r[2 * i..2 * i + 2].copy_from_slice(p);
            }
            (r[10], r[11], r[12]) = (fields.arena, fields.flags, fields.area);
            set_t(&mut out, files, ZONES_AT + *id as usize * 13, &r)?;
        }
        Edit::Story { id, fields } => {
            let k = id.checked_sub(0x100).map(usize::from).filter(|&k| k < STORY_COUNT);
            let k = k.ok_or_else(|| bad("TABLE_UNKNOWN_ENTRY", format!("There is no story battle {id:#x} (0x100–0x13F).")))?;
            let b = &data.encounters.story[k];
            check_formation(&fields.formation)?;
            if b.solo() {
                check_solo(*id, encounters::members(&fields.formation), encounters::members(&b.formation) >= encounters::SOLO_MIN)?;
            }
            let had: Vec<u8> = b.formation.iter().map(|f| f[0]).collect();
            let new: Vec<u8> = fields.formation.iter().filter(|f| f[1] > 0 && !had.contains(&f[0])).map(|f| f[0]).collect();
            check_modules(data, &new, b.module, &format!("story battle {id:#x}"))?;
            let mut r = [0u8; 9];
            for (i, f) in fields.formation.iter().enumerate() {
                r[2 * i..2 * i + 2].copy_from_slice(f);
            }
            (r[6], r[7], r[8]) = (fields.arena, fields.flags, fields.area);
            set_t(&mut out, files, STORY_AT + k * 9, &r)?;
        }
        Edit::Behaviour { id, value } => {
            if let Some(b) = id.checked_sub(0x100).and_then(|k| data.encounters.story.get(k as usize)) {
                if value & encounters::SOLO != 0 && !b.solo() {
                    check_solo(*id, encounters::members(&b.formation), true)?;
                }
            }
            let exe = set_behaviour(&files.current("exe")?, *id, *value)?;
            push(&mut out, files, "exe", exe)?;
        }
        Edit::Scripted { index, fields } => {
            check_species(fields.species, "The scripted battle")?;
            let exe = set_scripted(&files.current("exe")?, *index, fields)?;
            push(&mut out, files, "exe", exe)?;
        }
        Edit::Template { index, fields } => {
            let exe = set_template(&files.current("exe")?, *index, fields)?;
            push(&mut out, files, "exe", exe)?;
        }
    }
    Ok(out)
}

fn push(out: &mut Applied, files: &impl Files, id: &str, bytes: Vec<u8>) -> Result<()> {
    if bytes != files.current(id)? {
        out.files.push((id.to_string(), bytes));
    }
    Ok(())
}

/* ───────────────────────── EXE strings ───────────────────────── */

/// The EXE's file bytes, addressed by RAM address.
struct Exe {
    bytes: Vec<u8>,
    t_addr: u32,
    /// A shifted base's layout ([`crate::layout`]): table addresses below are retail's.
    layout: Option<std::sync::Arc<crate::layout::Layout>>,
}

impl Exe {
    fn new(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 0x800 || &bytes[..8] != b"PS-X EXE" {
            return Err(bad("EXE_BAD", "The project's EXE isn't a PS-X EXE."));
        }
        Ok(Self { t_addr: le32(bytes, 0x18), layout: crate::layout::for_exe(bytes), bytes: bytes.to_vec() })
    }

    /// File offset of a table known by its retail address.
    fn table(&self, retail: u32) -> Result<usize> {
        self.off(self.layout.as_ref().map_or(retail, |l| l.map(retail)))
    }

    fn off(&self, va: u32) -> Result<usize> {
        va.checked_sub(self.t_addr)
            .map(|d| d as usize + 0x800)
            .filter(|&o| o < self.bytes.len())
            .ok_or_else(|| bad("EXE_BAD", format!("{va:#010x} is outside the EXE.")))
    }

    /// The NUL-terminated string at `va` (raw bytes).
    fn str_at(&self, va: u32) -> Option<&[u8]> {
        let o = self.off(va).ok()?;
        let rest = &self.bytes[o..];
        Some(&rest[..rest.iter().position(|&c| c == 0)?])
    }

    /// Every table string pointer: (file offset of the pointer word, target).
    fn string_pointers(&self) -> Result<Vec<(usize, u32)>> {
        let mut out = Vec::new();
        let items = self.table(ITEM_TABLE)?;
        for i in 0..ITEM_COUNT {
            for at in [0, 16] {
                let o = items + i * ITEM_SIZE + at;
                out.push((o, le32(&self.bytes, o)));
            }
        }
        let roster = self.table(ROSTER)?;
        for r in 0..ROSTER_ROWS {
            for at in [0, 0x14] {
                let o = roster + r * ROW_SIZE + at;
                out.push((o, le32(&self.bytes, o)));
            }
        }
        Ok(out)
    }

    /// Who reads through the pointer word at file offset `o`.
    fn owner(&self, o: usize) -> String {
        let items = self.table(ITEM_TABLE).unwrap_or(0);
        let roster = self.table(ROSTER).unwrap_or(0);
        if (items..items + ITEM_COUNT * ITEM_SIZE).contains(&o) {
            let i = (o - items) / ITEM_SIZE;
            format!("item {i}{}", if (o - items).is_multiple_of(ITEM_SIZE) { "" } else { "'s description" })
        } else {
            let r = (o - roster) / ROW_SIZE;
            format!("toy row {r}{}", if (o - roster).is_multiple_of(ROW_SIZE) { "" } else { "'s description" })
        }
    }
}

/// Characters the string slot at `va` holds in place: the retail string
/// there and the zeros after it, up to the end of its 4-byte-aligned slot.
/// Pass the retail EXE: a shorter text written into the slot leaves it as
/// long as it was.
pub fn string_room(exe_bytes: &[u8], t_addr: u32, va: u32) -> u8 {
    let Some(o) = va.checked_sub(t_addr).map(|d| d as usize + 0x800).filter(|&o| o < exe_bytes.len()) else { return 0 };
    let rest = &exe_bytes[o..];
    let Some(n) = rest.iter().position(|&c| c == 0) else { return 0 };
    let end = ((va as usize + n + 1).next_multiple_of(4) - va as usize).min(rest.len());
    let zeros = rest[n..end].iter().take_while(|&&c| c == 0).count();
    (n + zeros - 1).min(255) as u8
}

fn ascii(text: &str, what: &str) -> Result<()> {
    if let Some(c) = text.chars().find(|c| !(' '..='~').contains(c)) {
        return Err(bad("TEXT_CHARS", format!("{what} can only use printable ASCII; “{c}” isn't one.")));
    }
    Ok(())
}

/// Points the `char*` at file offset `ptr_at` to `text`: the retail string
/// when it still reads `text`, else written over the current string when
/// only this pointer uses it and it fits, else another table string that
/// reads the same, else over a shared string that fits (the others named in
/// `also`).
fn set_string(exe: &mut Exe, retail: &Exe, ptr_at: usize, text: &str, what: &str, also: &mut Vec<String>) -> Result<()> {
    let cur = le32(&exe.bytes, ptr_at);
    if exe.str_at(cur) == Some(text.as_bytes()) {
        return Ok(());
    }
    ascii(text, what)?;
    let retail_ptr = le32(&retail.bytes, ptr_at);
    if exe.str_at(retail_ptr) == Some(text.as_bytes()) {
        exe.bytes[ptr_at..ptr_at + 4].copy_from_slice(&retail_ptr.to_le_bytes());
        return Ok(());
    }
    let ptrs = exe.string_pointers()?;
    let sharers: Vec<usize> = ptrs.iter().filter(|&&(o, p)| p == cur && o != ptr_at).map(|&(o, _)| o).collect();
    let room = string_room(&retail.bytes, retail.t_addr, cur) as usize;
    if sharers.is_empty() && text.len() <= room {
        write_in_place(exe, cur, text, room)?;
        return Ok(());
    }
    if let Some(&(_, p)) = ptrs.iter().find(|&&(_, p)| exe.str_at(p) == Some(text.as_bytes())) {
        exe.bytes[ptr_at..ptr_at + 4].copy_from_slice(&p.to_le_bytes());
        return Ok(());
    }
    if text.len() <= room {
        write_in_place(exe, cur, text, room)?;
        also.extend(sharers.iter().map(|&o| exe.owner(o)));
        return Ok(());
    }
    Err(bad(
        "TEXT_LONG",
        format!("{what} is {} characters; its slot in the EXE holds {room}{}.", text.len(), if sharers.is_empty() { "" } else { " (and other records share it)" }),
    ))
}

fn write_in_place(exe: &mut Exe, va: u32, text: &str, room: usize) -> Result<()> {
    let o = exe.off(va)?;
    let slot = &mut exe.bytes[o..o + room + 1];
    slot.fill(0);
    slot[..text.len()].copy_from_slice(text.as_bytes());
    Ok(())
}

/* ───────────────────────── items, roster, templates, scripted ───────────────────────── */

/// Writes the numeric fields of an item record.
pub fn put_item(r: &mut [u8], f: &ItemFields) {
    r[4..6].copy_from_slice(&f.price.to_le_bytes());
    r[6..8].copy_from_slice(&f.cls.to_le_bytes());
    (r[8], r[9], r[10], r[11]) = (f.stat, f.val, f.stat2, f.val2 as u8);
    r[12..14].copy_from_slice(&f.elem.to_le_bytes());
}

/// Item `id` with `f`; returns the EXE and the other records a shared string change shows on.
pub fn set_item(exe: &[u8], retail: &[u8], id: u16, f: &ItemFields) -> Result<(Vec<u8>, Vec<String>)> {
    if id as usize >= ITEM_COUNT {
        return Err(bad("TABLE_UNKNOWN_ENTRY", format!("There is no item {id} (0–{}).", ITEM_COUNT - 1)));
    }
    let (mut e, r) = (Exe::new(exe)?, Exe::new(retail)?);
    let at = e.table(ITEM_TABLE)? + id as usize * ITEM_SIZE;
    put_item(&mut e.bytes[at..at + ITEM_SIZE], f);
    let mut also = Vec::new();
    set_string(&mut e, &r, at, &f.name, "The name", &mut also)?;
    set_string(&mut e, &r, at + 16, &f.desc, "The description", &mut also)?;
    Ok((e.bytes, also))
}

/// Writes the numeric fields of a roster row.
pub fn put_toy(r: &mut [u8], f: &ToyFields) {
    r[4..6].copy_from_slice(&f.pp.to_le_bytes());
    r[6..8].copy_from_slice(&f.hp.to_le_bytes());
    r[8..10].copy_from_slice(&f.max_hp.to_le_bytes());
    r[0x0B] = f.category;
    r[0x0C..0x0E].copy_from_slice(&f.flags.to_le_bytes());
    r[0x0E..0x10].copy_from_slice(&f.target.to_le_bytes());
}

pub fn set_toy(exe: &[u8], retail: &[u8], row: u8, f: &ToyFields) -> Result<(Vec<u8>, Vec<String>)> {
    if row as usize >= ROSTER_ROWS {
        return Err(bad("TABLE_UNKNOWN_ENTRY", format!("There is no roster row {row} (0–{}).", ROSTER_ROWS - 1)));
    }
    if !(6..=8).contains(&f.category) {
        return Err(bad("TOY_CATEGORY", format!("Category {} isn't one the game knows (6 support, 7 attack all, 8 special).", f.category)));
    }
    let (mut e, r) = (Exe::new(exe)?, Exe::new(retail)?);
    let at = e.table(ROSTER)? + row as usize * ROW_SIZE;
    put_toy(&mut e.bytes[at..at + ROW_SIZE], f);
    let mut also = Vec::new();
    set_string(&mut e, &r, at, &f.name, "The name", &mut also)?;
    set_string(&mut e, &r, at + 0x14, &f.desc, "The description", &mut also)?;
    Ok((e.bytes, also))
}

/// A fixed char field: written only when the text changed (so retail
/// leftovers after the NUL, or Shift-JIS names, stay when untouched).
fn put_fixed(b: &mut [u8], text: &str, what: &str) -> Result<()> {
    if fixed_str(b) == text {
        return Ok(());
    }
    ascii(text, what)?;
    if text.len() >= b.len() {
        return Err(bad("TEXT_LONG", format!("{what} is {} characters; the field holds {}.", text.len(), b.len() - 1)));
    }
    b.fill(0);
    b[..text.len()].copy_from_slice(text.as_bytes());
    Ok(())
}

/// Writes a party template (0x28 B).
pub fn put_template(r: &mut [u8], f: &TemplateFields) -> Result<()> {
    put_fixed(&mut r[..12], &f.name, "The name")?;
    for (o, v) in [(0x0C, f.hp), (0x0E, f.pp), (0x10, f.max_hp), (0x12, f.max_pp), (0x14, f.level), (0x16, f.spd), (0x18, f.atk), (0x1A, f.def), (0x1C, f.pp_stat)] {
        r[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    (r[0x1E], r[0x1F]) = (f.luck, f.element);
    r[0x20..0x24].copy_from_slice(&f.affinity.to_le_bytes());
    r[0x24..0x28].copy_from_slice(&f.exp.to_le_bytes());
    Ok(())
}

/// Party template `index` (0 Knight, 1 Baby) as `exe` has it, through a shifted base's layout.
pub fn template_at(exe: &[u8], index: u8) -> Result<PartyTemplate> {
    if index > 1 {
        return Err(bad("TABLE_UNKNOWN_ENTRY", format!("There is no party template {index} (0 Knight, 1 Baby).")));
    }
    let e = Exe::new(exe)?;
    let at = e.table(TEMPLATES)? + index as usize * TEMPLATE_SIZE;
    let r = e.bytes.get(at..at + TEMPLATE_SIZE).ok_or_else(|| bad("EXE_BAD", "The party templates are outside the EXE."))?;
    Ok(crate::gamedata::growth::template(r))
}

pub fn set_template(exe: &[u8], index: u8, f: &TemplateFields) -> Result<Vec<u8>> {
    if index > 1 {
        return Err(bad("TABLE_UNKNOWN_ENTRY", format!("There is no party template {index} (0 Knight, 1 Baby).")));
    }
    if f.level == 0 || f.level > 99 {
        return Err(bad("LEVEL_RANGE", format!("Level {} is outside 1–99.", f.level)));
    }
    let mut e = Exe::new(exe)?;
    let at = e.table(TEMPLATES)? + index as usize * TEMPLATE_SIZE;
    put_template(&mut e.bytes[at..at + TEMPLATE_SIZE], f)?;
    Ok(e.bytes)
}

/// `exe` with story battle `id`'s behaviour word set: only a HackerGC base's EXE has the table
/// (its layout's `battleFlags`).
pub fn set_behaviour(exe: &[u8], id: u16, value: u32) -> Result<Vec<u8>> {
    let table = crate::layout::for_exe(exe).and_then(|l| l.battle_flags);
    let (at, count) = table.ok_or_else(|| bad("TABLE_NEEDS_HACKERGC", "Story-battle behaviour is a HackerGC base's table; this EXE has none."))?;
    let k = id.checked_sub(0x100).map(u32::from).filter(|&k| k < count);
    let k = k.ok_or_else(|| bad("TABLE_UNKNOWN_ENTRY", format!("There is no story battle {id:#x} in the behaviour table (0x100–{:#x}).", 0x100 + count - 1)))?;
    let t_addr = le32(exe, 0x18);
    let off = (at + 4 * k).checked_sub(t_addr).map(|o| o as usize + 0x800).filter(|&o| o + 4 <= exe.len());
    let off = off.ok_or_else(|| bad("TABLE_OUT_OF_RANGE", "The behaviour table is outside this EXE."))?;
    let mut out = exe.to_vec();
    out[off..off + 4].copy_from_slice(&value.to_le_bytes());
    Ok(out)
}

pub fn put_scripted(r: &mut [u8], f: &ScriptedFields) {
    r[0..2].copy_from_slice(&f.map.to_le_bytes());
    (r[2], r[3]) = (f.arena, f.species);
    r[4..8].copy_from_slice(&f.area.to_le_bytes());
}

pub fn set_scripted(exe: &[u8], index: u8, f: &ScriptedFields) -> Result<Vec<u8>> {
    if index > 1 {
        return Err(bad("TABLE_UNKNOWN_ENTRY", format!("There is no scripted battle {index} (0–1).")));
    }
    let mut e = Exe::new(exe)?;
    let at = e.table(encounters::SCRIPTED_TABLE)? + index as usize * 8;
    put_scripted(&mut e.bytes[at..at + 8], f);
    Ok(e.bytes)
}

/* ───────────────────────── drop codes ───────────────────────── */

/// Item ids a drop code can give: 0–214, or 500–516 (key items 198–214).
pub fn drop_target_ok(item: i16) -> bool {
    (0..ITEM_COUNT as i16).contains(&item) || (500..=500 + (ITEM_COUNT as i16 - 198 - 1)).contains(&item)
}

pub fn set_drop(ovr4: &[u8], code: u16, item: i16) -> Result<Vec<u8>> {
    if code as usize >= DROP_REMAP_LEN {
        return Err(bad("DROP_CODE", format!("Drop code {code} is past the table's {DROP_REMAP_LEN} entries.")));
    }
    if !drop_target_ok(item) {
        return Err(bad("DROP_ITEM", format!("Item {item} isn't one a drop can give (0–214, or 500–516 for key items).")));
    }
    let at = (DROP_REMAP - OVR_LOAD) as usize + code as usize * 2;
    let mut b = ovr4.to_vec();
    b.get_mut(at..at + 2).ok_or_else(|| bad("DATA_BAD_FORMAT", "ovr/4 is too short for the drop table."))?.copy_from_slice(&item.to_le_bytes());
    Ok(b)
}

/* ───────────────────────── shops ───────────────────────── */

/// Bytes a list takes in the pool: its entries and a 0, padded to 4 like
/// retail when `aligned` (the shop code reads the lists a byte at a time).
fn list_bytes(n: usize, aligned: bool) -> usize {
    if aligned {
        (n + 1).next_multiple_of(4)
    } else {
        n + 1
    }
}

/// The shops' lists repacked into the pool with shop `id`'s list replaced
/// (and, with `split`, given its own copy). Shops that shared a list on the
/// disc (`retail`) and read the same again share it again, so undoing a
/// split restores the disc's bytes. Returns the overlay and the shops that show the change with it.
pub fn set_shop(ovr2: &[u8], retail: Option<&[u8]>, id: u8, items: &[u8], split: bool) -> Result<(Vec<u8>, Vec<u8>)> {
    if id as usize >= SHOP_COUNT {
        return Err(bad("TABLE_UNKNOWN_ENTRY", format!("There is no shop {id} (0–{}).", SHOP_COUNT - 1)));
    }
    if items.is_empty() {
        return Err(bad("SHOP_EMPTY", "A shop needs at least one item."));
    }
    if items.len() > MAX_ENTRIES {
        return Err(bad("SHOP_LONG", format!("A shop shows at most {MAX_ENTRIES} items; this list has {}.", items.len())));
    }
    if let Some(&i) = items.iter().find(|&&i| i == 0 || i as usize >= ITEM_COUNT) {
        return Err(bad("SHOP_ITEM", format!("Item {i} can't be sold (1–{}).", ITEM_COUNT - 1)));
    }
    let pool = (POOL_START - OVR_LOAD) as usize;
    let table = (SHOP_TABLE - OVR_LOAD) as usize;
    if ovr2.len() < table + SHOP_COUNT * 4 {
        return Err(bad("DATA_BAD_FORMAT", "ovr/2 is too short for the shop table."));
    }
    let ptrs: Vec<u32> = (0..SHOP_COUNT).map(|i| le32(ovr2, table + i * 4)).collect();
    if let Some((i, p)) = ptrs.iter().enumerate().find(|(_, &p)| !(POOL_START..POOL_END).contains(&p)) {
        return Err(bad("DATA_BAD_FORMAT", format!("Shop {i} points outside the pool ({p:#010x}).")));
    }
    let list_at = |p: u32| -> Vec<u8> {
        let at = pool + (p - POOL_START) as usize;
        ovr2[at..table].iter().take_while(|&&c| c != 0).copied().collect()
    };
    // Distinct lists in pool order; each shop names one.
    let mut order: Vec<u32> = ptrs.clone();
    order.sort_unstable();
    order.dedup();
    let mut lists: Vec<Vec<u8>> = order.iter().map(|&p| list_at(p)).collect();
    // (`lists` is rebuilt below without the lists no shop names.)
    let mut owner: Vec<usize> = ptrs.iter().map(|p| order.iter().position(|q| q == p).unwrap()).collect();
    let mine = owner[id as usize];
    let shared: Vec<u8> = (0..SHOP_COUNT).filter(|&s| s != id as usize && owner[s] == mine).map(|s| s as u8).collect();
    if split && !shared.is_empty() {
        lists.insert(mine + 1, items.to_vec());
        for o in owner.iter_mut().filter(|o| **o > mine) {
            *o += 1;
        }
        owner[id as usize] = mine + 1;
    } else {
        lists[mine] = items.to_vec();
    }
    // Shops that shared a list on the disc and read the same again share it again.
    if let Some(r) = retail.filter(|r| r.len() >= table + SHOP_COUNT * 4) {
        let rptr = |s: usize| le32(r, table + s * 4);
        for a in 0..SHOP_COUNT {
            for b in a + 1..SHOP_COUNT {
                if rptr(a) == rptr(b) && owner[a] != owner[b] && lists[owner[a]] == lists[owner[b]] && !(split && (a == id as usize || b == id as usize)) {
                    let (keep, drop) = (owner[a].min(owner[b]), owner[a].max(owner[b]));
                    for o in owner.iter_mut().filter(|o| **o == drop) {
                        *o = keep;
                    }
                }
            }
        }
    }
    // Lists no shop names any more go.
    let used: Vec<usize> = (0..lists.len()).filter(|k| owner.contains(k)).collect();
    let lists: Vec<Vec<u8>> = used.iter().map(|&k| lists[k].clone()).collect();
    for o in owner.iter_mut() {
        *o = used.iter().position(|k| k == o).unwrap();
    }
    // Retail pads every list to 4 and fills the pool exactly; packed tight they leave room to grow.
    let cap = (POOL_END - POOL_START) as usize;
    let need = |aligned: bool| lists.iter().map(|l| list_bytes(l.len(), aligned)).sum::<usize>();
    let aligned = need(true) <= cap;
    if need(false) > cap {
        return Err(bad("SHOP_POOL", format!("The shop lists need {} B; the pool holds {cap} B. Shorten a list or share one.", need(false))));
    }
    let mut b = ovr2.to_vec();
    b[pool..table].fill(0);
    let mut at = 0usize;
    let mut starts = Vec::with_capacity(lists.len());
    for l in &lists {
        starts.push(at);
        b[pool + at..pool + at + l.len()].copy_from_slice(l);
        at += list_bytes(l.len(), aligned);
    }
    for (s, &o) in owner.iter().enumerate() {
        b[table + s * 4..table + s * 4 + 4].copy_from_slice(&(POOL_START + starts[o] as u32).to_le_bytes());
    }
    let also = if split { Vec::new() } else { shared };
    Ok((b, also))
}

/* ───────────────────────── toy abilities ───────────────────────── */

const PHASE_SIZE: usize = 0x2C;
/// The last toy effect retail uses (0x2F play-by-play); 0 marks an unused record.
pub const LAST_EFFECT: u16 = 0x2F;

/// Writes a phase record (0x2C B); bytes +0x15/+0x16 stay as they are.
pub fn put_phase(r: &mut [u8], f: &PhaseFields) -> Result<()> {
    put_fixed(&mut r[..20], &f.name, "The ability name")?;
    r[0x14] = f.flag;
    r[0x17] = f.attr;
    r[0x18..0x1C].copy_from_slice(&f.resist.to_le_bytes());
    r[0x1C..0x1E].copy_from_slice(&f.status.to_le_bytes());
    r[0x1E..0x20].copy_from_slice(&f.target.to_le_bytes());
    r[0x20..0x24].copy_from_slice(&f.min.to_le_bytes());
    r[0x24..0x28].copy_from_slice(&f.max.to_le_bytes());
    (r[0x28], r[0x29]) = (f.timing, f.kind);
    r[0x2A..0x2C].copy_from_slice(&f.effect.to_le_bytes());
    Ok(())
}

/// Writes a toy file's ability block (section 1 from its start).
pub fn put_abilities(s1: &mut [u8], f: &AbilityFields) -> Result<()> {
    if f.phases.len() != 3 {
        return Err(bad("TOY_PHASES", format!("A toy has 3 ability records, not {}.", f.phases.len())));
    }
    for (k, p) in f.phases.iter().enumerate() {
        put_phase(&mut s1[k * PHASE_SIZE..(k + 1) * PHASE_SIZE], p)?;
    }
    for (i, v) in f.dims.iter().enumerate() {
        s1[0x8A + 2 * i..0x8C + 2 * i].copy_from_slice(&v.to_le_bytes());
    }
    Ok(())
}

/// The toy file with its ability block replaced; notes flag effects the
/// battle code doesn't implement.
pub fn set_abilities(file: &[u8], f: &AbilityFields) -> Result<(Vec<u8>, Vec<String>)> {
    let s1 = file.get(4..8).map(|b| le32(b, 0) as usize).ok_or_else(|| bad("DATA_BAD_FORMAT", "The toy file has no header."))?;
    if s1 + 0x90 > file.len() {
        return Err(bad("DATA_BAD_FORMAT", "The toy file's ability block runs past its end."));
    }
    let mut b = file.to_vec();
    put_abilities(&mut b[s1..s1 + 0x90], f)?;
    let notes = f
        .phases
        .iter()
        .enumerate()
        .filter_map(|(k, p)| {
            let e = p.effect & !0x40;
            (e > LAST_EFFECT).then(|| format!("Ability {k}: effect {:#04x} is past the battle code's last, {LAST_EFFECT:#04x}.", p.effect))
        })
        .chain(f.phases.iter().enumerate().filter(|(_, p)| p.effect & !0x40 == 0x28).map(|(k, _)| format!("Ability {k}: effect 0x28 does nothing.")))
        .collect();
    Ok((b, notes))
}

/* ───────────────────────── encounter table T ───────────────────────── */

const ZONES_AT: usize = 0x280;
const STORY_AT: usize = 0xF80;

/// Writes `rec` at `off` in table T of `sys/9` and of its mirror `sys/10`.
/// The zone every retail interior paints: no battles, the house tune (bgm/1).
pub const INTERIOR_ZONE: u8 = 1;
/// Zones no retail chunk paints: each reads as [`INTERIOR_ZONE`] but for its music, which is 0.
pub const SPARE_ZONES: std::ops::RangeInclusive<u8> = 0x28..=0x3F;

/// The zone a house's inside paints its floor with so that `bgm/song` plays there (screen 23 §12.2): the field
/// picks the music of the zone under the Knight every frame, so it holds through stairs, battles, a rest and a
/// Continue. Zone 1 plays the house tune; another song takes a spare zone set as zone 1 is but for its music, the
/// one the project has so already, else the first it hasn't changed. Returns the zone and what `files` needs to
/// save for it.
pub fn music_zone(files: &impl Files, song: u8) -> Result<(u8, Applied)> {
    let rec = |b: &[u8], id: u8| -> Result<[u8; 13]> {
        let t = b.get(0x20..0x24).map(|w| le32(w, 0) as usize).ok_or_else(|| bad("DATA_BAD_FORMAT", "The zone table's file has no ghost.npc header."))?;
        let at = t + ZONES_AT + id as usize * 13;
        b.get(at..at + 13).and_then(|r| r.try_into().ok()).ok_or_else(|| bad("DATA_BAD_FORMAT", "The zone table runs past its file."))
    };
    let (now, was) = (files.current(encounters::PRIMARY)?, files.retail(encounters::PRIMARY)?);
    let mut want = rec(&now, INTERIOR_ZONE)?;
    if want[11] == song {
        return Ok((INTERIOR_ZONE, Applied::default()));
    }
    want[11] = song;
    for id in SPARE_ZONES.clone() {
        if rec(&now, id)? == want {
            return Ok((id, Applied::default()));
        }
    }
    let free = SPARE_ZONES.clone().find(|&id| rec(&now, id).ok() == rec(&was, id).ok());
    let id = free.ok_or_else(|| bad("MUSIC_ZONES_FULL", format!("Every spare zone (0x{:02X}–0x{:02X}) already plays another song or was changed; houses can play {} songs of their own.", SPARE_ZONES.start(), SPARE_ZONES.end(), SPARE_ZONES.clone().count())))?;
    let mut out = Applied::default();
    set_t(&mut out, files, ZONES_AT + id as usize * 13, &want)?;
    Ok((id, out))
}

fn set_t(out: &mut Applied, files: &impl Files, off: usize, rec: &[u8]) -> Result<()> {
    for id in [encounters::PRIMARY, encounters::MIRROR] {
        let mut b = files.current(id)?;
        let t = b.get(0x20..0x24).map(|w| le32(w, 0) as usize).ok_or_else(|| bad("DATA_BAD_FORMAT", format!("{id} has no ghost.npc header.")))?;
        let at = t + off;
        b.get_mut(at..at + rec.len()).ok_or_else(|| bad("DATA_BAD_FORMAT", format!("{id}: table T runs past the file.")))?.copy_from_slice(rec);
        push(out, files, id, b)?;
    }
    Ok(())
}

fn check_species(s: u8, what: &str) -> Result<()> {
    if s != 0 && !(crate::gamedata::enemy::FIRST as u8..=crate::gamedata::enemy::LAST as u8).contains(&s) {
        return Err(bad("SPECIES_RANGE", format!("{what} names species {s}; enemies are 2–111.")));
    }
    Ok(())
}

fn check_pairs(pairs: &[[u8; 2]; 5]) -> Result<()> {
    let mut ended = false;
    for (i, p) in pairs.iter().enumerate() {
        check_species(p[0], &format!("Candidate {i}"))?;
        check_species(p[1], &format!("Candidate {i}"))?;
        if p[0] == 0 && p[1] != 0 {
            return Err(bad("ZONE_PAIR", format!("Candidate {i} has a second enemy but no first.")));
        }
        if ended && p[0] != 0 {
            return Err(bad("ZONE_PAIR", format!("Candidate {i} comes after an empty one; the game stops at the first empty candidate.")));
        }
        ended |= p[0] == 0;
    }
    Ok(())
}

fn check_formation(f: &[[u8; 2]; 3]) -> Result<()> {
    for (i, s) in f.iter().enumerate() {
        check_species(s[0], &format!("Slot {i}"))?;
        if s[1] > 0 && s[0] == 0 {
            return Err(bad("STORY_SLOT", format!("Slot {i} has a count but no species.")));
        }
    }
    let n: u32 = f.iter().map(|s| s[1] as u32).sum();
    if n > 3 {
        return Err(bad("STORY_COUNT", format!("A battle holds 3 enemies; this formation has {n}.")));
    }
    Ok(())
}

/// A solo battle's formation must keep an enemy besides the first, which fights for the player
/// ([`encounters::SOLO`]): `now` enemies after the edit. Refused only when the edit is what takes it
/// below [`encounters::SOLO_MIN`] (`was_ok`), so a record that already is can still be fixed one
/// field at a time.
fn check_solo(id: u16, now: u32, was_ok: bool) -> Result<()> {
    if now < encounters::SOLO_MIN && was_ok {
        return Err(bad(
            "STORY_SOLO_FORMATION",
            format!(
                "Battle {id:#x} is a solo battle: the formation's first enemy fights for the player, so it needs a second to fight. \
                 With {now}, the game hangs at the first move aimed at every enemy."
            ),
        ));
    }
    Ok(())
}

/// Species added to a battle that loads `module` must not use effects that
/// live in another arena module.
fn check_modules(data: &GameData, added: &[u8], module: u8, what: &str) -> Result<()> {
    for &s in added {
        if let Some(e) = data.enemy(s as u16) {
            if module < 32 && e.mask & (1 << module) == 0 {
                return Err(bad(
                    "ENCOUNTER_MODULE",
                    format!("{} uses attacks that only exist in other arena modules; {what} loads OVR{module:02}.", if e.name.is_empty() { format!("enemy/{s}") } else { e.name.clone() }),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_s_behaviour_word_is_written_in_its_exe_and_retail_refuses() {
        // A made-up EXE: header, then 0x100 bytes of image at 0x80010000; the table at +0x40.
        let mut exe = vec![0u8; 0x900];
        exe[0..8].copy_from_slice(b"PS-X EXE");
        exe[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        exe[0x7FF] = 0xA5; // makes its hash this test's own
        assert_eq!(set_behaviour(&exe, 0x109, 1).unwrap_err().code, "TABLE_NEEDS_HACKERGC");
        let mut h = sha1_smol::Sha1::new();
        h.update(&exe);
        crate::layout::register(crate::layout::Layout { exe_sha1: h.digest().to_string(), battle_flags: Some((0x8001_0040, 0x40)), ..Default::default() });
        let out = set_behaviour(&exe, 0x109, 0x1_0001).unwrap();
        assert_eq!(&out[0x800 + 0x40 + 4 * 9..][..4], &[1, 0, 1, 0]);
        assert_eq!(set_behaviour(&exe, 0x140, 1).unwrap_err().code, "TABLE_UNKNOWN_ENTRY");
    }

    /// @LT-45/3: a solo battle keeps an enemy besides the one fighting for the player.
    #[test]
    fn a_solo_battle_keeps_a_second_enemy() {
        use encounters::members;
        assert_eq!(members(&[[11, 1], [0, 3], [87, 1]]), 1, "the game stops at the first empty species");
        assert_eq!(members(&[[11, 2], [0, 0], [0, 0]]), 2, "two of one species is two enemies");
        assert_eq!(check_solo(0x105, members(&[[102, 1], [0, 0], [0, 0]]), true).unwrap_err().code, "STORY_SOLO_FORMATION");
        assert!(check_solo(0x105, members(&[[102, 1], [87, 1], [0, 0]]), true).is_ok());
        // A record already below the minimum can still be edited.
        assert!(check_solo(0x109, 1, false).is_ok());
    }
}
