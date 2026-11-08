//! Camera flyovers: the flight table a flyover overlay reads (the `jp-prologue` pack's OVR25,
//! docs/research/10-jp-prologue.md and the decomp's docs/JP_PROLOGUE.md).
//!
//! The camera starts at waypoint 0 and flies to waypoints 1 .. count-1 over the overworld, steering
//! for each in turn. While it flies, it looks the way the waypoint it is heading for says: a pitch
//! (1024 looks straight down), a compass yaw and a roll, 4096 to a turn, eased in. A waypoint's mode
//! applies to the leg that ends at it. A non-zero page fades the page on screen out and that page in.
//!
//! Layout (little-endian), from the four-byte magic:
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 4 | magic |
//! | 4 | 1 | version (1) |
//! | 5 | 1 | waypoints in use |
//! | 6 | 1 | waypoint room |
//! | 7 | 1 | lines a page holds |
//! | 8 | 2 | caption room in bytes |
//! | 10 | 2 | pad |
//! | 12 | 16 × room | waypoints: mode, page, chunk u16, x, h, z (s16, chunk-relative, h up is negative), pitch, yaw, roll (u16) |
//! | … | caption room | pages, packed: each line ends in a zero, an empty line ends a page, an empty page ends the list |
use serde::{Deserialize, Serialize};

const HEADER: usize = 12;
const WAYPOINT: usize = 16;
const VERSION: u8 = 1;

/// The leg modes the flight task knows (`Mod_PrologueTask`), for the leg that ends at a waypoint.
/// (Mode 1 turns the next node's yaw, which arriving overwrites; nothing to offer.)
pub const MODES: [u8; 4] = [MODE_FLY, MODE_SWAY, MODE_SHARP, MODE_LOOK_AROUND];
/// Steer for the waypoint at 10 units a frame.
pub const MODE_FLY: u8 = 0;
/// As fly, weaving north and south.
pub const MODE_SWAY: u8 = 2;
/// As fly, steering half as fast again.
pub const MODE_SHARP: u8 = 3;
/// Stop, rise and look all the way round, then fly on.
pub const MODE_LOOK_AROUND: u8 = 5;

/// The screen captions are centred on, and the margin a line keeps.
pub const SCREEN_WIDTH: u32 = 512;
pub const LINE_MAX_PX: u32 = SCREEN_WIDTH - 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Waypoint {
    pub mode: u8,
    /// The caption page that fades in on the way here (1-based; 0 keeps the page on screen).
    pub page: u8,
    /// The overworld chunk `x`/`z` are relative to.
    pub chunk: u16,
    pub x: i16,
    /// Height; up is negative.
    pub h: i16,
    pub z: i16,
    /// The view's pitch (1024 looks straight down), yaw and roll, 4096 to a turn.
    pub pitch: u16,
    pub yaw: u16,
    pub roll: u16,
}

/// A flight: its waypoints and caption pages (`pages[p][l]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flyover {
    pub waypoints: Vec<Waypoint>,
    pub pages: Vec<Vec<String>>,
}

/// The room a table reserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Room {
    pub waypoints: usize,
    /// Bytes for the packed pages, the list's end included.
    pub text_bytes: usize,
    /// Lines a page can hold.
    pub lines: usize,
}

impl Room {
    /// Bytes the whole table takes, header included.
    pub fn table_len(&self) -> usize {
        HEADER + self.waypoints * WAYPOINT + self.text_bytes
    }
}

/// A flyover table found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Offset of the magic.
    pub at: usize,
    pub room: Room,
    pub flyover: Flyover,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

/// The flyover table after `magic` in `bytes`.
pub fn find(bytes: &[u8], magic: &str) -> Option<Table> {
    let m = magic.as_bytes();
    let at = bytes.windows(m.len()).position(|w| w == m)?;
    let h = bytes.get(at..at + HEADER)?;
    if h[4] != VERSION {
        return None;
    }
    let (count, room) = (h[5] as usize, Room { waypoints: h[6] as usize, text_bytes: u16_at(h, 8) as usize, lines: h[7] as usize });
    if count > room.waypoints || room.lines == 0 || bytes.len() < at + room.table_len() {
        return None;
    }
    let wp = &bytes[at + HEADER..];
    let waypoints = (0..count)
        .map(|i| {
            let w = &wp[i * WAYPOINT..(i + 1) * WAYPOINT];
            let s = |o: usize| u16_at(w, o);
            Waypoint { mode: w[0], page: w[1], chunk: s(2), x: s(4) as i16, h: s(6) as i16, z: s(8) as i16, pitch: s(10), yaw: s(12), roll: s(14) }
        })
        .collect();
    let text = &bytes[at + HEADER + room.waypoints * WAYPOINT..at + room.table_len()];
    Some(Table { at, room, flyover: Flyover { waypoints, pages: unpack_pages(text) } })
}

fn unpack_pages(text: &[u8]) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut i = 0;
    while i < text.len() && text[i] != 0 {
        let mut page = Vec::new();
        while i < text.len() && text[i] != 0 {
            let end = text[i..].iter().position(|&b| b == 0).map_or(text.len(), |e| i + e);
            page.push(String::from_utf8_lossy(&text[i..end]).into_owned());
            i = end + 1;
        }
        pages.push(page);
        i += 1;
    }
    pages
}

/// Bytes `pages` take packed, the list's end included.
pub fn text_len(pages: &[Vec<String>]) -> usize {
    pages.iter().map(|p| p.iter().map(|l| l.len() + 1).sum::<usize>() + 1).sum::<usize>() + 1
}

/// The credits font's characters: space, `!` to `` ` `` (capitals, digits, punctuation) and `a`-`z`.
pub fn caption_char(c: char) -> bool {
    c == ' ' || ('!'..='`').contains(&c) || c.is_ascii_lowercase()
}

/// Ink columns {first, last} of the credits font's 16×16 cells for `!` to `` ` `` (`Mod_Ink`).
const INK: [[u8; 2]; 64] = [
    [4, 11], [4, 12], [0, 15], [2, 14], [0, 15], [1, 15], [5, 10], [4, 12],
    [3, 11], [2, 13], [2, 13], [5, 11], [2, 13], [2, 6], [3, 13], [1, 14],
    [3, 12], [2, 14], [2, 13], [1, 14], [2, 13], [2, 14], [1, 15], [1, 14],
    [2, 14], [5, 10], [4, 10], [3, 11], [3, 12], [4, 12], [1, 14], [0, 15],
    [0, 15], [2, 14], [1, 15], [1, 14], [1, 14], [1, 14], [1, 15], [1, 14],
    [5, 10], [0, 15], [1, 15], [2, 13], [0, 15], [0, 15], [1, 14], [0, 14],
    [0, 15], [0, 15], [1, 14], [0, 14], [0, 14], [0, 15], [0, 15], [0, 15],
    [1, 14], [0, 15], [1, 11], [2, 12], [4, 11], [2, 13], [2, 14], [3, 10],
];

/// Pixels a caption line takes on screen (`Mod_GlyphPlace`): capitals and punctuation by their ink
/// plus one, lowercase 12, spaces 8.
pub fn line_width(line: &str) -> u32 {
    line.bytes()
        .map(|c| match c {
            0x20 => 8,
            0x61.. => 12,
            0x21..=0x60 => {
                let [a, b] = INK[(c - 0x21) as usize];
                (b - a) as u32 + 2
            }
            _ => 8,
        })
        .sum()
}

/// Why a flyover doesn't fit its table, the font or the world.
pub fn check(f: &Flyover, room: &Room, chunk_ok: &dyn Fn(u16) -> bool) -> Result<(), String> {
    let n = f.waypoints.len();
    if n < 2 || n > room.waypoints {
        return Err(format!("A flyover needs 2 to {} waypoints.", room.waypoints));
    }
    if f.pages.len() > u8::MAX as usize {
        return Err("Too many caption pages.".into());
    }
    for (p, page) in f.pages.iter().enumerate() {
        if page.is_empty() || page.len() > room.lines {
            return Err(format!("Page {} needs 1 to {} lines.", p + 1, room.lines));
        }
        for (l, line) in page.iter().enumerate() {
            let at = format!("Page {}, line {}", p + 1, l + 1);
            if line.is_empty() {
                return Err(format!("{at} is empty; an empty line ends the page."));
            }
            if let Some(c) = line.chars().find(|&c| !caption_char(c)) {
                return Err(format!("{at}: the caption font has no {c:?}."));
            }
            if line_width(line) > LINE_MAX_PX {
                return Err(format!("{at} is wider than the screen."));
            }
        }
    }
    let used = text_len(&f.pages);
    if used > room.text_bytes {
        return Err(format!("The pages take {used} bytes; the flyover has room for {}.", room.text_bytes));
    }
    for (i, w) in f.waypoints.iter().enumerate() {
        if !MODES.contains(&w.mode) {
            return Err(format!("Waypoint {i}: no leg mode {}.", w.mode));
        }
        if w.page as usize > f.pages.len() {
            return Err(format!("Waypoint {i} shows page {}; there are {} pages.", w.page, f.pages.len()));
        }
        if !chunk_ok(w.chunk) {
            return Err(format!("Waypoint {i}: chunk {} isn't part of the overworld.", w.chunk));
        }
        if w.pitch > 0xFFF || w.yaw > 0xFFF || w.roll > 0xFFF {
            return Err(format!("Waypoint {i}: angles run 0 to 4095."));
        }
    }
    Ok(())
}

/// Writes `f` over the table at `t.at` in `bytes` (checked first by [`check`]); the unused room is
/// zeroed.
pub fn write(bytes: &mut [u8], t: &Table, f: &Flyover) {
    let (at, room) = (t.at, t.room);
    bytes[at + 5] = f.waypoints.len() as u8;
    let wp = &mut bytes[at + HEADER..at + HEADER + room.waypoints * WAYPOINT];
    wp.fill(0);
    for (i, w) in f.waypoints.iter().enumerate() {
        let o = &mut wp[i * WAYPOINT..(i + 1) * WAYPOINT];
        o[0] = w.mode;
        o[1] = w.page;
        for (k, v) in [w.chunk, w.x as u16, w.h as u16, w.z as u16, w.pitch, w.yaw, w.roll].into_iter().enumerate() {
            o[2 + k * 2..4 + k * 2].copy_from_slice(&v.to_le_bytes());
        }
    }
    let text = &mut bytes[at + HEADER + room.waypoints * WAYPOINT..at + room.table_len()];
    text.fill(0);
    let mut i = 0;
    for page in &f.pages {
        for line in page {
            text[i..i + line.len()].copy_from_slice(line.as_bytes());
            i += line.len() + 1;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Vec<u8> {
        let mut b = vec![0xAAu8; 6];
        b.extend_from_slice(b"LTFY");
        b.extend_from_slice(&[1, 2, 3, 2, 24, 0, 0, 0]);
        for w in [[0u8, 0, 36, 0, 0x10, 0, 0x9C, 0xFF, 0x20, 0, 0, 4, 0, 0, 0, 0], [5, 1, 78, 0, 0xF0, 0xFF, 0, 0, 0, 0, 0, 2, 0, 8, 0, 0]] {
            b.extend_from_slice(&w);
        }
        b.extend_from_slice(&[0; 16]);
        let mut text = b"Hi\0there\0\0Two\0\0\0".to_vec();
        text.resize(24, 0);
        b.extend_from_slice(&text);
        b.extend_from_slice(&[0xBB; 3]);
        b
    }

    #[test]
    fn a_table_reads_back() {
        let b = table();
        let t = find(&b, "LTFY").unwrap();
        assert_eq!(t.at, 6);
        assert_eq!(t.room, Room { waypoints: 3, text_bytes: 24, lines: 2 });
        assert_eq!(t.flyover.pages, vec![vec!["Hi".to_string(), "there".into()], vec!["Two".into()]]);
        let w = t.flyover.waypoints[1];
        assert_eq!((w.mode, w.page, w.chunk, w.x, w.h, w.z, w.pitch, w.yaw), (5, 1, 78, -16, 0, 0, 512, 2048));
        assert_eq!(t.flyover.waypoints[0].h, -100);
        assert_eq!(text_len(&t.flyover.pages), 3 + 6 + 1 + 4 + 1 + 1);
    }

    #[test]
    fn a_table_writes_and_reads_back() {
        let mut b = table();
        let t = find(&b, "LTFY").unwrap();
        let mut f = t.flyover.clone();
        f.waypoints.push(Waypoint { mode: 3, page: 2, chunk: 80, x: 1, h: -2, z: 3, pitch: 4, yaw: 5, roll: 0 });
        f.pages = vec![vec!["One".into()], vec!["Two".into(), "lines".into()]];
        check(&f, &t.room, &|c| c >= 36).unwrap();
        write(&mut b, &t, &f);
        assert_eq!(find(&b, "LTFY").unwrap().flyover, f);
        assert_eq!(&b[..6], &[0xAA; 6]);
        assert_eq!(&b[b.len() - 3..], &[0xBB; 3]);
    }

    #[test]
    fn checks_refuse_what_the_game_cant_show() {
        let t = find(&table(), "LTFY").unwrap();
        let ok = |c: u16| c >= 36;
        let with = |g: &dyn Fn(&mut Flyover)| {
            let mut f = t.flyover.clone();
            g(&mut f);
            check(&f, &t.room, &ok)
        };
        assert!(with(&|_| {}).is_ok());
        assert!(with(&|f| f.waypoints.truncate(1)).is_err());
        assert!(with(&|f| f.waypoints[1].page = 3).is_err());
        assert!(with(&|f| f.waypoints[1].mode = 4).is_err());
        assert!(with(&|f| f.waypoints[1].chunk = 7).is_err());
        assert!(with(&|f| f.pages[0][0] = "no {brace}".into()).is_err());
        assert!(with(&|f| f.pages[0] = vec!["a".into(), "b".into(), "c".into()]).is_err());
        assert!(with(&|f| f.pages.push(vec!["far too many bytes".into()])).is_err());
        assert!(with(&|f| f.pages[1][0] = "M".repeat(40)).is_err());
    }

    #[test]
    fn lines_measure_like_the_game() {
        assert_eq!(line_width("a b"), 12 + 8 + 12);
        assert_eq!(line_width("I"), 10 - 5 + 2);
    }
}
