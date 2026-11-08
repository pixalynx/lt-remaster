//! PCSX-Redux save states (LT-11): a base package's boot state, read enough to
//! check it and to say where it starts.
//!
//! Redux's core saves a protobuf message, "PCSX-Redux SaveState v4", of about
//! 21 MB (`SaveStates::save`, which `/api/v1/state-raw` and Lua's
//! `PCSX.createSaveState` return). Its GUI writes the same bytes gzipped as
//! `.sstate` (about 1.7 MB). A state holds RAM, the scratchpad, the CPU, GPU,
//! SPU and the CD controller's registers, but not the disc: it belongs to the
//! image it was taken on, and loading it over another image runs the saved code
//! against that image's files.

use std::io::{Read, Write};
use std::ops::Range;

use crate::base::BaseError;

/// The only version Redux loads (`SaveStates::load` refuses others).
pub const VERSION: u32 = 4;
/// Bytes of RAM in a state: always 8 MB, whatever the 2/8 MB setting.
pub const RAM_BYTES: usize = 0x80_0000;
/// The scratchpad at 0x1F800000: the first 1 KB of the state's hardware page.
pub const SCRATCHPAD_BYTES: usize = 0x400;
/// The largest state accepted once inflated (Redux's own reader stops at 64 MB).
const MAX_RAW: u64 = 64 << 20;

fn bad(m: impl Into<String>) -> BaseError {
    BaseError { code: "SAVESTATE_BAD", message: m.into() }
}

/// A save state, inflated, with where its memory sits in the message.
#[derive(Debug, Clone)]
pub struct SaveState {
    raw: Vec<u8>,
    /// `save_state_info.version_string`, e.g. "PCSX-Redux SaveState v4".
    pub version_string: String,
    ram: Range<usize>,
    hardware: Range<usize>,
}

/// One protobuf field: number, wire type and its bytes (the payload for
/// length-delimited fields, the encoded value otherwise).
fn fields(b: &[u8]) -> impl Iterator<Item = Option<(u64, u8, Range<usize>)>> + '_ {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        if at >= b.len() {
            return None;
        }
        let field = (|| {
            let key = varint(b, &mut at)?;
            let (number, wire) = (key >> 3, (key & 7) as u8);
            let start = at;
            let range = match wire {
                0 => {
                    varint(b, &mut at)?;
                    start..at
                }
                1 => start..start.checked_add(8)?,
                2 => {
                    let len = usize::try_from(varint(b, &mut at)?).ok()?;
                    at..at.checked_add(len)?
                }
                5 => start..start.checked_add(4)?,
                _ => return None,
            };
            if range.end > b.len() {
                return None;
            }
            at = range.end;
            Some((number, wire, range))
        })();
        if field.is_none() {
            at = b.len();
        }
        Some(field)
    })
}

fn varint(b: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*at)?;
        *at += 1;
        v |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// The length-delimited field `number` of message `b`, as a range of `b`.
fn submessage(b: &[u8], number: u64) -> Result<Option<Range<usize>>, BaseError> {
    for f in fields(b) {
        let (n, wire, r) = f.ok_or_else(|| bad("The save state is cut short or damaged."))?;
        if n == number && wire == 2 {
            return Ok(Some(r));
        }
    }
    Ok(None)
}

impl SaveState {
    /// Reads a state as Redux writes it: gzipped (its GUI's `.sstate`) or raw.
    pub fn read(bytes: &[u8]) -> Result<Self, BaseError> {
        if bytes.starts_with(&[0x1F, 0x8B]) {
            let mut raw = Vec::new();
            flate2::read::GzDecoder::new(bytes)
                .take(MAX_RAW + 1)
                .read_to_end(&mut raw)
                .map_err(|e| bad(format!("The save state doesn't inflate: {e}.")))?;
            if raw.len() as u64 > MAX_RAW {
                return Err(bad("The save state inflates past 64 MB; it isn't a PCSX-Redux state."));
            }
            Self::from_raw(raw)
        } else {
            Self::from_raw(bytes.to_vec())
        }
    }

    /// Checks a raw state: a v4 message with its 8 MB of RAM and the hardware page.
    pub fn from_raw(raw: Vec<u8>) -> Result<Self, BaseError> {
        let not_redux = || bad("This isn't a PCSX-Redux save state (no \"SaveState v4\" header).");
        let info = submessage(&raw, 1).map_err(|_| not_redux())?.ok_or_else(not_redux)?;
        let (mut version, mut version_string) = (None, String::new());
        for f in fields(&raw[info.clone()]) {
            let (n, wire, r) = f.ok_or_else(not_redux)?;
            let r = info.start + r.start..info.start + r.end;
            match (n, wire) {
                (1, 2) => version_string = String::from_utf8_lossy(&raw[r]).into_owned(),
                (2, 0) => version = varint(&raw, &mut r.start.clone()),
                _ => {}
            }
        }
        match version {
            Some(v) if v == u64::from(VERSION) => {}
            Some(v) => return Err(bad(format!("It's a version {v} PCSX-Redux save state; Redux loads version {VERSION} only."))),
            None => return Err(not_redux()),
        }
        let memory = submessage(&raw, 3)?.ok_or_else(|| bad("The save state has no memory."))?;
        let mem = &raw[memory.clone()];
        let at = |r: Range<usize>| memory.start + r.start..memory.start + r.end;
        let ram = submessage(mem, 1)?.map(at).filter(|r| r.len() == RAM_BYTES).ok_or_else(|| bad("The save state has no 8 MB RAM block."))?;
        let hardware = submessage(mem, 4)?.map(at).filter(|r| r.len() >= SCRATCHPAD_BYTES).ok_or_else(|| bad("The save state has no scratchpad."))?;
        Ok(Self { raw, version_string, ram, hardware })
    }

    /// The protobuf `/api/v1/state-raw` and `PCSX.loadSaveState` take.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Main RAM from 0x80000000 (8 MB; a 2 MB game uses the first 2).
    pub fn ram(&self) -> &[u8] {
        &self.raw[self.ram.clone()]
    }

    /// The scratchpad (`PCSX.getScratchPtr()`), where the game keeps its context.
    pub fn scratchpad(&self) -> &[u8] {
        &self.raw[self.hardware.start..self.hardware.start + SCRATCHPAD_BYTES]
    }

    /// The chunk the game's context says is current (`ctx+0xE8`, `MAP_LOADING` §1.3).
    pub fn chunk(&self) -> u32 {
        let s = self.scratchpad();
        u32::from(u16::from_le_bytes([s[0xE8], s[0xE9]]))
    }

    /// The state as Redux's GUI writes `.sstate` files.
    pub fn gzip(&self) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&self.raw).expect("writing to a Vec");
        gz.finish().expect("writing to a Vec")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn varint_bytes(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }

    fn len_field(n: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = varint_bytes((n << 3) | 2);
        out.extend(varint_bytes(payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    /// A state shaped like Redux's: info, (no thumbnail), memory with RAM and the hardware page.
    pub(crate) fn sample(version: u8, chunk: u16) -> Vec<u8> {
        let mut info = len_field(1, b"PCSX-Redux SaveState v4");
        info.extend([0x10, version]);
        let mut hw = vec![0u8; 0x1_0000];
        hw[0xE8..0xEA].copy_from_slice(&chunk.to_le_bytes());
        let mut memory = len_field(1, &vec![0u8; RAM_BYTES]);
        memory.extend(len_field(2, &[0u8; 16]));
        memory.extend(len_field(4, &hw));
        let mut raw = len_field(1, &info);
        raw.extend(len_field(3, &memory));
        raw.extend(len_field(4, &[0x08, 0x01]));
        raw
    }

    #[test]
    fn a_redux_state_reads_raw_and_gzipped() {
        let raw = sample(4, 326);
        let s = SaveState::read(&raw).unwrap();
        assert_eq!((s.version_string.as_str(), s.chunk(), s.ram().len()), ("PCSX-Redux SaveState v4", 326, RAM_BYTES));
        let gz = s.gzip();
        assert!(gz.starts_with(&[0x1F, 0x8B]) && gz.len() < raw.len() / 100);
        let back = SaveState::read(&gz).unwrap();
        assert_eq!((back.raw(), back.chunk()), (raw.as_slice(), 326));
    }

    #[test]
    fn other_files_and_versions_are_refused() {
        assert_eq!(SaveState::read(&sample(3, 0)).unwrap_err().message, "It's a version 3 PCSX-Redux save state; Redux loads version 4 only.");
        for junk in [&b""[..], b"PS-X EXE", &[0x0A, 0xFF, 0xFF], &[0x1F, 0x8B, 0, 0]] {
            assert_eq!(SaveState::read(junk).unwrap_err().code, "SAVESTATE_BAD", "{junk:?}");
        }
        let mut cut = sample(4, 1);
        cut.truncate(cut.len() / 2);
        assert!(SaveState::read(&cut).is_err());
    }
}
