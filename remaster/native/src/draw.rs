//! A tick's assets as screen triangles: each chunk group and character part drawn through the
//! camera and matrices the capture recorded, polygon by polygon as the game's draw paths would.
//!
//! [`lit`] also gives each corner its world position and normal, and collects the shadow casters:
//! every polygon of each drawn chunk (all its groups, so a house just off screen still casts; kept
//! per mesh) and of each drawn character, opaque ones only (LT-112, `crate::light`).
//!
//! **The first flight's sky cells** (LT-143, `MeshKey::Sky`) are drawn as chunks are, with the
//! game's own tests (`func_80133914`'s depth limit is 0x1C00, not 0x3000). The game lights them by
//! NCDS with one normal for every polygon and fogs them by the depth cue toward a far colour:
//! [`SkyShade`] runs those GTE operations on PSoXide's GTE with the registers the capture took,
//! polygon by polygon in the game's order (RTPT, a quad's fourth vertex by RTPS, then NCDS a
//! corner), so the original look has the game's colours. The lit look takes the polygon's depth
//! cue as its fog (`Vertex::fog`) and lights the polygon's own colours.
//!
//! **Battles** (LT-116): the actors, Toys and arena are characters with the GTE's light at their draw.
//! Their lit polygons (`lt_core::mesh::flag::LIT`) take `Model_DrawParts`' own NCS/NCCS colours in the
//! original look ([`LightShade`]) and, lit, their own colours tinted by the actor's flash ([`flash`])
//! with the file's normals turned to world space. The arena drawn by `FUN_8014c97c` takes that
//! drawer's colours ([`arena_colour`]) and, lit, a lamp ([`arena_lamp`]). A part drawn through
//! `Model_DrawParts`' second entry keys on its farthest corner (`crate::gte::Path::ModelFar`).

use lt_assets::mesh::Corner;
use lt_assets::{Assets, Frame, MeshKey, TextureKey};
use lt_core::mesh::flag;
use lt_scene::{Camera, Record, SkyGte};

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
    /// How far into the fog the polygon is (0 none, 1 the far colour): the first flight's sky
    /// cells in the lit look ([`SkyShade`]).
    pub fog: f32,
    /// The game's water ([`crate::water`], lit only, LT-146), its distance across the ground to the
    /// shore, and its triangle's corners' nearest stretches of shore in world x, z (`x0, z0, x1, z1`;
    /// far off when there's none) (LT-148, [`crate::water::shore`]).
    pub water: bool,
    pub shore: f32,
    pub shores: [[f32; 4]; 3],
}

/// A stretch of shore that's none (LT-148): far off.
pub const NO_SHORE: [f32; 4] = [1.0e6; 4];

/// `func_80133914`'s depth limit (SZ below 0x1C00).
pub const SKY_FAR: u32 = 0x1C00;

/// PSoXide's GTE with the registers OVR24 lit a tick's cells with (LT-143).
pub struct SkyShade {
    gte: psx_gte_core::Gte,
    normal: [i16; 3],
}

const RTPT: u32 = 0x4A28_0030;
const RTPS: u32 = 0x4A18_0001;
const NCDS: u32 = 0x4AE8_0413;

fn xy(v: [i16; 3]) -> u32 {
    v[0] as u16 as u32 | (v[1] as u16 as u32) << 16
}

impl SkyShade {
    pub fn new(g: &SkyGte) -> SkyShade {
        let mut gte = psx_gte_core::Gte::new();
        for (i, &v) in g.control.iter().enumerate() {
            gte.write_control(i as u8, v);
        }
        SkyShade { gte, normal: g.normal }
    }

    /// A cell's model-view matrix into the rotation and translation registers.
    pub fn cell(&mut self, m: &lt_scene::Matrix) {
        let pair = |a: i16, b: i16| a as u16 as u32 | (b as u16 as u32) << 16;
        let r = m.m;
        for (i, v) in [pair(r[0][0], r[0][1]), pair(r[0][2], r[1][0]), pair(r[1][1], r[1][2]), pair(r[2][0], r[2][1]), r[2][2] as u16 as u32].into_iter().enumerate() {
            self.gte.write_control(i as u8, v);
        }
        for (i, &t) in m.t.iter().enumerate() {
            self.gte.write_control(5 + i as u8, t as u32);
        }
    }

    /// A polygon's corners (`pos` its vertices, `rgb` their colours): the colours `func_80133914`
    /// gives them, and the depth cue (0..1) it used.
    pub fn polygon(&mut self, pos: &[[i16; 3]], rgb: &[[u8; 3]]) -> ([[u8; 3]; 4], f32) {
        let g = &mut self.gte;
        for (k, v) in pos.iter().take(3).enumerate() {
            g.write_data(2 * k as u8, xy(*v));
            g.write_data(2 * k as u8 + 1, v[2] as u16 as u32);
        }
        g.execute(RTPT);
        if let Some(v) = pos.get(3) {
            g.write_data(0, xy(*v));
            g.write_data(1, v[2] as u16 as u32);
            g.execute(RTPS);
        }
        let fog = (g.read_data(8) as i16).clamp(0, 0x1000) as f32 / 4096.0;
        g.write_data(0, xy(self.normal));
        g.write_data(1, self.normal[2] as u16 as u32);
        let mut out = [[0; 3]; 4];
        for (o, c) in out.iter_mut().zip(rgb) {
            g.write_data(6, c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16);
            g.execute(NCDS);
            let w = g.read_data(22);
            *o = [w as u8, (w >> 8) as u8, (w >> 16) as u8];
        }
        (out, fog)
    }
}

/// How a cell's polygons are coloured: the game's colours (the original look), or their own with
/// the fog (lit).
struct Shade<'a> {
    sky: &'a mut SkyShade,
    lit: bool,
}

/// NCS and NCCS as `Model_DrawParts` runs them (with `sf = 1`, `lm = 1`).
const NCS: u32 = 0x4AC8_041E;
const NCCS: u32 = 0x4B08_041B;

/// PSoXide's GTE with a battle draw's light registers (LT-116): the colours `Model_DrawParts` gives
/// the corners of the polygons it lights by their normals. A textured class gets NCS (or NCT, the
/// same a normal at a time): the light alone, which the GPU modulates the texture by. An untextured
/// one gets NCCS (NCCT): the light times the polygon's own colour.
pub struct LightShade {
    gte: psx_gte_core::Gte,
}

impl LightShade {
    pub fn new(light: &lt_scene::GteLight) -> LightShade {
        let mut gte = psx_gte_core::Gte::new();
        for (i, v) in light.control() {
            gte.write_control(i, v);
        }
        LightShade { gte }
    }

    /// A lit corner's colour: its normal (model space) and own colour, and whether it's textured.
    pub fn corner(&mut self, normal: [i16; 3], rgb: [u8; 3], textured: bool) -> [u8; 3] {
        let g = &mut self.gte;
        g.write_data(0, xy(normal));
        g.write_data(1, normal[2] as u16 as u32);
        g.write_data(6, rgb[0] as u32 | (rgb[1] as u32) << 8 | (rgb[2] as u32) << 16);
        g.execute(if textured { NCS } else { NCCS });
        let w = g.read_data(22);
        [w as u8, (w >> 8) as u8, (w >> 16) as u8]
    }
}

/// The arena's corners as `FUN_8014c97c` colours them while a battle light is set (LT-116): one base
/// colour for the whole arena (NCS of the normal (0, 0, 0x800) under the arena's light), and the
/// light's colour near it. `d2 = |vertex + offset|² >> 10`; within the strength a corner takes the
/// light's colour, past it each channel falls by `d2 − strength`, never below the base.
pub fn arena_colour(light: &lt_scene::ArenaLight, base: [u8; 3], vertex: [i16; 3]) -> [u8; 3] {
    // Each sum goes to IR1–IR3 (16 bits) and SQR squares it; the three add in 32 bits.
    let d = [0, 1, 2].map(|i| vertex[i].wrapping_add(light.offset[i]) as i64);
    let d2 = (d.iter().map(|&v| (v * v) as u32).fold(0u32, u32::wrapping_add) >> 10) as i32;
    if d2 < light.strength {
        return light.rgb;
    }
    let past = d2 - light.strength;
    [0, 1, 2].map(|c| (light.rgb[c] as i32 - past).max(base[c] as i32) as u8)
}

/// The arena light as a lamp in the lit look (LT-116): at minus its offset in the arena's space,
/// falling off as the game's does (full within `d2` of the strength, nothing past `d2` of the strength
/// plus 255). The game takes the brighter of the light and the arena's `base` colour channel by
/// channel, so the lamp adds what the light has over the base (a vertex colour: 128 modulates by 1.0).
pub fn arena_lamp(light: &lt_scene::ArenaLight, base: [u8; 3], to_world: &Affine) -> Lamp {
    let radius = |d2: i32| ((d2.max(0) as f32) * 1024.0).sqrt();
    Lamp {
        pos: to_world.point(light.offset.map(|v| v.saturating_neg())),
        reach: radius(light.strength + 255).max(1.0),
        colour: [0, 1, 2].map(|c| (light.rgb[c] as f32 - base[c] as f32).max(0.0) / 128.0),
        corners: 0,
        inner: radius(light.strength),
    }
}

/// How much brighter (per channel) an actor's light is than the battle area's base light (LT-116):
/// a flash or a dim, which the lit look keeps by tinting the actor's albedo. The back colour plus
/// each light's colour, over the base's.
pub fn flash(light: &lt_scene::GteLight, base: &lt_scene::GteLight) -> V3 {
    let sum = |l: &lt_scene::GteLight, c: usize| l.bk[c] as f32 + l.lcm[c].iter().map(|&v| v as f32).sum::<f32>();
    [0, 1, 2].map(|c| {
        let b = sum(base, c);
        if b > 0.0 { (sum(light, c) / b).clamp(0.0, 4.0) } else { 1.0 }
    })
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

/// The camera a scene was projected through: world to view space, the projection distance and the
/// screen offset (`screen = offset + h · v.xy / v.z`, `v = view · world`, as [`Gte::screen`] has
/// it). The water's mirrored pass projects through it (LT-147, `crate::water::mirror_clip`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    pub view: Affine,
    pub h: f32,
    pub offset: [f32; 2],
}

/// What [`lit`] adds to a scene.
#[derive(Clone, Debug, Default)]
pub struct Lit {
    /// The camera's world position.
    pub eye: V3,
    /// The camera (this frame's, between ticks too).
    pub camera: Option<Projection>,
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
        let camera = Projection { view, h: cam.h as f32, offset: cam.offset.map(|v| v as f32) };
        s.lit = Some(Lit { eye: w.point([0, 0, 0]), camera: Some(camera), ..Lit::default() });
    }
    let mut sky = record.sky_gte.as_ref().map(SkyShade::new);
    let lit = normals.is_some();
    for (ci, c) in frame.chunks.iter().enumerate() {
        let Some(mesh) = assets.mesh(c.mesh) else { continue };
        let is_sky = matches!(c.mesh, MeshKey::Sky(_));
        // A sky cell's draws: the game's depth limit at its own reach, the colours from its GTE.
        let bounds = if is_sky && sight.far == FAR { Bounds { far: SKY_FAR, ..bounds } } else { bounds };
        let mut shade = sky.as_mut().filter(|_| is_sky).map(|sky| {
            sky.cell(&c.model_view);
            Shade { sky, lit }
        });
        let (mv, groups) = match pose {
            Some(p) => (p.chunks[ci].0, &p.chunks[ci].1),
            None => (Affine::of(&c.model_view), &c.groups),
        };
        let gte = gte_of(&c.model_view, pose.map(|_| &mv));
        let mut b = Batch { texture: c.texture, corners: Vec::new(), model: false };
        // The chunk's corners in world space, and its normals (a chunk's map to world is a shift).
        let world = to_world.map(|w| w.after(&mv));
        let (chunk_normals, chunk_lamps, chunk_casters, chunk_water) = match (normals.as_deref_mut(), &world) {
            // No lamps over the sea: the sky's warm colours aren't painted light.
            (Some(n), Some(_)) => {
                let water = n.water(c.mesh, c.texture, &mesh.corners, assets.texture(c.texture));
                let shore = n.shore(c.mesh, c.texture, &mesh.corners, &water);
                (Some(n.chunk(c.mesh, &mesh.corners)), (!is_sky).then(|| n.lamps(c.mesh, &mesh.corners)), Some(n.casters(c.mesh, &mesh.corners)), Some((water, shore)))
            }
            _ => (None, None, None, None),
        };
        for &g in groups {
            let Some(&(first, count)) = mesh.ranges.get(g as usize) else { continue };
            let range = &mesh.corners[first as usize..(first + count) as usize];
            let what = format!("chunk {} group {g}", c.chunk);
            let space = world.as_ref().zip(chunk_normals.as_ref()).map(|(w, n)| {
                let n = &n[first as usize..(first + count) as usize];
                (range.iter().map(|k| w.point(k.pos)).collect::<Vec<_>>(), n.iter().map(|&v| w.direction(v)).collect::<Vec<_>>())
            });
            let water = chunk_water.as_ref().zip(world.as_ref()).map(|((w, s), to)| (&w[first as usize..(first + count) as usize], &s[first as usize..(first + count) as usize], to));
            add(&mut s, &mut b, &gte, Path::Terrain, precision, bounds, range, &what, |k| k.pos.map(|v| v as f32), space.as_ref(), shade.as_mut(), water, None);
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
            // The polygons the GTE lights (battle's, LT-116) keep the file's normals; the rest take
            // theirs from this tick's pose. In world space.
            let file_normals = mesh.normals.get(first as usize..(first + count) as usize);
            let space = to_world.map(|w| {
                let w = w.after(&mv);
                let pos: Vec<V3> = range.iter().map(|k| w.point_f(posed(k))).collect();
                let mut n = smooth_normals(&pos);
                if let (Some(_), Some(file)) = (p.light, file_normals) {
                    for ((k, out), f) in range.iter().zip(n.iter_mut()).zip(file) {
                        if k.flags & flag::LIT != 0 && *f != [0; 3] {
                            *out = w.direction(f.map(|v| v as f32));
                        }
                    }
                }
                (pos, n)
            });
            if let Some((pos, _)) = &space {
                cast.extend(range.iter().zip(pos).filter(|(k, _)| k.flags & flag::SEMI == 0).map(|(k, &world)| CasterVertex { world, uv: k.uv, flags: k.flags }));
            }
            // A battle draw's lit corners (LT-116): the GTE's colours in the original look; in the
            // lit look their own (128 under a texture), tinted by the actor's flash.
            let colours: Option<Vec<[u8; 3]>> = p.light.as_ref().zip(file_normals).map(|(light, file)| {
                if lit {
                    let tint = record.battle_light.as_ref().map_or([1.0; 3], |base| flash(light, base));
                    range.iter().map(|k| if k.flags & flag::LIT != 0 { [0, 1, 2].map(|c| (k.rgb[c] as f32 * tint[c]).round().clamp(0.0, 255.0) as u8) } else { k.rgb }).collect()
                } else if let Some(al) = &p.arena_light {
                    // The arena with its light (`FUN_8014c97c`): the base colour, and the light's
                    // near it, from this tick's vertices.
                    let base = LightShade::new(light).corner([0, 0, 0x800], [128; 3], true);
                    range.iter().map(|k| arena_colour(al, base, p.vertices.get(k.vertex as usize).copied().unwrap_or(k.pos))).collect()
                } else {
                    let mut g = LightShade::new(light);
                    range.iter().zip(file).map(|(k, &n)| if k.flags & flag::LIT != 0 { g.corner(n, k.rgb, k.flags & flag::TEXTURED != 0) } else { k.rgb }).collect()
                }
            });
            // The arena light in the lit look: a lamp.
            if let (Some(al), Some(gl), Some(lit_), Some(w)) = (&p.arena_light, &p.light, s.lit.as_mut(), &to_world) {
                let base = LightShade::new(gl).corner([0, 0, 0x800], [128; 3], true);
                lit_.lamps.push(arena_lamp(al, base, &w.after(&mv)));
            }
            add(&mut s, &mut b, &gte, Path::of_entry(p.entry), precision, bounds, range, &what, posed, space.as_ref(), None, None, colours.as_deref());
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
fn add(s: &mut Scene, b: &mut Batch, gte: &Gte, path: Path, precision: Precision, bounds: Bounds, range: &[Corner], what: &str, pos: impl Fn(&Corner) -> V3, space: Option<&(Vec<V3>, Vec<V3>)>, mut shade: Option<&mut Shade>, water: Option<(&[bool], &[crate::water::ShoreAt], &Affine)>, colours: Option<&[[u8; 3]]>) {
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
        let shaded = shade.as_mut().map(|sh| {
            let corners: Vec<&Corner> = idx[..n].iter().map(|&i| &poly[i]).collect();
            let (rgb, fog) = sh.sky.polygon(&corners.iter().map(|k| k.pos).collect::<Vec<_>>(), &corners.iter().map(|k| k.rgb).collect::<Vec<_>>());
            if sh.lit {
                (corners.iter().map(|k| k.rgb).chain(std::iter::repeat([0; 3])).take(4).collect::<Vec<_>>(), fog)
            } else {
                (rgb.to_vec(), 0.0)
            }
        });
        let tris: &[[usize; 3]] = if n == 4 { &[[0, 1, 2], [1, 3, 2]] } else { &[[0, 1, 2]] };
        for t in tris {
            // The triangle's corners' nearest stretches of shore, in world x, z (LT-148).
            let shores = water.filter(|(w, _, _)| t.iter().any(|&v| w[start + idx[v]])).map_or([NO_SHORE; 3], |(_, sh, to)| {
                t.map(|v| {
                    sh[start + idx[v]].nearest.map_or(NO_SHORE, |[a, b]| {
                        let (p, q) = (to.point_f([a[0], 0.0, a[1]]), to.point_f([b[0], 0.0, b[1]]));
                        [p[0], p[2], q[0], q[2]]
                    })
                })
            });
            for &v in t {
                let k = &poly[idx[v]];
                let (world, normal) = space.map_or(([0.0; 3], [0.0; 3]), |(p, nm)| (p[start + idx[v]], nm[start + idx[v]]));
                let own = colours.and_then(|c| c.get(start + idx[v]).copied()).unwrap_or(k.rgb);
                let (rgb, fog) = shaded.as_ref().map_or((own, 0.0), |(c, f)| (c[v], *f));
                let (water, shore) = water.map_or((false, 0.0), |(w, d, _)| (w[start + idx[v]], d[start + idx[v]].distance));
                b.corners.push(Vertex { screen: screen[v], uv: k.uv, rgb, flags: k.flags, bucket, world, normal, fog, water, shore, shores });
            }
        }
    }
}
