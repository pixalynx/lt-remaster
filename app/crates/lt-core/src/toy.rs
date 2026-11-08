//! Living Toy packages (`toy/n`, ext `TOY{n+1}.BIN`): 8 self-relocating
//! sections. See `docs/research/02-models-textures-anims.md` §4.

use serde::Serialize;

use crate::container::{self, le16, Section};
use crate::pose::{self, PoseBlock};
use crate::tim::{Tim, TimInfo};
use crate::tmd::{self, TmdStats};

pub const SECTION_NAMES: [&str; 8] = [
    "Poses",
    "Ability data",
    "Legacy sprite script",
    "Sound effects (TSQ)",
    "Effect model (TMD)",
    "Sound bank (TVB)",
    "Texture (TIM)",
    "Main model (TMD)",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BattleSequence {
    pub index: usize,
    /// `(pose, speed)` pairs; a `(0xFF, n)` pair loops to entry n.
    pub entries: Vec<(u8, u8)>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToyPackage {
    pub size: usize,
    pub sections: Vec<Section>,
    pub model: Option<TmdStats>,
    pub effect_model: Option<TmdStats>,
    pub texture: Option<TimInfo>,
    pub poses: Option<PoseBlock>,
    /// Battle pose sequences 0–5 from the ability data (`+0x90`).
    pub sequences: Vec<BattleSequence>,
}

pub fn parse(b: &[u8]) -> Option<ToyPackage> {
    let sections = container::sections(b, 8)?;
    let slice = |i: usize| {
        let s = &sections[i];
        &b[s.offset as usize..(s.offset + s.len) as usize]
    };
    Some(ToyPackage {
        size: b.len(),
        model: tmd::stats(slice(7)),
        effect_model: tmd::stats(slice(4)),
        texture: Tim::parse(slice(6)).map(|t| t.info),
        poses: pose::parse(slice(0)),
        sequences: sequences(slice(1)),
        sections,
    })
}

/// Battle sequences: `u16 offset[12]` at +0x90 relative to +0xA8; 0–5 are pose
/// sequences. Entries run until speed 0 (hold) or a `0xFF` loop marker.
fn sequences(f1: &[u8]) -> Vec<BattleSequence> {
    if f1.len() < 0xA8 {
        return Vec::new();
    }
    let base = 0xA8;
    // Unused sequence slots point at the first action script (slot 6).
    let scripts = le16(f1, 0x90 + 6 * 2);
    (0..6)
        .filter_map(|i| {
            let ofs = le16(f1, 0x90 + i * 2);
            if ofs == scripts {
                return None;
            }
            let start = base + ofs as usize;
            let mut entries = Vec::new();
            let mut p = start;
            while let Some(pair) = f1.get(p..p + 2) {
                entries.push((pair[0], pair[1]));
                p += 2;
                if pair[1] == 0 || pair[0] == 0xFF || entries.len() >= 32 {
                    break;
                }
            }
            (!entries.is_empty()).then_some(BattleSequence { index: i, entries })
        })
        .collect()
}
