//! Character files (`.ltchar`): a character made outside the game (a model,
//! its pose block and animation set, as [`super::character`] brings them
//! into a pack) plus the VRAM it samples, so it can be brought into any area.
//!
//! NPC files carry no texture: a model samples the VRAM its area paints. A
//! character file's texels and CLUT therefore go into the field-wide TIMs
//! (sys/2, sys/7: every field area uploads them), in rectangles retail
//! leaves blank. [`patch_tim`] writes one; it refuses a rectangle that holds
//! anything other than zeros or the same halfwords, so two characters never
//! paint over each other or over retail art.
//!
//! Layout: `"LTCHAR\0\x01"`, u32 header length, the JSON header
//! ([`Header`]), then the model, the pose block and each patch's halfwords
//! (little-endian), in that order.

use serde::{Deserialize, Serialize};

use super::character::Character;
use super::{EditError, Result};

const MAGIC: &[u8; 8] = b"LTCHAR\0\x01";

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

/// Halfwords for one VRAM rectangle (x, w in halfwords).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VramPatch {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub data: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharFile {
    pub character: Character,
    pub vram: Vec<VramPatch>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    label: String,
    looks: u16,
    anims: Vec<Vec<(u8, u8)>>,
    model_len: u32,
    poses_len: Option<u32>,
    /// `[x, y, w, h]` per patch.
    vram: Vec<[u16; 4]>,
}

impl CharFile {
    pub fn write(&self) -> Vec<u8> {
        let c = &self.character;
        let h = Header {
            label: c.label.clone(),
            looks: c.looks,
            anims: c.anims.clone(),
            model_len: c.model.len() as u32,
            poses_len: c.poses.as_ref().map(|p| p.len() as u32),
            vram: self.vram.iter().map(|p| [p.x, p.y, p.w, p.h]).collect(),
        };
        let json = serde_json::to_vec(&h).expect("header serialises");
        let mut out = MAGIC.to_vec();
        out.extend((json.len() as u32).to_le_bytes());
        out.extend(json);
        out.extend(&c.model);
        if let Some(p) = &c.poses {
            out.extend(p);
        }
        for p in &self.vram {
            out.extend(p.data.iter().flat_map(|v| v.to_le_bytes()));
        }
        out
    }

    pub fn read(b: &[u8]) -> Result<Self> {
        if b.get(..8) != Some(MAGIC.as_slice()) {
            return Err(bad("CHARFILE_MAGIC", "This isn't a Living Tool character file."));
        }
        let short = || bad("CHARFILE_SHORT", "The character file ends early.");
        let n = u32::from_le_bytes(b.get(8..12).ok_or_else(short)?.try_into().expect("4 bytes")) as usize;
        let h: Header = serde_json::from_slice(b.get(12..12 + n).ok_or_else(short)?).map_err(|e| bad("CHARFILE_HEADER", format!("The character file's header doesn't read: {e}")))?;
        let mut at = 12 + n;
        let mut take = |len: usize| -> Result<Vec<u8>> {
            let s = b.get(at..at + len).ok_or_else(short)?.to_vec();
            at += len;
            Ok(s)
        };
        let model = take(h.model_len as usize)?;
        let poses = h.poses_len.map(|l| take(l as usize)).transpose()?;
        let mut vram = Vec::new();
        for [x, y, w, hh] in h.vram {
            let raw = take(w as usize * hh as usize * 2)?;
            vram.push(VramPatch { x, y, w, h: hh, data: raw.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect() });
        }
        super::tmdcodec::Tmd::parse(&model)?;
        if let Some(p) = &poses {
            crate::pose::parse(p).ok_or_else(|| bad("POSES_BAD", "The character's pose block doesn't parse."))?;
        }
        Ok(Self { character: Character { label: h.label, model, poses, anims: h.anims, looks: h.looks }, vram })
    }
}

/// `tim` (a 4/8/16 bpp TIM) with `p` written into its image, or `None` when
/// the rectangle isn't inside the TIM's image. Refuses (`VRAM_TAKEN`) when
/// a halfword there is neither zero nor already the patch's.
pub fn patch_tim(tim: &[u8], p: &VramPatch) -> Result<Option<Vec<u8>>> {
    let Some(t) = crate::tim::Tim::parse(tim) else { return Ok(None) };
    let img = t.info.image;
    if p.x < img.x || p.y < img.y || p.x + p.w > img.x + img.w || p.y + p.h > img.y + img.h {
        return Ok(None);
    }
    // The image block is the TIM's last: `bnum, x, y, w, h`, then halfword rows.
    let pixels = t.info.len - img.w as usize * img.h as usize * 2;
    let mut out = tim.to_vec();
    for row in 0..p.h as usize {
        for col in 0..p.w as usize {
            let at = pixels + ((p.y - img.y) as usize + row) * img.w as usize * 2 + ((p.x - img.x) as usize + col) * 2;
            let want = p.data[row * p.w as usize + col];
            let have = u16::from_le_bytes([out[at], out[at + 1]]);
            if have != 0 && have != want {
                return Err(bad("VRAM_TAKEN", format!("VRAM ({}, {}) already holds something; the character's texture would paint over it.", p.x as usize + col, p.y as usize + row)));
            }
            out[at..at + 2].copy_from_slice(&want.to_le_bytes());
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tim(x: u16, y: u16, w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0x10, 0, 0, 0, 0x02, 0, 0, 0];
        b.extend((12 + w as u32 * h as u32 * 2).to_le_bytes());
        for v in [x, y, w, h] {
            b.extend(v.to_le_bytes());
        }
        b.resize(b.len() + w as usize * h as usize * 2, 0);
        b
    }

    #[test]
    fn a_patch_lands_once_and_never_over_other_texels() {
        let t = tim(512, 0, 8, 8);
        let p = VramPatch { x: 514, y: 3, w: 2, h: 2, data: vec![1, 2, 3, 4] };
        let a = patch_tim(&t, &p).unwrap().unwrap();
        assert_eq!(patch_tim(&a, &p).unwrap().unwrap(), a, "the same patch again changes nothing");
        let row = |r: usize| 20 + r * 16;
        assert_eq!(&a[row(3) + 4..row(3) + 8], &[1, 0, 2, 0]);
        assert_eq!(&a[row(4) + 4..row(4) + 8], &[3, 0, 4, 0]);
        let other = VramPatch { data: vec![9, 9, 9, 9], ..p.clone() };
        assert_eq!(patch_tim(&a, &other).unwrap_err().code, "VRAM_TAKEN");
        assert!(patch_tim(&t, &VramPatch { x: 0, ..p }).unwrap().is_none(), "outside the image");
    }
}
