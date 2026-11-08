//! Chunk assets: a chunk's draw groups and its atlas, as Living Tool's editor decodes them
//! (`lt_core::mesh::chunk_mesh`, `field_vram`, `build_atlas`), from the disc or a project.
//!
//! The game loads a chunk's body into its slot (0x80010000 + slot × 0x2E000) as the file holds
//! it, then relocates the group table's pointers. `WorldChunk_DrawObjectMesh` gets a group's list
//! data, so the captured mesh offset is the group's `mesh_ofs`. A match also needs the group's
//! eight lists in RAM to equal the file's: a project's chunk that the running game hasn't loaded
//! is reported, not drawn.

use std::collections::HashMap;
use std::ops::Range;

use lt_core::mesh::{build_atlas, chunk_mesh, field_vram};
use lt_core::world::{self, LIST_DWORDS};
use lt_core::worldmap::WorldFiles;

use crate::mesh::{MeshData, TextureData};
use crate::tmd::{fnv, le32};

/// A chunk group, by its mesh offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub index: u32,
    /// Bytes of its eight lists in the chunk.
    pub lists: Range<usize>,
}

pub struct ChunkAsset {
    pub id: u16,
    /// FNV of the chunk's bytes as read: another version of the chunk is another asset.
    pub file: u64,
    pub mesh: MeshData,
    /// Its atlas (held by [`crate::Assets`]).
    pub texture: crate::TextureKey,
    pub groups: HashMap<u32, GroupInfo>,
    /// Each group's index and its culling centre (chunk-local, Y down): the point
    /// `WorldChunk_DrawObjects` tests against the view (LT-114 widens that test).
    pub centres: Vec<(u32, [i16; 3])>,
    raw: Vec<u8>,
}

impl ChunkAsset {
    /// The group at mesh offset `ofs`, if its lists in RAM (`ram`, read from the group's mesh
    /// address for [`ChunkAsset::list_bytes`]) are the file's.
    pub fn group(&self, ofs: u32, ram: &[u8]) -> Result<u32, String> {
        let g = self
            .groups
            .get(&ofs)
            .ok_or_else(|| format!("chunk/{} has no group at {ofs:#x}", self.id))?;
        if self.raw.get(g.lists.clone()) != Some(ram) {
            return Err(format!(
                "chunk/{} group {} differs in RAM from the chunk read",
                self.id, g.index
            ));
        }
        Ok(g.index)
    }

    /// Bytes to read from RAM for the group at `ofs`.
    pub fn list_bytes(&self, ofs: u32) -> usize {
        self.groups.get(&ofs).map_or(0, |g| g.lists.len())
    }
}

/// The shared field textures: SYS08 (`sys/7`), the region sets (`fe/k`) and `ChunkTextureSetIds`.
pub struct FieldTextures {
    atlas: Option<Vec<u8>>,
    sets: HashMap<u32, Option<Vec<u8>>>,
    ids: Vec<u16>,
}

impl FieldTextures {
    pub fn load(files: &dyn WorldFiles) -> Self {
        FieldTextures {
            atlas: files.file("sys/7").ok(),
            sets: HashMap::new(),
            ids: world::texset_ids(files.exe()).unwrap_or_default(),
        }
    }
}

/// The bytes of each group's lists: the 8 counts and their records.
fn group_lists(raw: &[u8], mesh_ofs: usize) -> Option<Range<usize>> {
    let mut p = mesh_ofs;
    for dw in LIST_DWORDS {
        let n = le32(raw.get(p..p + 4)?, 0) as usize;
        p += 4 + n * dw * 4;
    }
    (p <= raw.len()).then_some(mesh_ofs..p)
}

/// Decodes `chunk/id` from `files`, and its atlas. `None` when the chunk can't be read or parsed.
pub fn build(
    files: &dyn WorldFiles,
    tex: &mut FieldTextures,
    id: u16,
) -> Option<(ChunkAsset, TextureData)> {
    let raw = files.chunk(id as u32).ok()?;
    let parsed = world::mesh(&raw)?;
    let mut m = chunk_mesh(&parsed);
    let siblings: Vec<Vec<u8>> = if id as u32 >= world::GRID_CHUNKS {
        world::interior_group(id as u32)
            .into_iter()
            .filter(|&s| s != id as u32 && world::valid_chunk(s))
            .filter_map(|s| files.chunk(s).ok())
            .collect()
    } else {
        Vec::new()
    };
    let k = world::texset_for(&tex.ids, id as u32);
    let set = tex
        .sets
        .entry(k)
        .or_insert_with(|| files.file(&format!("fe/{k}")).ok())
        .clone();
    let vram = field_vram(tex.atlas.as_deref(), set.as_deref(), &siblings, &raw);
    let atlas = build_atlas(&mut m, &vram);
    let groups = parsed
        .groups
        .iter()
        .filter_map(|g| {
            Some((
                g.mesh_ofs,
                GroupInfo {
                    index: g.index,
                    lists: group_lists(&raw, g.mesh_ofs as usize)?,
                },
            ))
        })
        .collect();
    let centres = parsed.groups.iter().map(|g| (g.index, g.centre)).collect();
    let body = raw[..raw.len().min(world::TIM_OFFSET)].to_vec();
    let texture: TextureData = atlas.into();
    Some((
        ChunkAsset {
            id,
            file: fnv(&raw),
            mesh: MeshData::of(&m, &[]),
            texture: crate::texture_key(&texture),
            groups,
            centres,
            raw: body,
        },
        texture,
    ))
}
