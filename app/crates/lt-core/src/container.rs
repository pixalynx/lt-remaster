//! Self-relocating containers. Most game files start with `N` little-endian
//! u32 offsets (relative to the file start) that the loader adds the load
//! address to in place. There is no compression.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub index: usize,
    pub offset: u32,
    /// Bytes up to the next section (or the end of the file).
    pub len: u32,
}

/// Reads `count` u32 offsets and derives each section's length from the next
/// offset in file order. Returns `None` if the header is not plausible.
pub fn sections(bytes: &[u8], count: usize) -> Option<Vec<Section>> {
    if bytes.len() < count * 4 {
        return None;
    }
    let offsets: Vec<u32> = (0..count).map(|i| le32(bytes, i * 4)).collect();
    if offsets.iter().any(|&o| o as usize > bytes.len() || (o as usize) < count * 4) {
        return None;
    }
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&i| offsets[i]);
    let mut lens = vec![0u32; count];
    for (k, &i) in order.iter().enumerate() {
        let end = order.get(k + 1).map(|&j| offsets[j]).unwrap_or(bytes.len() as u32);
        lens[i] = end - offsets[i];
    }
    Some((0..count).map(|i| Section { index: i, offset: offsets[i], len: lens[i] }).collect())
}

pub(crate) fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

pub(crate) fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub(crate) fn le_i16(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}
