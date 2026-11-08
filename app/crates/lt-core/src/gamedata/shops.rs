//! Shops (PRD §4.7, research/04 §2.5).
//!
//! OVR02 `D_801FCE00` (file 0x1DE00): 23 × u32 pointers to 0-terminated u8
//! item-id lists packed in a 252-byte pool (file 0x1DD04–0x1DDFF). Several
//! shops point at the same list, so editing one edits all of them.

use serde::Serialize;

use crate::container::le32;

use super::{bad, Result, Source, OVR_LOAD};

pub const FILE: &str = "ovr/2";
pub const TABLE: u32 = 0x801F_CE00;
pub const SHOP_COUNT: usize = 23;
pub const POOL_START: u32 = 0x801F_CD04;
/// The pool ends where the pointer table starts.
pub const POOL_END: u32 = TABLE;
pub const MAX_ENTRIES: usize = 16;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shop {
    pub id: u8,
    /// RAM address of the list.
    pub ptr: u32,
    /// OVR02 file offset of the list.
    pub offset: u32,
    pub items: Vec<u8>,
    /// Other shops pointing at the same list.
    pub shared_with: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shops {
    pub file: &'static str,
    pub table_offset: u32,
    pub pool_offset: u32,
    pub pool_size: u32,
    /// Bytes the distinct lists take (entries + terminators).
    pub pool_used: u32,
    pub shops: Vec<Shop>,
}

impl Shops {
    pub fn load(src: Source<'_>) -> Result<Self> {
        // Where this release keeps the pool and the table in OVR02.
        let (pool_start, pool_end) = (src.exe.ovr_va(2, POOL_START), src.exe.ovr_va(2, POOL_END));
        let pool_offset = pool_start - OVR_LOAD;
        let table_offset = src.exe.ovr_va(2, TABLE) - OVR_LOAD;
        let b = src.file_part(FILE, pool_offset, (table_offset - pool_offset) as usize + SHOP_COUNT * 4)?;
        let ptrs: Vec<u32> = (0..SHOP_COUNT).map(|i| le32(&b, (table_offset - pool_offset) as usize + i * 4)).collect();
        let mut shops = Vec::with_capacity(SHOP_COUNT);
        for (i, &ptr) in ptrs.iter().enumerate() {
            if !(pool_start..pool_end).contains(&ptr) {
                return Err(bad(format!("shop {i} points outside the pool ({ptr:#010x})")));
            }
            let at = (ptr - pool_start) as usize;
            let len = b[at..].iter().position(|&c| c == 0).unwrap_or(b.len() - at);
            shops.push(Shop {
                id: i as u8,
                ptr,
                offset: ptr - OVR_LOAD,
                items: b[at..at + len].to_vec(),
                shared_with: ptrs
                    .iter()
                    .enumerate()
                    .filter(|&(j, &p)| j != i && p == ptr)
                    .map(|(j, _)| j as u8)
                    .collect(),
            });
        }
        let mut distinct: Vec<&Shop> = shops.iter().collect();
        distinct.sort_by_key(|s| s.ptr);
        distinct.dedup_by_key(|s| s.ptr);
        let pool_used = distinct.iter().map(|s| s.items.len() as u32 + 1).sum();
        Ok(Self { file: FILE, table_offset, pool_offset, pool_size: pool_end - pool_start, pool_used, shops })
    }
}
