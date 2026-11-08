//! A tick's assets as screen triangles: each chunk group and character part drawn through the
//! camera and matrices the capture recorded, polygon by polygon as the game's draw paths would.
//!
//! [`lit`] also gives each corner its world position and normal, and collects the shadow casters:
//! every polygon of each drawn chunk (all its groups, so a house just off screen still casts; kept
//! per mesh) and of each drawn character, opaque ones only (LT-112, `crate::light`).

use lt_assets::mesh::Corner;
use lt_assets::{Assets, Frame, MeshKey, TextureKey};
use lt_core::mesh::flag;
use lt_scene::{Camera, Record};

use crate::gte::{test_within, Bounds, Cull, Gte, Path, Precision, Screen, FAR};
use std::sync::Arc;

use crate::interp::Pose;
use crate::light::{smooth_normals, Affine, Lamp, Normals, V3};

/// One triangle corner on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub screen: Screen,
    pub uv: [u16; 2],
    pub rgb: [u8; 3],
    pub flags: u8,
    /// The polygon's ordering-table bucket: the 2D layer sorts against it.
    pub bucket: u32,
    /// World position and normal ([`lit`] only).
    pub world: V3,
    pub normal: V3,
}

/// A shadow caster's corner: world position and atlas texel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CasterVertex {
    pub world: V3,
    pub uv: [u16; 2],
    pub flags: u8,
}

/// A drawn chunk's shadow casters: its mesh's (kept per mesh), and the chunk's map to world space.
#[derive(Clone, Debug)]
pub struct ChunkCaster {
    pub mesh: MeshKey,
    pub texture: TextureKey,
    /// `crate::light::Normals::casters`: mesh-space corners, packed.
    pub bytes: Arc<Vec<u8>>,
    pub to_world: Affine,
}

/// What [`lit`] adds to a scene.
#[derive(Clone, Debug, Default)]
pub struct Lit {
    /// The camera's world position.
    pub eye: V3,
    /// Every polygon of each drawn chunk (all its groups, so a house just off screen still casts).
    pub chunk_casters: Vec<ChunkCaster>,
    /// The characters' triangles, posed this tick, in world space, by texture.
    pub casters: Vec<(TextureKey, Vec<CasterVertex>)>,
    /// The drawn chunks' lamps, in world space (`crate::light::lamps`).
    pub lamps: Vec<Lamp>,
}

/// Triangles that sample one texture.
#[derive(Clone, Debug, Default)]
pub struct Batch {
    pub texture: TextureKey,
    pub corners: Vec<Vertex>,
    /// Characters' triangles (else terrain).
    pub model: bool,
}

/// One polygon, drawn or not, kept for explaining a mismatch.
#[derive(Clone, Debug)]
pub struct Polygon {
    /// `chunk 78 group 12` or `character 5 object 0`.
    pub what: String,
    pub screen: Vec<Screen>,
    pub cull: Option<Cull>,
    pub flags: u8,
    pub rgb: [u8; 3],
}

/// How far past the game's screen a scene reaches (LT-114): `left` columns added each side of the
/// screen (whole pixels), and the depth polygons must stay under.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sight {
    pub left: u32,
    pub far: u32,
}

impl Sight {
    /// The game's screen.
    pub const GAME: Sight = Sight { left: 0, far: FAR };
}

impl Default for Sight {
    fn default() -> Self {
        Sight::GAME
    }
}

/// What a tick draws.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    /// The game's screen (320 or 512 × 224); a wider view adds `left` columns each side (screen x
    /// runs from `-left` to `width + left`).
    pub width: u32,
    pub height: u32,
    pub left: u32,
    pub batches: Vec<Batch>,
    /// Polygons drawn, and dropped by the draw paths' tests.
    pub polygons: u64,
    pub culled: u64,
    /// Every polygon with its screen position, when asked for ([`scene`]'s `keep`).
    pub all: Vec<Polygon>,
    keep: bool,
    /// World positions, normals and casters, when built by [`lit`].
    pub lit: Option<Lit>,
}

/// The polygons of a corner range: triangles are 3 corners, quads 6 (`0-1-2`, `1-3-2`).
fn polygons(corners: &[Corner]) -> impl Iterator<Item = (&[Corner], [usize; 4], usize)> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        if i >= corners.len() {
            return None;
        }
        let quad = corners[i].flags & flag::QUAD != 0;
        let (len, idx, n) = if quad { (6, [0, 1, 2, 4], 4) } else { (3, [0, 1, 2, 2], 3) };
        let poly = &corners[i..(i + len).min(corners.len())];
        i += len;
        (poly.len() == len).then_some((poly, idx, n))
    })
}

/// Projects a tick (nothing without a camera). `keep` keeps every polygon in [`Scene::all`].
pub fn scene(record: &Record, frame: &Frame, assets: &Assets, precision: Precision, keep: bool) -> Scene {
    build(record, frame, assets, precision, keep, None, None, Sight::GAME)
}

/// A frame between two ticks (LT-113): tick B's draws with `pose`'s transforms and vertices, in
/// floating point; lit when `normals` is given.
pub fn posed(record: &Record, frame: &Frame, assets: &Assets, normals: Option<&mut Normals>, pose: &Pose) -> Scene {
    build(record, frame, assets, Precision::Float, false, normals, Some(pose), Sight::GAME)
}

/// [`scene`] with world positions, normals and shadow casters, for lighting (`normals` keeps the
/// chunks' between ticks).
pub fn lit(record: &Record, frame: &Frame, assets: &Assets, precision: Precision, normals: &mut Normals) -> Scene {
    build(record, frame, assets, precision, false, Some(normals), None, Sight::GAME)
}

/// Any of the above through `sight` (LT-114): lit when `normals` is given, between ticks when
/// `pose` is (in floating point then, whatever `precision` says).
pub fn scene_with(record: &Record, frame: &Frame, assets: &Assets, precision: Precision, normals: Option<&mut Normals>, pose: Option<&Pose>, sight: Sight) -> Scene {
    let precision = if pose.is_some() { Precision::Float } else { precision };
    build(record, frame, assets, precision, false, normals, pose, sight)
}

#[allow(clippy::too_many_arguments)]
fn build(record: &Record, frame: &Frame, assets: &Assets, precision: Precision, keep: bool, mut normals: Option<&mut Normals>, pose: Option<&Pose>, sight: Sight) -> Scene {
    let Some(cam) = record.camera.as_ref() else { return Scene::default() };
    let width = if cam.wide { 512 } else { 320 };
    let mut s = Scene { width, height: 224, left: sight.left, keep, ..Scene::default() };
    let bounds = Bounds { x0: -(sight.left as f32), x1: (width + sight.left) as f32, far: sight.far };
    // World from view: the camera's view matrix inverted.
    let view = pose.map_or_else(|| Affine::of(&cam.view), |p| p.view);
    let to_world = normals.is_some().then(|| view.inverse()).flatten();
    // Each draw's GTE: the tick's own, or the pose's exact transform.
    let gte_of = |m: &lt_scene::Matrix, exact: Option<&Affine>| match exact {
        Some(a) => Gte::exact(a.m, a.t, cam.h, cam.offset),
        None => gte_for(cam, m),
    };
    if let Some(w) = &to_world {
        s.lit = Some(Lit { eye: w.point([0, 0, 0]), ..Lit::default() });
    }
    for (ci, c) in frame.chunks.iter().enumerate() {
        let Some(mesh) = assets.mesh(c.mesh) else { continue };
        let (mv, groups) = match pose {
            Some(p) => (p.chunks[ci].0, &p.chunks[ci].1),
            None => (Affine::of(&c.model_view), &c.groups),
        };
        let gte = gte_of(&c.model_view, pose.map(|_| &mv));
        let mut b = Batch { texture: c.texture, corners: Vec::new(), model: false };
        // The chunk's corners in world space, and its normals (a chunk's map to world is a shift).
        let world = to_world.map(|w| w.after(&mv));
        let (chunk_normals, chunk_lamps, chunk_casters) = match (normals.as_deref_mut(), &world) {
            (Some(n), Some(_)) => (Some(n.chunk(c.mesh, &mesh.corners)), Some(n.lamps(c.mesh, &mesh.corners)), Some(n.casters(c.mesh, &mesh.corners))),
            _ => (None, None, None),
        };
        for &g in groups {
            let Some(&(first, count)) = mesh.ranges.get(g as usize) else { continue };
            let range = &mesh.corners[first as usize..(first + count) as usize];
            let what = format!("chunk {} group {g}", c.chunk);
            let space = world.as_ref().zip(chunk_normals.as_ref()).map(|(w, n)| {
                let n = &n[first as usize..(first + count) as usize];
                (range.iter().map(|k| w.point(k.pos)).collect::<Vec<_>>(), n.iter().map(|&v| w.direction(v)).collect::<Vec<_>>())
            });
            add(&mut s, &mut b, &gte, Path::Terrain, precision, bounds, range, &what, |k| k.pos.map(|v| v as f32), space.as_ref());
        }
        if let (Some(lit), Some(w), Some(bytes)) = (s.lit.as_mut(), &world, chunk_casters) {
            lit.chunk_casters.push(ChunkCaster { mesh: c.mesh, texture: c.texture, bytes, to_world: *w });
            for l in chunk_lamps.iter().flat_map(|v| v.iter()) {
                let pos = w.apply(l.pos.map(|v| v as f64)).map(|v| v as f32);
                lit.lamps.push(Lamp { pos, ..*l });
            }
        }
        s.batches.push(b);
    }
    for (hi, ch) in frame.characters.iter().enumerate() {
        let MeshKey::Model(_) = ch.mesh else { continue };
        let Some(mesh) = assets.mesh(ch.mesh) else { continue };
        let mut b = Batch { texture: ch.texture, corners: Vec::new(), model: true };
        let mut cast = Vec::new();
        for (pi, p) in ch.parts.iter().enumerate() {
            let Some(&(first, count)) = mesh.ranges.get(p.object as usize) else { continue };
            let range = &mesh.corners[first as usize..(first + count) as usize];
            let between = pose.and_then(|ps| ps.parts.get(hi)).and_then(|v| v.get(pi));
            let mv = between.map_or_else(|| Affine::of(&p.model_view), |b| b.0);
            let gte = gte_of(&p.model_view, between.map(|b| &b.0));
            let what = format!("character {} object {}", ch.index, p.object);
            let rest = |k: &Corner| k.pos.map(|v| v as f32);
            let posed = |k: &Corner| -> V3 {
                match between {
                    Some(b) => b.1.get(k.vertex as usize).copied().unwrap_or_else(|| rest(k)),
                    None => p.vertices.get(k.vertex as usize).map_or_else(|| rest(k), |v| v.map(|x| x as f32)),
                }
            };
            // A character's normals come from this tick's pose, in world space.
            let space = to_world.map(|w| {
                let w = w.after(&mv);
                let pos: Vec<V3> = range.iter().map(|k| w.point_f(posed(k))).collect();
                let n = smooth_normals(&pos);
                (pos, n)
            });
            if let Some((pos, _)) = &space {
                cast.extend(range.iter().zip(pos).filter(|(k, _)| k.flags & flag::SEMI == 0).map(|(k, &world)| CasterVertex { world, uv: k.uv, flags: k.flags }));
            }
            add(&mut s, &mut b, &gte, Path::Model, precision, bounds, range, &what, posed, space.as_ref());
        }
        if let Some(lit) = s.lit.as_mut() {
            lit.casters.push((ch.texture, cast));
        }
        s.batches.push(b);
    }
    s
}

fn gte_for(cam: &Camera, m: &lt_scene::Matrix) -> Gte {
    Gte::new(m, cam.h, cam.offset)
}

#[allow(clippy::too_many_arguments)]
fn add(s: &mut Scene, b: &mut Batch, gte: &Gte, path: Path, precision: Precision, bounds: Bounds, range: &[Corner], what: &str, pos: impl Fn(&Corner) -> V3, space: Option<&(Vec<V3>, Vec<V3>)>) {
    let mut at = 0usize;
    for (poly, idx, n) in polygons(range) {
        let start = at;
        at += poly.len();
        let verts: Vec<V3> = idx[..n].iter().map(|&i| pos(&poly[i])).collect();
        let d = test_within(gte, path, precision, &verts, bounds);
        if s.keep {
            s.all.push(Polygon { what: what.to_string(), screen: d.screen.clone(), cull: d.cull, flags: poly[0].flags, rgb: poly[0].rgb });
        }
        if d.cull.is_some() {
            s.culled += 1;
            continue;
        }
        let (screen, bucket) = (d.screen, d.bucket);
        s.polygons += 1;
        let tris: &[[usize; 3]] = if n == 4 { &[[0, 1, 2], [1, 3, 2]] } else { &[[0, 1, 2]] };
        for t in tris {
            for &v in t {
                let k = &poly[idx[v]];
                let (world, normal) = space.map_or(([0.0; 3], [0.0; 3]), |(p, nm)| (p[start + idx[v]], nm[start + idx[v]]));
                b.corners.push(Vertex { screen: screen[v], uv: k.uv, rgb: k.rgb, flags: k.flags, bucket, world, normal });
            }
        }
    }
}
