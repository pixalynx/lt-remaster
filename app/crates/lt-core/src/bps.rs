//! BPS patches (byuu's "beat" format), the default patch format for builds
//! (`docs/screens/15-file-patcher.md`).
//!
//! ```text
//! "BPS1"  varint sourceSize, targetSize, metadataSize, metadata
//! actions until the footer: varint (length − 1) << 2 | kind
//!   0 SourceRead   copy from source at the output offset
//!   1 TargetRead   copy bytes from the patch
//!   2 SourceCopy   signed varint delta to the source cursor, then copy
//!   3 TargetCopy   signed varint delta to the target cursor, then copy (may overlap)
//! footer: u32 sourceCRC32, targetCRC32, patchCRC32 (over everything before it)
//! ```

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BpsInfo {
    pub size: u64,
    pub source_size: u64,
    pub target_size: u64,
    /// Free-form metadata (Living Tool writes a short note).
    pub metadata: String,
    pub source_crc32: String,
    pub target_crc32: String,
    pub patch_crc32: String,
    /// The patch's own checksum matches its bytes.
    pub patch_ok: bool,
}

const FOOTER: usize = 12;

/// CRC32 as BPS footers and the UI print it (lowercase hex).
pub fn crc32_hex(bytes: &[u8]) -> String {
    format!("{:08x}", crc32fast::hash(bytes))
}

fn varint(p: &[u8], i: &mut usize) -> Result<u64, String> {
    let (mut data, mut shift) = (0u64, 1u64);
    loop {
        let x = *p.get(*i).ok_or("the patch ends inside a number")?;
        *i += 1;
        data = data.checked_add((x & 0x7F) as u64 * shift).ok_or("a number in the patch overflows")?;
        if x & 0x80 != 0 {
            return Ok(data);
        }
        shift = shift.checked_shl(7).ok_or("a number in the patch overflows")?;
        data = data.checked_add(shift).ok_or("a number in the patch overflows")?;
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

struct Header {
    info: BpsInfo,
    /// Offset of the first action.
    actions: usize,
}

fn header(p: &[u8]) -> Result<Header, String> {
    if p.len() < 4 + 3 + FOOTER || &p[..4] != b"BPS1" {
        return Err("not a BPS patch (no BPS1 header)".into());
    }
    let mut i = 4;
    let source_size = varint(p, &mut i)?;
    let target_size = varint(p, &mut i)?;
    let meta_len = varint(p, &mut i)? as usize;
    let meta = p.get(i..i + meta_len).ok_or("the metadata runs past the end")?;
    i += meta_len;
    let end = p.len() - FOOTER;
    let patch_crc = le32(p, end + 8);
    Ok(Header {
        info: BpsInfo {
            size: p.len() as u64,
            source_size,
            target_size,
            metadata: String::from_utf8_lossy(meta).trim().to_string(),
            source_crc32: format!("{:08x}", le32(p, end)),
            target_crc32: format!("{:08x}", le32(p, end + 4)),
            patch_crc32: format!("{patch_crc:08x}"),
            patch_ok: crc32fast::hash(&p[..p.len() - 4]) == patch_crc,
        },
        actions: i,
    })
}

/// Reads the header and checksums without applying anything.
pub fn inspect(patch: &[u8]) -> Result<BpsInfo, String> {
    header(patch).map(|h| h.info)
}

/// Applies `patch` to `source`. Fails if the source, the result or the patch
/// itself doesn't match the checksums the patch carries.
pub fn apply(patch: &[u8], source: &[u8]) -> Result<Vec<u8>, String> {
    let h = header(patch)?;
    let info = &h.info;
    if !info.patch_ok {
        return Err("PATCH_CORRUPT: the patch's own checksum doesn't match".into());
    }
    if source.len() as u64 != info.source_size || format!("{:08x}", crc32fast::hash(source)) != info.source_crc32 {
        return Err(format!(
            "PATCH_WRONG_SOURCE: the patch needs {} bytes with CRC32 {}",
            info.source_size, info.source_crc32
        ));
    }
    let size = usize::try_from(info.target_size).map_err(|_| "the target is too large")?;
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let (mut i, end) = (h.actions, patch.len() - FOOTER);
    let (mut src_rel, mut dst_rel) = (0i64, 0i64);
    let delta = |p: &[u8], i: &mut usize| -> Result<i64, String> {
        let d = varint(p, i)?;
        let mag = (d >> 1) as i64;
        Ok(if d & 1 != 0 { -mag } else { mag })
    };
    while i < end {
        let data = varint(patch, &mut i)?;
        let len = ((data >> 2) + 1) as usize;
        if out.len() + len > size {
            return Err("an action writes past the target size".into());
        }
        match data & 3 {
            0 => {
                let at = out.len();
                out.extend_from_slice(source.get(at..at + len).ok_or("SourceRead past the end of the source")?);
            }
            1 => {
                out.extend_from_slice(patch.get(i..i + len).filter(|_| i + len <= end).ok_or("TargetRead past the end of the patch")?);
                i += len;
            }
            2 => {
                src_rel += delta(patch, &mut i)?;
                let at = usize::try_from(src_rel).map_err(|_| "SourceCopy before the start")?;
                out.extend_from_slice(source.get(at..at + len).ok_or("SourceCopy past the end of the source")?);
                src_rel += len as i64;
            }
            _ => {
                dst_rel += delta(patch, &mut i)?;
                let mut at = usize::try_from(dst_rel).map_err(|_| "TargetCopy before the start")?;
                for _ in 0..len {
                    let b = *out.get(at).ok_or("TargetCopy reads past what's written")?;
                    out.push(b);
                    at += 1;
                }
                dst_rel += len as i64;
            }
        }
    }
    if out.len() != size {
        return Err(format!("the patch produced {} bytes, not {}", out.len(), size));
    }
    if format!("{:08x}", crc32fast::hash(&out)) != info.target_crc32 {
        return Err("PATCH_BAD_RESULT: the result doesn't match the patch's target CRC32".into());
    }
    Ok(out)
}

/* ───────────────────────── Choosing a source ───────────────────────── */

/// How an image fits a patch's source (its header's size and CRC32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceFit {
    /// Size and CRC32 both match.
    Match,
    /// The size matches and the image's CRC32 isn't known yet; applying checks it.
    SizeOnly,
    /// The size or the CRC32 differs: not the image the patch was made from.
    Wrong,
}

/// How an image of `size` bytes, with `crc32` when it is known, fits a patch that needs
/// `need_size` bytes with CRC32 `need_crc32`.
pub fn source_fit(need_size: u64, need_crc32: &str, size: u64, crc32: Option<&str>) -> SourceFit {
    match crc32 {
        _ if size != need_size => SourceFit::Wrong,
        Some(c) if c.eq_ignore_ascii_case(need_crc32) => SourceFit::Match,
        Some(_) => SourceFit::Wrong,
        None => SourceFit::SizeOnly,
    }
}

/// Which image a patch applies to, out of `candidates` (size, CRC32 when known), in order of
/// preference: the first that matches by size and CRC32, else the first whose size matches
/// and whose CRC32 isn't known yet. `None` when none can be the source (`PATCH_WRONG_SOURCE`).
/// The desktop offers the open disc first, then the retail disc kept beside a mounted base, so
/// a project's own retail BPS applies on a base and a patch made against the base still
/// applies to the base.
pub fn choose_source(need_size: u64, need_crc32: &str, candidates: &[(u64, Option<&str>)]) -> Option<(usize, SourceFit)> {
    let fits: Vec<SourceFit> = candidates.iter().map(|&(size, crc)| source_fit(need_size, need_crc32, size, crc)).collect();
    [SourceFit::Match, SourceFit::SizeOnly].into_iter().find_map(|want| fits.iter().position(|&f| f == want).map(|i| (i, want)))
}

/* ───────────────────────── Creating ───────────────────────── */

/// A matching run shorter than this is cheaper inlined as TargetRead
/// (`make_patch.py` MIN_SAME).
const MIN_SAME: usize = 8;
/// Whole equal blocks are skipped without a byte loop.
const BLOCK: usize = 4096;

fn put_varint(v: &mut Vec<u8>, mut n: u64) {
    loop {
        let x = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            v.push(0x80 | x);
            return;
        }
        v.push(x);
        n -= 1;
    }
}

/// Writes a BPS patch as a same-offset diff, as `gc_decomp/tools/make_patch.py`
/// does: runs that equal the source at the same offset become SourceRead, the
/// rest TargetRead. A build never moves data that stays, so this finds nearly
/// everything. Feed the target in order, with the source bytes at the same
/// offsets (shorter or empty past the source's end).
pub struct Encoder {
    out: Vec<u8>,
    pending_same: u64,
    literal: Vec<u8>,
    fed: u64,
    /// The SourceCopy cursor (where the last SourceCopy ended).
    src_rel: u64,
    target_size: u64,
    /// Target bytes carried over from the source.
    pub same: u64,
    /// Target bytes stored in the patch.
    pub new: u64,
}

impl Encoder {
    pub fn new(source_size: u64, target_size: u64, metadata: &str) -> Self {
        let mut out = b"BPS1".to_vec();
        put_varint(&mut out, source_size);
        put_varint(&mut out, target_size);
        put_varint(&mut out, metadata.len() as u64);
        out.extend_from_slice(metadata.as_bytes());
        Self { out, pending_same: 0, literal: Vec::new(), fed: 0, src_rel: 0, target_size, same: 0, new: 0 }
    }

    fn flush_literal(&mut self) {
        if !self.literal.is_empty() {
            put_varint(&mut self.out, ((self.literal.len() as u64 - 1) << 2) | 1);
            self.out.append(&mut self.literal);
        }
    }

    fn flush_same(&mut self) {
        if self.pending_same > 0 {
            put_varint(&mut self.out, (self.pending_same - 1) << 2);
            self.pending_same = 0;
        }
    }

    fn push_same(&mut self, n: usize) {
        self.flush_literal();
        self.pending_same += n as u64;
        self.same += n as u64;
    }

    fn push_new(&mut self, bytes: &[u8]) {
        self.flush_same();
        self.literal.extend_from_slice(bytes);
        self.new += bytes.len() as u64;
    }

    /// The next `dst.len()` target bytes and the source bytes at the same
    /// offset (`src` may be shorter than `dst`).
    pub fn feed(&mut self, src: &[u8], dst: &[u8]) {
        self.fed += dst.len() as u64;
        if src.len() >= dst.len() && src[..dst.len()] == *dst {
            self.push_same(dst.len());
            return;
        }
        let n = dst.len();
        let mut i = 0;
        while i < n {
            if i % BLOCK == 0 && i + BLOCK <= n && i + BLOCK <= src.len() && src[i..i + BLOCK] == dst[i..i + BLOCK] {
                self.push_same(BLOCK);
                i += BLOCK;
                continue;
            }
            let eq_at = |j: usize| j < src.len() && src[j] == dst[j];
            let eq = eq_at(i);
            let mut j = i + 1;
            while j < n && eq_at(j) == eq {
                j += 1;
            }
            if eq && j - i >= MIN_SAME {
                self.push_same(j - i);
            } else {
                self.push_new(&dst[i..j]);
            }
            i = j;
        }
    }

    fn source_copy(&mut self, at: u64, len: usize) {
        self.flush_same();
        self.flush_literal();
        put_varint(&mut self.out, ((len as u64 - 1) << 2) | 2);
        let d = at as i64 - self.src_rel as i64;
        put_varint(&mut self.out, (d.unsigned_abs() << 1) | (d < 0) as u64);
        self.src_rel = at + len as u64;
        self.same += len as u64;
    }

    /// The next `dst.len()` target bytes, diffed against `src`, which are the
    /// source bytes at `src_at` (anywhere in the source): equal runs become
    /// SourceCopy. A build uses this for sectors past the source's end that
    /// resemble a source sector (a relocated file's old extent, the postgap).
    pub fn feed_from(&mut self, src: &[u8], src_at: u64, dst: &[u8]) {
        self.fed += dst.len() as u64;
        let n = dst.len();
        let mut i = 0;
        while i < n {
            let eq_at = |j: usize| j < src.len() && src[j] == dst[j];
            let eq = eq_at(i);
            let mut j = i + 1;
            while j < n && eq_at(j) == eq {
                j += 1;
            }
            if eq && j - i >= MIN_SAME {
                self.source_copy(src_at + i as u64, j - i);
            } else {
                self.push_new(&dst[i..j]);
            }
            i = j;
        }
    }

    /// Ends the patch: the footer holds the source, target and patch CRC32s.
    pub fn finish(mut self, source_crc32: u32, target_crc32: u32) -> Result<Vec<u8>, String> {
        if self.fed != self.target_size {
            return Err(format!("fed {} target bytes, not {}", self.fed, self.target_size));
        }
        self.flush_same();
        self.flush_literal();
        let mut out = self.out;
        out.extend_from_slice(&source_crc32.to_le_bytes());
        out.extend_from_slice(&target_crc32.to_le_bytes());
        let c = crc32fast::hash(&out);
        out.extend_from_slice(&c.to_le_bytes());
        Ok(out)
    }
}

/// A patch from two in-memory images (small inputs; builds stream through
/// [`Encoder`]).
pub fn create(source: &[u8], target: &[u8], metadata: &str) -> Vec<u8> {
    let mut e = Encoder::new(source.len() as u64, target.len() as u64, metadata);
    let end = source.len().min(target.len());
    e.feed(&source[..end], target);
    e.finish(crc32fast::hash(source), crc32fast::hash(target)).expect("whole target fed")
}

/// Applies `patch` without keeping the result: returns the target's size and
/// CRC32, reading source bytes through `source_at(offset, buf)`. Used to check
/// a new patch reproduces the build (as `make_patch.py` does after creating).
/// The patch's own checksum is checked; the source's is not (the caller just
/// computed it). TargetCopy, which [`Encoder`] never writes, is refused.
pub fn apply_crc(patch: &[u8], mut source_at: impl FnMut(u64, &mut [u8]) -> Result<(), String>) -> Result<(u64, u32), String> {
    let h = header(patch)?;
    if !h.info.patch_ok {
        return Err("PATCH_CORRUPT: the patch's own checksum doesn't match".into());
    }
    let (mut i, end) = (h.actions, patch.len() - FOOTER);
    let mut crc = crc32fast::Hasher::new();
    let mut written = 0u64;
    let mut src_rel = 0i64;
    let mut buf = vec![0u8; 1 << 20];
    let mut copy = |at: u64, mut len: u64, crc: &mut crc32fast::Hasher| -> Result<(), String> {
        let mut at = at;
        while len > 0 {
            let n = len.min(buf.len() as u64) as usize;
            source_at(at, &mut buf[..n])?;
            crc.update(&buf[..n]);
            at += n as u64;
            len -= n as u64;
        }
        Ok(())
    };
    while i < end {
        let data = varint(patch, &mut i)?;
        let len = (data >> 2) + 1;
        if written + len > h.info.target_size {
            return Err("an action writes past the target size".into());
        }
        match data & 3 {
            0 => copy(written, len, &mut crc)?,
            1 => {
                let bytes = patch.get(i..i + len as usize).filter(|_| i + len as usize <= end).ok_or("TargetRead past the end of the patch")?;
                crc.update(bytes);
                i += len as usize;
            }
            2 => {
                let d = varint(patch, &mut i)?;
                src_rel += if d & 1 != 0 { -((d >> 1) as i64) } else { (d >> 1) as i64 };
                let at = u64::try_from(src_rel).map_err(|_| "SourceCopy before the start")?;
                copy(at, len, &mut crc)?;
                src_rel += len as i64;
            }
            _ => return Err("TargetCopy isn't supported by the streaming check".into()),
        }
        written += len;
    }
    Ok((written, crc.finalize()))
}

/// Where [`apply_stream`] puts the target: bytes in order, and reads back of
/// bytes already pushed (TargetCopy).
pub trait Target {
    fn push(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn read_back(&mut self, at: u64, buf: &mut [u8]) -> Result<(), String>;
}

/// Applies `patch` without holding either image in memory (bases, about
/// 437 MB): source bytes come through `source_at(offset, buf)`, the target
/// goes to `target` in order. Every action kind is supported. The patch's own
/// checksum is checked first, the target's size and CRC32 at the end; the
/// source's CRC32 is the caller's (it knows the disc's fingerprint).
pub fn apply_stream(patch: &[u8], mut source_at: impl FnMut(u64, &mut [u8]) -> Result<(), String>, target: &mut impl Target) -> Result<BpsInfo, String> {
    const CHUNK: usize = 1 << 20;
    let h = header(patch)?;
    if !h.info.patch_ok {
        return Err("PATCH_CORRUPT: the patch's own checksum doesn't match".into());
    }
    let (mut i, end) = (h.actions, patch.len() - FOOTER);
    let mut crc = crc32fast::Hasher::new();
    let (mut written, mut src_rel, mut dst_rel) = (0u64, 0i64, 0i64);
    let mut buf = vec![0u8; CHUNK];
    let delta = |p: &[u8], i: &mut usize| -> Result<i64, String> {
        let d = varint(p, i)?;
        let mag = (d >> 1) as i64;
        Ok(if d & 1 != 0 { -mag } else { mag })
    };
    while i < end {
        let data = varint(patch, &mut i)?;
        let len = (data >> 2) + 1;
        if written + len > h.info.target_size {
            return Err("an action writes past the target size".into());
        }
        match data & 3 {
            kind @ (0 | 2) => {
                let mut at = if kind == 0 {
                    written
                } else {
                    src_rel += delta(patch, &mut i)?;
                    u64::try_from(src_rel).map_err(|_| "SourceCopy before the start")?
                };
                if at + len > h.info.source_size {
                    return Err("a copy reads past the end of the source".into());
                }
                let mut left = len;
                while left > 0 {
                    let n = left.min(CHUNK as u64) as usize;
                    source_at(at, &mut buf[..n])?;
                    crc.update(&buf[..n]);
                    target.push(&buf[..n])?;
                    (at, left) = (at + n as u64, left - n as u64);
                }
                if kind == 2 {
                    src_rel += len as i64;
                }
            }
            1 => {
                let bytes = patch.get(i..i + len as usize).filter(|_| i + len as usize <= end).ok_or("TargetRead past the end of the patch")?;
                crc.update(bytes);
                target.push(bytes)?;
                i += len as usize;
            }
            _ => {
                dst_rel += delta(patch, &mut i)?;
                let at = u64::try_from(dst_rel).map_err(|_| "TargetCopy before the start")?;
                if at >= written {
                    return Err("TargetCopy reads past what's written".into());
                }
                // Bytes at `at..written` are final; past that the copy repeats them with period `d`.
                let d = written - at;
                let first = d.min(len).min(CHUNK as u64) as usize;
                target.read_back(at, &mut buf[..first])?;
                let mut left = len;
                let mut from = at;
                if d >= len || d as usize > CHUNK {
                    // No overlap within a chunk: plain chunked copy.
                    let mut n = first;
                    loop {
                        crc.update(&buf[..n]);
                        target.push(&buf[..n])?;
                        (from, left) = (from + n as u64, left - n as u64);
                        if left == 0 {
                            break;
                        }
                        n = left.min(CHUNK as u64) as usize;
                        target.read_back(from, &mut buf[..n])?;
                    }
                } else {
                    let period = buf[..first].to_vec();
                    let reps = (CHUNK / period.len()).max(1);
                    let block: Vec<u8> = period.iter().copied().cycle().take(period.len() * reps).collect();
                    while left > 0 {
                        let n = left.min(block.len() as u64) as usize;
                        crc.update(&block[..n]);
                        target.push(&block[..n])?;
                        left -= n as u64;
                    }
                }
                dst_rel += len as i64;
            }
        }
        written += len;
    }
    if written != h.info.target_size {
        return Err(format!("the patch produced {written} bytes, not {}", h.info.target_size));
    }
    if format!("{:08x}", crc.finalize()) != h.info.target_crc32 {
        return Err("PATCH_BAD_RESULT: the result doesn't match the patch's target CRC32".into());
    }
    Ok(h.info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_round_trips() {
        let source: Vec<u8> = (0..50_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut target = source.clone();
        target[100] ^= 0xFF; // one byte
        for b in &mut target[9_000..9_003] {
            *b = 0; // a short run, inlined
        }
        for b in &mut target[20_000..28_193] {
            *b ^= 0x5A; // a long run across blocks
        }
        target.extend((0..7_000u32).map(|i| i as u8)); // appended
        let p = create(&source, &target, "{\"tool\":\"test\"}");
        // Appended bytes that repeat source bytes elsewhere become SourceCopy.
        let mut e = Encoder::new(source.len() as u64, target.len() as u64 + 4_096, "");
        e.feed(&source, &target[..50_000]);
        e.feed(&[], &target[50_000..]);
        let mut moved = source[1_000..5_096].to_vec();
        moved[7] ^= 1;
        e.feed_from(&source[1_000..5_096], 1_000, &moved);
        let mut grown = target.clone();
        grown.extend_from_slice(&moved);
        let q = e.finish(crc32fast::hash(&source), crc32fast::hash(&grown)).unwrap();
        assert!(q.len() < p.len() + 40, "{} vs {}", q.len(), p.len());
        assert_eq!(apply(&q, &source).unwrap(), grown);
        let info = inspect(&p).unwrap();
        assert_eq!((info.source_size, info.target_size, info.patch_ok), (50_000, 57_000, true));
        assert_eq!(info.metadata, "{\"tool\":\"test\"}");
        assert_eq!(apply(&p, &source).unwrap(), target);
        let (size, crc) = apply_crc(&p, |at, buf| {
            buf.copy_from_slice(&source[at as usize..at as usize + buf.len()]);
            Ok(())
        })
        .unwrap();
        assert_eq!((size, crc), (target.len() as u64, crc32fast::hash(&target)));
        // Identical images make a tiny patch.
        let same = create(&source, &source, "");
        assert!(same.len() < 32);
        assert_eq!(apply(&same, &source).unwrap(), source);
    }

    struct Mem(Vec<u8>);
    impl Target for Mem {
        fn push(&mut self, b: &[u8]) -> Result<(), String> {
            self.0.extend_from_slice(b);
            Ok(())
        }
        fn read_back(&mut self, at: u64, buf: &mut [u8]) -> Result<(), String> {
            buf.copy_from_slice(&self.0[at as usize..at as usize + buf.len()]);
            Ok(())
        }
    }

    #[test]
    fn stream_apply_matches_every_action_kind() {
        // A hand-made patch: TargetRead, an overlapping and a plain TargetCopy, SourceRead, SourceCopy.
        let mut body = Vec::new();
        let act = |v: &mut Vec<u8>, len: u64, kind: u64| put_varint(v, ((len - 1) << 2) | kind);
        let delta = |v: &mut Vec<u8>, d: i64| put_varint(v, (d.unsigned_abs() << 1) | (d < 0) as u64);
        act(&mut body, 2, 1);
        body.extend_from_slice(b"ab");
        act(&mut body, 3_000_000, 3); // "abab…" past one chunk
        delta(&mut body, 0);
        act(&mut body, 5, 0);
        act(&mut body, 4, 2);
        delta(&mut body, 50);
        act(&mut body, 6, 3);
        delta(&mut body, -3_000_000);
        // SourceRead copies at the output offset, so the source is 3 MB too.
        let source: Vec<u8> = (0..3_000_100u32).map(|i| (i % 251) as u8).collect();
        let mut target = b"ab".repeat(1_500_001);
        target.extend_from_slice(&source[3_000_002..3_000_007]);
        target.extend_from_slice(&source[50..54]);
        target.extend_from_slice(&target[..6].to_vec());
        let mut p = b"BPS1".to_vec();
        put_varint(&mut p, source.len() as u64);
        put_varint(&mut p, target.len() as u64);
        put_varint(&mut p, 0);
        p.extend_from_slice(&body);
        p.extend_from_slice(&crc32fast::hash(&source).to_le_bytes());
        p.extend_from_slice(&crc32fast::hash(&target).to_le_bytes());
        let c = crc32fast::hash(&p);
        p.extend_from_slice(&c.to_le_bytes());
        assert_eq!(apply(&p, &source).unwrap(), target);
        let mut out = Mem(Vec::new());
        let info = apply_stream(&p, |at, buf| { buf.copy_from_slice(&source[at as usize..at as usize + buf.len()]); Ok(()) }, &mut out).unwrap();
        assert_eq!((out.0 == target, info.target_size), (true, target.len() as u64));
    }

    fn put(v: &mut Vec<u8>, mut n: u64) {
        loop {
            let x = (n & 0x7F) as u8;
            n >>= 7;
            if n == 0 {
                v.push(0x80 | x);
                return;
            }
            v.push(x);
            n -= 1;
        }
    }

    fn signed(v: &mut Vec<u8>, d: i64) {
        put(v, ((d.unsigned_abs()) << 1) | (d < 0) as u64);
    }

    fn finish(mut p: Vec<u8>, source: &[u8], target: &[u8]) -> Vec<u8> {
        p.extend_from_slice(&crc32fast::hash(source).to_le_bytes());
        p.extend_from_slice(&crc32fast::hash(target).to_le_bytes());
        let c = crc32fast::hash(&p);
        p.extend_from_slice(&c.to_le_bytes());
        p
    }

    #[test]
    fn applies_all_four_actions() {
        let source = b"hello world, hello disc".to_vec();
        let target = b"hello there, hello hello disc!!!".to_vec();
        let mut p = b"BPS1".to_vec();
        put(&mut p, source.len() as u64);
        put(&mut p, target.len() as u64);
        put(&mut p, 4);
        p.extend_from_slice(b"note");
        put(&mut p, ((6 - 1) << 2) | 0); // SourceRead "hello "
        put(&mut p, ((5 - 1) << 2) | 1); // TargetRead "there"
        p.extend_from_slice(b"there");
        put(&mut p, ((8 - 1) << 2) | 2); // SourceCopy ", hello " from 11
        signed(&mut p, 11);
        put(&mut p, ((6 - 1) << 2) | 3); // TargetCopy "hello " from 0
        signed(&mut p, 0);
        put(&mut p, ((4 - 1) << 2) | 2); // SourceCopy "disc" from 19 (cursor 19 after the first copy)
        signed(&mut p, 0);
        put(&mut p, ((3 - 1) << 2) | 1); // TargetRead "!!!"
        p.extend_from_slice(b"!!!");
        let p = finish(p, &source, &target);
        let info = inspect(&p).unwrap();
        assert_eq!((info.source_size, info.target_size, info.metadata.as_str(), info.patch_ok), (23, 32, "note", true));
        assert_eq!(apply(&p, &source).unwrap(), target);
        assert!(apply(&p, b"another source of the right length!").unwrap_err().starts_with("PATCH_WRONG_SOURCE"));
    }

    /// @LT-59/1 @LT-59/3: the open disc (a base) first, then the retail disc beside it; a patch
    /// that fits neither has no source (PATCH_WRONG_SOURCE).
    #[test]
    fn chooses_the_source_by_size_and_crc() {
        const RETAIL: u64 = 437_436_720;
        const BASE: u64 = 437_441_424;
        let disc_and_retail = [(BASE, Some("2b9e4c11")), (RETAIL, Some("cdc72c62"))];
        // A project's own BPS on a base is made against retail: the retail disc beside the base.
        assert_eq!(choose_source(RETAIL, "cdc72c62", &disc_and_retail), Some((1, SourceFit::Match)));
        // A patch made against the base itself: the open disc.
        assert_eq!(choose_source(BASE, "2b9e4c11", &disc_and_retail), Some((0, SourceFit::Match)));
        // CRC32s compare without case.
        assert_eq!(choose_source(RETAIL, "CDC72C62", &disc_and_retail), Some((1, SourceFit::Match)));
        // Neither: the right size isn't enough when the CRC32 is known to differ.
        assert_eq!(choose_source(RETAIL, "deadbeef", &disc_and_retail), None);
        assert_eq!(choose_source(1234, "cdc72c62", &disc_and_retail), None);
        // No base: the open disc only, matched as before.
        assert_eq!(choose_source(RETAIL, "cdc72c62", &[(RETAIL, Some("cdc72c62"))]), Some((0, SourceFit::Match)));
        // A disc reopened without its checksum: the size picks it, applying checks the CRC32.
        assert_eq!(choose_source(RETAIL, "cdc72c62", &[(RETAIL, None)]), Some((0, SourceFit::SizeOnly)));
        // A known match wins over an unchecked one that comes first.
        assert_eq!(choose_source(RETAIL, "cdc72c62", &[(RETAIL, None), (RETAIL, Some("cdc72c62"))]), Some((1, SourceFit::Match)));
        assert_eq!(source_fit(RETAIL, "cdc72c62", BASE, None), SourceFit::Wrong);
    }
}
