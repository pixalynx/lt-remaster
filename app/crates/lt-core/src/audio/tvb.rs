//! TVB voice banks: 128 sample slots of SPU-ADPCM. Read by `UploadSpuSoundBank`
//! (0x801995D0, `main_801897ac.c:4341`); `docs/screens/14-audio.md` §4.5.
//!
//! | offset | size  | meaning                                                   |
//! |--------|-------|-----------------------------------------------------------|
//! | 0x000  | 0x200 | `u32 offset[128]` from +0x800; an unused slot = `size`     |
//! | 0x200  | 0x1FC | not read by the loader (non-zero in retail)               |
//! | 0x3FC  | 4     | `u32 size`: ADPCM bytes uploaded (`SpuWrite` length)      |
//! | 0x400  | 0x200 | `u32 ADSR1 \| ADSR2 << 16` per slot (`SeqVoice_ApplySample`) |
//! | 0x600  | 0x200 | not read                                                  |
//! | 0x800  | size  | headerless SPU-ADPCM                                      |

use crate::container::le32;

pub const SLOTS: usize = 128;
pub const HEADER: usize = 0x800;
pub const SIZE_AT: usize = 0x3FC;
pub const ADSR_AT: usize = 0x400;
/// The retail default ADSR word (ADSR1 0x80FF, ADSR2 0x1FC0).
pub const DEFAULT_ADSR: u32 = 0x1FC0_80FF;

#[derive(Debug, Clone)]
pub struct Tvb<'a> {
    pub offsets: [u32; SLOTS],
    /// ADPCM byte size at +0x3FC.
    pub size: u32,
    pub adsr: [u32; SLOTS],
    /// The ADPCM body (`size` bytes, or less if the section is short).
    pub adpcm: &'a [u8],
    /// Bytes available after the header (the section may be sector padded).
    pub available: usize,
}

/// One used slot and the bytes it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSpan {
    pub slot: usize,
    pub offset: u32,
    /// Up to the next distinct offset (or `size`).
    pub bytes: u32,
    /// The lowest slot at this offset.
    pub first: usize,
    /// Every other slot at the same offset.
    pub shares: Vec<usize>,
}

impl<'a> Tvb<'a> {
    pub fn parse(b: &'a [u8]) -> Option<Self> {
        if b.len() < HEADER {
            return None;
        }
        let size = le32(b, SIZE_AT);
        let mut offsets = [0u32; SLOTS];
        let mut adsr = [0u32; SLOTS];
        for i in 0..SLOTS {
            offsets[i] = le32(b, i * 4);
            adsr[i] = le32(b, ADSR_AT + i * 4);
        }
        // Every used offset must land inside the declared body.
        if offsets.iter().any(|&o| o > size) || size as usize > 0x80000 {
            return None;
        }
        let body = &b[HEADER..];
        let adpcm = &body[..(size as usize).min(body.len())];
        Some(Self { offsets, size, adsr, adpcm, available: body.len() })
    }

    pub fn used(&self, slot: usize) -> bool {
        self.offsets[slot] < self.size
    }

    /// Distinct used offsets in ascending order.
    pub fn distinct_offsets(&self) -> Vec<u32> {
        let mut v: Vec<u32> = (0..SLOTS).filter(|&i| self.used(i)).map(|i| self.offsets[i]).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Used slots with their byte spans and alias groups.
    pub fn spans(&self) -> Vec<Option<SlotSpan>> {
        let distinct = self.distinct_offsets();
        (0..SLOTS)
            .map(|slot| {
                if !self.used(slot) {
                    return None;
                }
                let offset = self.offsets[slot];
                let k = distinct.binary_search(&offset).ok()?;
                let next = distinct.get(k + 1).copied().unwrap_or(self.size);
                let group: Vec<usize> = (0..SLOTS).filter(|&i| self.used(i) && self.offsets[i] == offset).collect();
                Some(SlotSpan {
                    slot,
                    offset,
                    bytes: next - offset,
                    first: group[0],
                    shares: group.into_iter().filter(|&i| i != slot).collect(),
                })
            })
            .collect()
    }

    /// The ADPCM bytes of one slot (to the next distinct offset).
    pub fn slot_bytes(&self, slot: usize) -> Option<&'a [u8]> {
        if slot >= SLOTS || !self.used(slot) {
            return None;
        }
        let offset = self.offsets[slot];
        let next = self.distinct_offsets().into_iter().find(|&o| o > offset).unwrap_or(self.size);
        let (a, b) = (offset as usize, (next as usize).min(self.adpcm.len()));
        self.adpcm.get(a..b.max(a))
    }

    pub fn used_count(&self) -> usize {
        (0..SLOTS).filter(|&i| self.used(i)).count()
    }

    /// Header + body: what the file region holds.
    pub fn total_bytes(&self) -> usize {
        HEADER + self.size as usize
    }
}

/// The TVB region with `slot`'s sample (and every slot sharing its offset)
/// replaced by `adpcm` (padded to a whole block): later samples move by the
/// size change, unused slots keep pointing at the new size, the header's
/// unread regions and ADSR words stay. Returns header + body, unpadded.
pub fn replace_slot(region: &[u8], slot: usize, adpcm: &[u8]) -> Option<Vec<u8>> {
    let t = Tvb::parse(region)?;
    if slot >= SLOTS || !t.used(slot) {
        return None;
    }
    let off = t.offsets[slot];
    let next = t.distinct_offsets().into_iter().find(|&o| o > off).unwrap_or(t.size);
    let mut new = adpcm.to_vec();
    new.resize(adpcm.len().next_multiple_of(16), 0);
    let delta = new.len() as i64 - (next - off) as i64;
    let mut out = region[..HEADER].to_vec();
    let body = &region[HEADER..HEADER + t.size as usize];
    out.extend_from_slice(&body[..off as usize]);
    out.extend_from_slice(&new);
    out.extend_from_slice(&body[next as usize..]);
    let size = (t.size as i64 + delta) as u32;
    for i in 0..SLOTS {
        let o = t.offsets[i];
        let moved = if o > off || o >= t.size { (o as i64 + delta) as u32 } else { o };
        out[i * 4..i * 4 + 4].copy_from_slice(&moved.to_le_bytes());
    }
    out[SIZE_AT..SIZE_AT + 4].copy_from_slice(&size.to_le_bytes());
    Some(out)
}

/// The TVB region with `adpcm` (padded to a whole block) appended as the sample of `slot`, an
/// unused slot, with ADSR word `adsr`; every unused slot keeps pointing at the new size.
pub fn add_slot(region: &[u8], slot: usize, adpcm: &[u8], adsr: u32) -> Option<Vec<u8>> {
    let t = Tvb::parse(region)?;
    if slot >= SLOTS || t.used(slot) {
        return None;
    }
    let mut new = adpcm.to_vec();
    new.resize(adpcm.len().next_multiple_of(16), 0);
    let size = t.size + new.len() as u32;
    let mut out = region[..HEADER].to_vec();
    for i in 0..SLOTS {
        let o = if i == slot { t.size } else if t.used(i) { t.offsets[i] } else { size };
        out[i * 4..i * 4 + 4].copy_from_slice(&o.to_le_bytes());
    }
    out[SIZE_AT..SIZE_AT + 4].copy_from_slice(&size.to_le_bytes());
    out[ADSR_AT + slot * 4..ADSR_AT + slot * 4 + 4].copy_from_slice(&adsr.to_le_bytes());
    out.extend_from_slice(&region[HEADER..HEADER + t.size as usize]);
    out.extend_from_slice(&new);
    Some(out)
}

/// The TVB region with `slot` unused, and its sample's bytes out unless another slot shares them.
pub fn remove_slot(region: &[u8], slot: usize) -> Option<Vec<u8>> {
    let t = Tvb::parse(region)?;
    if slot >= SLOTS || !t.used(slot) {
        return None;
    }
    let mut keep = [false; SLOTS];
    for (i, k) in keep.iter_mut().enumerate() {
        *k = i != slot && t.used(i);
    }
    let shared = (0..SLOTS).any(|i| i != slot && t.used(i) && t.offsets[i] == t.offsets[slot]);
    if shared {
        let mut out = region[..HEADER + t.size as usize].to_vec();
        out[slot * 4..slot * 4 + 4].copy_from_slice(&t.size.to_le_bytes());
        return Some(out);
    }
    keep_slots(region, &keep)
}

/// The TVB region with only the samples of the slots `keep` names (and the slots sharing them),
/// packed in their old order; every other slot unused. The header's unread regions and the ADSR
/// words stay. The body keeps at least one silent block: the loader uploads `size` bytes, and a
/// zero-length SPU DMA isn't one. Returns header + body, unpadded.
pub fn keep_slots(region: &[u8], keep: &[bool; SLOTS]) -> Option<Vec<u8>> {
    let t = Tvb::parse(region)?;
    let kept: Vec<u32> = t.distinct_offsets().into_iter().filter(|&o| (0..SLOTS).any(|i| keep[i] && t.used(i) && t.offsets[i] == o)).collect();
    let mut out = region[..HEADER].to_vec();
    let mut body = Vec::new();
    let mut moved = std::collections::BTreeMap::new();
    for &o in &kept {
        let next = t.distinct_offsets().into_iter().find(|&n| n > o).unwrap_or(t.size);
        moved.insert(o, body.len() as u32);
        body.extend_from_slice(t.adpcm.get(o as usize..(next as usize).min(t.adpcm.len()))?);
    }
    if body.is_empty() {
        body.extend_from_slice(&[0, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    let size = body.len() as u32;
    for i in 0..SLOTS {
        let o = if keep[i] && t.used(i) { moved.get(&t.offsets[i]).copied().unwrap_or(size) } else { size };
        out[i * 4..i * 4 + 4].copy_from_slice(&o.to_le_bytes());
    }
    out[SIZE_AT..SIZE_AT + 4].copy_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&body);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeping_slots_packs_their_samples() {
        let mut b = bank();
        b[HEADER..HEADER + 32].fill(0x11);
        b[HEADER + 32..HEADER + 64].fill(0x22);
        let mut keep = [false; SLOTS];
        keep[2] = true;
        let out = keep_slots(&b, &keep).unwrap();
        let t = Tvb::parse(&out).unwrap();
        assert_eq!(t.size, 32);
        // Only slot 2 is kept: slot 1 shared its sample but isn't named, slot 0's sample went.
        assert_eq!((t.offsets[0], t.offsets[1], t.offsets[2]), (32, 32, 0));
        assert!(!t.used(0) && !t.used(1) && t.used(2));
        assert_eq!(t.slot_bytes(2).unwrap(), &[0x22; 32][..]);
        let none = keep_slots(&b, &[false; SLOTS]).unwrap();
        assert_eq!(Tvb::parse(&none).unwrap().size, 16);
    }

    fn bank() -> Vec<u8> {
        let mut b = vec![0u8; HEADER + 64];
        let size = 64u32;
        for i in 0..SLOTS {
            let o = match i {
                0 => 0,
                1 | 2 => 32,
                _ => size,
            };
            b[i * 4..i * 4 + 4].copy_from_slice(&o.to_le_bytes());
            b[ADSR_AT + i * 4..ADSR_AT + i * 4 + 4].copy_from_slice(&DEFAULT_ADSR.to_le_bytes());
        }
        b[SIZE_AT..SIZE_AT + 4].copy_from_slice(&size.to_le_bytes());
        b
    }

    #[test]
    fn spans_and_aliases() {
        let b = bank();
        let t = Tvb::parse(&b).unwrap();
        assert_eq!(t.used_count(), 3);
        let s = t.spans();
        assert_eq!(s[0].as_ref().unwrap().bytes, 32);
        let s2 = s[2].as_ref().unwrap();
        assert_eq!((s2.offset, s2.bytes, s2.first), (32, 32, 1));
        assert_eq!(s2.shares, vec![1]);
        assert!(s[3].is_none());
        assert_eq!(t.slot_bytes(1).unwrap().len(), 32);
    }

    #[test]
    fn replacing_a_slot_moves_the_rest() {
        let mut b = bank();
        b[HEADER + 32..HEADER + 64].fill(0xAB);
        let out = replace_slot(&b, 0, &[0x11; 40]).unwrap();
        let t = Tvb::parse(&out).unwrap();
        assert_eq!(t.size, 48 + 32);
        assert_eq!((t.offsets[0], t.offsets[1], t.offsets[2], t.offsets[3]), (0, 48, 48, 80));
        assert_eq!(t.slot_bytes(1).unwrap(), &[0xAB; 32][..]);
        assert_eq!(&t.slot_bytes(0).unwrap()[..40], &[0x11; 40][..]);
        assert_eq!(t.adsr, Tvb::parse(&b).unwrap().adsr);
    }

    #[test]
    fn adding_and_removing_a_slot() {
        let mut b = bank();
        b[HEADER..HEADER + 64].fill(0x5A);
        assert!(add_slot(&b, 1, &[0x33; 20], 0x1234).is_none(), "slot 1 is in use");
        let out = add_slot(&b, 7, &[0x33; 20], 0x1234).unwrap();
        let t = Tvb::parse(&out).unwrap();
        // Padded to a block, after the old body; unused slots point at the new end.
        assert_eq!((t.size, t.offsets[7], t.offsets[8]), (96, 64, 96));
        assert_eq!(&t.slot_bytes(7).unwrap()[..20], &[0x33; 20][..]);
        assert_eq!(t.slot_bytes(0), Tvb::parse(&b).unwrap().slot_bytes(0));
        assert_eq!(t.adsr[7], 0x1234);
        // Out again, the body is the old one.
        let removed = remove_slot(&out, 7).unwrap();
        let back = Tvb::parse(&removed).unwrap();
        assert_eq!((back.size, back.used(7)), (64, false));
        assert_eq!(back.slot_bytes(2), Tvb::parse(&b).unwrap().slot_bytes(2));
        // A sample another slot shares stays.
        let unshared = remove_slot(&b, 1).unwrap();
        let shared = Tvb::parse(&unshared).unwrap();
        assert_eq!((shared.size, shared.used(1), shared.used(2)), (64, false, true));
    }
}
