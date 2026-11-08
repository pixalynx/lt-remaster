//! Dialogue lines → NPC pack bytes (screen 12, PRD 12 §5.3 "Strategy A":
//! code never moves).
//!
//! A line that fits its pool slot is written in place. A line that grows is
//! appended in a *relocation area* inserted at the retail end of the D-span
//! (the first model section), and every operand that pointed at the old
//! string is repointed; the old slot keeps the retail text, so a reference
//! the disassembler missed shows old text instead of crashing. Inserting
//! shifts the model sections, so the `+8` model table moves with them (the P
//! chain only points below D in retail, which is checked).
//!
//! Edits are always applied to the retail text: [`apply`] first restores the
//! pool, the text operands and the relocation area from retail, then lays out
//! the full set of edits again. The result depends only on the edit set, never
//! on save order, and other edits to the pack (placement words, screen 10)
//! are kept because they live outside those ranges.
//!
//! Limits (PRD 12 §6): a relocated string must start below D + 0x10000 (u16
//! operands) and the pack must stay within the loader's 0x31000 bytes
//! (98 sectors, `FUN_LoadNpcBinAndOverlay` "NPC BIN size over").

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::container::le32;
use crate::npc::{NpcPack, CAP_SECTORS};
use crate::text::{self, Tok};
use crate::tier2::Operand;

use super::{EditError, Result};

/// Highest D-relative offset a u16 operand can hold.
pub const REACH_MAX: u32 = 0xFFFF;
/// The NPC loader cap in bytes.
pub const CAP_BYTES: u32 = CAP_SECTORS * 2048;
/// Zero bytes kept after the last non-zero byte when the pack grows: a model
/// section may end in zero words, and RAM past the file is not cleared.
const KEEP_TAIL: usize = 64;

/* ───────────────────────── Markup ───────────────────────── */

/// Parses the editor's markup (the inverse of [`text::markup`]): `{box:n}`,
/// `{brk:n}`, `{brk}`, `{wait}`, `{nl}`, `{XX}` (one byte, hex), `$A`–`$H`,
/// `#nnn`, `~nn`; everything else is printable ASCII.
pub fn parse_markup(s: &str) -> Result<Vec<Tok>> {
    let mut out: Vec<Tok> = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Tok>| {
        if !text.is_empty() {
            out.push(Tok::Text(std::mem::take(text)));
        }
    };
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'{' {
            let end = s[i..].find('}').map(|e| i + e).ok_or_else(|| bad_code(&s[i..]))?;
            let code = &s[i + 1..end];
            flush(&mut text, &mut out);
            let num = |p: &str| p.parse::<u8>().map_err(|_| bad_code(&s[i..=end]));
            out.push(match code {
                "wait" => Tok::Wait,
                "nl" => Tok::Nl,
                "brk" => Tok::Raw(vec![0x04]),
                _ if code.starts_with("box:") => Tok::Box(num(&code[4..])?),
                _ if code.starts_with("brk:") => Tok::BoxBreak(num(&code[4..])?),
                _ if code.len() == 2 && code.bytes().all(|x| x.is_ascii_hexdigit()) => {
                    Tok::Raw(vec![u8::from_str_radix(code, 16).expect("hex")])
                }
                _ => return Err(bad_code(&s[i..=end])),
            });
            i = end + 1;
            continue;
        }
        if c == b'$' && matches!(b.get(i + 1), Some(b'A'..=b'H')) {
            flush(&mut text, &mut out);
            out.push(Tok::Name(b[i + 1] as char));
            i += 2;
            continue;
        }
        if c == b'#' && b.get(i + 1..i + 4).is_some_and(|d| d.iter().all(u8::is_ascii_digit)) {
            flush(&mut text, &mut out);
            out.push(Tok::Item(s[i + 1..i + 4].parse().expect("digits")));
            i += 4;
            continue;
        }
        if c == b'~' && b.get(i + 1..i + 3).is_some_and(|d| d.iter().all(u8::is_ascii_digit)) {
            flush(&mut text, &mut out);
            out.push(Tok::Toy(s[i + 1..i + 3].parse().expect("digits")));
            i += 3;
            continue;
        }
        let ch = s[i..].chars().next().expect("char");
        if !(' '..='~').contains(&ch) {
            return Err(EditError::new(
                "TEXT_BAD_CHAR",
                format!("“{ch}” has no glyph in the dialogue font; use printable ASCII or a font-mapped character."),
            ));
        }
        text.push(ch);
        i += 1;
    }
    flush(&mut text, &mut out);
    Ok(out)
}

fn bad_code(code: &str) -> EditError {
    EditError::new("TEXT_BAD_CODE", format!("“{code}” is not a control code (use {{box:n}}, {{wait}}, {{nl}} or {{XX}})."))
}

/// Tokens → string bytes (no NUL). `decode(encode(t))` gives `t` back.
pub fn encode(toks: &[Tok]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in toks {
        match t {
            Tok::Text(s) => out.extend(s.bytes()),
            Tok::Box(n) => out.extend([0x05, *n, 0x00]),
            Tok::BoxBreak(n) => out.extend([0x04, 0x05, *n, 0x00]),
            Tok::Wait => out.push(0x06),
            Tok::Nl => out.push(0x07),
            Tok::Name(c) => out.extend([b'$', *c as u8]),
            Tok::Item(n) => out.extend(format!("#{n:03}").bytes()),
            Tok::Toy(n) => out.extend(format!("~{n:02}").bytes()),
            Tok::Raw(b) => out.extend(b),
        }
    }
    out
}

/// Markup → string bytes (no NUL).
pub fn encode_markup(s: &str) -> Result<Vec<u8>> {
    let bytes = encode(&parse_markup(s)?);
    if bytes.contains(&0) && !text_safe(&bytes) {
        return Err(EditError::new("TEXT_NUL", "A {00} byte would end the line early."));
    }
    Ok(bytes)
}

/// The string reads back whole: [`text::c_string`] stops only at its end.
fn text_safe(bytes: &[u8]) -> bool {
    let mut padded = bytes.to_vec();
    padded.push(0);
    text::c_string(&padded, 0).len() == bytes.len()
}

/* ───────────────────────── References ───────────────────────── */

/// Every text operand of the pack's reachable programs: string offset →
/// instruction offsets (D-relative; the u16 operand is at `site + 1`).
/// Opcodes 0x01–0x03 (say), 0x1F (speaker), 0x3A/0x3B (choice), 0x44 (objective).
pub fn text_refs(pack: &NpcPack, b: &[u8]) -> BTreeMap<u32, Vec<u32>> {
    let mut out: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for (_, _, res) in pack.programs(b) {
        let Ok(code) = res else { continue };
        for i in &code {
            for o in &i.operands {
                if let Operand::Text(t) | Operand::Name(t) | Operand::Objective(t) | Operand::Choice(t) = *o {
                    out.entry(t as u32).or_default().insert(i.at);
                }
            }
        }
    }
    out.into_iter().map(|(k, v)| (k, v.into_iter().collect())).collect()
}

/// A pool string's in-place room: retail offset → (bytes incl. NUL, slot bytes).
fn slots(pack: &NpcPack, b: &[u8]) -> BTreeMap<u32, (u32, u32)> {
    let pool = pack.pool(b);
    let (code_start, _) = pack.code_span(b);
    let mut out = BTreeMap::new();
    for (k, s) in pool.iter().enumerate() {
        let next = pool.get(k + 1).map(|n| n.offset).unwrap_or(code_start.max(s.offset + s.len));
        let room = s.slot.min(next.saturating_sub(s.offset)).max(s.len);
        out.insert(s.offset, (s.len, room));
    }
    out
}

/// The first model section after D (the retail end of the D-span).
fn first_model(pack: &NpcPack) -> u32 {
    pack.models.iter().copied().filter(|&m| m > pack.d).min().unwrap_or(pack.size as u32)
}

fn rd16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

/* ───────────────────────── Apply ───────────────────────── */

/// One edited line in the result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placed {
    /// Retail D-relative offset (the line's stable id).
    pub off: u32,
    /// Where the text is now (D-relative); equal to `off` when written in place.
    pub at: u32,
    /// Bytes including the NUL.
    pub len: u32,
    /// Operand sites repointed (D-relative instruction offsets).
    pub sites: Vec<u32>,
}

/// What a text save does to the pack: sizes for the budget panel and where
/// each edited line went.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextReport {
    /// File bytes after the edit (whole sectors) and on the disc.
    pub file_bytes: u32,
    pub retail_bytes: u32,
    /// Bytes up to the last non-zero byte (what the budget bar shows).
    pub used_bytes: u32,
    pub retail_used_bytes: u32,
    pub sectors: u32,
    pub cap_sectors: u32,
    /// D to the first model section, before and after.
    pub d_span: u32,
    pub retail_d_span: u32,
    /// Bytes inserted at the end of the D-span (grown lines and edited programs).
    pub appended: u32,
    /// The part of `appended` that edited programs and their new strings take.
    pub script_bytes: u32,
    /// The part of `appended` that trigger table 2 takes when records or actors were added ([`super::door`]).
    pub trigger_bytes: u32,
    /// The part of `appended` that added actors' placement programs take ([`super::actor`]).
    pub actor_bytes: u32,
    /// The part of `appended` that the removal block takes ([`super::remove`]).
    pub removed_bytes: u32,
    /// The part of `appended` that edited placement ladders take ([`super::ladder`]).
    pub ladder_bytes: u32,
    pub in_place: Vec<Placed>,
    pub relocated: Vec<Placed>,
}

fn trailing_zeros(b: &[u8]) -> usize {
    b.iter().rev().take_while(|&&x| x == 0).count()
}

/// Checks that `current` is `retail` plus edits this module, the script
/// module (programs in the relocation area, repointed records) or the
/// placement and trigger writers make, and returns the size of its relocation
/// area.
fn relocation_of(r: &NpcPack, retail: &[u8], c: &NpcPack, current: &[u8]) -> Result<u32> {
    let changed = || EditError::new("TEXT_LAYOUT_CHANGED", "This pack was replaced as a whole file; text edits need the retail layout. Revert it first.");
    if (c.p, c.d, c.models.len()) != (r.p, r.d, r.models.len()) || c.actors.len() < r.actors.len() {
        return Err(changed());
    }
    let (rfm, cfm) = (first_model(r), first_model(c));
    if cfm < rfm || !(cfm - rfm).is_multiple_of(4) {
        return Err(changed());
    }
    let delta = cfm - rfm;
    for (rm, cm) in r.models.iter().zip(&c.models) {
        if *cm != if *rm >= rfm { rm + delta } else { *rm } {
            return Err(changed());
        }
    }
    // Every reachable instruction is the retail one except for text operands.
    let d = r.d as usize;
    for (_, _, res) in r.programs(retail) {
        let Ok(code) = res else { continue };
        for i in &code {
            let text_operand = i.operands.first().is_some_and(|o| matches!(o, Operand::Text(_) | Operand::Name(_) | Operand::Objective(_) | Operand::Choice(_)));
            for k in 0..i.bytes.len() {
                let at = d + i.at as usize + k;
                if !(text_operand && (k == 1 || k == 2)) && retail.get(at) != current.get(at) {
                    return Err(EditError::new(
                        "TEXT_SCRIPTS_CHANGED",
                        "This pack's scripts differ from retail; text edits need the retail scripts. Revert it first.",
                    ));
                }
            }
        }
    }
    Ok(delta)
}

/// Inserts `area` at file offset `at`, moving every model section at or after
/// it (the `+8` table). Refuses when a P-chain entry points past `at`.
fn insert_area(b: &mut Vec<u8>, pack: &NpcPack, at: u32, area: &[u8]) -> Result<()> {
    if area.is_empty() {
        return Ok(());
    }
    // P chain: P-relative u32s from P+4 up to a 0 (relocated by the loader).
    let mut o = pack.p as usize + 4;
    while o + 4 <= b.len() && le32(b, o) != 0 {
        if pack.p + le32(b, o) >= at {
            return Err(EditError::new("TEXT_P_CHAIN", "A P-chain entry points past the end of the D-span, so the models can't move."));
        }
        o += 4;
        if o > pack.d as usize {
            break;
        }
    }
    let delta = area.len() as u32;
    for i in 0..pack.models.len() {
        let o = 8 + i * 4;
        let v = le32(b, o);
        if v >= at {
            b[o..o + 4].copy_from_slice(&(v + delta).to_le_bytes());
        }
    }
    b.splice(at as usize..at as usize, area.iter().copied());
    Ok(())
}

/// Removes `len` bytes at `at` (a previous relocation area), moving the models back.
fn remove_area(b: &mut Vec<u8>, pack: &NpcPack, at: u32, len: u32) {
    if len == 0 {
        return;
    }
    for i in 0..pack.models.len() {
        let o = 8 + i * 4;
        let v = le32(b, o);
        if v >= at + len {
            b[o..o + 4].copy_from_slice(&(v - len).to_le_bytes());
        }
    }
    b.drain(at as usize..(at + len) as usize);
}

/// Lays out `edits` (retail offset → markup) over the retail text of
/// `current`. `retail` is the pack as the disc has it. Lines equal to retail
/// are left alone; the project's edited programs ([`super::script`]) are kept.
/// Returns the new pack and the report.
pub fn apply(retail: &[u8], current: &[u8], edits: &BTreeMap<u32, String>) -> Result<(Vec<u8>, TextReport)> {
    let mut set = super::pack::Edits::recover(retail, current)?;
    set.text = edits.clone();
    let (b, report, _) = layout(retail, current, &set)?;
    Ok((b, report))
}

/// The pack with exactly the edit set `set`, laid out from retail: grown
/// lines first in the relocation area, then the new strings and the edited
/// programs (retail entry → program, every actor record of one repointed;
/// then the programs added actors have of their own), then trigger table 2
/// with the added records merged in ([`super::door`]), then the placement
/// programs of the added actors ([`super::actor`]), whose records follow the
/// retail ones, then the removal block ([`super::remove`]) and the edited
/// placement ladders ([`super::ladder`]). Placement and trigger positions of
/// `current` are kept.
pub fn layout(retail: &[u8], current: &[u8], set: &super::pack::Edits) -> Result<(Vec<u8>, TextReport, Vec<super::script::Placed>)> {
    // Characters from other packs come off first and go back on last ([`super::character`]).
    let base = super::character::base(retail, current)?;
    let current: &[u8] = &base;
    let looks = super::character::looks(retail, &set.characters)?;
    for a in &set.actors {
        if let Some(k) = a.character.filter(|&k| k as usize >= set.characters.len()) {
            return Err(EditError::new("CHARACTER_UNKNOWN", format!("An added actor looks like character {k}; the pack has {}.", set.characters.len())));
        }
    }
    let (edits, doors, actors, removed) = (&set.text, &set.doors[..], &set.actors[..], &set.removed);
    let r = NpcPack::parse(retail).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The retail pack has no header."))?;
    // Only removed smoke is hidden.
    let hidden: BTreeSet<u32> = set.hidden.intersection(removed).copied().collect();
    super::remove::check(retail, removed, &hidden)?;
    // Objects taken out run nothing, so a program only they use is dropped.
    let silent: BTreeSet<u32> = removed.iter().copied().filter(|&s| super::remove::how(r.actors[s as usize].d) == super::remove::How::Silent).collect();
    let scripts: super::script::Scripts = set
        .scripts
        .iter()
        .filter(|(entry, _)| r.actors.iter().any(|a| a.script == **entry && !silent.contains(&a.slot)))
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let scripts = &scripts;
    let c = NpcPack::parse(current).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The project's pack has no header."))?;
    let old_area = relocation_of(&r, retail, &c, current)?;
    let d = r.d as usize;
    let rfm = first_model(&r);
    // Every retail trigger record's bytes, wherever the project has it now.
    let pos = super::door::positions(retail, current)?;

    // 1. Back to the retail text: drop the relocation area, restore the pool, the
    //    operands, and the records' tail (count, pass1, both trigger tables).
    let mut b = current.to_vec();
    remove_area(&mut b, &c, rfm, old_area);
    let rd = r.d_block(retail);
    let pass1_at = d + 4 + 16 * r.actors.len();
    let len1 = crate::trigger::table_len(rd, pass1_at - d + 2).ok_or_else(|| EditError::new("TRIGGER_NO_TABLE", "The retail pack's trigger table 1 doesn't parse."))?;
    let tail_end = pass1_at + 2 + len1 + super::actor::room_bytes(retail)?;
    b[d..d + 4].copy_from_slice(&retail[d..d + 4]);
    b[pass1_at..tail_end].copy_from_slice(&retail[pass1_at..tail_end]);
    let refs = text_refs(&r, retail);
    let slot = slots(&r, retail);
    if let (Some((&first, _)), (cs, _)) = (slot.iter().next(), r.code_span(retail)) {
        let (s, e) = (d + first as usize, d + cs as usize);
        b[s..e].copy_from_slice(&retail[s..e]);
    }
    for sites in refs.values() {
        for &site in sites {
            let at = d + site as usize + 1;
            b[at..at + 2].copy_from_slice(&retail[at..at + 2]);
        }
    }

    // 2. Lay out the edits.
    let mut in_place = Vec::new();
    let mut relocated = Vec::new();
    let mut area: Vec<u8> = Vec::new();
    for (&off, markup) in edits {
        let Some(&(retail_len, room)) = slot.get(&off) else {
            return Err(EditError::new("TEXT_UNKNOWN_LINE", format!("No pool string starts at D+0x{off:04X}.")));
        };
        let bytes = encode_markup(markup)?;
        let rd = text::c_string(retail, d + off as usize);
        if bytes == rd {
            continue;
        }
        let len = bytes.len() as u32 + 1;
        let sites = refs.get(&off).cloned().unwrap_or_default();
        if len <= room {
            // In place: the new text, its NUL, and zeros over the rest of the old text.
            let at = d + off as usize;
            let clear = len.max(retail_len) as usize;
            b[at..at + clear].fill(0);
            b[at..at + bytes.len()].copy_from_slice(&bytes);
            in_place.push(Placed { off, at: off, len, sites: Vec::new() });
            continue;
        }
        if sites.is_empty() {
            return Err(EditError::new(
                "TEXT_ORPHAN_GROWS",
                format!("D+0x{off:04X} has no operand pointing at it, so it can only change within its {room}-byte slot ({len} B now)."),
            ));
        }
        let new_off = rfm - r.d + area.len() as u32;
        if new_off > REACH_MAX {
            return Err(EditError::new(
                "TEXT_REACH",
                format!("D+0x{off:04X} would move to D+0x{new_off:X}, past the u16 operand reach (0xFFFF). Shorten this or another grown line."),
            ));
        }
        area.extend_from_slice(&bytes);
        area.push(0);
        while !area.len().is_multiple_of(4) {
            area.push(0);
        }
        for &site in &sites {
            let at = d + site as usize + 1;
            b[at..at + 2].copy_from_slice(&(new_off as u16).to_le_bytes());
        }
        relocated.push(Placed { off, at: new_off, len, sites });
    }

    // 2b. Programs: records back to retail (removal changes more than the
    //     script word), then the edited programs after the lines.
    for i in 0..r.actors.len() {
        let o = d + 4 + 16 * i;
        b[o..o + 16].copy_from_slice(&retail[o..o + 16]);
    }
    let text_len = area.len() as u32;
    let moved: BTreeMap<u32, u32> = relocated.iter().map(|p| (p.off, p.at)).collect();
    let text_at = |o: u16| -> Option<u16> {
        let o = o as u32;
        slot.contains_key(&o).then(|| moved.get(&o).copied().unwrap_or(o) as u16)
    };
    let (code, mut programs, own) = super::script::place(&r, retail, scripts, actors, rfm - r.d + text_len, &text_at)?;
    for p in programs.iter_mut() {
        p.slots.retain(|s| !silent.contains(s));
    }
    area.extend_from_slice(&code);
    for p in &programs {
        for &s in &p.slots {
            let o = d + 4 + 16 * s as usize;
            b[o..o + 4].copy_from_slice(&p.at.to_le_bytes());
        }
    }

    // 2c. The trigger tables with the project's positions. Added records or
    //     actors move table 2 into the area (pass1, s16, repointed); added
    //     actors' records follow the retail ones, so pass1 and table 1 move up.
    let script_len = area.len() as u32 - text_len;
    let mut t1 = retail[pass1_at..pass1_at + 2 + len1].to_vec();
    let groups = r.triggers(retail);
    for rec in groups.iter().filter(|g| g.pass == 0).flat_map(|g| &g.rungs).flat_map(|x| &x.records) {
        let (at, _, _) = super::trigger::record_pos(rec);
        let now = &pos[&at];
        let o = d + at as usize - pass1_at;
        t1[o..o + now.len()].copy_from_slice(now);
    }
    let mut trigger_len = 0;
    let mut actor_len = 0;
    if doors.is_empty() && actors.is_empty() {
        b[pass1_at..pass1_at + t1.len()].copy_from_slice(&t1);
        for rec in groups.iter().filter(|g| g.pass == 1).flat_map(|g| &g.rungs).flat_map(|x| &x.records) {
            let (at, _, _) = super::trigger::record_pos(rec);
            let now = &pos[&at];
            b[d + at as usize..d + at as usize + now.len()].copy_from_slice(now);
        }
    } else {
        super::actor::check(retail, actors)?;
        while !area.len().is_multiple_of(4) {
            area.push(0);
        }
        let at = rfm - r.d + area.len() as u32;
        if at > 0x7FFF {
            return Err(EditError::new(
                "DOOR_REACH",
                format!("Trigger table 2 would start at D+0x{at:X}, past the s16 reach of pass1 (0x7FFF). Shorten grown lines or edited programs."),
            ));
        }
        let table = super::door::emit(retail, &pos, doors)?;
        area.extend_from_slice(&table);
        while !area.len().is_multiple_of(4) {
            area.push(0);
        }
        trigger_len = area.len() as u32 - text_len - script_len;
        t1[0..2].copy_from_slice(&(at as u16).to_le_bytes());
        let programs_at = area.len();
        for (i, a) in actors.iter().enumerate() {
            let at = rfm - r.d + area.len() as u32;
            area.extend_from_slice(&super::actor::program(a));
            let mut src: [u8; 16] = b[d + 4 + 16 * a.from as usize..d + 20 + 16 * a.from as usize].try_into().expect("16 bytes");
            // A program of its own, else the one its source runs (edited or not).
            if let Some(p) = own.iter().find(|p| p.added == i) {
                src[0..4].copy_from_slice(&p.at.to_le_bytes());
            }
            let look = a.character.map(|k| looks[k as usize]);
            b[pass1_at + 16 * i..pass1_at + 16 * (i + 1)].copy_from_slice(&super::actor::record(src, a, at, look));
        }
        actor_len = (area.len() - programs_at) as u32;
        b[d..d + 4].copy_from_slice(&((r.actors.len() + actors.len()) as u32).to_le_bytes());
        let t1_at = pass1_at + 16 * actors.len();
        b[t1_at..t1_at + t1.len()].copy_from_slice(&t1);
        b[t1_at + t1.len()..tail_end].fill(0);
    }
    // 2d. The removal block, last, over the retail records ([`super::remove`]).
    let mut removed_len = 0;
    if !removed.is_empty() {
        while !area.len().is_multiple_of(4) {
            area.push(0);
        }
        let at = rfm - r.d + area.len() as u32;
        area.extend_from_slice(&super::remove::block());
        super::remove::write(&mut b, &r, removed, &hidden, at);
        removed_len = super::remove::BLOCK_LEN as u32;
    }
    // 2e. Edited placement ladders, each with its header; only its actor's record
    //     points at it ([`super::ladder`]). A removed actor's block wins.
    let ladders: super::ladder::Ladders = set.ladders.iter().filter(|(s, _)| !removed.contains(s)).map(|(s, l)| (*s, l.clone())).collect();
    let ladder_start = area.len();
    for (&slot, rungs) in &ladders {
        super::ladder::check(retail, slot, rungs)?;
        while !area.len().is_multiple_of(4) {
            area.push(0);
        }
        let at = rfm - r.d + area.len() as u32 + super::ladder::HEADER_LEN as u32;
        area.extend_from_slice(&super::ladder::program(slot, rungs));
        let o = d + 4 + 16 * slot as usize + 8;
        b[o..o + 4].copy_from_slice(&at.to_le_bytes());
    }
    let ladder_len = (area.len() - ladder_start) as u32;
    let base = NpcPack::parse(&b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The pack lost its header."))?;
    insert_area(&mut b, &base, rfm, &area)?;

    // 3. Length: whole sectors, never shorter than retail, keeping a little of the zero tail.
    let tz = trailing_zeros(&b);
    let used = b.len() - tz;
    let keep = tz.min(KEEP_TAIL).min(trailing_zeros(retail));
    let file = (used + keep).div_ceil(2048).max(retail.len().div_ceil(2048)) * 2048;
    b.resize(file, 0);
    if file as u32 > CAP_BYTES {
        return Err(EditError::new(
            "NPC_CAP",
            format!(
                "The pack would be {} sectors ({} B used); the loader takes {CAP_SECTORS} (0x31000 B). Shorten the grown lines by {} B.",
                file / 2048,
                used,
                used + keep - CAP_BYTES as usize
            ),
        ));
    }

    // 4. Verify: every edited line reads back at every site; untouched lines still read retail.
    let out = NpcPack::parse(&b).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The pack lost its header."))?;
    let od = out.d_block(&b);
    for (&off, sites) in &refs {
        let want: Vec<u8> = match edits.get(&off) {
            Some(m) => encode_markup(m)?,
            None => text::c_string(retail, d + off as usize).to_vec(),
        };
        for &site in sites {
            let target = rd16(od, site as usize + 1) as usize;
            if text::c_string(od, target) != want.as_slice() {
                return Err(EditError::new("TEXT_VERIFY", format!("After the edit, the operand at D+0x{site:04X} doesn't read D+0x{off:04X}'s text.")));
            }
        }
    }
    // The added trigger records and actors read back.
    let key = |a: &super::actor::AddedActor| ((a.from, a.home, a.x, a.h, a.z, a.dir, a.hidden, a.own.is_some(), a.spot, a.branches.clone(), a.otherwise_hidden, a.walk.clone()), (a.house, a.smoke));
    // A copy of a retail scene director reads back hidden, asked to or not (an examine spot never does).
    let director = |from: u32| r.actors.get(from as usize).is_some_and(|s| super::actor::looks_hidden(s.b, s.d));
    let want = |a: &super::actor::AddedActor| {
        ((a.from, a.home, a.x, a.h, a.z, a.dir, !a.spot && (a.hidden || director(a.from)), a.own.is_some(), a.spot, a.branches.clone(), a.otherwise_hidden, a.walk.clone()), (a.house, a.smoke))
    };
    let got = super::actor::recover(retail, &b)?;
    if got.len() != actors.len() || !got.iter().zip(actors).all(|(x, y)| key(x) == want(y)) {
        return Err(EditError::new("ACTOR_VERIFY", "After the edit, the added actors don't read back."));
    }
    for p in &own {
        let slot = r.actors.len() + p.added;
        let ok = crate::tier2::disassemble(od, p.at).is_ok_and(|code| code.iter().map(|i| i.bytes.len()).sum::<usize>() == p.len as usize)
            && out.actors.get(slot).is_some_and(|a| a.script == p.at);
        if !ok {
            return Err(EditError::new("SCRIPT_VERIFY", format!("After the edit, actor {slot}'s own program doesn't read back at D+0x{:04X}.", p.at)));
        }
    }
    if super::remove::recover(retail, &b)? != *removed || super::remove::recover_hidden(retail, &b)? != hidden {
        return Err(EditError::new("REMOVE_VERIFY", "After the edit, the removed actors don't read back."));
    }
    if super::ladder::recover(retail, &b)? != ladders {
        return Err(EditError::new("LADDER_VERIFY", "After the edit, the edited placement ladders don't read back."));
    }
    let back = super::door::recover(retail, &b)?;
    let canon = super::door::canonical(retail, doors)?;
    let same = |a: &super::door::Added, b: &super::door::Added| a.chunk == b.chunk && a.cue == b.cue && super::door::words(&a.record) == super::door::words(&b.record);
    if back.len() != canon.len() || !back.iter().zip(&canon).all(|(a, b)| same(a, b)) {
        return Err(EditError::new("DOOR_VERIFY", "After the edit, trigger table 2 doesn't read back the added records."));
    }
    // Every edited program reads back whole where its records point.
    for p in &programs {
        let ok = crate::tier2::disassemble(od, p.at).is_ok_and(|code| code.iter().map(|i| i.bytes.len()).sum::<usize>() == p.len as usize)
            && p.slots.iter().all(|&s| out.actors.get(s as usize).is_some_and(|a| a.script == p.at));
        if !ok {
            return Err(EditError::new("SCRIPT_VERIFY", format!("After the edit, the program for D+0x{:04X} doesn't read back at D+0x{:04X}.", p.entry, p.at)));
        }
    }

    // Freed models stay freed only while no live actor uses them (putting an actor back brings its model back).
    let freeable = super::character::freeable(retail, removed, actors)?;
    let freed = super::character::Freed {
        models: set.freed.models.intersection(&freeable.models).copied().collect(),
        poses: set.freed.poses.intersection(&freeable.poses).copied().collect(),
    };
    let b = super::character::apply(retail, &b, &set.characters, &freed)?;
    let report = TextReport {
        file_bytes: b.len() as u32,
        retail_bytes: retail.len() as u32,
        used_bytes: used as u32,
        retail_used_bytes: (retail.len() - trailing_zeros(retail)) as u32,
        sectors: (b.len() / 2048) as u32,
        cap_sectors: CAP_SECTORS,
        d_span: out.d_span,
        retail_d_span: r.d_span,
        appended: area.len() as u32,
        script_bytes: script_len,
        trigger_bytes: trigger_len,
        actor_bytes: actor_len,
        removed_bytes: removed_len,
        ladder_bytes: ladder_len,
        in_place,
        relocated,
    };
    Ok((b, report, programs))
}

/* ───────────────────────── Reading back ───────────────────────── */

/// A line whose project text differs from retail.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditedLine {
    /// Retail D-relative offset (stable id).
    pub off: u32,
    /// Where the project's text is (D-relative).
    pub at: u32,
    pub markup: String,
    /// Bytes including the NUL.
    pub len: u32,
    pub relocated: bool,
}

/// The lines whose text in `current` differs from `retail`, found through
/// their operand sites (so relocated lines are followed). Orphans are
/// compared in place.
pub fn edited_lines(retail: &[u8], current: &[u8]) -> Result<Vec<EditedLine>> {
    let base = super::character::base(retail, current)?;
    let current: &[u8] = &base;
    let r = NpcPack::parse(retail).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The retail pack has no header."))?;
    let c = NpcPack::parse(current).ok_or_else(|| EditError::new("NPC_BAD_HEADER", "The project's pack has no header."))?;
    let (rd, cd) = (r.d_block(retail), c.d_block(current));
    let refs = text_refs(&r, retail);
    let mut out = Vec::new();
    for s in r.pool(retail) {
        let at = match refs.get(&s.offset).and_then(|sites| sites.first()) {
            Some(&site) if (site as usize + 3) <= cd.len() => rd16(cd, site as usize + 1) as u32,
            _ => s.offset,
        };
        let now = text::c_string(cd, at as usize);
        if at != s.offset || now != text::c_string(rd, s.offset as usize) {
            out.push(EditedLine {
                off: s.offset,
                at,
                markup: text::markup(&text::decode(now)),
                len: now.len() as u32 + 1,
                relocated: at != s.offset,
            });
        }
    }
    Ok(out)
}

/// The project's edits as `apply` takes them (retail offset → markup).
pub fn edit_set(retail: &[u8], current: &[u8]) -> Result<BTreeMap<u32, String>> {
    Ok(edited_lines(retail, current)?.into_iter().map(|l| (l.off, l.markup)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_round_trip() {
        let b = b"\x05\x03\x00Good morning,\x07$A.\x06I'm off #123 ~15\x04\x05\x00\x00\x04\x81\x40";
        let toks = text::decode(b);
        let m = text::markup(&toks);
        // Raw pairs come back as single bytes; the bytes are what must match.
        assert_eq!(encode(&parse_markup(&m).unwrap()), b.to_vec());
        assert_eq!(encode(&toks), b.to_vec());
        assert_eq!(encode_markup("{box:5}Hi{nl}$B{wait}{brk}").unwrap(), b"\x05\x05\x00Hi\x07$B\x06\x04".to_vec());
        assert_eq!(parse_markup("{bogus}").unwrap_err().code, "TEXT_BAD_CODE");
        assert_eq!(parse_markup("café").unwrap_err().code, "TEXT_BAD_CHAR");
        assert_eq!(encode_markup("a{00}b").unwrap_err().code, "TEXT_NUL");
    }
}
