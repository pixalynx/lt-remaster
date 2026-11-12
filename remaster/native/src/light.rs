//! Dynamic lighting for the native view (LT-112, research 16 §5.3).
//!
//! **World space.** The capture gives each draw the GTE's model-view matrix, and the camera its
//! view matrix (world to view; in the 512 mode it carries the 1.6 x-stretch). Inverting the view
//! takes every corner to world space, where distances and angles are true. World space is the
//! game's: Y down, the current chunk's origin.
//!
//! **Normals.** None are on the disc for the field (every chunk and field character is prelit).
//! They're computed as `remaster_preview` does: area-weighted face normals, smoothed across corners
//! that share a position and face within 50°. A chunk's are computed once per mesh; a character's
//! every tick, from its pose.
//!
//! **The light** comes from the area's rig ([`Rig`]): a sun or room light with a shadow map, a sky
//! and ground for the ambient, and point lights at the lamps and fires the art already shows
//! ([`lamps`]). The baked vertex colours stay as the art's own shading; where a real shadow now
//! falls, part of their darkening is lifted so the shadow doesn't darken twice.
//!
//! **Where the lamps are.** The game has no light objects. `Field_DrawTorchFlames`, despite the
//! name, draws grey chimney smoke (all 55 retail type-5 records are smoke), and no field fire is an
//! animated texture. What marks a lamp, a fireplace or a brazier is its paint: the artists lit the
//! surfaces around it with warm vertex colours brighter than neutral (128). Clusters of those
//! corners become point lights, found per chunk mesh with no data added by hand.

use std::collections::HashMap;
use std::sync::Arc;

use lt_assets::mesh::Corner;
use lt_assets::MeshKey;
use lt_scene::{Matrix, Record};

pub type V3 = [f32; 3];

/// What the native 3D looks like.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// The game's own: unlit, baked colours, exactly the LT-110 image.
    Original,
    /// Lit by the area's rig, with shadows.
    #[default]
    Lit,
}

impl Look {
    pub fn name(self) -> &'static str {
        match self {
            Look::Original => "original",
            Look::Lit => "lit",
        }
    }
}

/// Which parts of the lighting are on (the remaster window's Lights toggles, LT-117): the main
/// light's shadows, the lamps and the water (LT-146). Off, the rig's sun, sky and baked colours
/// still light the scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lights {
    pub shadows: bool,
    pub lamps: bool,
    pub water: bool,
}

impl Lights {
    pub const ALL: Lights = Lights { shadows: true, lamps: true, water: true };

    /// `u` with what's off taken out (no shadow pass runs without a map).
    pub fn apply(self, mut u: Uniforms) -> Uniforms {
        if !self.shadows {
            u.shadow_on = false;
        }
        if !self.lamps {
            u.points.clear();
        }
        if !self.water {
            u.water = false;
        }
        u
    }
}

impl Default for Lights {
    fn default() -> Self {
        Lights::ALL
    }
}

/// An affine map in double precision: `m · v + t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: [[f64; 3]; 3],
    pub t: [f64; 3],
}

impl Affine {
    /// A GTE matrix (rotation in 4.12, translation in units).
    pub fn of(m: &Matrix) -> Affine {
        Affine { m: m.m.map(|r| r.map(|v| v as f64 / 4096.0)), t: m.t.map(|v| v as f64) }
    }

    pub fn apply(&self, v: [f64; 3]) -> [f64; 3] {
        let r = |i: usize| self.m[i][0] * v[0] + self.m[i][1] * v[1] + self.m[i][2] * v[2] + self.t[i];
        [r(0), r(1), r(2)]
    }

    pub fn inverse(&self) -> Option<Affine> {
        let m = &self.m;
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if det.abs() < 1e-9 {
            return None;
        }
        let c = |a: usize, b: usize, c: usize, d: usize| (m[a][b] * m[c][d] - m[a][d] * m[c][b]) / det;
        let inv = [[c(1, 1, 2, 2), -c(0, 1, 2, 2), c(0, 1, 1, 2)], [-c(1, 0, 2, 2), c(0, 0, 2, 2), -c(0, 0, 1, 2)], [c(1, 0, 2, 1), -c(0, 0, 2, 1), c(0, 0, 1, 1)]];
        let t = [0, 1, 2].map(|i| -(inv[i][0] * self.t[0] + inv[i][1] * self.t[1] + inv[i][2] * self.t[2]));
        Some(Affine { m: inv, t })
    }

    /// `self` after `b`: `self(b(v))`.
    pub fn after(&self, b: &Affine) -> Affine {
        let mut m = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| self.m[i][k] * b.m[k][j]).sum();
            }
        }
        Affine { m, t: self.apply(b.t) }
    }

    /// As a WGSL `mat4x4<f32>`, by columns.
    pub fn columns(&self) -> [[f32; 4]; 4] {
        let m = &self.m;
        [[m[0][0], m[1][0], m[2][0], 0.0], [m[0][1], m[1][1], m[2][1], 0.0], [m[0][2], m[1][2], m[2][2], 0.0], [self.t[0], self.t[1], self.t[2], 1.0]].map(|c| c.map(|v| v as f32))
    }

    pub fn point(&self, v: [i16; 3]) -> V3 {
        self.apply(v.map(|x| x as f64)).map(|x| x as f32)
    }

    pub fn point_f(&self, v: V3) -> V3 {
        self.apply(v.map(|x| x as f64)).map(|x| x as f32)
    }

    /// A direction through the linear part, unit length.
    pub fn direction(&self, n: V3) -> V3 {
        let r = |i: usize| (self.m[i][0] * n[0] as f64 + self.m[i][1] * n[1] as f64 + self.m[i][2] * n[2] as f64) as f32;
        norm([r(0), r(1), r(2)])
    }
}

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l > 1e-12 {
        a.map(|v| v / l)
    } else {
        [0.0; 3]
    }
}

/// Corners are smoothed with faces within 50° of their own.
pub const SMOOTH_COS: f32 = 0.642_787_6;

/// Smoothed normals for a triangle list (`pos`, three corners a triangle), facing out of the side
/// the GTE draws: a triangle the game draws has its corners clockwise on screen (Y down), which in
/// the GTE's right-handed space makes `(c − a) × (b − a)` point at the viewer.
pub fn smooth_normals(pos: &[V3]) -> Vec<V3> {
    let tris = pos.len() / 3;
    let face: Vec<V3> = (0..tris).map(|t| cross(sub(pos[3 * t + 2], pos[3 * t]), sub(pos[3 * t + 1], pos[3 * t]))).collect();
    let unit: Vec<V3> = face.iter().map(|&f| norm(f)).collect();
    let key = |p: V3| (p[0].round() as i32, p[1].round() as i32, p[2].round() as i32);
    let mut at: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::with_capacity(pos.len());
    for (i, &p) in pos.iter().enumerate().take(tris * 3) {
        at.entry(key(p)).or_default().push((i / 3) as u32);
    }
    let mut out = vec![[0.0; 3]; pos.len()];
    for (i, n) in out.iter_mut().enumerate().take(tris * 3) {
        let t = i / 3;
        let own = unit[t];
        let mut sum = [0.0f32; 3];
        for &o in &at[&key(pos[i])] {
            if dot(unit[o as usize], own) >= SMOOTH_COS {
                sum = add(sum, face[o as usize]);
            }
        }
        *n = if sum == [0.0; 3] { own } else { norm(sum) };
    }
    out
}

/// Per chunk mesh, kept across ticks (chunks don't move): its corners' normals, its lamps and its
/// shadow casters.
#[derive(Default)]
pub struct Normals {
    chunks: HashMap<MeshKey, Arc<Vec<V3>>>,
    lamps: HashMap<MeshKey, Arc<Vec<Lamp>>>,
    casters: HashMap<MeshKey, Arc<Vec<u8>>>,
    water: HashMap<(MeshKey, lt_assets::TextureKey), Arc<Vec<bool>>>,
    shore: HashMap<(MeshKey, lt_assets::TextureKey), Arc<Vec<crate::water::ShoreAt>>>,
}

impl Normals {
    pub fn chunk(&mut self, key: MeshKey, corners: &[Corner]) -> Arc<Vec<V3>> {
        self.chunks.entry(key).or_insert_with(|| Arc::new(smooth_normals(&corners.iter().map(|c| c.pos.map(|v| v as f32)).collect::<Vec<_>>()))).clone()
    }

    /// Which of the mesh's corners are water (LT-146, [`crate::water::corners`]), with its atlas.
    pub fn water(&mut self, key: MeshKey, texture: lt_assets::TextureKey, corners: &[Corner], tex: Option<&lt_assets::mesh::TextureData>) -> Arc<Vec<bool>> {
        self.water.entry((key, texture)).or_insert_with(|| Arc::new(crate::water::corners(corners, tex))).clone()
    }

    /// Each of the mesh's corners' distance to the shore (LT-148, [`crate::water::shore`]), its
    /// water being `water` (what [`Normals::water`] gave): worked out once a mesh.
    pub fn shore(&mut self, key: MeshKey, texture: lt_assets::TextureKey, corners: &[Corner], water: &[bool]) -> Arc<Vec<crate::water::ShoreAt>> {
        self.shore.entry((key, texture)).or_insert_with(|| Arc::new(crate::water::shore(corners, water))).clone()
    }

    pub fn lamps(&mut self, key: MeshKey, corners: &[Corner]) -> Arc<Vec<Lamp>> {
        self.lamps.entry(key).or_insert_with(|| Arc::new(lamps(corners))).clone()
    }

    /// The mesh's opaque triangles as shadow casters, in mesh space: [`CASTER_BYTES`] a corner
    /// (position, texel, flags), the layout `shaders/shadow.wgsl` reads.
    pub fn casters(&mut self, key: MeshKey, corners: &[Corner]) -> Arc<Vec<u8>> {
        self.casters.entry(key).or_insert_with(|| Arc::new(caster_bytes(corners.as_chunks::<3>().0.iter().filter(|t| t[0].flags & lt_core::mesh::flag::SEMI == 0).flatten().map(|k| (k.pos.map(|v| v as f32), k.uv, k.flags))))).clone()
    }
}

/// Bytes of a shadow caster's corner.
pub const CASTER_BYTES: u64 = 24;

/// Caster corners packed for the GPU: position, texel, flags.
pub fn caster_bytes(corners: impl Iterator<Item = (V3, [u16; 2], u8)>) -> Vec<u8> {
    let mut out = Vec::new();
    for (p, uv, flags) in corners {
        for f in p.iter().chain([uv[0] as f32, uv[1] as f32].iter()) {
            out.extend_from_slice(&f.to_le_bytes());
        }
        out.extend_from_slice(&(flags as u32).to_le_bytes());
    }
    out
}

/// A light the art shows: where (mesh space), its colour (brightest channel 1) and its reach.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lamp {
    pub pos: V3,
    pub colour: V3,
    pub reach: f32,
    /// Distinct painted positions it was found from.
    pub corners: u32,
    /// Zero for a lamp from the art; the battle arena's light (LT-116) falls off as the game's does:
    /// its full colour within `inner`, then linearly in the distance squared to nothing at `reach`.
    pub inner: f32,
}

/// A corner's paint is firelight: textured and modulated, red at least 200 (1.56× neutral), green
/// at least 130, and red at least 60 over blue.
pub fn firelight(c: &Corner) -> bool {
    use lt_core::mesh::flag;
    let [r, g, b] = c.rgb.map(|v| v as i32);
    c.flags & flag::TEXTURED != 0 && c.flags & flag::RAW == 0 && r >= 200 && g >= 130 && r >= b + 60
}

/// Painted positions within this of a cluster's centre join it.
const LAMP_JOIN: f32 = 160.0;
/// A cluster needs this many distinct painted positions to be a lamp.
const LAMP_MIN: u32 = 4;
/// The light sits this far above its pool's centre (Y down).
const LAMP_LIFT: f32 = 64.0;

/// The lamps a mesh's paint shows: firelight corners clustered by position, each cluster a light
/// above its centre, coloured by its paint, reaching a little past its pool.
pub fn lamps(corners: &[Corner]) -> Vec<Lamp> {
    let mut seen = std::collections::HashSet::new();
    let mut clusters: Vec<(V3, [f32; 3], u32, f32)> = Vec::new(); // sum of positions, of colours, count, radius
    for c in corners.iter().filter(|c| firelight(c)) {
        if !seen.insert(c.pos) {
            continue;
        }
        let p = c.pos.map(|v| v as f32);
        let rgb = c.rgb.map(|v| v as f32);
        let near = clusters.iter_mut().find(|k| {
            let centre = k.0.map(|v| v / k.2 as f32);
            dot(sub(p, centre), sub(p, centre)) <= LAMP_JOIN * LAMP_JOIN
        });
        match near {
            Some(k) => {
                let centre = k.0.map(|v| v / k.2 as f32);
                k.3 = k.3.max(dot(sub(p, centre), sub(p, centre)).sqrt());
                k.0 = add(k.0, p);
                k.1 = add(k.1, rgb);
                k.2 += 1;
            }
            None => clusters.push((p, rgb, 1, 0.0)),
        }
    }
    clusters
        .into_iter()
        .filter(|k| k.2 >= LAMP_MIN)
        .map(|(sum, rgb, n, radius)| {
            let centre = sum.map(|v| v / n as f32);
            let top = rgb.iter().copied().fold(1.0f32, f32::max);
            Lamp { pos: add(centre, [0.0, -LAMP_LIFT, 0.0]), colour: rgb.map(|v| v / top), reach: (radius * 1.5).max(220.0), corners: n, inner: 0.0 }
        })
        .collect()
}

/// What kind of place a chunk is, for its rig.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    /// Under the sky: the overworld grid, its towns, towns past the grid, flight.
    Outdoor,
    /// A house or other room.
    Interior,
    /// Caves, towers, castles.
    Dungeon,
    /// A battle (LT-116): lit by the game's own battle light ([`Rig::battle`]).
    Battle,
}

/// Dungeons by chunk id (research 03, "Overworld vs town vs dungeon" and the area table): the Cave
/// of Fear (grid 42), the North Tower (433–442), the Holy Armor dungeon (453–459), the switch
/// castle (468–492) and God's Tower (506–522). 453, 455–457 and 506–522 are also the game's own
/// list of dungeon chunks for wandering monsters (`D_801FC924`).
const DUNGEONS: &[(u16, u16)] = &[(42, 42), (433, 442), (453, 459), (468, 492), (506, 522)];
/// Towns past the grid (chunk ids 324 and up are rooms otherwise): Trisken.
const OUTDOOR_PAST_GRID: &[(u16, u16)] = &[(372, 372)];
const GRID_CHUNKS: u16 = 324;
/// The field context's mode while flying (`ctx+0x43`).
const MODE_FLIGHT: u8 = 3;
/// How strong a battle light is in the lit look for each 1.0 of the game's modulation (a back colour
/// of 0x800 modulates the texture by 1.0; each light adds up to its colour times 2 on top).
const BATTLE_LIGHT: f32 = 0.5;

pub fn area(chunk: u16, mode: u8) -> Area {
    let within = |list: &[(u16, u16)]| list.iter().any(|&(a, b)| (a..=b).contains(&chunk));
    if mode == lt_assets::BATTLE_MODE {
        Area::Battle
    } else if mode == MODE_FLIGHT {
        Area::Outdoor
    } else if within(DUNGEONS) {
        Area::Dungeon
    } else if chunk < GRID_CHUNKS || within(OUTDOOR_PAST_GRID) {
        Area::Outdoor
    } else {
        Area::Interior
    }
}

/// An area's light (colours are linear, 1.0 a full-strength white light).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rig {
    pub area: Area,
    /// The direction the main light travels, in world space (Y down), and its colour.
    pub sun_dir: V3,
    pub sun: V3,
    /// The ambient from above and below.
    pub sky: V3,
    pub ground: V3,
    /// How much of the baked vertex colours stays (1 all of it), and how much of their darkening
    /// a real shadow lifts.
    pub baked: f32,
    pub delit: f32,
    /// Before the tone curve.
    pub exposure: f32,
    /// Each lamp's strength (times its paint's colour).
    pub lamp: f32,
    /// A second light without a shadow (battle's, LT-116): the direction it travels and its colour.
    pub fill_dir: V3,
    pub fill: V3,
}

impl Rig {
    pub fn of(area: Area) -> Rig {
        match area {
            // A high sun from the far left (the field camera looks along +Z), so shadows fall
            // toward the camera and to the right, where they're seen; a pale blue sky.
            Area::Outdoor => Rig {
                area,
                sun_dir: norm([0.45, 0.74, -0.50]),
                sun: [0.86, 0.79, 0.68],
                sky: [0.30, 0.34, 0.42],
                ground: [0.20, 0.18, 0.15],
                baked: 1.0,
                delit: 0.5,
                exposure: 1.0,
                lamp: 1.1,
                fill_dir: [0.0, 1.0, 0.0],
                fill: [0.0; 3],
            },
            // A room: soft light from above, warm walls, the lamps doing the rest.
            Area::Interior => Rig {
                area,
                sun_dir: norm([0.25, 0.90, -0.36]),
                sun: [0.58, 0.52, 0.44],
                sky: [0.44, 0.40, 0.35],
                ground: [0.26, 0.23, 0.19],
                baked: 1.0,
                delit: 0.4,
                exposure: 1.0,
                lamp: 1.4,
                fill_dir: [0.0, 1.0, 0.0],
                fill: [0.0; 3],
            },
            // Dim and cool, so lamps carry the scene.
            Area::Dungeon => Rig {
                area,
                sun_dir: norm([-0.30, 0.86, -0.40]),
                sun: [0.46, 0.50, 0.60],
                sky: [0.40, 0.42, 0.50],
                ground: [0.22, 0.22, 0.26],
                baked: 1.0,
                delit: 0.3,
                exposure: 1.0,
                lamp: 1.8,
                fill_dir: [0.0, 1.0, 0.0],
                fill: [0.0; 3],
            },
            // The game's base battle light (two white lights of 0.5 from above, front left and back
            // right, a back colour of 0.5) for a battle tick that recorded none.
            Area::Battle => Rig::battle(&lt_scene::GteLight {
                llm: [[-2365, -2365, 2365], [2365, -2365, -2365], [0, -4096, 0]],
                lcm: [[0x800, 0x800, 0], [0x800, 0x800, 0], [0x800, 0x800, 0]],
                bk: [0x800; 3],
            }),
        }
    }

    /// The lit look's rig from the game's battle light (LT-116, `lt_scene::Record::battle_light`):
    /// - the key is the brighter of its two lights, casting the shadow; the other is the fill;
    /// - the back colour is the ambient, a little brighter from above than from below;
    /// - each 1.0 of the game's modulation is [`BATTLE_LIGHT`] of light.
    ///
    /// The game's lights are directions toward the light (Y down); a rig's travel the other way.
    pub fn battle(l: &lt_scene::GteLight) -> Rig {
        let toward = |i: usize| norm(l.llm[i].map(|v| v as f32));
        let colour = |j: usize| [0, 1, 2].map(|c| l.lcm[c][j] as f32 / 4096.0 * 2.0 * BATTLE_LIGHT);
        let back = l.bk.map(|v| v as f32 / 4096.0 * 2.0 * BATTLE_LIGHT);
        let strength = |j: usize| colour(j).iter().sum::<f32>();
        let (key, fill) = if strength(1) > strength(0) { (1, 0) } else { (0, 1) };
        let away = |v: V3| v.map(|x| -x);
        // More of the light from the key than the game gives it, less from all round: the same
        // brightness lit, with the shapes and their shadows reading.
        let scale = |v: V3, k: f32| v.map(|x| x * k);
        Rig {
            area: Area::Battle,
            sun_dir: away(toward(key)),
            sun: scale(colour(key), 1.35),
            sky: scale(back, 0.95),
            ground: scale(back, 0.6),
            baked: 1.0,
            delit: 0.4,
            exposure: 1.0,
            // A lamp's colour is what the game's vertex colour rises by (1.0 = 128,
            // `crate::draw::arena_lamp`), over the light the rig gives the ground.
            lamp: 2.0 * BATTLE_LIGHT,
            fill_dir: away(toward(fill)),
            fill: scale(colour(fill), 0.75),
        }
    }

    pub fn for_record(r: &Record) -> Rig {
        match (area(r.chunk, r.mode), r.battle_light.as_ref()) {
            (Area::Battle, Some(l)) => Rig::battle(l),
            (a, _) => Rig::of(a),
        }
    }
}

/// Point lights at most (lamps past this are the farthest from what's on screen, and dropped).
pub const MAX_POINTS: usize = 16;
/// The shadow map's size (one map: GC's field camera sees a small, fixed-height patch, so one map
/// fitted to it keeps under a world unit a texel).
pub const SHADOW_SIZE: u32 = 2048;
/// How far toward the light, past the nearest receiver, a caster can stand and still be in the
/// map (the field's tallest pieces are under 1,000 units).
const CASTER_REACH: f32 = 1536.0;

/// The lit pass's uniforms, laid out for WGSL (`shaders/lit.wgsl`, `struct Light`).
#[derive(Clone, Debug, PartialEq)]
pub struct Uniforms {
    pub rig: Rig,
    pub eye: V3,
    /// World to the shadow map's clip space (column-major), and whether there's a map.
    pub shadow: [[f32; 4]; 4],
    pub shadow_on: bool,
    /// World units a shadow-map texel covers.
    pub texel: f32,
    /// Point lights: position, reach, colour, and the inner radius of one that falls off as the
    /// battle arena's light does (0 for a lamp; [`Lamp::inner`]).
    pub points: Vec<(V3, f32, V3, f32)>,
    /// A debug view instead of the colour (`LT_LIGHT_DEBUG`): 1 the main light's visibility, 2 the
    /// normals, 3 the shadow map's depth against the receiver's, 4 the lamps' light, 5 the water
    /// (LT-146, white).
    pub debug: u32,
    /// The colour a polygon's fog (`draw::Vertex::fog`) blends toward: the first flight's far
    /// colour (LT-143, the GTE's RFC/GFC/BFC), 0..1.
    pub fog: V3,
    /// The water (LT-146): drawn as water, and the waves' clock in seconds ([`crate::water::clock`]).
    pub water: bool,
    pub time: f32,
}

impl Uniforms {
    pub const BYTES: usize = 16 * 6 + 64 + 16 + 16 * 2 * MAX_POINTS + 16 + 16 + 32;

    pub fn bytes(&self) -> Vec<u8> {
        let mut f: Vec<f32> = Vec::with_capacity(Self::BYTES / 4);
        let v4 = |f: &mut Vec<f32>, v: V3, w: f32| f.extend_from_slice(&[v[0], v[1], v[2], w]);
        let r = &self.rig;
        v4(&mut f, r.sun_dir, 0.0);
        v4(&mut f, r.sun, 0.0);
        v4(&mut f, r.sky, 0.0);
        v4(&mut f, r.ground, 0.0);
        f.extend_from_slice(&[r.baked, r.delit, r.exposure, self.points.len().min(MAX_POINTS) as f32]);
        v4(&mut f, self.eye, 0.0);
        for col in self.shadow {
            f.extend_from_slice(&col);
        }
        f.extend_from_slice(&[self.texel, if self.shadow_on { 1.0 } else { 0.0 }, 1.0 / SHADOW_SIZE as f32, self.debug as f32]);
        for k in 0..MAX_POINTS {
            match self.points.get(k) {
                Some(&(p, reach, c, inner)) => {
                    v4(&mut f, p, reach);
                    v4(&mut f, c, inner);
                }
                None => f.extend_from_slice(&[0.0; 8]),
            }
        }
        v4(&mut f, self.fog, 0.0);
        f.extend_from_slice(&[self.time, if self.water { 1.0 } else { 0.0 }, 0.0, 0.0]);
        v4(&mut f, r.fill_dir, 0.0);
        v4(&mut f, r.fill, 0.0);
        f.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
}

/// The light's frame: forward along the sun, and two axes across it.
fn light_axes(dir: V3) -> (V3, V3, V3) {
    let f = norm(dir);
    let helper = if f[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let right = norm(cross(helper, f));
    let up = cross(f, right);
    (right, up, f)
}

/// The shadow map's projection: across the sun, the receivers' extent (what's on screen; a caster
/// outside it can't shade it, the projection being parallel); along the sun, from the nearest
/// caster to the farthest receiver. Returns the matrix and the world units a texel covers.
pub fn shadow_fit(dir: V3, receivers: &[V3], casters: &[V3]) -> Option<([[f32; 4]; 4], f32)> {
    if receivers.is_empty() {
        return None;
    }
    let (r, u, f) = light_axes(dir);
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for &p in receivers {
        let q = [dot(p, r), dot(p, u), dot(p, f)];
        for k in 0..3 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    let mut near = lo[2] - CASTER_REACH;
    for &p in casters {
        let (x, y, z) = (dot(p, r), dot(p, u), dot(p, f));
        if x >= lo[0] && x <= hi[0] && y >= lo[1] && y <= hi[1] {
            near = near.min(z);
        }
    }
    let pad = 16.0;
    let (cx, cy) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
    let (hx, hy) = ((hi[0] - lo[0]) / 2.0 + pad, (hi[1] - lo[1]) / 2.0 + pad);
    let (z0, z1) = (near - pad, hi[2] + pad);
    let dz = (z1 - z0).max(1.0);
    // Rows of the 4×4 (x' = (p·r − cx)/hx …, z' = (p·f − z0)/dz), stored by columns.
    let rows = [[r[0] / hx, r[1] / hx, r[2] / hx, -cx / hx], [u[0] / hy, u[1] / hy, u[2] / hy, -cy / hy], [f[0] / dz, f[1] / dz, f[2] / dz, -z0 / dz], [0.0, 0.0, 0.0, 1.0]];
    let cols = [0, 1, 2, 3].map(|c| [rows[0][c], rows[1][c], rows[2][c], rows[3][c]]);
    Some((cols, 2.0 * hx.max(hy) / SHADOW_SIZE as f32))
}

/// The lit pass's uniforms for a tick: its area's rig, the shadow map fitted to what's on screen,
/// and its lamps (`None` for a scene not built by [`crate::draw::lit`]).
pub fn uniforms(r: &Record, scene: &crate::draw::Scene) -> Option<Uniforms> {
    let lit = scene.lit.as_ref()?;
    let rig = Rig::for_record(r);
    let receivers: Vec<V3> = scene.batches.iter().flat_map(|b| b.corners.iter().map(|c| c.world)).collect();
    let fit = shadow_fit(rig.sun_dir, &receivers, &[]);
    let centre = if receivers.is_empty() { lit.eye } else { receivers.iter().fold([0.0; 3], |a, &p| add(a, p)).map(|v| v / receivers.len() as f32) };
    let points = nearest_lamps(&lit.lamps, centre, rig.lamp);
    if std::env::var("LT_LIGHT_TRACE").is_ok() {
        eprintln!("light: {:?} eye {:?}; {} receivers; texel {:?}; lamps {:?}", rig.area, lit.eye, receivers.len(), fit.map(|f| f.1), points.iter().map(|p| p.0.map(|v| v.round())).collect::<Vec<_>>());
    }
    Some(Uniforms {
        rig,
        eye: lit.eye,
        shadow: fit.map_or([[0.0; 4]; 4], |f| f.0),
        shadow_on: fit.is_some(),
        texel: fit.map_or(1.0, |f| f.1),
        points,
        debug: std::env::var("LT_LIGHT_DEBUG").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
        fog: sky_fog(r),
        water: true,
        time: crate::water::clock(r.frame, r.frame, 0.0),
    })
}

/// The first flight's far colour (control registers 21–23, 12.4 fixed point), 0..1; black elsewhere
/// (no polygon has fog there).
pub fn sky_fog(r: &Record) -> V3 {
    r.sky_gte.as_ref().map_or([0.0; 3], |g| [21, 22, 23].map(|i| ((g.control[i] as i32 >> 4).clamp(0, 255)) as f32 / 255.0))
}

/// The lamps nearest `centre` (what's on screen), at most [`MAX_POINTS`], as point lights.
pub fn nearest_lamps(lamps: &[Lamp], centre: V3, strength: f32) -> Vec<(V3, f32, V3, f32)> {
    let mut v: Vec<&Lamp> = lamps.iter().collect();
    let d = |l: &Lamp| dot(sub(l.pos, centre), sub(l.pos, centre));
    v.sort_by(|a, b| d(a).total_cmp(&d(b)));
    v.into_iter().take(MAX_POINTS).map(|l| (l.pos, l.reach, l.colour.map(|c| c * strength), l.inner)).collect()
}

#[cfg(test)]
mod lights_tests {
    use super::*;

    #[test]
    fn lights_off_take_out_the_shadow_and_the_lamps() {
        let u = Uniforms { rig: Rig::of(Area::Outdoor), eye: [0.0; 3], shadow: [[0.0; 4]; 4], shadow_on: true, texel: 1.0, points: vec![([0.0; 3], 100.0, [1.0; 3], 0.0)], debug: 0, fog: [0.0; 3], water: true, time: 0.0 };
        assert_eq!(Lights::ALL.apply(u.clone()), u);
        let off = Lights { shadows: false, lamps: false, water: false }.apply(u.clone());
        assert!(!off.shadow_on && off.points.is_empty() && !off.water);
        assert_eq!(off.rig, u.rig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_inverse_undoes_the_view() {
        let m = Matrix { m: [[4096, 0, 0], [0, 3547, -2048], [0, 2048, 3547]], t: [10, -300, 2000] };
        let v = Affine::of(&m);
        let inv = v.inverse().unwrap();
        let p = [123.0, -45.0, 678.0];
        let back = inv.apply(v.apply(p));
        assert!((0..3).all(|i| (back[i] - p[i]).abs() < 1e-6));
        let id = inv.after(&v);
        assert!((0..3).all(|i| (0..3).all(|j| (id.m[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-9)));
    }

    #[test]
    fn a_floor_faces_up_and_a_crease_stays_sharp() {
        // Two triangles of a floor at y = 0 (Y down), clockwise seen from above (from −Y).
        let floor = [[0.0, 0.0, 0.0], [0.0, 0.0, 100.0], [100.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 0.0, 100.0], [100.0, 0.0, 100.0]];
        for n in smooth_normals(&floor) {
            assert!((n[1] + 1.0).abs() < 1e-5, "{n:?}");
        }
        // A wall standing on the floor's edge, at 90°: its corners keep their own normal.
        let mut both = floor.to_vec();
        both.extend_from_slice(&[[100.0, 0.0, 0.0], [100.0, -100.0, 0.0], [100.0, 0.0, 100.0]]);
        let n = smooth_normals(&both);
        assert!((n[2][1] + 1.0).abs() < 1e-5, "the floor corner at the crease: {:?}", n[2]);
        assert!(n[6][1].abs() < 1e-5 && n[6][0].abs() > 0.99, "the wall: {:?}", n[6]);
    }

    #[test]
    fn areas_follow_the_chunk_table() {
        assert_eq!(area(78, 0), Area::Outdoor);
        assert_eq!(area(80, 3), Area::Outdoor);
        assert_eq!(area(372, 0), Area::Outdoor);
        assert_eq!(area(330, 0), Area::Interior);
        assert_eq!(area(326, 0), Area::Interior);
        assert_eq!(area(454, 0), Area::Dungeon);
        assert_eq!(area(433, 0), Area::Dungeon);
        assert_eq!(area(42, 0), Area::Dungeon);
    }

    #[test]
    fn the_shadow_fit_holds_the_receivers() {
        let dir = norm([-0.4, 0.8, 0.4]);
        let recv = vec![[0.0, 0.0, 0.0], [500.0, 0.0, 300.0], [-200.0, 0.0, 800.0]];
        let cast = vec![[100.0, -200.0, 100.0]];
        let (m, texel) = shadow_fit(dir, &recv, &cast).unwrap();
        let clip = |p: V3| [0, 1, 2].map(|i| m[0][i] * p[0] + m[1][i] * p[1] + m[2][i] * p[2] + m[3][i]);
        for p in recv.iter().chain(cast.iter()) {
            let c = clip(*p);
            assert!(c[0].abs() <= 1.0 && c[1].abs() <= 1.0 && (0.0..=1.0).contains(&c[2]), "{p:?} → {c:?}");
        }
        // The caster is nearer the sun than the receiver under it.
        assert!(clip(cast[0])[2] < clip([100.0, 0.0, 100.0])[2]);
        assert!(texel > 0.0 && texel < 1.0);
        assert_eq!(Uniforms { rig: Rig::of(Area::Outdoor), eye: [0.0; 3], shadow: m, shadow_on: true, texel, points: vec![], debug: 0, fog: [0.0; 3], water: true, time: 0.0 }.bytes().len(), Uniforms::BYTES);
    }

    #[test]
    fn painted_firelight_makes_a_lamp() {
        use lt_core::mesh::flag;
        let corner = |pos: [i16; 3], rgb: [u8; 3]| Corner { pos, vertex: 0, uv: [0, 0], rgb, flags: flag::TEXTURED };
        let mut cs = Vec::new();
        // A warm pool of five painted positions round (100, 0, 100), and plain floor far off.
        for (x, z) in [(80, 80), (120, 80), (80, 120), (120, 120), (100, 100)] {
            cs.push(corner([x, 0, z], [230, 170, 90]));
        }
        for (x, z) in [(900, 900), (940, 900), (900, 940)] {
            cs.push(corner([x, 0, z], [128, 128, 128]));
        }
        // Two warm corners alone are paint, not a lamp.
        cs.push(corner([-900, 0, 0], [230, 170, 90]));
        cs.push(corner([-920, 0, 0], [230, 170, 90]));
        let l = lamps(&cs);
        assert_eq!(l.len(), 1, "{l:?}");
        assert!((l[0].pos[0] - 100.0).abs() < 1.0 && (l[0].pos[2] - 100.0).abs() < 1.0 && l[0].pos[1] < 0.0);
        assert!(l[0].colour[0] == 1.0 && l[0].colour[2] < l[0].colour[1]);
    }
}
