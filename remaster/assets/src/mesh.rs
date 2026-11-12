//! What the renderer uploads for an asset: triangle corners in draw order, cut into ranges (a
//! chunk's groups, a model's objects), and the atlas they sample.

use lt_core::mesh::{Atlas, RenderMesh, Tile};

/// One triangle corner, 16 bytes as uploaded ([`Corner::bytes`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Corner {
    /// Rest position, chunk- or model-local, Y down.
    pub pos: [i16; 3],
    /// The vertex it takes its position from each tick: an index into the object's live
    /// vertices (models), or into the chunk's pool (chunks, whose vertices don't move).
    pub vertex: u16,
    /// Atlas texel.
    pub uv: [u16; 2],
    /// Colour (128 = 1.0 under a texture) and the polygon's `lt_core::mesh::flag` bits.
    pub rgb: [u8; 3],
    pub flags: u8,
}

impl Corner {
    pub const BYTES: usize = 16;

    pub fn bytes(corners: &[Corner]) -> Vec<u8> {
        let mut out = Vec::with_capacity(corners.len() * Self::BYTES);
        for c in corners {
            for v in c.pos {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&c.vertex.to_le_bytes());
            for v in c.uv {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&c.rgb);
            out.push(c.flags);
        }
        out
    }
}

/// A mesh ready to upload.
#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub corners: Vec<Corner>,
    /// Per range (chunk group or model object): first corner and corner count.
    pub ranges: Vec<(u32, u32)>,
    /// Each corner's normal from the file (4.12, rest pose), for a polygon the GTE lights
    /// (`lt_core::mesh::flag::LIT`); zero for a prelit one (LT-116). Not uploaded: the renderer
    /// turns them to world space and lights the battle with them.
    pub normals: Vec<[i16; 3]>,
}

impl MeshData {
    /// Triangles of `m` (quads are TMD "Z" order: 0-1-2 and 1-3-2), one range per group, after
    /// [`lt_core::mesh::build_atlas`] has rewritten the UVs. `base[g]` is subtracted from group
    /// `g`'s vertex indices (a model object's first vertex; 0 for chunks).
    pub fn of(m: &RenderMesh, base: &[u32]) -> MeshData {
        let mut out = MeshData {
            corners: Vec::with_capacity(m.polys.len() * 6),
            ranges: Vec::with_capacity(m.groups.len()),
            normals: Vec::with_capacity(m.polys.len() * 6),
        };
        for (g, grp) in m.groups.iter().enumerate() {
            let first = out.corners.len() as u32;
            let b = base.get(g).copied().unwrap_or(0);
            for p in &m.polys[grp.first as usize..(grp.first + grp.count) as usize] {
                let tris: &[[usize; 3]] = if p.n == 4 {
                    &[[0, 1, 2], [1, 3, 2]]
                } else {
                    &[[0, 1, 2]]
                };
                for t in tris {
                    for &c in t {
                        let v = p.v[c];
                        out.corners.push(Corner {
                            pos: m.verts.get(v as usize).copied().unwrap_or_default(),
                            vertex: (v as u32).saturating_sub(b) as u16,
                            uv: p.uv[c],
                            rgb: [p.rgb[c][0], p.rgb[c][1], p.rgb[c][2]],
                            flags: p.flags,
                        });
                        let lit = p.flags & lt_core::mesh::flag::LIT != 0;
                        out.normals.push(if lit { m.normals.get(p.nrm[c] as usize).copied().unwrap_or_default() } else { [0; 3] });
                    }
                }
            }
            out.ranges.push((first, out.corners.len() as u32 - first));
        }
        out
    }
}

/// An atlas ready to upload, and the tiles it was cut from.
#[derive(Clone, Debug)]
pub struct TextureData {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    pub tiles: Vec<Tile>,
}

impl From<Atlas> for TextureData {
    fn from(a: Atlas) -> Self {
        TextureData {
            w: a.w,
            h: a.h,
            rgba: a.rgba,
            tiles: a.tiles,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lt_core::mesh::{Group, Poly};

    #[test]
    fn quads_split_and_corners_index_their_object() {
        let mut m = RenderMesh {
            verts: vec![[0, 0, 0]; 8],
            ..Default::default()
        };
        m.polys.push(Poly {
            n: 3,
            v: [0, 1, 2, 2],
            group: 0,
            ..Default::default()
        });
        m.polys.push(Poly {
            n: 4,
            v: [4, 5, 6, 7],
            group: 1,
            rgb: [[1, 2, 3, 0]; 4],
            flags: 9,
            ..Default::default()
        });
        m.groups = vec![
            Group {
                centre: [0; 3],
                first: 0,
                count: 1,
            },
            Group {
                centre: [0; 3],
                first: 1,
                count: 1,
            },
        ];
        let d = MeshData::of(&m, &[0, 4]);
        assert_eq!(d.ranges, vec![(0, 3), (3, 6)]);
        let v: Vec<u16> = d.corners[3..].iter().map(|c| c.vertex).collect();
        assert_eq!(
            v,
            vec![0, 1, 2, 1, 3, 2],
            "object 1's own vertices, quad as 0-1-2 and 1-3-2"
        );
        assert_eq!(Corner::bytes(&d.corners[3..4]).len(), Corner::BYTES);
        assert_eq!(&Corner::bytes(&d.corners[3..4])[12..], &[1, 2, 3, 9]);
    }
}
