//! The text form of a Tier-2 program (PRD 11 §5.4): one statement per line,
//! IF cards as `if … { } else { }`, labels for GOTO targets. It maps 1:1 to
//! [`super::ast`], so `parse(print(p))` gives `p` back and the bytes match.
//!
//! ```text
//! // npc/7 · actor 3 · D+0x2024
//! if flag(0x004) {
//!   say @0x0D4C  // "I guess it was just / an illusion..."
//!   set 0x081
//! } else {
//!   cutscene 11  // Knight walks waypoints
//!   say "Good morning!{nl}Wake up, wake up!"
//!   warp facing:0 chunk/78 x:-32 z:-880 flags:0x0201
//! }
//! ```
//!
//! - `@0x1BCC` keeps a string of the pack's pool (by its retail offset); a
//!   quoted string is a new one, in the Text & dialogue markup (`{nl}`,
//!   `{wait}`, `{box:n}`, `$A`, `#nnn`, `~nn`, `{XX}`), with `\"` and `\\`.
//! - Conditions: `flag(f)`, `yes`, `rubies(n)`, `item(i)`, `item_alt(i)`,
//!   `choice(text)`; `if !cond { … }` runs its body when the condition fails.
//! - Operands are positional; `name:value` is accepted and checked. Chunks are
//!   written `chunk/78`. The final `end` is implied.
//! - `wipe_scratch` is `set 0x0FD`. `block_begin`/`block_else`/`block_end`
//!   are the raw markers, printed only where retail blocks don't pair up.

use serde::Serialize;

use super::ast::{by_name, cond_name, cond_op, mnemonic, CodeRef, Kind, Stmt, TextRef, Val, MNEMONICS};
use crate::text;

/* ───────────────────────── Diagnostics ───────────────────────── */

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A problem in a program. `line`/`col` are 1-based (0 = the whole program).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diag {
    pub line: u32,
    pub col: u32,
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

impl Diag {
    pub fn error(line: u32, col: u32, code: &'static str, message: impl Into<String>) -> Self {
        Self { line, col, severity: Severity::Error, code, message: message.into() }
    }

    pub fn warning(line: u32, code: &'static str, message: impl Into<String>) -> Self {
        Self { line, col: 0, severity: Severity::Warning, code, message: message.into() }
    }

    pub fn info(line: u32, code: &'static str, message: impl Into<String>) -> Self {
        Self { line, col: 0, severity: Severity::Info, code, message: message.into() }
    }
}

/* ───────────────────────── Printing ───────────────────────── */

/// Names of the canned tasks `D_801FC40C[0..16]` (opcode 0x23).
pub const CUTSCENES: [&str; 16] = [
    "actor approaches the Knight",
    "Knight jumps",
    "full heal",
    "scene setup",
    "world-map pins",
    "world-map pins",
    "actor flickers in",
    "commit the typed name",
    "commit the typed name",
    "scene setup",
    "exit to the field",
    "Knight walks waypoints",
    "commit the typed name",
    "flight scene",
    "flight scene",
    "camera shake",
];

const PREVIEW: usize = 56;

fn quote_bytes(b: &[u8]) -> String {
    let m = text::markup(&text::decode(b));
    let mut s = String::with_capacity(m.len() + 2);
    s.push('"');
    for c in m.chars() {
        if c == '"' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

fn hex4(v: u16) -> String {
    format!("@0x{v:04X}")
}

fn text_val(t: &TextRef) -> String {
    match t {
        TextRef::Pool(o) => hex4(*o),
        TextRef::New(b) => quote_bytes(b),
    }
}

fn num(kind: Kind, v: i64) -> String {
    match kind {
        Kind::Flag => format!("0x{v:03X}"),
        Kind::Chunk => format!("chunk/{v}"),
        Kind::MapId => format!("0x{v:X}"),
        Kind::U16Hex => format!("0x{v:04X}"),
        Kind::U32Hex => format!("0x{v:08X}"),
        _ => v.to_string(),
    }
}

fn val(kind: Kind, v: &Val) -> String {
    match v {
        Val::Num(n) => num(kind, *n),
        Val::Text(t) => text_val(t),
        Val::Code(CodeRef::Label(l)) => l.clone(),
        Val::Code(CodeRef::At(t)) | Val::Walk(t) => hex4(*t),
        Val::Path(p) => {
            let mut s = format!("to({} {} {} {}", p.x, p.h, p.z, p.dir);
            for w in &p.words {
                s.push_str(&format!(" 0x{w:04X}"));
            }
            s.push(')');
            s
        }
    }
}

fn trim_preview(s: String) -> String {
    let flat = s.replace('\n', " / ");
    if flat.chars().count() > PREVIEW {
        format!("{}…", flat.chars().take(PREVIEW - 1).collect::<String>())
    } else {
        flat
    }
}

struct Printer<'a> {
    out: String,
    preview: &'a dyn Fn(u16) -> Option<String>,
}

impl Printer<'_> {
    fn line(&mut self, depth: usize, s: &str, comment: Option<String>) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(s);
        if let Some(c) = comment {
            self.out.push_str("  // ");
            self.out.push_str(&c);
        }
        self.out.push('\n');
    }

    fn text_comment(&self, v: Option<&Val>) -> Option<String> {
        match v {
            Some(Val::Text(TextRef::Pool(o))) => (self.preview)(*o).map(|p| format!("\"{}\"", trim_preview(p))),
            _ => None,
        }
    }

    fn cond(&self, op: u8, arg: Option<&Val>) -> String {
        let name = cond_name(op).unwrap_or("?");
        match (op, arg) {
            (0x0A, _) => name.to_string(),
            (_, Some(v)) => {
                let kind = mnemonic(op).args.first().map(|a| a.1).unwrap_or(Kind::U16);
                format!("{name}({})", val(kind, v))
            }
            _ => format!("{name}()"),
        }
    }

    fn stmts(&mut self, stmts: &[Stmt], depth: usize) {
        for s in stmts {
            self.stmt(s, depth, "");
        }
    }

    /// `lead` is `"} else "` when the statement continues an else-if chain.
    fn stmt(&mut self, s: &Stmt, depth: usize, lead: &str) {
        match s {
            Stmt::Label { name, .. } => self.line(depth.saturating_sub(1), &format!("{name}:"), None),
            Stmt::Op { op, args, .. } => {
                let m = mnemonic(*op);
                if *op == 0x04 && args.first() == Some(&Val::Num(0xFD)) {
                    self.line(depth, "wipe_scratch", Some("flags 0–63 cleared".into()));
                    return;
                }
                let mut s = m.name.to_string();
                let named = m.args.len() > 1;
                for ((name, kind), v) in m.args.iter().zip(args) {
                    s.push(' ');
                    if named && *kind != Kind::Chunk {
                        s.push_str(name);
                        s.push(':');
                    }
                    s.push_str(&val(*kind, v));
                }
                let comment = if *op == 0x23 {
                    match args.first() {
                        Some(Val::Num(n)) => CUTSCENES.get(*n as usize).map(|c| c.to_string()),
                        _ => None,
                    }
                } else {
                    self.text_comment(args.iter().find(|v| matches!(v, Val::Text(_))))
                };
                self.line(depth, &s, comment);
            }
            Stmt::If { op, arg, then, els, .. } => {
                let cond = self.cond(*op, arg.as_ref());
                let comment = self.text_comment(arg.as_ref());
                if then.is_empty() && !els.is_empty() {
                    self.line(depth, &format!("{lead}if !{cond} {{"), comment);
                    self.stmts(els, depth + 1);
                    self.line(depth, "}", None);
                    return;
                }
                self.line(depth, &format!("{lead}if {cond} {{"), comment);
                self.stmts(then, depth + 1);
                match els.as_slice() {
                    [] => self.line(depth, "}", None),
                    [chained @ Stmt::If { .. }] => self.stmt(chained, depth, "} else "),
                    _ => {
                        self.line(depth, "} else {", None);
                        self.stmts(els, depth + 1);
                        self.line(depth, "}", None);
                    }
                }
            }
        }
    }
}

/// The program as text. The final top-level `end` is left out (the assembler
/// adds it back). `preview` gives the current text of a pool string for the
/// trailing comments; `header` becomes a first comment line.
pub fn print(stmts: &[Stmt], header: Option<&str>, preview: &dyn Fn(u16) -> Option<String>) -> String {
    let mut p = Printer { out: String::new(), preview };
    if let Some(h) = header {
        p.line(0, &format!("// {h}"), None);
    }
    let body = match stmts.last() {
        Some(Stmt::Op { op: 0x00, .. }) => &stmts[..stmts.len() - 1],
        _ => stmts,
    };
    p.stmts(body, 0);
    p.out
}

/* ───────────────────────── Lexing ───────────────────────── */

#[derive(Debug, Clone, PartialEq)]
enum Tk {
    Ident(String),
    Num(i64),
    Str(String),
    At(i64),
    Colon,
    Slash,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Bang,
    Semi,
    Newline,
    Eof,
}

#[derive(Debug, Clone)]
struct Token {
    tk: Tk,
    line: u32,
    col: u32,
}

fn parse_int(s: &str) -> Option<i64> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let body = body.replace('_', "");
    let v = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(h) => i64::from_str_radix(h, 16).ok()?,
        None => body.parse::<i64>().ok()?,
    };
    Some(if neg { -v } else { v })
}

fn lex(src: &str) -> Result<Vec<Token>, Diag> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let (mut i, mut line, mut col) = (0usize, 1u32, 1u32);
    while i < chars.len() {
        let c = chars[i];
        let start = (line, col);
        let push = |out: &mut Vec<Token>, tk: Tk| out.push(Token { tk, line: start.0, col: start.1 });
        match c {
            '\n' => {
                push(&mut out, Tk::Newline);
                i += 1;
                line += 1;
                col = 1;
                continue;
            }
            ' ' | '\t' | '\r' => {}
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' => push(&mut out, Tk::Slash),
            ':' => push(&mut out, Tk::Colon),
            '(' => push(&mut out, Tk::LParen),
            ')' => push(&mut out, Tk::RParen),
            '{' => push(&mut out, Tk::LBrace),
            '}' => push(&mut out, Tk::RBrace),
            '!' => push(&mut out, Tk::Bang),
            ';' => push(&mut out, Tk::Semi),
            '"' => {
                let mut s = String::new();
                let mut j = i + 1;
                loop {
                    match chars.get(j) {
                        None | Some('\n') => return Err(Diag::error(start.0, start.1, "DSL_STRING", "This string has no closing quote.")),
                        Some('"') => break,
                        Some('\\') => {
                            match chars.get(j + 1) {
                                Some(&e @ ('"' | '\\')) => s.push(e),
                                _ => return Err(Diag::error(start.0, start.1, "DSL_STRING", "Only \\\" and \\\\ are escapes; use {nl} for a new line.")),
                            }
                            j += 2;
                            continue;
                        }
                        Some(&ch) => s.push(ch),
                    }
                    j += 1;
                }
                push(&mut out, Tk::Str(s));
                col += (j + 1 - i) as u32;
                i = j + 1;
                continue;
            }
            '@' | '-' | '0'..='9' => {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let word: String = chars[i..j].iter().collect();
                let digits = word.trim_start_matches('@');
                let tk = match parse_int(digits) {
                    Some(v) if c == '@' => Tk::At(v),
                    Some(v) => Tk::Num(v),
                    None => return Err(Diag::error(start.0, start.1, "DSL_NUMBER", format!("“{word}” is not a number (use 12, -32 or 0x1F)."))),
                };
                push(&mut out, tk);
                col += (j - i) as u32;
                i = j;
                continue;
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                push(&mut out, Tk::Ident(chars[i..j].iter().collect()));
                col += (j - i) as u32;
                i = j;
                continue;
            }
            other => return Err(Diag::error(start.0, start.1, "DSL_CHAR", format!("Unexpected “{other}”."))),
        }
        i += 1;
        col += 1;
    }
    out.push(Token { tk: Tk::Eof, line, col });
    Ok(out)
}

/* ───────────────────────── Parsing ───────────────────────── */

struct Parser {
    toks: Vec<Token>,
    k: usize,
    diags: Vec<Diag>,
}

type PResult<T> = Result<T, Diag>;

const KEYWORDS: &[&str] = &["if", "else", "wipe_scratch"];
/// Stop after this many errors (the rest are usually knock-on effects).
const MAX_ERRORS: usize = 20;

impl Parser {
    fn peek(&self) -> &Tk {
        &self.toks[self.k].tk
    }

    fn peek_at(&self, n: usize) -> &Tk {
        &self.toks[(self.k + n).min(self.toks.len() - 1)].tk
    }

    fn tok(&self) -> &Token {
        &self.toks[self.k]
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.k].clone();
        if self.k < self.toks.len() - 1 {
            self.k += 1;
        }
        t
    }

    fn err<T>(&self, code: &'static str, message: impl Into<String>) -> PResult<T> {
        let t = self.tok();
        Err(Diag::error(t.line, t.col, code, message))
    }

    fn at_end_of_stmt(&self) -> bool {
        matches!(self.peek(), Tk::Newline | Tk::Semi | Tk::RBrace | Tk::Eof)
    }

    fn skip_blank(&mut self) {
        while matches!(self.peek(), Tk::Newline | Tk::Semi) {
            self.bump();
        }
    }

    /// After an error: drop the rest of the line (an `{` on it still opens a block).
    fn recover(&mut self) -> bool {
        while !matches!(self.peek(), Tk::Newline | Tk::Semi | Tk::Eof | Tk::RBrace) {
            if self.peek() == &Tk::LBrace {
                self.bump();
                return true;
            }
            self.bump();
        }
        false
    }

    fn stmts(&mut self, nested: bool) -> Vec<Stmt> {
        let mut out = Vec::new();
        loop {
            self.skip_blank();
            match self.peek() {
                Tk::Eof => {
                    if nested {
                        let t = self.tok();
                        self.diags.push(Diag::error(t.line, t.col, "DSL_BRACE", "A block is missing its closing “}”."));
                    }
                    return out;
                }
                Tk::RBrace => {
                    if nested {
                        return out;
                    }
                    let t = self.bump();
                    self.diags.push(Diag::error(t.line, t.col, "DSL_BRACE", "This “}” closes nothing."));
                }
                _ => match self.stmt() {
                    Ok(s) => {
                        out.push(s);
                        if !self.at_end_of_stmt() {
                            let t = self.tok().clone();
                            self.diags.push(Diag::error(t.line, t.col, "DSL_EXTRA", "Unexpected text after the statement; put one statement per line."));
                            self.recover();
                        }
                    }
                    Err(d) => {
                        self.diags.push(d);
                        if self.diags.len() >= MAX_ERRORS {
                            return out;
                        }
                        // A block whose header failed still has a body: parse it so its "}" pairs up.
                        if self.recover() {
                            let _ = self.stmts(true);
                            if self.peek() == &Tk::RBrace {
                                self.bump();
                            }
                        }
                    }
                },
            }
        }
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let line = self.tok().line;
        let name = match self.peek().clone() {
            Tk::Ident(n) => n,
            _ => return self.err("DSL_STATEMENT", "Expected a statement (an opcode name, `if`, or `label:`)."),
        };
        if name == "if" {
            return self.if_stmt();
        }
        if self.peek_at(1) == &Tk::Colon && matches!(self.peek_at(2), Tk::Newline | Tk::Semi | Tk::Eof | Tk::RBrace) {
            if by_name(&name).is_some() || KEYWORDS.contains(&name.as_str()) {
                return self.err("DSL_LABEL", format!("“{name}” is an opcode or keyword; pick another label name."));
            }
            self.bump();
            self.bump();
            return Ok(Stmt::Label { name, line });
        }
        let col = self.tok().col;
        self.bump();
        if name == "wipe_scratch" {
            return Ok(Stmt::Op { op: 0x04, args: vec![Val::Num(0xFD)], line });
        }
        let Some(m) = by_name(&name) else {
            let hint = MNEMONICS.iter().find(|m| m.name.starts_with(&name[..name.len().min(3)])).map(|m| format!(" Did you mean `{}`?", m.name)).unwrap_or_default();
            return Err(Diag::error(line, col, "DSL_OPCODE", format!("`{name}` is not an opcode.{hint}")));
        };
        let mut args = Vec::with_capacity(m.args.len());
        for (k, (aname, kind)) in m.args.iter().enumerate() {
            if self.at_end_of_stmt() {
                let list: Vec<&str> = m.args.iter().map(|a| a.0).collect();
                return self.err("DSL_ARITY", format!("`{}` needs {} operand{} ({}).", m.name, m.args.len(), if m.args.len() == 1 { "" } else { "s" }, list.join(" ")));
            }
            if let (Tk::Ident(n), Tk::Colon) = (self.peek().clone(), self.peek_at(1).clone()) {
                if n != *aname {
                    return self.err("DSL_ARG_NAME", format!("`{}` takes `{aname}` as operand {} (not `{n}`).", m.name, k + 1));
                }
                self.bump();
                self.bump();
            }
            args.push(self.value(*kind)?);
        }
        if !self.at_end_of_stmt() {
            return self.err("DSL_ARITY", format!("`{}` takes {} operand{}.", m.name, m.args.len(), if m.args.len() == 1 { "" } else { "s" }));
        }
        Ok(Stmt::Op { op: m.op, args, line })
    }

    fn value(&mut self, kind: Kind) -> PResult<Val> {
        let t = self.tok().clone();
        let (lo, hi) = kind.range();
        let ranged = |v: i64| -> PResult<i64> {
            if v < lo || v > hi {
                Err(Diag::error(t.line, t.col, "DSL_RANGE", format!("{v} is out of range ({lo}–{hi}).")))
            } else {
                Ok(v)
            }
        };
        let d16 = |v: i64, what: &str| -> PResult<u16> {
            if (0..=0xFFFF).contains(&v) {
                Ok(v as u16)
            } else {
                Err(Diag::error(t.line, t.col, "DSL_RANGE", format!("{what} 0x{v:X} is past the u16 operand reach (0xFFFF).")))
            }
        };
        match (kind, t.tk.clone()) {
            (Kind::Text, Tk::At(v)) => {
                self.bump();
                Ok(Val::Text(TextRef::Pool(d16(v, "Text offset")?)))
            }
            (Kind::Text, Tk::Str(s)) => {
                self.bump();
                let bytes = crate::edit::text::encode_markup(&s).map_err(|e| Diag::error(t.line, t.col, e.code, e.message))?;
                Ok(Val::Text(TextRef::New(bytes)))
            }
            (Kind::Text, _) => self.err("DSL_TEXT", "Expected a quoted string or a pool offset like @0x1BCC."),
            (Kind::Code, Tk::Ident(n)) => {
                self.bump();
                Ok(Val::Code(CodeRef::Label(n)))
            }
            (Kind::Code, Tk::At(v)) => {
                self.bump();
                Ok(Val::Code(CodeRef::At(d16(v, "Code offset")?)))
            }
            (Kind::Code, _) => self.err("DSL_CODE", "Expected a label name or a code offset like @0x3ADD."),
            (Kind::Walk, Tk::At(v) | Tk::Num(v)) => {
                self.bump();
                Ok(Val::Walk(d16(v, "Walk offset")?))
            }
            (Kind::Walk, Tk::Ident(n)) if n == "to" => {
                self.bump();
                if self.peek() != &Tk::LParen {
                    return self.err("DSL_WALK", "Write a new walk path as to(x h z dir), for example to(-1600 -129 -856 1024).");
                }
                self.bump();
                let mut nums = Vec::new();
                while let Tk::Num(v) = self.peek().clone() {
                    self.bump();
                    nums.push(v);
                }
                if self.peek() != &Tk::RParen || nums.len() < 4 {
                    return self.err("DSL_WALK", "Write a new walk path as to(x h z dir [walk words…]).");
                }
                self.bump();
                let s16 = |v: i64| -> PResult<i16> {
                    i16::try_from(v).map_err(|_| Diag::error(t.line, t.col, "DSL_RANGE", format!("{v} doesn't fit a walk coordinate.")))
                };
                let u16v = |v: i64| -> PResult<u16> {
                    u16::try_from(v).map_err(|_| Diag::error(t.line, t.col, "DSL_RANGE", format!("{v} doesn't fit a walk word.")))
                };
                let words = nums[4..].iter().map(|&w| u16v(w)).collect::<PResult<Vec<u16>>>()?;
                if words.contains(&0) {
                    return self.err("DSL_WALK", "A walk word of 0 ends the walk; leave it out.");
                }
                Ok(Val::Path(crate::tier2::ast::WalkPath { x: s16(nums[0])?, h: s16(nums[1])?, z: s16(nums[2])?, dir: u16v(nums[3])?, words }))
            }
            (Kind::Walk, _) => self.err("DSL_WALK", "Expected a walk program offset like @0x2F10, or a new path to(x h z dir)."),
            (Kind::Chunk, Tk::Ident(n)) if n == "chunk" => {
                self.bump();
                if self.peek() != &Tk::Slash {
                    return self.err("DSL_CHUNK", "Write chunks as chunk/78.");
                }
                self.bump();
                match self.peek().clone() {
                    Tk::Num(v) => {
                        self.bump();
                        Ok(Val::Num(ranged(v)?))
                    }
                    _ => self.err("DSL_CHUNK", "Write chunks as chunk/78."),
                }
            }
            (_, Tk::Num(v)) => {
                self.bump();
                Ok(Val::Num(ranged(v)?))
            }
            (_, Tk::Str(_)) => self.err("DSL_TYPE", "A string isn't allowed here; this operand is a number."),
            (_, Tk::At(_)) => self.err("DSL_TYPE", "An @offset isn't allowed here; this operand is a number."),
            _ => self.err("DSL_TYPE", "Expected a number."),
        }
    }

    fn cond(&mut self) -> PResult<(u8, Option<Val>)> {
        let name = match self.peek().clone() {
            Tk::Ident(n) => n,
            _ => return self.err("DSL_COND", "Expected a condition: flag(…), yes, rubies(…), item(…), item_alt(…) or choice(…)."),
        };
        let Some(op) = cond_op(&name) else {
            return self.err("DSL_COND", format!("`{name}` is not a condition; use flag(…), yes, rubies(…), item(…), item_alt(…) or choice(…)."));
        };
        self.bump();
        if op == 0x0A {
            return Ok((op, None));
        }
        if self.peek() != &Tk::LParen {
            return self.err("DSL_COND", format!("Write `{name}(…)`."));
        }
        self.bump();
        let kind = mnemonic(op).args[0].1;
        let v = self.value(kind)?;
        if self.peek() != &Tk::RParen {
            return self.err("DSL_COND", "Expected “)”.");
        }
        self.bump();
        Ok((op, Some(v)))
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        if self.peek() != &Tk::LBrace {
            return self.err("DSL_BRACE", "Expected “{”.");
        }
        self.bump();
        let body = self.stmts(true);
        if self.peek() == &Tk::RBrace {
            self.bump();
        }
        Ok(body)
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let line = self.tok().line;
        self.bump();
        let negated = self.peek() == &Tk::Bang;
        if negated {
            self.bump();
        }
        let (op, arg) = self.cond()?;
        let body = self.block()?;
        // `else` may follow on the next line.
        let save = self.k;
        self.skip_blank();
        let has_else = matches!(self.peek(), Tk::Ident(n) if n == "else");
        if !has_else {
            self.k = save;
            return Ok(if negated {
                Stmt::If { op, arg, then: Vec::new(), els: body, line }
            } else {
                Stmt::If { op, arg, then: body, els: Vec::new(), line }
            });
        }
        if negated {
            return self.err("DSL_ELSE", "An `if !…` has no else; write `if cond { … } else { … }`.");
        }
        self.bump();
        let els = if matches!(self.peek(), Tk::Ident(n) if n == "if") { vec![self.if_stmt()?] } else { self.block()? };
        Ok(Stmt::If { op, arg, then: body, els, line })
    }
}

/// Parses a program. On errors the statements parsed so far come back with them.
pub fn parse(src: &str) -> (Vec<Stmt>, Vec<Diag>) {
    let toks = match lex(src) {
        Ok(t) => t,
        Err(d) => return (Vec::new(), vec![d]),
    };
    let mut p = Parser { toks, k: 0, diags: Vec::new() };
    let stmts = p.stmts(false);
    (stmts, p.diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(src: &str) -> String {
        let (s, d) = parse(src);
        assert!(d.is_empty(), "{d:?}");
        print(&s, None, &|_| None)
    }

    #[test]
    fn prints_and_parses_cards() {
        let src = "if flag(0x004) {\n  say @0x0D4C\n  wipe_scratch\n} else if !yes {\n  warp facing:0 chunk/78 x:-32 z:-880 flags:0x0201\n}\nL1:\ngoto L1\n";
        assert_eq!(round(src), src.replace("wipe_scratch", "wipe_scratch  // flags 0–63 cleared"));
        // Names are optional and checked; positional works.
        assert_eq!(round("warp 0 chunk/78 -32 -880 513\n"), "warp facing:0 chunk/78 x:-32 z:-880 flags:0x0201\n");
        assert_eq!(round("say \"Hi \\\"$A\\\"{nl}bye\"\n"), "say \"Hi \\\"$A\\\"{nl}bye\"\n");
    }

    #[test]
    fn reports_errors_with_lines() {
        let (_, d) = parse("set 0x81\nsay 12\nbogus 1\nwarp facing:0 chunk/1 z:1\nif flag(1) {\n set\n}\n");
        let codes: Vec<(u32, &str)> = d.iter().map(|d| (d.line, d.code)).collect();
        assert_eq!(codes, vec![(2, "DSL_TEXT"), (3, "DSL_OPCODE"), (4, "DSL_ARG_NAME"), (6, "DSL_ARITY")]);
        let (_, d) = parse("if flag(1) {\n  set 1\n");
        assert_eq!(d[0].code, "DSL_BRACE");
        let (_, d) = parse("wait 300\n");
        assert_eq!(d[0].code, "DSL_RANGE");
    }
}
