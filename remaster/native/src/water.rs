//! The game's water in the lit look (LT-146).
//!
//! **Which polygons are water.** The game has no water flag, and its water isn't one texture:
//! - the first flight's sea (OVR24) is a static blue texture on its chunk page;
//! - the world map's sea is the grid chunks' own blue texture;
//! - towns' water samples the field effect page's columns, which the game scrolls every frame
//!   (`FE_BIN_Format.md`), and which no file holds (lt-core paints them its water blue).
//!
//! What they share: a water polygon lies flat, and its texels are water blue, on their own and as
//! the polygon's colour modulates them (the ice cave's white floor, shaded blue, isn't water). And a water texture is laid on level ground only, where a blue wall's
//! (the ice cave's) also tops its walls. [`corners`] marks each triangle of a mesh that is level
//! within about 14° ([`FLAT`]) and blue by [`blue`] over its texture tile's opaque texels, when
//! that tile's triangles in the mesh are mostly level ([`LEVEL_TILE`]). A roof or a wall isn't
//! level; grass, sand, stone and wood aren't blue; ice walls' tiles mostly stand.
//!
//! **What the lit look draws** (`shaders/lit.wgsl`), in linear light:
//! - ripples: [`ripple_map`] (a tileable water surface made in code) laid four times over the water
//!   at unlike sizes, angles and drifts, as Bevy's water example layers its normal maps. Read at the
//!   mip a pixel covers, so far water calms instead of shimmering;
//! - the game's water colour as the base, lit as before, shaded by the ripples around its own
//!   colour (their slope against the sun, crests lighter than troughs);
//! - the sky reflected by a Fresnel term (little looking down, more at a grazing angle, held to
//!   0.55). The sky is the water's own colour, lighter toward the horizon, so the art's blue stays;
//! - the sun's glitter on the ripple facets turned toward the eye, out of shadow (a tight lobe:
//!   the field's camera looks near the sun's mirror angle).
//!
//! The clock is the game's frame count, so frames between ticks move on smoothly ([`clock`]). The
//! original look never changes.
//!
//! **The scene reflected** (LT-147). All the game's water is level, so the scene mirrored about
//! its plane is an exact reflection. `composite` draws the lit corners again through
//! [`mirror_clip`] (the frame's camera after the reflection about [`plane`]) at a
//! [`MIRROR_DIVISOR`]th of the picture's size, leaving out the water and what's under the plane;
//! the water reads that picture at its pixel, pushed by the ripples, by a Fresnel term of its own
//! (0.15 looking down, to 0.6 grazing). The pass runs only when water is drawn.
//!
//! **The shore** (LT-148). The game has no bed under its water, so its depth is its distance to the
//! shore: [`shore`] finds where the land comes down to the water's plane and gives each water corner
//! its distance and its nearest stretch of shore, once a mesh. The lit look measures each pixel's
//! distance to its triangle's corners' stretches, and draws the shallows lighter and greener, the
//! caustic web on their bed (the ripple map's alpha, `caustic_web`) and a lapping line of foam.

use lt_assets::mesh::{Corner, TextureData};
use lt_core::mesh::flag;

use crate::draw::{Projection, Scene};

/// A triangle is level when its normal is within this of straight up or down (cos 14°).
pub const FLAT: f32 = 0.97;

/// A tile is water's only when this share of its triangles in the mesh are level.
pub const LEVEL_TILE: f32 = 0.75;

/// The game's frame rate (NTSC field rate): the waves' clock.
pub const FIELD_HZ: f64 = 59.94;

/// Whether a colour (0..255) is water blue: blue the strongest channel by a margin over red, no
/// weaker than green by much (sea water runs to teal), bright enough and saturated.
pub fn blue(c: [f32; 3]) -> bool {
    let [r, g, b] = c;
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    b >= 64.0 && b >= 1.2 * r && b >= 0.9 * g && max > 0.0 && (max - min) / max >= 0.25
}

/// Each atlas tile's mean colour over its opaque texels (`None` for a tile with none).
pub fn tile_colours(tex: &TextureData) -> Vec<Option<[f32; 3]>> {
    tex.tiles
        .iter()
        .map(|t| {
            let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
            for y in t.y as u32..(t.y + t.h) as u32 {
                for x in t.x as u32..(t.x + t.w) as u32 {
                    let o = ((y * tex.w + x) * 4) as usize;
                    if o + 3 >= tex.rgba.len() || tex.rgba[o + 3] == 0 {
                        continue;
                    }
                    for (s, &v) in sum.iter_mut().zip(&tex.rgba[o..o + 3]) {
                        *s += v as f32;
                    }
                    n += 1.0;
                }
            }
            (n > 0.0).then(|| sum.map(|v| v / n))
        })
        .collect()
}

/// Which corners of a mesh (whole triangles: [`Corner`]s three at a time) are water, its atlas
/// `tex` for the textured ones.
pub fn corners(corners: &[Corner], tex: Option<&TextureData>) -> Vec<bool> {
    let colours = tex.map(tile_colours);
    let tile_of = |c: &Corner| -> Option<usize> {
        let [u, v] = c.uv;
        tex?.tiles.iter().position(|t| (t.x..t.x + t.w).contains(&u) && (t.y..t.y + t.h).contains(&v))
    };
    let tris = corners.as_chunks::<3>().0;
    let level: Vec<bool> = tris
        .iter()
        .map(|tri| {
            let p = tri.map(|c| c.pos.map(|v| v as f32));
            let (a, b) = ([0, 1, 2].map(|i| p[1][i] - p[0][i]), [0, 1, 2].map(|i| p[2][i] - p[0][i]));
            let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            len > 0.0 && (n[1] / len).abs() >= FLAT
        })
        .collect();
    let tiles: Vec<Option<usize>> = tris.iter().map(|tri| if tri[0].flags & flag::TEXTURED != 0 { tile_of(&tri[0]) } else { None }).collect();
    // How many of each tile's triangles are level, of all.
    let mut per_tile = vec![(0u32, 0u32); tex.map_or(0, |t| t.tiles.len())];
    for (t, &l) in tiles.iter().zip(&level) {
        if let Some(i) = *t {
            per_tile[i].0 += u32::from(l);
            per_tile[i].1 += 1;
        }
    }
    let mut out = vec![false; corners.len()];
    for (k, tri) in tris.iter().enumerate() {
        if !level[k] {
            continue;
        }
        let shade = tri.iter().fold([0.0f32; 3], |s, c| [0, 1, 2].map(|i| s[i] + c.rgb[i] as f32 / 3.0));
        let colour = if tri[0].flags & flag::TEXTURED != 0 {
            let Some(i) = tiles[k] else { continue };
            let (flat, all) = per_tile[i];
            // The texture itself blue (a white floor tinted blue by its shading isn't water).
            let Some(mean) = colours.as_ref().and_then(|c| c[i]).filter(|&m| flat as f32 >= LEVEL_TILE * all as f32 && blue(m)) else { continue };
            if tri[0].flags & flag::RAW != 0 {
                mean
            } else {
                [0, 1, 2].map(|i| (mean[i] * shade[i] / 128.0).min(255.0))
            }
        } else {
            shade
        };
        if blue(colour) {
            out[k * 3..k * 3 + 3].fill(true);
        }
    }
    out
}

/// The distance to the shore a water corner has at most (LT-148): open water.
pub const SHORE_MAX: f32 = 1024.0;

/// A land corner this near the water's plane lies on it (LT-148).
pub const SHORE_LEVEL: f32 = 2.0;

/// Each corner's distance across the ground (x, z) to the shore and the nearest stretch of it
/// (LT-148, [`ShoreAt`]); none for land corners. The game has no bed under its water, so this is
/// its depth.
///
/// The shore is where the land comes down to the water's plane (the height most water corners lie
/// at), along each land triangle with corners above it and at or under it:
/// - where it crosses the plane, between its corners above and under (a sandy bank: the water runs
///   on under the sand, which hides it as it rises);
/// - its corners on the plane (a wall's foot). Land only at or under the plane isn't shore (an
///   island's skirt under the sea);
/// - where the water ends, at an edge no other water triangle shares, unless that edge is on the
///   mesh's own border (the water goes on into the next chunk, whose shore this mesh can't see).
pub fn shore(corners: &[Corner], water: &[bool]) -> Vec<ShoreAt> {
    let xz = |c: &Corner| (c.pos[0], c.pos[2]);
    let mut heights: Vec<(i16, usize)> = Vec::new();
    for (c, _) in corners.iter().zip(water).filter(|(_, &w)| w) {
        match heights.iter_mut().find(|h| h.0 == c.pos[1]) {
            Some(h) => h.1 += 1,
            None => heights.push((c.pos[1], 1)),
        }
    }
    let Some(level) = heights.iter().max_by_key(|h| h.1).map(|h| h.0 as f32) else { return vec![ShoreAt::default(); corners.len()] };
    let (mut lo, mut hi) = ((i16::MAX, i16::MAX), (i16::MIN, i16::MIN));
    for c in corners {
        let (x, z) = xz(c);
        lo = (lo.0.min(x), lo.1.min(z));
        hi = (hi.0.max(x), hi.1.max(z));
    }
    let mut segments: Vec<([f32; 2], [f32; 2])> = Vec::new();
    // Each water edge, either way round, and how many water triangles have it.
    let mut edges: std::collections::HashMap<Edge, u32> = std::collections::HashMap::new();
    for (tri, w) in corners.as_chunks::<3>().0.iter().zip(water.as_chunks::<3>().0) {
        if w[0] {
            for k in 0..3 {
                let (a, b) = (xz(&tri[k]), xz(&tri[(k + 1) % 3]));
                if a != b {
                    *edges.entry(if a < b { (a, b) } else { (b, a) }).or_default() += 1;
                }
            }
            continue;
        }
        // Land: where it comes down from above the water to meet the plane (within
        // [`SHORE_LEVEL`] of it counts as on it). Land that only lies at or under it (an island's
        // skirt under the sea) isn't shore: Y is down, so above is less.
        let d = tri.map(|c| c.pos[1] as f32 - level);
        let side = |v: f32| if v.abs() <= SHORE_LEVEL { 0 } else { v.signum() as i32 };
        if !(d.iter().any(|&v| side(v) < 0) && d.iter().any(|&v| side(v) >= 0)) {
            continue;
        }
        let p = |c: &Corner| [c.pos[0] as f32, c.pos[2] as f32];
        let mut at: Vec<[f32; 2]> = (0..3).filter(|&k| side(d[k]) == 0).map(|k| p(&tri[k])).collect();
        for k in 0..3 {
            let j = (k + 1) % 3;
            if side(d[k]) * side(d[j]) < 0 {
                let t = d[k] / (d[k] - d[j]);
                let (a, b) = (p(&tri[k]), p(&tri[j]));
                at.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        }
        match at.len() {
            0 => {}
            1 => segments.push((at[0], at[0])),
            2 => segments.push((at[0], at[1])),
            _ => segments.extend([(at[0], at[1]), (at[1], at[2]), (at[2], at[0])]),
        }
    }
    let border = |(a, b): Edge| (a.0 == b.0 && (a.0 == lo.0 || a.0 == hi.0)) || (a.1 == b.1 && (a.1 == lo.1 || a.1 == hi.1));
    segments.extend(edges.into_iter().filter(|&(e, n)| n == 1 && !border(e)).map(|((a, b), _)| ([a.0 as f32, a.1 as f32], [b.0 as f32, b.1 as f32])));
    let nearest = |p: [f32; 2]| -> ShoreAt {
        let mut best = ShoreAt { distance: SHORE_MAX, nearest: None };
        for &(a, b) in &segments {
            let d = segment_distance(p, a, b);
            if d < best.distance {
                best = ShoreAt { distance: d, nearest: Some([a, b]) };
            }
        }
        best
    };
    let mut known: std::collections::HashMap<(i16, i16), ShoreAt> = std::collections::HashMap::new();
    corners.iter().zip(water).map(|(c, &w)| if w { *known.entry(xz(c)).or_insert_with(|| nearest([c.pos[0] as f32, c.pos[2] as f32])) } else { ShoreAt::default() }).collect()
}

/// A mesh edge across the ground: its two ends' (x, z), the lesser first.
type Edge = ((i16, i16), (i16, i16));

/// A water corner's shore (LT-148, [`shore`]): its distance across the ground to it (at most
/// [`SHORE_MAX`]), and the stretch of shore that's nearest, in mesh x, z (none past that). The lit
/// pass measures each pixel's distance to its triangle's corners' stretches, so the foam keeps its
/// width inside a triangle whose corners all lie by the shore.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShoreAt {
    pub distance: f32,
    pub nearest: Option<[[f32; 2]; 2]>,
}

/// `p`'s distance to the segment from `a` to `b`.
pub fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (d, e) = ([b[0] - a[0], b[1] - a[1]], [p[0] - a[0], p[1] - a[1]]);
    let len = d[0] * d[0] + d[1] * d[1];
    let t = if len > 0.0 { ((e[0] * d[0] + e[1] * d[1]) / len).clamp(0.0, 1.0) } else { 0.0 };
    let (x, z) = (e[0] - t * d[0], e[1] - t * d[1]);
    (x * x + z * z).sqrt()
}

/// The ripple map's side (texels): one tile of the water's surface.
pub const RIPPLE_SIZE: usize = 256;

/// Points of the caustic web in a tile (LT-148): its cells' count.
const CAUSTIC_CELLS: usize = 24;

/// A tileable caustic web (LT-148): where the light a rippled surface bends gathers on a bed,
/// bright lines between cells. Each texel's nearness to the border between its two nearest of
/// [`CAUSTIC_CELLS`] points (Worley's F2 − F1, on the torus, so it tiles): 1 on the border, falling
/// to 0 a few texels off it.
fn caustic_web(n: usize) -> Vec<f64> {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let points: Vec<[f64; 2]> = (0..CAUSTIC_CELLS).map(|_| [rand(), rand()]).collect();
    let mut out = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            let p = [(x as f64 + 0.5) / n as f64, (y as f64 + 0.5) / n as f64];
            let (mut f1, mut f2) = (f64::MAX, f64::MAX);
            for q in &points {
                let dx = (p[0] - q[0]).abs().min(1.0 - (p[0] - q[0]).abs());
                let dy = (p[1] - q[1]).abs().min(1.0 - (p[1] - q[1]).abs());
                let d = (dx * dx + dy * dy).sqrt();
                if d < f1 {
                    f2 = f1;
                    f1 = d;
                } else if d < f2 {
                    f2 = d;
                }
            }
            let edge = (f2 - f1) * n as f64 / 6.0;
            out.push((1.0 - edge).clamp(0.0, 1.0).powi(2));
        }
    }
    out
}

/// A tileable water surface for the lit look's ripples, and its mip chain (largest first), RGBA8:
/// the surface's slope along x and z (red, green: 0.5 + slope / 2, the steepest at the edges) and
/// its height (blue: 0.5 + height / 2); alpha is the shallows' caustic web (LT-148,
/// `caustic_web`).
///
/// It's a sum of about 160 waves whose wavenumbers are whole numbers of cycles a tile, so it tiles
/// with no seam: their lengths from half the tile to an eighth of a texel's worth, their strength
/// falling with frequency as a sea's does, mostly along a wind (cos² spreading), phases from a
/// fixed seed. The mips average the slopes, so far water goes calm instead of shimmering. Made in
/// code (no texture file): Bevy's water example's technique (layered scrolling normal maps), on a
/// surface of our own.
pub fn ripple_map() -> Vec<Vec<u8>> {
    let n = RIPPLE_SIZE;
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    // (kx, kz, amplitude, phase): whole cycles a tile.
    let wind = (0.31f64).atan2(1.0);
    let mut waves = Vec::new();
    while waves.len() < 160 {
        let (kx, kz) = ((rand() * 61.0) as i32 - 30, (rand() * 61.0) as i32 - 30);
        let k = ((kx * kx + kz * kz) as f64).sqrt();
        if !(2.0..=30.0).contains(&k) {
            continue;
        }
        let spread = ((kz as f64).atan2(kx as f64) - wind).cos();
        let a = k.powf(-1.6) * (0.25 + 0.75 * spread * spread);
        waves.push((kx as f64, kz as f64, a, rand() * std::f64::consts::TAU));
    }
    let tau_n = std::f64::consts::TAU / n as f64;
    let mut field = vec![[0.0f64; 3]; n * n];
    for (y, row) in field.chunks_mut(n).enumerate() {
        for (x, f) in row.iter_mut().enumerate() {
            for &(kx, kz, a, ph) in &waves {
                let phase = tau_n * (kx * x as f64 + kz * y as f64) + ph;
                let (s, c) = phase.sin_cos();
                // Height, and its slope per texel along x and z.
                f[2] += a * s;
                f[0] += a * c * tau_n * kx;
                f[1] += a * c * tau_n * kz;
            }
        }
    }
    let peak = |i: usize| field.iter().map(|f| f[i].abs()).fold(0.0f64, f64::max).max(1e-9);
    let (sx, sz, sh) = (peak(0).max(peak(1)), peak(0).max(peak(1)), peak(2));
    let web = caustic_web(n);
    // Slopes and height (-1..1), and the web (0..1).
    let mut level: Vec<[f64; 4]> = field.iter().zip(&web).map(|(f, &c)| [f[0] / sx, f[1] / sz, f[2] / sh, c]).collect();
    let mut side = n;
    let mut out = Vec::new();
    loop {
        out.push(level.iter().flat_map(|f| [0, 1, 2].map(|i| ((0.5 + f[i] / 2.0).clamp(0.0, 1.0) * 255.0).round() as u8).into_iter().chain([(f[3].clamp(0.0, 1.0) * 255.0).round() as u8])).collect());
        if side == 1 {
            break;
        }
        let half = side / 2;
        level = (0..half * half)
            .map(|i| {
                let (x, y) = (i % half * 2, i / half * 2);
                let at = |dx: usize, dy: usize| level[(y + dy) * side + x + dx];
                [0, 1, 2, 3].map(|c| (at(0, 0)[c] + at(1, 0)[c] + at(0, 1)[c] + at(1, 1)[c]) / 4.0)
            })
            .collect();
        side = half;
    }
    out
}

/// The mirrored pass's resolution: the picture's divided by this each way (LT-147). The ripples
/// distort the reflection by more than a pixel, so half is enough.
pub const MIRROR_DIVISOR: u32 = 2;

/// The mirrored pass's depth range (view z): nearer than [`MIRROR_NEAR`] is clipped.
pub const MIRROR_NEAR: f64 = 16.0;
pub const MIRROR_FAR: f64 = 65536.0;

/// The water plane a scene's reflection is drawn about (LT-147): the height most of its water
/// corners lie at (all the game's water is level, at 0 nearly everywhere, −512 in chunk 153); none
/// without water.
pub fn plane(scene: &Scene) -> Option<f32> {
    let mut counts: Vec<(i32, usize)> = Vec::new();
    for v in scene.batches.iter().flat_map(|b| &b.corners).filter(|v| v.water) {
        let y = v.world[1].round() as i32;
        match counts.iter_mut().find(|c| c.0 == y) {
            Some(c) => c.1 += 1,
            None => counts.push((y, 1)),
        }
    }
    counts.into_iter().max_by_key(|c| (c.1, -c.0)).map(|c| c.0 as f32)
}

/// Whether a water corner at height `y` lies on the mirrored `plane` (water at another height
/// shows no reflection, its mirror being elsewhere).
pub fn on_plane(y: f32, plane: f32) -> bool {
    (y - plane).abs() < 1.5
}

/// World space to the clip space of the scene mirrored about the water plane at height `y`
/// (LT-147), column-major: a point `p` lands where its reflection `(p.x, 2y − p.y, p.z)` would be
/// on screen through `cam`, mapped as the lit pass maps the screen (`composite::pack_lit`: `width`
/// × `height`, `left` columns more each side). The divide is the GPU's (w = view z), so what
/// crosses the camera's plane is clipped, and the depth is a perspective one from
/// [`MIRROR_NEAR`] to [`MIRROR_FAR`].
pub fn mirror_clip(cam: &Projection, y: f32, width: f32, height: f32, left: f32) -> [[f32; 4]; 4] {
    let (m, t, y) = (cam.view.m, cam.view.t, y as f64);
    // view · reflect: the y column negated, the translation moved by 2y along it.
    let mr = [0, 1, 2].map(|i| [m[i][0], -m[i][1], m[i][2]]);
    let tr = [0, 1, 2].map(|i| t[i] + 2.0 * y * m[i][1]);
    let (h, ox, oy) = (cam.h as f64, cam.offset[0] as f64, cam.offset[1] as f64);
    let (w, hh, left) = ((width + 2.0 * left) as f64, height as f64, left as f64);
    let k = MIRROR_FAR / (MIRROR_FAR - MIRROR_NEAR);
    // Clip space from view space (vx, vy, vz, 1): x and y the screen mapping times vz.
    let p = [
        [2.0 * h / w, 0.0, (ox + left + 0.5) * 2.0 / w - 1.0, 0.0],
        [0.0, -2.0 * h / hh, 1.0 - (oy + 0.5) * 2.0 / hh, 0.0],
        [0.0, 0.0, k, -MIRROR_NEAR * k],
        [0.0, 0.0, 1.0, 0.0],
    ];
    let rows = p.map(|p| {
        let mut r = [0.0f64; 4];
        for (j, v) in r.iter_mut().enumerate().take(3) {
            *v = p[0] * mr[0][j] + p[1] * mr[1][j] + p[2] * mr[2][j];
        }
        r[3] = p[0] * tr[0] + p[1] * tr[1] + p[2] * tr[2] + p[3];
        r
    });
    [0, 1, 2, 3].map(|c| [0, 1, 2, 3].map(|r| rows[r][c] as f32))
}

/// The waves' clock, in seconds, for a frame `t` (0..1) of the way from the tick that started at
/// frame `a` to the one that started at `b`: the game's own frames, so a tick's view and the
/// frames between agree.
pub fn clock(a: u64, b: u64, t: f64) -> f32 {
    ((a as f64 + (b as f64 - a as f64) * t) / FIELD_HZ) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use lt_core::mesh::Tile;

    #[test]
    fn blues_are_water_and_greens_greys_and_reds_arent() {
        for c in [[40.0, 90.0, 200.0], [82.0, 143.0, 220.0], [20.0, 120.0, 130.0]] {
            assert!(blue(c), "{c:?}");
        }
        for c in [[60.0, 160.0, 60.0], [128.0, 128.0, 140.0], [200.0, 80.0, 40.0], [10.0, 10.0, 30.0], [200.0, 200.0, 240.0]] {
            assert!(!blue(c), "{c:?}");
        }
    }

    fn quad(y: [i16; 4], uv: [u16; 2], rgb: [u8; 3]) -> Vec<Corner> {
        let p = [[0, y[0], 0], [100, y[1], 0], [0, y[2], 100], [100, y[3], 100]];
        [0, 1, 2, 1, 3, 2].iter().map(|&i| Corner { pos: p[i], vertex: i as u16, uv, rgb, flags: flag::TEXTURED | flag::QUAD }).collect()
    }

    /// A level blue quad is water; tilted, or green, it isn't; a colour that darkens the blue
    /// texture to black isn't either.
    #[test]
    fn level_blue_polygons_are_water() {
        let mut rgba = vec![0u8; 16 * 8 * 4];
        for y in 0..8 {
            for x in 0..16 {
                let o = (y * 16 + x) * 4;
                rgba[o..o + 4].copy_from_slice(if x < 8 { &[40, 90, 200, 255] } else { &[60, 160, 60, 255] });
            }
        }
        let t = |x: u16| Tile { tpage: 0, clut: 0, x, y: 0, w: 8, h: 8, u0: 0, v0: 0 };
        let tex = TextureData { w: 16, h: 8, rgba, tiles: vec![t(0), t(8)] };
        let water = |c: &[Corner]| corners(c, Some(&tex));
        assert!(water(&quad([0; 4], [2, 2], [128; 3])).iter().all(|&w| w), "level, blue");
        assert!(!water(&quad([0, -80, 0, -80], [2, 2], [128; 3])).iter().any(|&w| w), "a slope");
        assert!(!water(&quad([0; 4], [10, 2], [128; 3])).iter().any(|&w| w), "green");
        assert!(!water(&quad([0; 4], [2, 2], [8; 3])).iter().any(|&w| w), "modulated to black");
        // A blue tile mostly on walls: its one level quad isn't water (the ice cave's wall tops).
        let mut walls = quad([0; 4], [2, 2], [128; 3]);
        for _ in 0..4 {
            walls.extend(quad([0, -80, 0, -80], [2, 2], [128; 3]));
        }
        assert!(!water(&walls).iter().any(|&w| w), "a wall texture's level top");
        // A white texture shaded blue by its corners isn't water.
        let mut white = tex.clone();
        for p in white.rgba.chunks_mut(4) {
            p.copy_from_slice(&[250, 253, 254, 255]);
        }
        assert!(!corners(&quad([0; 4], [2, 2], [60, 90, 120]), Some(&white)).iter().any(|&w| w), "white, shaded blue");
    }

    /// The ripple map tiles (its last column runs on into its first), its slopes reach the edges of
    /// their range, and its mips calm down to level.
    #[test]
    fn the_ripple_map_tiles_and_calms_with_distance() {
        let m = ripple_map();
        assert_eq!(m.len(), 9);
        assert_eq!(m[0].len(), RIPPLE_SIZE * RIPPLE_SIZE * 4);
        let px = |l: &[u8], side: usize, x: usize, y: usize, c: usize| l[(y * side + x) * 4 + c] as i32;
        let n = RIPPLE_SIZE;
        // The height across the seam steps no more than it does inside the tile.
        let seam = (0..n).map(|y| (px(&m[0], n, n - 1, y, 2) - px(&m[0], n, 0, y, 2)).abs()).max().unwrap();
        let inside = (0..n).map(|y| (px(&m[0], n, n / 2, y, 2) - px(&m[0], n, n / 2 + 1, y, 2)).abs()).max().unwrap();
        assert!(seam <= inside + 2, "seam {seam}, inside {inside}");
        let spread = |l: &[u8]| l.chunks(4).map(|p| (p[0] as i32 - 128).abs()).max().unwrap();
        assert!(spread(&m[0]) >= 120, "slopes use their range");
        assert!(spread(&m[8]) <= 4, "the last mip is level");
    }

    /// The mirrored pass's matrix puts a point where its reflection about the plane projects
    /// through the camera (`Gte::screen`), mapped as the lit pass maps the screen, wide or not.
    #[test]
    fn the_mirror_projects_a_points_reflection() {
        use crate::gte::Gte;
        use crate::light::Affine;
        // A field camera: tilted 50° down, the 512 mode's 1.6 x-stretch in its first row.
        let (s, c) = (50f64.to_radians().sin(), 50f64.to_radians().cos());
        let view = Affine { m: [[1.6, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]], t: [-300.0, 120.0, 2600.0] };
        let cam = Projection { view, h: 892.0, offset: [257.0, 112.0] };
        let g = Gte::exact(view.m, view.t, 892, [257, 112]);
        for (left, plane) in [(0.0f32, 0.0f32), (64.0, 0.0), (0.0, -512.0)] {
            let clip = mirror_clip(&cam, plane, 512.0, 224.0, left);
            for p in [[100.0f32, -200.0, 300.0], [-400.0, -50.0, 900.0], [250.0, plane - 1.0, -100.0]] {
                let c = |r: usize| (0..3).map(|k| clip[k][r] as f64 * p[k] as f64).sum::<f64>() + clip[3][r] as f64;
                let (x, y, z, w) = (c(0), c(1), c(2), c(3));
                let s = g.float([p[0], 2.0 * plane - p[1], p[2]]);
                let left = left as f64;
                let want_x = (s.x as f64 + left + 0.5) / (512.0 + 2.0 * left) * 2.0 - 1.0;
                let want_y = 1.0 - (s.y as f64 + 0.5) / 224.0 * 2.0;
                assert!((x / w - want_x).abs() < 1e-4 && (y / w - want_y).abs() < 1e-4, "{p:?} about {plane}: {} {} want {want_x} {want_y}", x / w, y / w);
                assert!((w - s.z as f64).abs() < 0.05 && (0.0..1.0).contains(&(z / w)), "depth {w} {}", z / w);
            }
        }
    }

    /// The plane is the height most water lies at; no water, no plane.
    #[test]
    fn the_plane_is_where_most_water_lies() {
        use crate::draw::{Batch, Vertex};
        let v = |y: f32, water: bool| Vertex { world: [0.0, y, 0.0], water, ..Vertex::default() };
        let mut s = Scene::default();
        assert_eq!(plane(&s), None);
        s.batches.push(Batch { corners: vec![v(-80.0, false), v(0.2, true), v(-0.1, true), v(-512.0, true)], ..Batch::default() });
        assert_eq!(plane(&s), Some(0.0));
        assert!(on_plane(0.4, 0.0) && !on_plane(-512.0, 0.0));
    }

    /// A water strip 4 tiles wide beside a bank (and under a roof), its other sides on the mesh's
    /// border: each water corner's distance to the shore is how far it lies from the bank, where the
    /// water ends; the border, where it goes on into the next chunk, isn't shore.
    #[test]
    fn water_knows_its_distance_to_the_shore() {
        let tri = |p: [[i16; 3]; 3]| p.map(|pos| Corner { pos, vertex: 0, uv: [0, 0], rgb: [128; 3], flags: 0 });
        let mut corners = Vec::new();
        let mut water = Vec::new();
        // Water: x 0..512 (4 tiles of 128), z 0..256, at y 0.
        for i in 0..4i16 {
            let (x0, x1) = (i * 128, i * 128 + 128);
            for t in [tri([[x0, 0, 0], [x1, 0, 0], [x0, 0, 256]]), tri([[x1, 0, 0], [x1, 0, 256], [x0, 0, 256]])] {
                corners.extend(t);
                water.extend([true; 3]);
            }
        }
        // The bank: from x 0 back to x −128, rising.
        corners.extend(tri([[0, 0, 0], [-128, -80, 0], [0, 0, 256]]));
        // A roof over the water's far end, 300 up: not the shore.
        corners.extend(tri([[384, -300, 0], [512, -300, 0], [384, -300, 256]]));
        water.extend([false; 6]);
        let d: Vec<f32> = shore(&corners, &water).iter().map(|s| s.distance).collect();
        let at = |x: i16| corners.iter().zip(&d).find(|(c, _)| c.pos == [x, 0, 0]).map(|(_, &d)| d).unwrap();
        assert_eq!(at(0), 0.0, "on the bank's foot");
        assert_eq!((at(128), at(256), at(512)), (128.0, 256.0, 512.0), "the border and the roof aren't the shore");
        assert!(d[corners.len() - 6..].iter().all(|&v| v == 0.0), "land has none");
        // No shore in the mesh: open water.
        let open = shore(&corners[..24], &water[..24]);
        assert!(open.iter().all(|s| s.distance == SHORE_MAX && s.nearest.is_none()));
        // A corner's nearest stretch is the bank's foot.
        let near = shore(&corners, &water);
        let i = corners.iter().position(|c| c.pos == [256, 0, 0]).unwrap();
        assert_eq!(near[i].nearest.map(|[a, b]| (a[0], b[0])), Some((0.0, 0.0)));
        // A sandy bank: the water runs on under it (to x −128); the bank slopes from 40 under the
        // water there to 40 above it at x 64, so the shore is where it crosses, at x −32.
        let mut sandy = corners[..24].to_vec();
        let mut wet = water[..24].to_vec();
        sandy.extend(tri([[-128, 0, 0], [0, 0, 0], [-128, 0, 256]]));
        sandy.extend(tri([[0, 0, 0], [0, 0, 256], [-128, 0, 256]]));
        sandy.extend(tri([[-128, 40, 0], [64, -40, 0], [-128, 40, 256]]));
        wet.extend([true, true, true, true, true, true, false, false, false]);
        let d: Vec<f32> = shore(&sandy, &wet).iter().map(|s| s.distance).collect();
        let at = |x: i16| sandy.iter().zip(&d).find(|(c, w)| c.pos == [x, 0, 0] && **w > 0.0).map(|(_, &d)| d).unwrap();
        assert_eq!((at(128), at(256)), (160.0, 288.0), "from where the bank crosses the water");
    }

    /// The caustic web (the ripple map's alpha) tiles, has bright lines and dark cells, and calms
    /// with distance as the ripples do.
    #[test]
    fn the_caustic_web_tiles_with_lines_and_cells() {
        let m = ripple_map();
        let n = RIPPLE_SIZE;
        let a = |x: usize, y: usize| m[0][(y * n + x) * 4 + 3] as i32;
        let lines = (0..n * n).filter(|&i| m[0][i * 4 + 3] > 200).count();
        let cells = (0..n * n).filter(|&i| m[0][i * 4 + 3] < 20).count();
        assert!(lines > n * n / 50 && cells > n * n / 2, "{lines} line texels, {cells} cell texels");
        let seam = (0..n).map(|y| (a(n - 1, y) - a(0, y)).abs()).max().unwrap();
        let inside = (0..n).map(|y| (a(n / 2, y) - a(n / 2 + 1, y)).abs()).max().unwrap();
        assert!(seam <= inside + 2, "seam {seam}, inside {inside}");
        let spread = |l: &[u8]| l.chunks(4).map(|p| p[3] as i32).max().unwrap() - l.chunks(4).map(|p| p[3] as i32).min().unwrap();
        assert!(spread(&m[6]) < spread(&m[0]) / 2, "far caustics calm");
    }

    #[test]
    fn the_clock_runs_through_ticks() {
        assert_eq!(clock(100, 102, 0.0), clock(98, 100, 1.0));
        assert!(clock(100, 102, 0.5) > clock(100, 102, 0.25));
        assert!((clock(0, 60, 1.0) - 1.001).abs() < 1e-3);
    }
}
