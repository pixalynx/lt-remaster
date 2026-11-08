//! A small deterministic software rasterizer for orthographic top-down views
//! of world chunks (`docs/screens/09-world-viewer.md` §5.2).
//!
//! - Chunk-local x maps to columns and +z to rows going **up** (north up),
//!   `px = (x + 0x800)·s`, `py = (0x800 − z)·s`, `s = size / 4096`.
//! - Depth is the height: PSX Y points down, so the smallest y wins.
//! - Textured polygons sample the simulated VRAM ([`Vram`]) with affine UVs
//!   and modulate by the vertex colour (128 = 1.0) unless the raw bit is set;
//!   texel 0x0000 is never drawn.
//! - Semi-transparent polygons are drawn after the opaque ones, lowest first,
//!   blending by the tpage ABR mode on texels with the STP bit.
//! - Quads are PSX strip order: triangles 0-1-2 and 1-3-2.

use crate::vram::{c5, Texel, Vram, WATER_RGB};
use crate::world::{Mesh, Prim};

/// RGBA8 image with a height buffer.
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    depth: Vec<i32>,
}

impl Canvas {
    pub fn new(w: usize, h: usize, clear: [u8; 4]) -> Self {
        let mut rgba = vec![0u8; w * h * 4];
        for px in rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&clear);
        }
        Self { w, h, rgba, depth: vec![i32::MAX; w * h] }
    }
}

/// How a chunk is drawn from above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Textures through the VRAM model, flat or gouraud vertex colours.
    Textured,
    /// Polygon edges coloured by draw group on a dark ground (the editor's
    /// "draw groups" view).
    Groups,
}

/// Orthographic mapping from chunk-local units to canvas pixels:
/// `px = (x − left)·scale`, `py = (top − z)·scale`.
#[derive(Debug, Clone, Copy)]
pub struct Ortho {
    /// Pixels per world unit.
    pub scale: f32,
    /// Chunk-local x at the left edge and z at the top edge.
    pub left: f32,
    pub top: f32,
}

impl Ortho {
    /// The whole chunk (±0x800) fills a `size`×`size` canvas.
    pub fn chunk(size: usize) -> Self {
        Self { scale: size as f32 / 4096.0, left: -2048.0, top: 2048.0 }
    }

    /// The smallest square around the mesh's vertices (plus a 4% margin) fills
    /// the canvas; the whole chunk if the mesh is empty.
    pub fn fit(mesh: &Mesh, size: usize) -> Self {
        let used = mesh.prims.iter().flat_map(|p| p.idx[..p.n as usize].iter().map(|&i| vertex(mesh, i)));
        let (mut x0, mut x1, mut z0, mut z1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for v in used {
            x0 = x0.min(v[0] as i32);
            x1 = x1.max(v[0] as i32);
            z0 = z0.min(v[2] as i32);
            z1 = z1.max(v[2] as i32);
        }
        if x0 > x1 {
            return Self::chunk(size);
        }
        let span = ((x1 - x0).max(z1 - z0) as f32 * 1.08).max(256.0);
        let (cx, cz) = ((x0 + x1) as f32 / 2.0, (z0 + z1) as f32 / 2.0);
        Self { scale: size as f32 / span, left: cx - span / 2.0, top: cz + span / 2.0 }
    }

    #[inline]
    fn project(&self, v: [i16; 3]) -> (f32, f32) {
        ((v[0] as f32 - self.left) * self.scale, (self.top - v[2] as f32) * self.scale)
    }
}

/// Draws a chunk mesh. `vram` is required for [`Style::Textured`].
pub fn draw_mesh(cv: &mut Canvas, mesh: &Mesh, vram: Option<&Vram>, ortho: Ortho, style: Style) {
    match style {
        Style::Textured => draw_textured(cv, mesh, vram, ortho),
        Style::Groups => draw_groups(cv, mesh, ortho),
    }
}

fn draw_textured(cv: &mut Canvas, mesh: &Mesh, vram: Option<&Vram>, ortho: Ortho) {
    let mut semi: Vec<(i32, &Prim)> = Vec::new();
    for p in &mesh.prims {
        if p.semi() {
            let top = p.idx[..p.n as usize].iter().map(|&i| vertex(mesh, i)[1] as i32).max().unwrap_or(0);
            semi.push((-top, p));
            continue;
        }
        draw_prim(cv, mesh, vram, ortho, p, false);
    }
    // Lowest (largest y) first.
    semi.sort_by_key(|(k, _)| *k);
    for (_, p) in semi {
        draw_prim(cv, mesh, vram, ortho, p, true);
    }
}

#[inline]
fn vertex(mesh: &Mesh, i: u16) -> [i16; 3] {
    mesh.pool.get(i as usize).copied().unwrap_or([0, 0, 0])
}

#[derive(Clone, Copy, Default)]
struct Vtx {
    x: f32,
    y: f32,
    h: f32,
    c: [f32; 3],
    u: f32,
    v: f32,
}

fn draw_prim(cv: &mut Canvas, mesh: &Mesh, vram: Option<&Vram>, ortho: Ortho, p: &Prim, blend: bool) {
    let mut vx = [Vtx::default(); 4];
    for k in 0..p.n as usize {
        let v = vertex(mesh, p.idx[k]);
        let (x, y) = ortho.project(v);
        let c = p.rgb[k];
        vx[k] = Vtx { x, y, h: v[1] as f32, c: [c[0] as f32, c[1] as f32, c[2] as f32], u: p.uv[k][0] as f32, v: p.uv[k][1] as f32 };
    }
    tri(cv, vram, p, blend, &vx[0], &vx[1], &vx[2]);
    if p.n == 4 {
        tri(cv, vram, p, blend, &vx[1], &vx[3], &vx[2]);
    }
}

fn tri(cv: &mut Canvas, vram: Option<&Vram>, p: &Prim, blend: bool, a: &Vtx, b: &Vtx, c: &Vtx) {
    let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    if area.abs() < 1e-6 {
        return;
    }
    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as i32;
    let max_x = a.x.max(b.x).max(c.x).ceil().min(cv.w as f32 - 1.0) as i32;
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as i32;
    let max_y = a.y.max(b.y).max(c.y).ceil().min(cv.h as f32 - 1.0) as i32;
    if min_x > max_x || min_y > max_y {
        return;
    }
    let inv = 1.0 / area;
    let textured = p.textured() && vram.is_some();
    let abr = (p.tpage >> 5) & 3;
    for py in min_y..=max_y {
        let sy = py as f32 + 0.5;
        for px in min_x..=max_x {
            let sx = px as f32 + 0.5;
            // Barycentric weights (top-left rule approximated by a tiny epsilon).
            let w0 = ((b.x - sx) * (c.y - sy) - (b.y - sy) * (c.x - sx)) * inv;
            let w1 = ((c.x - sx) * (a.y - sy) - (c.y - sy) * (a.x - sx)) * inv;
            let w2 = 1.0 - w0 - w1;
            if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                continue;
            }
            let h = (w0 * a.h + w1 * b.h + w2 * c.h).round() as i32;
            let i = py as usize * cv.w + px as usize;
            if h > cv.depth[i] {
                continue;
            }
            let shade = [
                w0 * a.c[0] + w1 * b.c[0] + w2 * c.c[0],
                w0 * a.c[1] + w1 * b.c[1] + w2 * c.c[1],
                w0 * a.c[2] + w1 * b.c[2] + w2 * c.c[2],
            ];
            let (rgb, stp) = if textured {
                let u = (w0 * a.u + w1 * b.u + w2 * c.u).clamp(0.0, 255.0) as u8;
                let v = (w0 * a.v + w1 * b.v + w2 * c.v).clamp(0.0, 255.0) as u8;
                let (t, stp) = match vram.unwrap().texel(p.tpage, p.clut, u, v) {
                    Texel::Clear => continue,
                    Texel::Colour(t) => ([c5(t), c5(t >> 5), c5(t >> 10)], t & 0x8000 != 0),
                    Texel::Water => (WATER_RGB, false),
                };
                if p.raw() {
                    (t, stp)
                } else {
                    (modulate(t, shade), stp)
                }
            } else {
                ([shade[0] as u8, shade[1] as u8, shade[2] as u8], true)
            };
            let o = i * 4;
            if blend && stp {
                let back = [cv.rgba[o], cv.rgba[o + 1], cv.rgba[o + 2]];
                for k in 0..3 {
                    let (bk, f) = (back[k] as i32, rgb[k] as i32);
                    cv.rgba[o + k] = match abr {
                        0 => (bk + f) / 2,
                        1 => bk + f,
                        2 => bk - f,
                        _ => bk + f / 4,
                    }
                    .clamp(0, 255) as u8;
                }
                cv.rgba[o + 3] = 255;
            } else {
                cv.rgba[o..o + 3].copy_from_slice(&rgb);
                cv.rgba[o + 3] = 255;
                if !blend {
                    cv.depth[i] = h;
                }
            }
        }
    }
}

#[inline]
fn modulate(t: [u8; 3], shade: [f32; 3]) -> [u8; 3] {
    let m = |t: u8, s: f32| ((t as f32 * s) / 128.0).min(255.0) as u8;
    [m(t[0], shade[0]), m(t[1], shade[1]), m(t[2], shade[2])]
}

/* ───────────────────────── Draw-group wireframe ───────────────────────── */

/// Background of the draw-group view.
pub const GROUPS_BG: [u8; 4] = [20, 20, 20, 255];

/// A bright colour per draw group (golden-ratio hue steps).
pub fn group_colour(g: u32) -> [u8; 3] {
    let h = (g as f32 * 0.618_034).fract() * 6.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let lift = |c: f32| (64.0 + c * 191.0) as u8;
    [lift(r), lift(g), lift(b)]
}

fn draw_groups(cv: &mut Canvas, mesh: &Mesh, ortho: Ortho) {
    for g in &mesh.groups {
        let col = group_colour(g.index);
        for p in &mesh.prims[g.first as usize..(g.first + g.count) as usize] {
            let order: &[usize] = if p.n == 4 { &[0, 1, 3, 2] } else { &[0, 1, 2] };
            for k in 0..order.len() {
                let a = ortho.project(vertex(mesh, p.idx[order[k]]));
                let b = ortho.project(vertex(mesh, p.idx[order[(k + 1) % order.len()]]));
                line(cv, a, b, col);
            }
        }
    }
}

fn line(cv: &mut Canvas, a: (f32, f32), b: (f32, f32), col: [u8; 3]) {
    let (mut x0, mut y0) = (a.0.floor() as i32, a.1.floor() as i32);
    let (x1, y1) = (b.0.floor() as i32, b.1.floor() as i32);
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    let mut guard = 0;
    loop {
        if x0 >= 0 && y0 >= 0 && (x0 as usize) < cv.w && (y0 as usize) < cv.h {
            let o = (y0 as usize * cv.w + x0 as usize) * 4;
            cv.rgba[o..o + 3].copy_from_slice(&col);
            cv.rgba[o + 3] = 255;
        }
        if (x0 == x1 && y0 == y1) || guard > 20_000 {
            break;
        }
        guard += 1;
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}
