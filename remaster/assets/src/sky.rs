//! The first flight's sky cells (LT-143): OVR24's sea and islands.
//!
//! OVR24 draws its map as cells, each a type's mesh: a record at 0x8001000C + type × 16 holds the
//! cell's primitive lists and its vertex pool, which `func_80133914` gets. The lists are a chunk
//! group's eight lists, record for record (`lt_core::world::lists`); the pool is `[x, y, z, pad]`
//! a vertex. Both are in RAM only (the overlay's data), so a cell's mesh is read from RAM and
//! decoded as a chunk group, its atlas cut from the running game's VRAM as a model's is. A mesh is
//! its bytes' hash: a cell met again is the same asset.

use lt_core::mesh::{build_atlas, chunk_mesh, OwnedVram};
use lt_core::world::{self, MeshGroup};

use crate::mesh::{MeshData, TextureData};
use crate::tmd::{fnv_on, Ram};

/// Records in one list at most, and vertices in a pool (a guard against reading garbage).
const MAX_RECORDS: usize = 0x2000;
const MAX_VERTICES: usize = 0x2000;

/// A sky cell type's mesh and atlas.
pub struct SkyAsset {
    pub mesh: MeshData,
    pub texture: crate::TextureKey,
}

/// The bytes of the eight lists at `lists`, and of the pool's vertices they use. `None` when the
/// lists don't parse.
pub fn read(ram: &dyn Ram, lists: u32) -> Option<Vec<u8>> {
    let mut len = 0usize;
    for dw in world::LIST_DWORDS {
        let n = u32::from_le_bytes(ram.read(lists + len as u32, 4).try_into().ok()?) as usize;
        if n > MAX_RECORDS {
            return None;
        }
        len += 4 + n * dw * 4;
    }
    Some(ram.read(lists, len))
}

/// The key of the cell at `lists` and `pool`: its lists' and vertices' hash.
pub fn key(body: &[u8], verts: &[u8]) -> u64 {
    fnv_on(fnv_on(crate::tmd::fnv(b"sky"), body), verts)
}

/// The pool's bytes for the vertices `prims` use.
pub fn pool_bytes(ram: &dyn Ram, pool: u32, prims: &[world::Prim]) -> Option<Vec<u8>> {
    let n = prims.iter().flat_map(|p| p.idx[..p.n as usize].iter().copied()).max().map_or(0, |m| m as usize + 1);
    (n <= MAX_VERTICES).then(|| ram.read(pool, n * 8))
}

/// Decodes a cell type: its lists `body` (from [`read`]) and its pool's bytes, one group; the
/// atlas from `vram`.
pub fn build(body: &[u8], verts: &[u8], vram: &OwnedVram) -> Option<(MeshData, TextureData)> {
    let (prims, lists, _) = world::lists(body, 0)?;
    let pool: Vec<[i16; 3]> = verts.as_chunks::<8>().0.iter().map(|v| [0, 2, 4].map(|i| i16::from_le_bytes([v[i], v[i + 1]]))).collect();
    let count = prims.len() as u32;
    let group = MeshGroup { index: 0, mesh_ofs: 0, centre: [0; 3], lists, first: 0, count, bbox: [0; 6] };
    let parsed = world::Mesh { groups: vec![group], pool, prims, mesh_end: body.len() as u32 };
    let mut m = chunk_mesh(&parsed);
    let atlas = build_atlas(&mut m, vram);
    Some((MeshData::of(&m, &[]), atlas.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One flat triangle in a cell's lists, its pool of three vertices.
    #[test]
    fn a_cell_reads_and_decodes() {
        let mut ram = vec![0u8; 0x200];
        let mut at = 0x100;
        let mut put = |w: u32| {
            ram[at..at + 4].copy_from_slice(&w.to_le_bytes());
            at += 4;
        };
        put(1);
        put(0x2040_4040);
        put(0x0001_0000);
        put(0x0000_0002);
        for _ in 0..7 {
            put(0);
        }
        for (k, v) in [[0i16, 0, 0], [100, 0, 0], [0, 0, 100]].iter().enumerate() {
            for (i, c) in v.iter().enumerate() {
                ram[0x20 + k * 8 + i * 2..0x22 + k * 8 + i * 2].copy_from_slice(&c.to_le_bytes());
            }
        }
        let read_ram = |a: u32, n: usize| ram[a as usize..a as usize + n].to_vec();
        let body = read(&read_ram, 0x100).unwrap();
        assert_eq!(body.len(), 4 * 8 + 12);
        let (prims, _, _) = world::lists(&body, 0).unwrap();
        let verts = pool_bytes(&read_ram, 0x20, &prims).unwrap();
        assert_eq!(verts.len(), 3 * 8);
        let (mesh, _) = build(&body, &verts, &OwnedVram::whole(vec![0; 1024 * 512])).unwrap();
        assert_eq!(mesh.ranges, vec![(0, 3)]);
        assert_eq!(mesh.corners.iter().map(|c| c.pos).collect::<Vec<_>>(), vec![[0, 0, 0], [100, 0, 0], [0, 0, 100]]);
        assert_eq!(mesh.corners[0].rgb, [0x40, 0x40, 0x40]);
    }
}
