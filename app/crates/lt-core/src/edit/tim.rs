//! TIM palette colours → file bytes (screen 07). A CLUT entry is one BGR555
//! halfword (`b5 g5 r5`, bit 15 = STP, the semi-transparency flag); 0x0000 is
//! drawn transparent. The CLUT block follows the 8-byte TIM header:
//! `u32 bnum, u16 x, y, w, h`, then `w × h` halfwords, row-major.

use crate::container::le16;
use crate::tim::Tim;

use super::{EditError, Result};

/// `#RRGGBB` → BGR555 keeping `old`'s STP bit. Black stays opaque (0x8000)
/// unless the entry was transparent, because 0x0000 means transparent.
pub fn encode_color(hex: &str, old: u16) -> Result<u16> {
    let h = hex.trim_start_matches('#');
    let bad = || EditError::new("TIM_BAD_COLOR", format!("“{hex}” is not a #RRGGBB colour."));
    if h.len() != 6 {
        return Err(bad());
    }
    let v = u32::from_str_radix(h, 16).map_err(|_| bad())?;
    let (r, g, b) = ((v >> 16) as u16 & 0xFF, (v >> 8) as u16 & 0xFF, v as u16 & 0xFF);
    let rgb = (r >> 3) | (g >> 3) << 5 | (b >> 3) << 10;
    let stp = old & 0x8000;
    Ok(match rgb | stp {
        0 => 0x8000,
        w => w,
    })
}

/// File offset of CLUT entry (`row`, `entry`) of the TIM at `tim` in `file`.
pub fn clut_entry_offset(file: &[u8], tim: usize, row: usize, entry: usize) -> Result<usize> {
    let t = file.get(tim..).and_then(Tim::parse).ok_or_else(|| EditError::new("NOT_A_TIM", format!("No TIM at offset {tim:#x}.")))?;
    let c = t.info.clut.ok_or_else(|| EditError::new("TIM_NO_CLUT", "This TIM has no palette (16 bpp)."))?;
    if row >= c.h as usize || entry >= c.w as usize {
        return Err(EditError::new("TIM_CLUT_RANGE", format!("The palette has {} rows of {} colours.", c.h, c.w)));
    }
    Ok(tim + 8 + 12 + (row * c.w as usize + entry) * 2)
}

/// Sets one palette colour (`None` = transparent 0x0000). Returns the new
/// file and the halfword written.
pub fn set_color(file: &[u8], tim: usize, row: usize, entry: usize, color: Option<&str>) -> Result<(Vec<u8>, u16)> {
    let at = clut_entry_offset(file, tim, row, entry)?;
    let old = le16(file, at);
    let word = match color {
        Some(hex) => encode_color(hex, old)?,
        None => 0,
    };
    let mut out = file.to_vec();
    out[at..at + 2].copy_from_slice(&word.to_le_bytes());
    Ok((out, word))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tim::bgr555_to_hex;

    #[test]
    fn colours_round_trip_through_the_hex_the_ui_shows() {
        for w in [0x0001u16, 0x7FFF, 0x7C00, 0x03E0, 0x001F, 0x8000, 0x8421, 0x3DEF] {
            let hex = bgr555_to_hex(w).unwrap();
            assert_eq!(encode_color(&hex, w).unwrap(), w, "{w:#06x} via {hex}");
        }
        // Black written over a transparent entry stays visible.
        assert_eq!(encode_color("#000000", 0).unwrap(), 0x8000);
        assert_eq!(encode_color("#FF0000", 0x8000).unwrap(), 0x801F);
        assert_eq!(encode_color("red", 0).unwrap_err().code, "TIM_BAD_COLOR");
    }
}
