//! Dialogue text: plain ASCII with control codes, drawn with the 8×12 SYS14
//! font. Spec: `docs/screens/12-text-dialogue-editor.md` §4.2–4.4.
//!
//! The Japanese release (Knight & Baby) has the same codes around plain Shift-JIS text. It is
//! decoded for display only: [`decode`] reads the open disc's [`Encoding`]; the editor's markup,
//! line widths and encoder are ASCII (USA) only.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::Serialize;

/// How a release's text is encoded ([`crate::release::ReleaseInfo::text`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Encoding {
    /// USA and EU: ASCII; a byte ≥ 0x80 starts a two-byte glyph pair, kept raw.
    #[default]
    Ascii,
    /// Japan: Shift-JIS; a valid two-byte character decodes to its Unicode.
    ShiftJis,
}

static ENCODING: AtomicU8 = AtomicU8::new(0);

/// Makes `e` what [`decode`] reads: the open disc's encoding. Living Tool has one disc open at a
/// time and sets it when that disc opens.
pub fn set_encoding(e: Encoding) {
    ENCODING.store(e as u8, Ordering::Relaxed);
}

/// The open disc's text encoding (ASCII until a Japanese disc opens).
pub fn encoding() -> Encoding {
    match ENCODING.load(Ordering::Relaxed) {
        1 => Encoding::ShiftJis,
        _ => Encoding::Ascii,
    }
}

/// One two-byte Shift-JIS character, if `pair` is one.
pub fn sjis_char(pair: &[u8]) -> Option<char> {
    let (s, bad) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(pair);
    let mut it = s.chars();
    match (bad, it.next(), it.next()) {
        (false, Some(c), None) if pair.len() == 2 && !c.is_ascii() => Some(c),
        _ => None,
    }
}

/// Shift-JIS bytes as text (U+FFFD for what isn't Shift-JIS).
pub fn sjis(b: &[u8]) -> String {
    encoding_rs::SHIFT_JIS.decode_without_bom_handling(b).0.into_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "t", content = "v")]
pub enum Tok {
    Text(String),
    /// `05 n 00`: open a box at position/style n.
    Box(u8),
    /// `04 05 n 00`: close the box and open the next.
    BoxBreak(u8),
    /// `06`: wait for Cross, next page.
    Wait,
    /// `07`: newline.
    Nl,
    /// `$A`…`$H`: Knight name, Baby name, other inserts.
    Name(char),
    /// `#nnn`: item name.
    Item(u16),
    /// `~nn`: Living Toy name.
    Toy(u8),
    /// Bytes kept verbatim (≥ 0x80 pairs, malformed codes). Never lossy.
    Raw(Vec<u8>),
}

/// Reads the string at `at` up to its terminating NUL (excluded). The `05 n 00`
/// box code contains a NUL of its own, so codes are stepped over, not scanned.
pub fn c_string(b: &[u8], at: usize) -> &[u8] {
    let rest = b.get(at..).unwrap_or(&[]);
    let mut i = 0usize;
    while i < rest.len() {
        match rest[i] {
            0x00 => break,
            0x05 => i += 3,
            0x04 if rest.get(i + 1) == Some(&0x05) => i += 4,
            c if c >= 0x80 => i += 2,
            _ => i += 1,
        }
    }
    &rest[..i.min(rest.len())]
}

/// Decodes string bytes (no trailing NUL) into tokens, in the open disc's [`encoding`].
pub fn decode(b: &[u8]) -> Vec<Tok> {
    decode_with(b, encoding())
}

/// [`decode`] in a given encoding.
pub fn decode_with(b: &[u8], enc: Encoding) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Tok>| {
        if !text.is_empty() {
            out.push(Tok::Text(std::mem::take(text)));
        }
    };
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        match c {
            0x04 if b.get(i + 1) == Some(&0x05) && i + 3 < b.len() + 1 => {
                flush(&mut text, &mut out);
                out.push(Tok::BoxBreak(*b.get(i + 2).unwrap_or(&0)));
                i += 4;
            }
            0x05 => {
                flush(&mut text, &mut out);
                out.push(Tok::Box(*b.get(i + 1).unwrap_or(&0)));
                i += 3;
            }
            0x06 => {
                flush(&mut text, &mut out);
                out.push(Tok::Wait);
                i += 1;
            }
            0x07 => {
                flush(&mut text, &mut out);
                out.push(Tok::Nl);
                i += 1;
            }
            b'$' if matches!(b.get(i + 1), Some(b'A'..=b'H')) => {
                flush(&mut text, &mut out);
                out.push(Tok::Name(b[i + 1] as char));
                i += 2;
            }
            b'#' if b.get(i + 1..i + 4).is_some_and(|d| d.iter().all(u8::is_ascii_digit)) => {
                flush(&mut text, &mut out);
                let n = std::str::from_utf8(&b[i + 1..i + 4]).unwrap().parse().unwrap();
                out.push(Tok::Item(n));
                i += 4;
            }
            b'~' if b.get(i + 1..i + 3).is_some_and(|d| d.iter().all(u8::is_ascii_digit)) => {
                flush(&mut text, &mut out);
                let n = std::str::from_utf8(&b[i + 1..i + 3]).unwrap().parse().unwrap();
                out.push(Tok::Toy(n));
                i += 3;
            }
            0x20..=0x7E => {
                text.push(c as char);
                i += 1;
            }
            0x80..=0xFF if enc == Encoding::ShiftJis && b.get(i..i + 2).and_then(sjis_char).is_some() => {
                text.push(sjis_char(&b[i..i + 2]).expect("checked"));
                i += 2;
            }
            _ => {
                flush(&mut text, &mut out);
                let n = if c >= 0x80 { 2 } else { 1 };
                out.push(Tok::Raw(b[i..(i + n).min(b.len())].to_vec()));
                i += n;
            }
        }
    }
    flush(&mut text, &mut out);
    out
}

/// Plain-text rendering for lists: `|` for newlines, `¶` for waits.
pub fn plain(toks: &[Tok]) -> String {
    let mut s = String::new();
    for t in toks {
        match t {
            Tok::Text(x) => s.push_str(x),
            Tok::Nl => s.push('|'),
            Tok::Wait => s.push_str(" ¶ "),
            Tok::Name('A') => s.push_str("$A"),
            Tok::Name('B') => s.push_str("$B"),
            Tok::Name(c) => {
                s.push('$');
                s.push(*c);
            }
            Tok::Item(n) => s.push_str(&format!("#{n:03}")),
            Tok::Toy(n) => s.push_str(&format!("~{n:02}")),
            Tok::Box(_) | Tok::BoxBreak(_) | Tok::Raw(_) => {}
        }
    }
    s.trim().to_string()
}

/// The editor's markup: `{box:n}` for `05 n 00`, `{brk:n}` for `04 05 n 00`,
/// `{wait}` (06), `{nl}` (07), `$A`…`$H`, `#nnn`, `~nn` verbatim, `{brk}` for a
/// bare 04 and `{XX}` for any other byte. Same shape as the screens' mocks.
pub fn markup(toks: &[Tok]) -> String {
    let mut s = String::new();
    for t in toks {
        match t {
            Tok::Text(x) => s.push_str(x),
            Tok::Box(n) => s.push_str(&format!("{{box:{n}}}")),
            Tok::BoxBreak(n) => s.push_str(&format!("{{brk:{n}}}")),
            Tok::Wait => s.push_str("{wait}"),
            Tok::Nl => s.push_str("{nl}"),
            Tok::Name(c) => {
                s.push('$');
                s.push(*c);
            }
            Tok::Item(n) => s.push_str(&format!("#{n:03}")),
            Tok::Toy(n) => s.push_str(&format!("~{n:02}")),
            Tok::Raw(b) if b.as_slice() == [0x04] => s.push_str("{brk}"),
            Tok::Raw(b) => b.iter().for_each(|x| s.push_str(&format!("{{{x:02X}}}"))),
        }
    }
    s
}

/// Markup of the string at `at` in `d` (see [`markup`]).
pub fn markup_at(d: &[u8], at: usize) -> String {
    markup(&decode(c_string(d, at)))
}

/// Width in pixels of one line: 8 px per glyph, `'` 3 px, `^` 12 px. Name
/// inserts count as `name_width` glyphs (worst case 8).
pub fn line_width(toks: &[Tok], name_width: u32) -> u32 {
    toks.iter()
        .map(|t| match t {
            Tok::Text(x) => x.chars().map(|c| match c {
                '\'' => 3,
                '^' => 12,
                _ => 8,
            }).sum(),
            Tok::Name(_) => name_width * 8,
            Tok::Raw(_) => 8,
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_codes() {
        let b = b"\x05\x03\x00Good morning,\x07$A.\x06I'm off\x04\x05\x00\x00~15";
        let toks = decode(b);
        assert_eq!(toks[0], Tok::Box(3));
        assert_eq!(toks[1], Tok::Text("Good morning,".into()));
        assert_eq!(toks[2], Tok::Nl);
        assert_eq!(toks[3], Tok::Name('A'));
        assert!(toks.contains(&Tok::Wait));
        assert!(toks.contains(&Tok::BoxBreak(0)));
        assert_eq!(*toks.last().unwrap(), Tok::Toy(15));
        assert_eq!(line_width(&[Tok::Text("I'm".into())], 8), 19);
        assert_eq!(markup(&toks), "{box:3}Good morning,{nl}$A.{wait}I'm off{brk:0}~15");
    }

    /// Knight & Baby's npc/7: the same codes around Shift-JIS; ASCII keeps the pairs raw.
    #[test]
    fn decodes_shift_jis_for_the_japanese_release() {
        let b = b"\x82\xa8\x82\xcd\x82\xe6\x82\xa4 $A\x07\x83\x4c\x83\x87\x81\x45\x06";
        let toks = decode_with(b, Encoding::ShiftJis);
        assert_eq!(toks[0], Tok::Text("おはよう ".into()));
        assert_eq!(toks[1], Tok::Name('A'));
        assert_eq!(toks[2], Tok::Nl);
        assert_eq!(toks[3], Tok::Text("キョ・".into()));
        assert_eq!(plain(&toks), "おはよう $A|キョ・ ¶");
        assert_eq!(decode_with(b, Encoding::Ascii)[0], Tok::Raw(vec![0x82, 0xa8]));
        assert_eq!(decode(b), decode_with(b, Encoding::Ascii));
        assert_eq!(sjis(b"\x83\x69\x83\x43\x83\x67"), "ナイト");
    }
}
