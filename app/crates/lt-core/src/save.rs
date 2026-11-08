//! PS1 memory cards and Guardian's Crusade saves. One 8 KB block per save,
//! named `BASLUSP00811KNIGHTnn`. Spec: `docs/screens/18-save-editor.md` §4.
//! The EU and JP releases name theirs `BESLES-01793KNIGHTnn` and `BISLPS-01531KNIGHTnn`
//! ([`crate::release::ReleaseInfo::save_prefix`]); their slot layout is unverified.

use serde::{Deserialize, Serialize};

use crate::container::{le16, le32};
use crate::release::Release;
use crate::tim::bgr555_to_rgba;

pub const CARD_BYTES: usize = 131_072;
pub const BLOCK: usize = 0x2000;
/// The USA release's save names (the others: [`crate::release::ReleaseInfo::save_prefix`]).
pub const NAME_PREFIX: &str = "BASLUSP00811KNIGHT";
pub const HEADER_MAGIC: u32 = 0x5828_5592;
/// The 0x300-byte save slot inside the 8 KB file.
pub const SLOT: usize = 0xC00;
pub const SLOT_BYTES: usize = 0x300;
const SAVEBLOCK: usize = SLOT + 0x2C;
const SB: usize = 0x2C;

/// OVR05 `D_80157F24[22]`: save-point names, used in the card title.
pub const SAVE_POINTS: [&str; 22] = [
    "Kerple", "Trisken", "Kell", "God's Tower", "Zed Harbor", "Den Heldar", "Jungo", "Zephyr", "Coastal Inn",
    "San Claria", "Orgo", "Carmarthen", "Castle Ryan", "Cozy Tunnel", "Kuldo Ruins", "Nord Temple", "Isten", "Picard",
    "Garam", "Denvrado", "Penguii", "Holy Shrine",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    /// Block 1–15.
    pub block: usize,
    /// 0x51 first, 0x52 middle, 0x53 last, 0xA0 free, 0xA1–0xA3 deleted.
    pub state: u32,
    pub size: u32,
    pub name: String,
    /// Byte 0x7F of the frame.
    pub checksum: u8,
    /// XOR of bytes 0x00–0x7E equals byte 0x7F.
    pub checksum_ok: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyMember {
    pub name: String,
    pub hp: u16,
    pub pp: u16,
    pub max_hp: u16,
    pub max_pp: u16,
    pub level: u16,
    pub agility: u16,
    pub strength: u16,
    pub power: u16,
    pub pp_growth: u16,
    pub luck: u8,
    pub weapon_element: u8,
    pub affinity: u32,
    pub exp: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSlot {
    /// Equipped (carried items) or flag byte (key items).
    pub a: u8,
    pub id: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Save {
    pub block: usize,
    pub file_name: String,
    /// Card title, SJIS full-width decoded to ASCII where possible.
    pub title: String,
    /// The same title as Unicode full-width text (what the BIOS shows).
    pub title_fw: String,
    /// The `nn` of `KNIGHTnn`; the title shows nn + 1.
    pub file_no: u32,
    /// 16×16 RGBA of icon frame 0.
    pub icon_rgba: Vec<u8>,
    /// Every icon frame (1–3), 16×16 RGBA each.
    pub icon_frames: Vec<Vec<u8>>,
    /// The raw 0x300-byte slot at file offset 0xC00 (the editor works on it).
    pub slot: Vec<u8>,
    pub header_ok: bool,
    pub backup_ok: bool,
    /// The game header (0x400) and its backup (0x480) are byte-identical.
    pub backup_equal: bool,
    pub header_magic: u32,
    pub header_version: u32,
    pub header_sum: u32,
    /// Stored body sum (written, never checked by the game).
    pub body_sum: u32,
    /// Body sum recomputed with the "fresh file" rule (field taken as 0).
    pub body_sum_fresh: u32,
    // Preview
    pub level: u16,
    pub location_index: u8,
    // SaveBlock
    pub flags_set: Vec<u16>,
    pub toys_owned: Vec<u16>,
    pub rubies: u32,
    /// +0x50, the `$E` insert: the name the Knight gives the Jungo couple's son (npc/35).
    pub third_name: String,
    pub map: u16,
    pub position: (i16, i16, i16),
    pub objective: Vec<u8>,
    pub play_frames: u32,
    pub steps: u32,
    pub location: String,
    pub key_items: Vec<ItemSlot>,
    pub items: Vec<ItemSlot>,
    pub party: Vec<PartyMember>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub header_ok: bool,
    pub directory: Vec<DirEntry>,
    pub saves: Vec<Save>,
}

pub fn parse_card(b: &[u8]) -> Option<Card> {
    parse_card_in(b, Release::Usa)
}

/// [`parse_card`] listing `release`'s saves.
pub fn parse_card_in(b: &[u8], release: Release) -> Option<Card> {
    let prefix = release.info().save_prefix;
    if b.len() != CARD_BYTES {
        return None;
    }
    let header_ok = &b[0..2] == b"MC";
    let directory: Vec<DirEntry> = (1..16)
        .map(|block| {
            let f = &b[block * 128..block * 128 + 128];
            DirEntry {
                block,
                state: le32(f, 0),
                size: le32(f, 4),
                name: ascii(&f[0x0A..0x0A + 21]),
                checksum: f[0x7F],
                checksum_ok: f[..0x7F].iter().fold(0u8, |x, &v| x ^ v) == f[0x7F],
            }
        })
        .collect();
    let saves = directory
        .iter()
        .filter(|d| d.state == 0x51 && d.name.starts_with(prefix))
        .filter_map(|d| parse_save(d.block, d.name.clone(), &b[d.block * BLOCK..(d.block + 1) * BLOCK]))
        .collect();
    Some(Card { header_ok, directory, saves })
}

/// Where flag word `w` lives in the save block: words 0–15 are `flags[16]`;
/// a HackerGC base keeps words 16–20 (flags 0x200–0x29F) in the block's spare
/// bytes, `pad1B4` (word 16) and `pad1C0` (17–20) (gc_decomp `src/hgc/hgc_flags.c`).
pub fn flag_word_offset(w: usize) -> Option<usize> {
    match w {
        0..=15 => Some(w * 4),
        16 => Some(0x1B4),
        17..=20 => Some(0x1C0 + (w - 17) * 4),
        _ => None,
    }
}

/// Flag words a save has under the open engine (16 on retail).
pub fn flag_words() -> usize {
    (crate::engine::active().flag_count.min(crate::flags::MAX_COUNT as u32) / 32) as usize
}

pub fn parse_save(block: usize, file_name: String, f: &[u8]) -> Option<Save> {
    parse_save_words(block, file_name, f, flag_words())
}

/// [`parse_save`] reading `words` flag words.
fn parse_save_words(block: usize, file_name: String, f: &[u8], words: usize) -> Option<Save> {
    if f.len() < BLOCK || &f[0..2] != b"SC" {
        return None;
    }
    let header_ok = game_header_ok(&f[0x400..0x480]);
    let backup_ok = game_header_ok(&f[0x480..0x500]);
    let sb = &f[SAVEBLOCK..SAVEBLOCK + 0x2C8];
    let words: Vec<u32> = (0..words).map(|w| le32(sb, flag_word_offset(w).expect("flag word"))).collect();
    let flags_set = (0..words.len() as u16 * 32).filter(|&i| words[i as usize / 32] & (1 << (i % 32)) != 0).collect();
    let toys_owned = (0..96u16).filter(|&i| le32(sb, 0x40 + (i as usize / 32) * 4) & (1 << (i % 32)) != 0).collect();
    let slots = |at: usize| (0..20).map(|i| ItemSlot { a: sb[at + i * 2], id: sb[at + i * 2 + 1] }).collect::<Vec<_>>();
    let party = (0..2).map(|i| member(&sb[0x148 + i * 0x28..0x148 + (i + 1) * 0x28])).collect();
    let body_sum = le32(f, SLOT + 0x2F4);
    let no = file_no(&file_name);
    Some(Save {
        block,
        file_name,
        title: sjis_title(&f[4..0x44]),
        title_fw: sjis_fullwidth(&f[4..0x44]),
        file_no: no,
        icon_rgba: icon_frame(f, 0),
        icon_frames: (0..icon_count(f)).map(|k| icon_frame(f, k)).collect(),
        slot: f[SLOT..SLOT + SLOT_BYTES].to_vec(),
        header_ok,
        backup_ok,
        backup_equal: f[0x400..0x480] == f[0x480..0x500],
        header_magic: le32(f, 0x400),
        header_version: le32(f, 0x404),
        header_sum: le32(f, 0x400 + 0x7C),
        body_sum,
        body_sum_fresh: body_sum_with(f, 0),
        level: le16(f, SLOT + 2),
        location_index: f[SLOT + 0x28],
        flags_set,
        toys_owned,
        rubies: le32(sb, 0x4C),
        third_name: ascii(&sb[0x50..0x5C]),
        map: le16(sb, 0x5C),
        position: (le16(sb, 0x5E) as i16, le16(sb, 0x60) as i16, le16(sb, 0x62) as i16),
        objective: sb[0x68..0xD0].to_vec(),
        play_frames: le32(sb, 0xD0),
        steps: le32(sb, 0xD4),
        location: ascii(&sb[0xD8..0xEC]),
        key_items: slots(0xF8),
        items: slots(0x120),
        party,
    })
}

/// Words summed by the writer: `MemCard_ComputeChecksum(slot, 0xBF)` runs
/// `while (--n != -1)`, i.e. 0xBF words, one past the sum field at +0x2F4.
pub const BODY_SUM_WORDS: usize = 0xBF;

/// `~Σ u32[0..0xBF]` over the slot with the stored sum replaced by `prev`.
pub fn body_sum_with(f: &[u8], prev: u32) -> u32 {
    let mut sum = 0u32;
    for i in 0..BODY_SUM_WORDS {
        let at = SLOT + i * 4;
        let w = if at == SLOT + 0x2F4 { prev } else { le32(f, at) };
        sum = sum.wrapping_add(w);
    }
    !sum
}

fn game_header_ok(h: &[u8]) -> bool {
    if le32(h, 0) != HEADER_MAGIC {
        return false;
    }
    let sum = (0..31).fold(0u32, |s, i| s.wrapping_add(le32(h, i * 4)));
    !sum == le32(h, 31 * 4)
}

fn member(r: &[u8]) -> PartyMember {
    PartyMember {
        name: ascii(&r[0..12]),
        hp: le16(r, 0x0C),
        pp: le16(r, 0x0E),
        max_hp: le16(r, 0x10),
        max_pp: le16(r, 0x12),
        level: le16(r, 0x14),
        agility: le16(r, 0x16),
        strength: le16(r, 0x18),
        power: le16(r, 0x1A),
        pp_growth: le16(r, 0x1C),
        luck: r[0x1E],
        weapon_element: r[0x1F],
        affinity: le32(r, 0x20),
        exp: le32(r, 0x24) as i32,
    }
}

/// Icon frames shown by the BIOS: low nibble of header byte 2 (0x11–0x13).
fn icon_count(f: &[u8]) -> usize {
    ((f[2] & 0x0F) as usize).clamp(1, 3)
}

/// Icon frame `k`: 16×16 4bpp at 0x80 + 0x80·k through the CLUT at 0x60.
fn icon_frame(f: &[u8], k: usize) -> Vec<u8> {
    let pal: Vec<[u8; 4]> = (0..16).map(|i| bgr555_to_rgba(le16(f, 0x60 + i * 2))).collect();
    let mut out = Vec::with_capacity(16 * 16 * 4);
    let at = 0x80 + k * 0x80;
    for &b in &f[at..at + 128] {
        out.extend_from_slice(&pal[(b & 15) as usize]);
        out.extend_from_slice(&pal[(b >> 4) as usize]);
    }
    out
}

fn ascii(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b[..end].iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '·' }).collect::<String>().trim_end().to_string()
}

/// Full-width SJIS → ASCII for the characters titles use; others become `·`.
pub fn sjis_title(b: &[u8]) -> String {
    let mut s = String::new();
    for pair in b.chunks_exact(2) {
        let c = u16::from_be_bytes([pair[0], pair[1]]);
        s.push(match c {
            0x8140 => ' ',
            0x8146 => ':',
            0x8144 => '.',
            0x8149 => '!',
            0x815E => '/',
            0x817C => '-',
            0x824F..=0x8258 => (b'0' + (c - 0x824F) as u8) as char,
            0x8260..=0x8279 => (b'A' + (c - 0x8260) as u8) as char,
            0x8281..=0x829A => (b'a' + (c - 0x8281) as u8) as char,
            0x0000 => break,
            _ => '·',
        });
    }
    s.trim_end().to_string()
}

/// `nn` from `BASLUSP00811KNIGHTnn` (or another release's name; 0 if the name has no number).
pub fn file_no(name: &str) -> u32 {
    Release::ALL.iter().find_map(|r| name.strip_prefix(r.info().save_prefix)).and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// Full-width SJIS → Unicode full-width for the characters titles use.
pub fn sjis_fullwidth(b: &[u8]) -> String {
    let mut s = String::new();
    for pair in b.chunks_exact(2) {
        let c = u16::from_be_bytes([pair[0], pair[1]]);
        let u = match c {
            0x0000 => break,
            0x8140 => 0x3000,
            0x8143 => 0xFF0C,
            0x8144 => 0xFF0E,
            0x8145 => 0x30FB,
            0x8146 => 0xFF1A,
            0x8148 => 0xFF1F,
            0x8149 => 0xFF01,
            0x815E => 0xFF0F,
            0x8166 => 0x2019,
            0x817B => 0xFF0B,
            0x817C => 0xFF0D,
            0x8195 => 0xFF06,
            0x824F..=0x8258 => 0xFF10 + (c - 0x824F) as u32,
            0x8260..=0x8279 => 0xFF21 + (c - 0x8260) as u32,
            0x8281..=0x829A => 0xFF41 + (c - 0x8281) as u32,
            _ => 0x30FB,
        };
        s.push(char::from_u32(u).unwrap_or('・'));
    }
    s
}

/* ───────────────────────── Writing ───────────────────────── */

/// What the save editor changes. Everything else in the slot round-trips.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveEdit {
    pub knight_name: String,
    pub baby_name: String,
    pub third_name: String,
    pub rubies: u32,
    pub play_frames: u32,
    pub steps: u32,
    pub map: u16,
    pub x: i16,
    pub h: i16,
    pub z: i16,
    pub save_point: u8,
    /// 16 words of `g_EventFlags`, then a HackerGC base's extra words
    /// ([`flag_word_offset`]; at most [`flag_words`]).
    pub flags: Vec<u32>,
    /// 3 words of owned Living Toys.
    pub toys: Vec<u32>,
    /// 20 carried items.
    pub items: Vec<EditItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditItem {
    pub equipped: bool,
    pub id: u8,
}

fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_str(b: &mut [u8], o: usize, n: usize, s: &str) -> Result<(), String> {
    if s.len() > n || !s.bytes().all(|c| (0x20..0x7F).contains(&c)) {
        return Err(format!("“{s}” must be at most {n} printable ASCII characters"));
    }
    b[o..o + n].fill(0);
    b[o..o + s.len()].copy_from_slice(s.as_bytes());
    Ok(())
}

/// Writes the edited fields into a 0x300-byte slot, mirroring the slot-header
/// copies `MemCard_BuildSaveHeader` makes (name at +0x08, play time at +0x04,
/// save point at +0x28).
pub fn apply(slot: &mut [u8], e: &SaveEdit) -> Result<(), String> {
    apply_words(slot, e, flag_words())
}

/// [`apply`] taking up to `words` flag words.
fn apply_words(slot: &mut [u8], e: &SaveEdit, words: usize) -> Result<(), String> {
    if slot.len() < SLOT_BYTES {
        return Err("slot is shorter than 0x300 bytes".into());
    }
    if !(16..=words.max(16)).contains(&e.flags.len()) || e.toys.len() != 3 || e.items.len() != 20 {
        return Err(format!("expected 16 to {} flag words, 3 toy words and 20 item slots", words.max(16)));
    }
    if e.save_point as usize >= SAVE_POINTS.len() {
        return Err(format!("save point {} is not one of the {} in OVR05", e.save_point, SAVE_POINTS.len()));
    }
    put_str(slot, SB + 0x148, 12, &e.knight_name)?;
    put_str(slot, 0x08, 12, &e.knight_name)?;
    put_str(slot, SB + 0x148 + 0x28, 12, &e.baby_name)?;
    put_str(slot, SB + 0x50, 12, &e.third_name)?;
    put32(slot, SB + 0x4C, e.rubies);
    put32(slot, SB + 0xD0, e.play_frames);
    put32(slot, 0x04, e.play_frames);
    put32(slot, SB + 0xD4, e.steps);
    put16(slot, SB + 0x5C, e.map);
    put16(slot, SB + 0x5E, e.x as u16);
    put16(slot, SB + 0x60, e.h as u16);
    put16(slot, SB + 0x62, e.z as u16);
    slot[SB + 0xF7] = e.save_point;
    slot[0x28] = e.save_point;
    for (i, w) in e.flags.iter().enumerate() {
        put32(slot, SB + flag_word_offset(i).expect("flag word"), *w);
    }
    for (i, w) in e.toys.iter().enumerate() {
        put32(slot, SB + 0x40 + 4 * i, *w);
    }
    for (k, it) in e.items.iter().enumerate() {
        slot[SB + 0x120 + 2 * k] = it.equipped as u8;
        slot[SB + 0x121 + 2 * k] = it.id;
    }
    Ok(())
}

/// `MemCard_FormatFullwidthDigits`: zero-padded to `width`, never truncated.
fn fw_digits(out: &mut Vec<u8>, v: u32, width: usize) {
    let digits = v.to_string();
    for _ in digits.len()..width {
        out.extend_from_slice(&[0x82, 0x4F]);
    }
    for d in digits.bytes() {
        out.extend_from_slice(&[0x82, d + 0x1F]);
    }
}

/// `MemCard_ConvertAsciiToFullwidth`: stops at the first unsupported byte;
/// returns how many characters it wrote.
fn fw_ascii(out: &mut Vec<u8>, s: &[u8], len: usize) -> usize {
    let mut n = 0;
    for &c in s.iter().take(len) {
        let pair = match c {
            b'0'..=b'9' | b'A'..=b'Z' => [0x82, c + 0x1F],
            b'a'..=b'z' => [0x82, c + 0x20],
            b' ' => [0x81, 0x40],
            b'!' => [0x81, 0x49],
            b'&' => [0x81, 0x95],
            b',' => [0x81, 0x43],
            b'.' => [0x81, 0x44],
            b'+' => [0x81, 0x7B],
            b'-' => [0x81, 0x7C],
            b'|' => [0x81, 0x45],
            b'\'' => [0x81, 0x66],
            b'?' => [0x81, 0x48],
            _ => break,
        };
        out.extend_from_slice(&pair);
        n += 1;
    }
    n
}

/// The 64-byte SJIS card title, built from the slot header the way
/// `func_8015447C` does: `ＧＣ　[　]n　<name ×8><save point ×12>[　]hh：mm`.
pub fn title_for(slot: &[u8], file_no: u32) -> Vec<u8> {
    const SPACE: [u8; 2] = [0x81, 0x40];
    let mut t = vec![0x82, 0x66, 0x82, 0x62, 0x81, 0x40];
    if file_no < 9 {
        t.extend_from_slice(&SPACE);
    }
    fw_digits(&mut t, file_no + 1, 1);
    t.extend_from_slice(&SPACE);
    let name: Vec<u8> = slot[0x08..0x14].iter().copied().take_while(|&c| c != 0).collect();
    for _ in fw_ascii(&mut t, &name, 7)..8 {
        t.extend_from_slice(&SPACE);
    }
    let point = SAVE_POINTS.get(slot[0x28] as usize).copied().unwrap_or("");
    for _ in fw_ascii(&mut t, point.as_bytes(), 11)..12 {
        t.extend_from_slice(&SPACE);
    }
    let frames = le32(slot, 0x04).min(0x0CDF_E5C4);
    let (h, m) = (frames / 216_000, (frames % 216_000) / 3600);
    if h < 99 {
        t.extend_from_slice(&SPACE);
    }
    fw_digits(&mut t, h, 2);
    t.extend_from_slice(&[0x81, 0x46]);
    fw_digits(&mut t, m, 2);
    t.resize(64, 0);
    t
}

/// Rewrites one save file (8 KB) on a card image in place: applies `edit` to
/// its slot, regenerates the title and stores a fresh body sum. The directory
/// frame, game header and its backup are untouched.
pub fn write_save(card: &mut [u8], block: usize, edit: Option<&SaveEdit>) -> Result<(), String> {
    if card.len() != CARD_BYTES || !(1..16).contains(&block) {
        return Err("not a 128 KB card or no such block".into());
    }
    let frame = &card[block * 128..block * 128 + 128];
    let name = ascii(&frame[0x0A..0x0A + 21]);
    if le32(frame, 0) != 0x51 || !name.starts_with(NAME_PREFIX) {
        return Err(format!("block {block} does not hold a Guardian's Crusade save"));
    }
    let f = &mut card[block * BLOCK..(block + 1) * BLOCK];
    if let Some(e) = edit {
        apply(&mut f[SLOT..SLOT + SLOT_BYTES], e)?;
    }
    let title = title_for(&f[SLOT..SLOT + SLOT_BYTES], file_no(&name));
    f[4..0x44].copy_from_slice(&title);
    let sum = body_sum_with(f, 0);
    put32(f, SLOT + 0x2F4, sum);
    Ok(())
}

/// A single-save `.mcs` file: the 128-byte directory frame, then the 8 KB file.
pub fn to_mcs(card: &[u8], block: usize) -> Vec<u8> {
    let mut out = card[block * 128..block * 128 + 128].to_vec();
    out.extend_from_slice(&card[block * BLOCK..(block + 1) * BLOCK]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_s_extra_flags_live_in_the_save_block_s_spare_bytes() {
        let mut f = vec![0u8; BLOCK];
        f[0..2].copy_from_slice(b"SC");
        put32(&mut f, SAVEBLOCK + 0x1B4, 1 << 16); // flag 0x210
        put32(&mut f, SAVEBLOCK + 0x1C0 + 12, 1 << 31); // flag 0x29F
        assert!(parse_save_words(1, "x".into(), &f, 16).unwrap().flags_set.is_empty(), "retail has 512 flags");
        assert_eq!(parse_save_words(1, "x".into(), &f, 21).unwrap().flags_set, [0x210, 0x29F]);
        let mut slot = f[SLOT..SLOT + SLOT_BYTES].to_vec();
        let mut words = vec![0u32; 21];
        words[17] = 1; // flag 0x220
        let e = SaveEdit {
            knight_name: String::new(),
            baby_name: String::new(),
            third_name: String::new(),
            rubies: 0,
            play_frames: 0,
            steps: 0,
            map: 0,
            x: 0,
            h: 0,
            z: 0,
            save_point: 0,
            flags: words,
            toys: vec![0; 3],
            items: (0..20).map(|_| EditItem { equipped: false, id: 0 }).collect(),
        };
        assert!(apply_words(&mut slot, &e, 16).is_err(), "retail takes 16 words");
        apply_words(&mut slot, &e, 21).unwrap();
        assert_eq!((le32(&slot, SB + 0x1B4), le32(&slot, SB + 0x1C0)), (0, 1));
    }

    #[test]
    fn digits_and_ascii() {
        let mut v = Vec::new();
        fw_digits(&mut v, 10, 1);
        assert_eq!(v, [0x82, 0x50, 0x82, 0x4F]);
        let mut v = Vec::new();
        assert_eq!(fw_ascii(&mut v, b"Ab_c", 7), 2);
        assert_eq!(v, [0x82, 0x60, 0x82, 0x82]);
    }
}
