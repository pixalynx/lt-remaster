//! Character models: each drawn part tied back to a TMD object in a file.
//!
//! `Npc_DrawIfVisible(obj, index, model, …)` builds a model's parts into the character's packet
//! slot (`obj+0x31`): one 0x3C part record per TMD object, at `D_801FD760 + slot × 0x3C` (a
//! chest's lid is the next one). A part record starts with its polygon count per TMD class (the
//! 20 build passes, `lt_core::mesh::TMD_CLASSES`) and holds its vertex count at +0x38: together,
//! the part's [`Fingerprint`].
//!
//! A part matches in one of two ways:
//! - **By the model pointer:** an NPC's `model` argument is its TMD in RAM, relocated, and the
//!   object the part's record sits at has the part's fingerprint. The TMD's [`signature`] then
//!   names its file in [`ModelIndex`].
//! - **By fingerprint:** the party's models were built from `sys/6` when the field loaded, and
//!   their `model` argument is a leftover NPC model (RAM no longer holds the Knight's TMD). The
//!   index finds the one object on the disc with the part's fingerprint.
//!
//! The index holds every TMD in the files that carry models (999 on the USA disc: the NPC packs,
//! `sys/6`'s Knight and Baby, `sys/9`'s overworld travellers, and the battle files). A model no
//! file holds (a project's own character, say) is drawn from its TMD in RAM
//! ([`ModelSource::Ram`]). NPC models carry no texture; they sample the field's VRAM, so their atlas
//! is decoded from the running game's VRAM the first time the model is seen in an area.
//!
//! Not covered: a party model no file holds. RAM keeps only its parts (the TMD is gone), so it
//! would have to be read from the packets `Model_DrawParts` draws (each polygon's record starts
//! with pointers to its three or four live vertices, then the GPU packets per buffer): LT-137.

use std::collections::HashMap;

use lt_core::fs::{sub_table, Kind};
use lt_core::mesh::{model_parts, tmd_mesh, Container, RenderMesh, TMD_CLASSES};
use lt_core::npc::NpcPack;
use lt_core::worldmap::WorldFiles;

use crate::mesh::MeshData;
use crate::tmd::{objects, signature};

/// The packet slots' part records (`D_801FD760`).
pub const PART_RECORDS: u32 = 0x801F_D760;
pub const PART_BYTES: u32 = 0x3C;
/// A part record's vertex count.
pub const PART_VERTS: usize = 0x38;
/// A character's packet slot (`Character_Struct+0x31`).
pub const CHARACTER_SLOT: u32 = 0x31;
/// Files that hold models, searched in this order: a part drawn in the field prefers a field
/// model when a battle file has an object of the same shape.
pub const MODEL_FILES: [Kind; 7] = [
    Kind::Sys,
    Kind::Npc,
    Kind::Knight,
    Kind::Pb,
    Kind::Toy,
    Kind::Enemy,
    Kind::Bf,
];

/// A TMD object's shape: vertex count and polygons per class pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    pub verts: u32,
    pub classes: [u16; TMD_CLASSES.len()],
}

impl Fingerprint {
    /// A part record's (0x3C bytes).
    pub fn of_part(record: &[u8]) -> Fingerprint {
        let mut classes = [0u16; TMD_CLASSES.len()];
        for (k, c) in classes.iter_mut().enumerate() {
            *c = u16::from_le_bytes([record[k * 2], record[k * 2 + 1]]);
        }
        Fingerprint {
            verts: u16::from_le_bytes([record[PART_VERTS], record[PART_VERTS + 1]]) as u32,
            classes,
        }
    }

    /// Every object of a decoded TMD.
    pub fn of_mesh(m: &RenderMesh, verts: &[u32]) -> Vec<Fingerprint> {
        let mut out: Vec<Fingerprint> = verts
            .iter()
            .map(|&verts| Fingerprint {
                verts,
                classes: [0; TMD_CLASSES.len()],
            })
            .collect();
        for p in &m.polys {
            if let Some(f) = out.get_mut(p.group as usize) {
                f.classes[p.slot as usize] += 1;
            }
        }
        out
    }
}

/// Where a model's TMD came from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ModelSource {
    /// A TMD at byte `at` of a file; `model` is its actor model number in an NPC pack.
    Disc {
        file: String,
        at: usize,
        model: Option<usize>,
    },
    /// No file holds it: read from RAM at this address.
    Ram { at: u32 },
}

impl std::fmt::Display for ModelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelSource::Disc {
                file,
                model: Some(m),
                ..
            } => write!(f, "{file}#{m}"),
            ModelSource::Disc { file, at, .. } => write!(f, "{file}@{at:#x}"),
            ModelSource::Ram { at } => write!(f, "RAM {at:#010x}"),
        }
    }
}

struct Indexed {
    source: ModelSource,
    tmd: Vec<u8>,
}

/// Every model on the disc by signature, and every object by fingerprint. A model several files
/// share is one entry (the first in [`MODEL_FILES`] order).
#[derive(Default)]
pub struct ModelIndex {
    by_sig: HashMap<u64, Indexed>,
    by_shape: HashMap<Fingerprint, Vec<(u64, u16)>>,
    /// TMDs read (copies included).
    pub found: usize,
}

impl ModelIndex {
    /// An index that knows no files: every model comes from RAM.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn build(files: &dyn WorldFiles) -> Self {
        Self::build_from(files, &MODEL_FILES)
    }

    /// An index of the models in files of `kinds` only.
    pub fn build_from(files: &dyn WorldFiles, kinds: &[Kind]) -> Self {
        let mut ix = ModelIndex::default();
        for &kind in kinds {
            let t = sub_table(kind);
            for n in 0..t.count {
                let file = format!("{}/{n}", t.prefix);
                let Ok(b) = files.file(&file) else { continue };
                // NPC packs number their actor models; `sys/6` holds the Knight (0) and the Baby (1).
                let at = |m: &[u8]| m.as_ptr() as usize - b.as_ptr() as usize;
                let models: HashMap<usize, usize> = if kind == Kind::Npc {
                    NpcPack::parse(&b)
                        .map(|p| {
                            (0..p.group_a(&b))
                                .filter_map(|i| Some((at(p.model(&b, i)?), i)))
                                .collect()
                        })
                        .unwrap_or_default()
                } else if file == "sys/6" {
                    [Container::Sys6Knight, Container::Sys6Baby]
                        .into_iter()
                        .enumerate()
                        .filter_map(|(i, c)| Some((at(model_parts(c, &b)?.main), i)))
                        .collect()
                } else {
                    HashMap::new()
                };
                for at in (0..b.len().saturating_sub(12)).step_by(4) {
                    if b[at] != 0x41
                        || b[at + 1..at + 8] != [0; 7]
                        || !(1..=64).contains(&b[at + 8])
                        || b[at + 9..at + 12] != [0; 3]
                    {
                        continue;
                    }
                    let Some(sig) = signature(&b[at..]) else {
                        continue;
                    };
                    ix.found += 1;
                    if ix.by_sig.contains_key(&sig) {
                        continue;
                    }
                    let objs = objects(&b[at..]).expect("signed");
                    let end = objs
                        .iter()
                        .map(|o| {
                            (o.verts + o.n_verts as usize * 8)
                                .max(o.normals + o.n_normals as usize * 8)
                                .max(o.prims + o.prim_bytes)
                        })
                        .max()
                        .unwrap_or(0);
                    let tmd = b[at..(at + end).min(b.len())].to_vec();
                    let Some(m) = tmd_mesh(&tmd) else { continue };
                    let verts: Vec<u32> = objs.iter().map(|o| o.n_verts).collect();
                    for (o, fp) in Fingerprint::of_mesh(&m, &verts).into_iter().enumerate() {
                        ix.by_shape.entry(fp).or_default().push((sig, o as u16));
                    }
                    ix.by_sig.insert(
                        sig,
                        Indexed {
                            source: ModelSource::Disc {
                                file: file.clone(),
                                at,
                                model: models.get(&at).copied(),
                            },
                            tmd,
                        },
                    );
                }
            }
        }
        ix
    }

    /// Distinct models.
    pub fn len(&self) -> usize {
        self.by_sig.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_sig.is_empty()
    }

    pub fn get(&self, sig: u64) -> Option<(&ModelSource, &[u8])> {
        self.by_sig.get(&sig).map(|i| (&i.source, i.tmd.as_slice()))
    }

    /// Objects with this shape, in [`MODEL_FILES`] order.
    pub fn shaped(&self, fp: &Fingerprint) -> &[(u64, u16)] {
        self.by_shape.get(fp).map_or(&[], Vec::as_slice)
    }
}

/// A model's mesh: one range per TMD object, corner vertices indexing that object's own vertices
/// (what a part record's live vertices are).
pub struct ModelAsset {
    pub sig: u64,
    pub source: ModelSource,
    /// Per object: vertex count.
    pub verts: Vec<u32>,
    pub shapes: Vec<Fingerprint>,
    /// The mesh before its UVs are moved to an atlas (each area's VRAM gives its own atlas).
    pub(crate) render: RenderMesh,
    /// The mesh with atlas UVs (the atlas layout depends only on the UVs, not the texels).
    pub mesh: MeshData,
}

/// Builds the model whose TMD (file form) is `tmd`. `None` if lt-core can't read it.
pub fn build(sig: u64, source: ModelSource, tmd: &[u8]) -> Option<ModelAsset> {
    let objs = objects(tmd)?;
    let render = tmd_mesh(tmd)?;
    let verts: Vec<u32> = objs.iter().map(|o| o.n_verts).collect();
    let base: Vec<u32> = verts
        .iter()
        .scan(0u32, |s, &n| Some(std::mem::replace(s, *s + n)))
        .collect();
    // Where each polygon's texels go in an atlas: the packing reads only the UVs (every texel
    // owned here, so no polygon is flagged as sampling unowned VRAM).
    let mut placed = render.clone();
    let blank =
        lt_core::mesh::OwnedVram::whole(vec![0; lt_core::vram::VRAM_W * lt_core::vram::VRAM_H]);
    let _ = lt_core::mesh::build_atlas(&mut placed, &blank);
    let shapes = Fingerprint::of_mesh(&render, &verts);
    Some(ModelAsset {
        sig,
        source,
        verts,
        shapes,
        mesh: MeshData::of(&placed, &base),
        render,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_record_gives_its_shape() {
        let mut rec = vec![0u8; PART_BYTES as usize];
        rec[16 * 2] = 11; // class 16
        rec[19 * 2] = 101;
        rec[PART_VERTS..PART_VERTS + 2].copy_from_slice(&285u16.to_le_bytes());
        let fp = Fingerprint::of_part(&rec);
        assert_eq!(fp.verts, 285);
        assert_eq!(
            (fp.classes[16], fp.classes[19], fp.classes[0]),
            (11, 101, 0)
        );
        assert_eq!(
            ModelSource::Disc {
                file: "npc/6".into(),
                at: 0x40,
                model: Some(3)
            }
            .to_string(),
            "npc/6#3"
        );
        assert_eq!(
            ModelSource::Disc {
                file: "sys/9".into(),
                at: 0x3914,
                model: None
            }
            .to_string(),
            "sys/9@0x3914"
        );
    }
}
