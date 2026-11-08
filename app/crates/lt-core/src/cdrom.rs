//! CD-ROM XA Mode 2 sector framing: building and checking the raw 2352-byte
//! sectors a build writes. Port of `gc_decomp/tools/inject_disc.py:68-143`
//! (`encode_mode2_form1`, `blank_sector`), which reproduces the retail image.
//!
//! ```text
//! 0x000  12  sync 00 FF×10 00
//! 0x00C   4  header: BCD minute, second, frame of lba + 150; mode 02
//! 0x010   4  subheader: file, channel, submode, coding
//! 0x014   4  subheader copy
//! 0x018 2048 user data (Form 1)
//! 0x818   4  EDC: CRC-32 (reflected poly 0xD8018001) over 0x010..0x817, LE
//! 0x81C 172  ECC P: RS(26,24) over 86 columns, header bytes taken as zero
//! 0x8C8 104  ECC Q: RS(45,43) over 52 diagonals, same
//! ```
//!
//! Submodes on this disc: `0x08` (data) on every sector of a file but its
//! last, which is `0x89` (EOF | data | EOR); movies interleave `0x48` video
//! with Form 2 `0x64` audio. The 150 postgap sectors after the last file have
//! sync and header only (subheader, data, EDC and ECC all zero).

use crate::disc::{RAW_SECTOR, USER_DATA, USER_DATA_OFFSET};

pub const SYNC: [u8; 12] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
pub const HEADER: usize = 0x00C;
pub const SUBHEADER: usize = 0x010;
pub const EDC: usize = 0x818;
pub const ECC_P: usize = 0x81C;
pub const ECC_Q: usize = 0x8C8;

/// Submode bits (CD-ROM XA).
pub const SM_EOR: u8 = 0x01;
pub const SM_DATA: u8 = 0x08;
pub const SM_FORM2: u8 = 0x20;
pub const SM_EOF: u8 = 0x80;

/// Subheader of every sector of a data file but its last.
pub const SUB_DATA: [u8; 4] = [0, 0, SM_DATA, 0];
/// Subheader of a data file's last sector.
pub const SUB_LAST: [u8; 4] = [0, 0, SM_EOF | SM_DATA | SM_EOR, 0];

pub type Raw = [u8; RAW_SECTOR];

const fn tables() -> ([u8; 256], [u8; 256], [u32; 256]) {
    let (mut f, mut b, mut e) = ([0u8; 256], [0u8; 256], [0u32; 256]);
    let mut i = 0;
    while i < 256 {
        let j = ((i << 1) ^ if i & 0x80 != 0 { 0x11D } else { 0 }) & 0xFF;
        f[i] = j as u8;
        b[i ^ j] = i as u8;
        let mut edc = i as u32;
        let mut k = 0;
        while k < 8 {
            edc = (edc >> 1) ^ if edc & 1 != 0 { 0xD801_8001 } else { 0 };
            k += 1;
        }
        e[i] = edc;
        i += 1;
    }
    (f, b, e)
}

const LUTS: ([u8; 256], [u8; 256], [u32; 256]) = tables();
static F_LUT: [u8; 256] = LUTS.0;
static B_LUT: [u8; 256] = LUTS.1;
static EDC_LUT: [u32; 256] = LUTS.2;

/// The CD-ROM EDC (a reflected CRC-32 with poly 0xD8018001, no final xor).
pub fn edc(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |e, &x| (e >> 8) ^ EDC_LUT[((e ^ x as u32) & 0xFF) as usize])
}

/// One Reed-Solomon product-code pass (P or Q), as in ECMA-130 Annex A.
fn ecc_block(src: &[u8], major_count: usize, minor_count: usize, major_mult: usize, minor_inc: usize, dest: &mut [u8]) {
    let size = major_count * minor_count;
    for major in 0..major_count {
        let mut index = (major >> 1) * major_mult + (major & 1);
        let (mut a, mut b) = (0u8, 0u8);
        for _ in 0..minor_count {
            let t = src[index];
            index += minor_inc;
            if index >= size {
                index -= size;
            }
            a ^= t;
            b ^= t;
            a = F_LUT[a as usize];
        }
        a = B_LUT[(F_LUT[a as usize] ^ b) as usize];
        dest[major] = a;
        dest[major + major_count] = a ^ b;
    }
}

/// EDC, P and Q for a Form 1 sector whose sync, header, subheader and user
/// data are already in place.
fn checksums(raw: &Raw) -> ([u8; 4], [u8; 172], [u8; 104]) {
    let e = edc(&raw[SUBHEADER..EDC]).to_le_bytes();
    // Mode 2: the 4 header bytes are taken as zero for the ECC.
    // src = 4 zero bytes ‖ subheader, user data, EDC (2064 = 86 × 24), then P.
    let mut buf = [0u8; 2064 + 172];
    buf[4..2060].copy_from_slice(&raw[SUBHEADER..EDC]);
    buf[2060..2064].copy_from_slice(&e);
    let (src, p) = buf.split_at_mut(2064);
    ecc_block(src, 86, 24, 2, 86, p);
    let mut q = [0u8; 104];
    ecc_block(&buf, 52, 43, 86, 88, &mut q);
    let mut pp = [0u8; 172];
    pp.copy_from_slice(&buf[2064..]);
    (e, pp, q)
}

/// Puts `data` (≤ 2048 bytes, zero-padded) into a Form 1 sector and
/// recomputes EDC and ECC. Sync, header and subheader are kept.
pub fn encode_form1(raw: &mut Raw, data: &[u8]) {
    let user = &mut raw[USER_DATA_OFFSET..USER_DATA_OFFSET + USER_DATA];
    user[..data.len()].copy_from_slice(data);
    user[data.len()..].fill(0);
    let (e, p, q) = checksums(raw);
    raw[EDC..EDC + 4].copy_from_slice(&e);
    raw[ECC_P..ECC_Q].copy_from_slice(&p);
    raw[ECC_Q..].copy_from_slice(&q);
}

/// BCD minute, second, frame of `lba` (`lba + 150` frames, 75 per second).
pub fn msf(lba: u32) -> [u8; 3] {
    let a = lba + 150;
    let bcd = |n: u32| (((n / 10) << 4) | (n % 10)) as u8;
    [bcd(a / 4500), bcd((a / 75) % 60), bcd(a % 75)]
}

/// The LBA a sector's header names, if it is valid BCD.
pub fn header_lba(raw: &Raw) -> Option<u32> {
    let un = |b: u8| ((b >> 4) <= 9 && (b & 15) <= 9).then_some((b >> 4) as u32 * 10 + (b & 15) as u32);
    let (m, s, f) = (un(raw[HEADER])?, un(raw[HEADER + 1])?, un(raw[HEADER + 2])?);
    ((m * 60 + s) * 75 + f).checked_sub(150)
}

/// A sector that doesn't exist in the source yet: sync, header for `lba`,
/// mode 2 and `sub` in both subheader copies; everything else zero.
pub fn blank(lba: u32, sub: [u8; 4]) -> Raw {
    let mut raw = [0u8; RAW_SECTOR];
    raw[..12].copy_from_slice(&SYNC);
    raw[HEADER..HEADER + 3].copy_from_slice(&msf(lba));
    raw[HEADER + 3] = 2;
    raw[SUBHEADER..SUBHEADER + 4].copy_from_slice(&sub);
    raw[SUBHEADER + 4..SUBHEADER + 8].copy_from_slice(&sub);
    raw
}

/// A postgap sector as retail has them (185,835–185,984): sync and header,
/// the rest zero (no subheader, no EDC/ECC).
pub fn postgap(lba: u32) -> Raw {
    blank(lba, [0; 4])
}

pub fn subheader(raw: &Raw) -> [u8; 4] {
    raw[SUBHEADER..SUBHEADER + 4].try_into().expect("4 bytes")
}

/// Sets both subheader copies.
pub fn set_subheader(raw: &mut Raw, sub: [u8; 4]) {
    raw[SUBHEADER..SUBHEADER + 4].copy_from_slice(&sub);
    raw[SUBHEADER + 4..SUBHEADER + 8].copy_from_slice(&sub);
}

pub fn is_form2(raw: &Raw) -> bool {
    raw[SUBHEADER + 2] & SM_FORM2 != 0
}

/// A Mode 2 sector without its sync and header: both subheader copies and
/// 2328 bytes (Form 1: 2048 of data, then EDC and ECC; Form 2: 2324 of
/// data, then EDC). Movies are staged as runs of these.
pub const PAYLOAD: usize = 2336;
/// Where a Form 2 sector's EDC sits (it covers the subheader and the 2324 data bytes).
pub const FORM2_EDC: usize = 0x92C;

/// The sector at `lba` holding `payload` (≥ [`PAYLOAD`] bytes): sync, header,
/// and the EDC (and ECC for Form 1) recomputed for whichever form its
/// subheader says.
pub fn from_payload(lba: u32, payload: &[u8]) -> Raw {
    let mut raw = blank(lba, [0; 4]);
    raw[SUBHEADER..SUBHEADER + PAYLOAD].copy_from_slice(&payload[..PAYLOAD]);
    if is_form2(&raw) {
        let e = edc(&raw[SUBHEADER..FORM2_EDC]);
        raw[FORM2_EDC..FORM2_EDC + 4].copy_from_slice(&e.to_le_bytes());
    } else {
        let data = raw[USER_DATA_OFFSET..USER_DATA_OFFSET + USER_DATA].to_vec();
        encode_form1(&mut raw, &data);
    }
    raw
}

/// Like [`check_form1`] for either form: a Form 2 sector needs its sync,
/// header, mode, matching subheader copies and EDC (0 = not computed, as
/// the standard allows).
pub fn check_mode2(raw: &Raw, lba: u32) -> Result<(), SectorFault> {
    if !is_form2(raw) {
        return check_form1(raw, lba);
    }
    if raw[..12] != SYNC {
        return Err(SectorFault::Sync);
    }
    if header_lba(raw) != Some(lba) {
        return Err(SectorFault::Header);
    }
    if raw[HEADER + 3] != 2 {
        return Err(SectorFault::Mode);
    }
    if raw[SUBHEADER..SUBHEADER + 4] != raw[SUBHEADER + 4..SUBHEADER + 8] {
        return Err(SectorFault::SubheaderCopy);
    }
    let stored = u32::from_le_bytes(raw[FORM2_EDC..FORM2_EDC + 4].try_into().unwrap());
    if stored != 0 && stored != edc(&raw[SUBHEADER..FORM2_EDC]) {
        return Err(SectorFault::Edc);
    }
    Ok(())
}

/// What's wrong with a sector that should be a valid Form 1 sector at `lba`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectorFault {
    Sync,
    Header,
    Mode,
    SubheaderCopy,
    Form2,
    Edc,
    Ecc,
}

/// Checks sync, header MSF = `lba`, mode 2, matching subheader copies,
/// Form 1, EDC, and ECC P and Q.
pub fn check_form1(raw: &Raw, lba: u32) -> Result<(), SectorFault> {
    if raw[..12] != SYNC {
        return Err(SectorFault::Sync);
    }
    if header_lba(raw) != Some(lba) {
        return Err(SectorFault::Header);
    }
    if raw[HEADER + 3] != 2 {
        return Err(SectorFault::Mode);
    }
    if raw[SUBHEADER..SUBHEADER + 4] != raw[SUBHEADER + 4..SUBHEADER + 8] {
        return Err(SectorFault::SubheaderCopy);
    }
    if is_form2(raw) {
        return Err(SectorFault::Form2);
    }
    let (e, p, q) = checksums(raw);
    if raw[EDC..EDC + 4] != e {
        return Err(SectorFault::Edc);
    }
    if raw[ECC_P..ECC_Q] != p || raw[ECC_Q..] != q {
        return Err(SectorFault::Ecc);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msf_and_header() {
        assert_eq!(msf(185_985), [0x41, 0x21, 0x60]);
        assert_eq!(msf(0), [0x00, 0x02, 0x00]);
        let raw = blank(86_013, SUB_DATA);
        assert_eq!(header_lba(&raw), Some(86_013));
        assert_eq!(subheader(&raw), SUB_DATA);
    }

    #[test]
    fn encode_then_check() {
        let mut raw = blank(1234, SUB_LAST);
        encode_form1(&mut raw, b"hello");
        assert_eq!(check_form1(&raw, 1234), Ok(()));
        assert_eq!(check_form1(&raw, 1235), Err(SectorFault::Header));
        let mut bad = raw;
        bad[100] ^= 1;
        assert_eq!(check_form1(&bad, 1234), Err(SectorFault::Edc));
        let mut bad = raw;
        bad[ECC_Q + 3] ^= 1;
        assert_eq!(check_form1(&bad, 1234), Err(SectorFault::Ecc));
        // An all-zero Form 1 sector has an all-zero EDC and ECC.
        let mut zero = blank(7, [0; 4]);
        encode_form1(&mut zero, &[]);
        assert!(zero[EDC..].iter().all(|&b| b == 0));
    }
}
