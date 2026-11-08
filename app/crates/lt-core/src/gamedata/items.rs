//! Items (PRD §4.6) and the enemy drop-code remap (§4.8).
//!
//! Items: EXE `D_801B5614` (file 0x42E14), 215 × 20 B `ItemRec`
//! (`include/game.h:315`): `{char* name; u16 price; u16 class; u8 stat, value;
//! u8 stat2; s8 value2; u16 element; u16 unused; char* desc}`.
//! Drop remap: OVR04 `D_801FDAFC` (file 0x1EAFC), s16[206]; `Enemy_OnDefeatRewards`
//! gives `D_801FDAFC[code]`, not the code itself.

use serde::Serialize;

use crate::container::{le16, le32, le_i16};

use super::{bad, Result, Source, OVR_LOAD};

pub const ITEM_TABLE: u32 = 0x801B_5614;
pub const ITEM_COUNT: usize = 215;
pub const ITEM_SIZE: usize = 20;
pub const DROP_REMAP: u32 = 0x801F_DAFC;
pub const DROP_REMAP_LEN: usize = 206;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: u16,
    pub name: String,
    pub price: u16,
    /// `&0x3800`: 0x0800 sword, 0x1000 armor, 0x1800 helmet, 0x2000 shield, 0x2800 ring.
    pub cls: u16,
    /// 1 ATK, 2 DEF, 5 SPD, 6 heal HP, 7 heal PP, 8 cure, 9 inflict, 10 immunity mask.
    pub stat: u8,
    pub val: u8,
    pub stat2: u8,
    pub val2: i8,
    /// Element bits (weapon element / armor affinity).
    pub elem: u16,
    /// +0x0E, zero in retail.
    pub unused: u16,
    pub desc: String,
    /// `char*` of the name and description.
    pub name_ptr: u32,
    pub desc_ptr: u32,
    /// Characters each string's slot holds in place.
    pub name_room: u8,
    pub desc_room: u8,
    /// Other items reading the same name / description string.
    pub name_shared: u16,
    pub desc_shared: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemTable {
    pub items: Vec<Item>,
    /// Drop code → item id (500+ = key-item space, item 198 + (v − 500)).
    pub drop_remap: Vec<i16>,
}

impl ItemTable {
    pub fn load(src: Source<'_>, ovr04: &[u8]) -> Result<Self> {
        let b = src.exe_bytes(src.exe.va(ITEM_TABLE), ITEM_COUNT * ITEM_SIZE)?;
        let room = |va: u32| crate::edit::tables::string_room(&src.retail_exe.bytes, src.retail_exe.t_addr, va);
        let ptr = |i: usize, at: usize| le32(&b[i * ITEM_SIZE..], at);
        let shared = |i: usize, at: usize| (0..ITEM_COUNT).filter(|&j| j != i && ptr(j, at) == ptr(i, at)).count() as u16;
        let items = (0..ITEM_COUNT)
            .map(|i| {
                let r = &b[i * ITEM_SIZE..(i + 1) * ITEM_SIZE];
                Item {
                    name_ptr: le32(r, 0),
                    desc_ptr: le32(r, 16),
                    name_room: room(le32(r, 0)),
                    desc_room: room(le32(r, 16)),
                    name_shared: shared(i, 0),
                    desc_shared: shared(i, 16),
                    id: i as u16,
                    name: src.exe_str(le32(r, 0)).unwrap_or_default(),
                    price: le16(r, 4),
                    cls: le16(r, 6),
                    stat: r[8],
                    val: r[9],
                    stat2: r[10],
                    val2: r[11] as i8,
                    elem: le16(r, 12),
                    unused: le16(r, 14),
                    desc: src.exe_str(le32(r, 16)).unwrap_or_default(),
                }
            })
            .collect();
        let at = (src.exe.ovr_va(4, DROP_REMAP) - OVR_LOAD) as usize;
        let raw = ovr04
            .get(at..at + DROP_REMAP_LEN * 2)
            .ok_or_else(|| bad("ovr/4 is too short for the drop remap"))?;
        let drop_remap = (0..DROP_REMAP_LEN).map(|i| le_i16(raw, i * 2)).collect();
        Ok(Self { items, drop_remap })
    }
}
