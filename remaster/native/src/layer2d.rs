//! The 2D layer: the ordering table's packets the native view doesn't draw itself (HUD, text,
//! menus, effects, shadows), decoded the way PSoXide's hardware renderer decodes them
//! (`psx_gpu_render::Translator`), each with its bucket.
//!
//! **Sorting by bucket.** The game puts each primitive in bucket screen Z / 4 (the farthest vertex
//! for chunk polygons, the nearest for models), and the GPU draws from the farthest bucket to the
//! nearest. The native 3D records each pixel's polygon bucket, and a 2D primitive shows where that
//! polygon's bucket is farther than its own: exactly what the GPU's painting gives.
//!
//! Testing against per-pixel depth instead (bucket `b` as depth `4b`) gets it wrong wherever a
//! polygon spans depth. A full-screen fade in bucket b covers every terrain polygon whose farthest
//! vertex is beyond 4b, including those polygons' nearer pixels; per-pixel depth let them through.
//! Within one bucket the GPU draws the later-linked packet first, so an effect linked after the
//! terrain is behind it. The HUD (`HUD_FlushTextBufferToOT`) and the `SetDrawMove`s are in bucket 0,
//! in front of everything.
//!
//! **A frozen frame** (LT-140): the quads that show a frozen frame (`FUN_8018afa0`'s, noted by the
//! takeover in [`Table::frozen_quads`]) are marked ([`Vertex::frozen`]): the shader draws them from
//! the native view's own picture of that frame, by position, dimmed by the quads' colour. In a
//! widened view each is copied into the margin on the side it reaches, so the frozen field carries on
//! past the 4:3 under the menu.
//!
//! **A full-screen fade** (LT-140, LT-150): a quad of one colour covering the whole screen (a chunk
//! load's fade out and in, the battle loading screen's tint), which the GPU clips to the 4:3, is
//! carried into a widened view's margins too, over a frozen frame or the live field: it darkens them
//! as it darkens the middle.

use emulator_core::gpu::{gp0_command_word_count, GpuCmdFifo, GpuCmdLogEntry};
use psx_gpu_render::Translator;

use crate::takeover::{Linked, Table};

/// One vertex of the 2D layer: PSoXide's `HwVertex` (84 bytes, its layout), its bucket, and its
/// position in floating point (the integer one, but for a billboard's corners above 1×).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub pos: [i16; 2],
    pub color: [u8; 4],
    pub uv: [u16; 2],
    pub flags: u32,
    pub tex_window: u32,
    pub planes: [[u32; 4]; 4],
    /// The packet's ordering-table bucket.
    pub bucket: f32,
    /// Where the shader places it (VRAM pixels).
    pub fpos: [f32; 2],
    /// LT-140: [`FROZEN`] on a frozen frame's quad, [`FROZEN_COPY`] on its copy in a margin; drawn
    /// from the native picture of that frame when the compositor has one (a copy isn't drawn
    /// otherwise). 0 for every other vertex.
    pub frozen: u32,
}

/// [`Vertex::frozen`]'s values.
pub const FROZEN: u32 = 1;
pub const FROZEN_COPY: u32 = 2;

impl Vertex {
    pub const BYTES: usize = 100;

    pub fn write(&self, out: &mut Vec<u8>) {
        for v in self.pos {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.color);
        for v in self.uv {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.tex_window.to_le_bytes());
        for p in self.planes {
            for w in p {
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
        out.extend_from_slice(&self.bucket.to_le_bytes());
        for v in self.fpos {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.frozen.to_le_bytes());
    }
}

/// A run of vertices drawn with one blend and one draw-area clip (inclusive VRAM rect).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// 0 opaque, 1 average, 2 add, 3 subtract, 4 add-quarter.
    pub blend: usize,
    pub first: u32,
    pub count: u32,
    pub clip: [u16; 4],
    /// A billboard's in a widened view: clipped to the clip's lines only, its columns the whole
    /// picture's (LT-144).
    pub wide: bool,
    /// Drawn only in a widened view's margin on that side, the clip's lines (LT-149: a battle's sky
    /// carried on past the game's 4:3, which stays the game's).
    pub margin: Option<Side>,
}

/// A side of a widened view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// The table's kept packets as triangles.
#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub vertices: Vec<Vertex>,
    pub runs: Vec<Run>,
    /// GP0 commands decoded, and those that drew nothing (environment, VRAM copies, NOPs).
    pub commands: u32,
    pub silent: u32,
    /// Commands drawn as billboards (LT-144), and of them those put back (trail sparkles, smoke).
    pub billboards: u32,
    pub restored: u32,
    /// Each billboard drawn: its caller and its corners where the shader puts them (VRAM pixels).
    pub quads: Vec<Quad>,
    /// Commands of the characters' blob shadows drawn.
    pub blob_shadows: u32,
    /// A battle's sky packets carried into a widened view's margins (LT-149).
    pub battle_sky: u32,
    /// Packets showing a frozen frame (LT-140), and their copies in a widened view's margins.
    pub frozen: u32,
    pub frozen_copies: u32,
    /// Copies of full-screen quads of one colour (fades, tints) in a widened view's margins
    /// (LT-140, LT-150), and the packets carried so (their addresses, each once).
    pub screen_copies: u32,
    pub screened: Vec<u32>,
}

/// A billboard the 2D layer drew.
#[derive(Clone, Debug, PartialEq)]
pub struct Quad {
    pub caller: &'static str,
    pub corners: Vec<[f32; 2]>,
    /// One the game didn't draw.
    pub restored: bool,
}

/// How the 2D layer draws (LT-144). The default is the game's: every packet as the GPU draws it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Leave the characters' blob shadows out: the lit look is casting real ones.
    pub no_blob_shadows: bool,
    /// Billboards with sub-pixel corners, projected again in floating point (above 1×).
    pub float: bool,
    /// A widened view: billboards unclipped by the game's 4:3 draw area, and the trail sparkles
    /// it drops near the 4:3 edges put back. The first flight's sky (LT-143) is stretched across,
    /// and a battle's sky (LT-149) carried on into the margins.
    pub wide: bool,
    /// In a widened view, full-screen fades kept to the 4:3 as the game draws them, not carried
    /// into the margins (a check's: LT-150's).
    pub fades_4_3: bool,
}

/// How far past each side the first flight's sky is stretched in a widened view (pixels): wider
/// than any margin. Its colour changes only down the screen, so the sky in the margins is the sky.
const SKY_REACH: i16 = 1024;

/// Bits of `HwVertex::flags` (the shader's): the attribute planes and the sprite walk give each
/// PSX pixel its exact colour and texel; a billboard with sub-pixel corners interpolates them.
const CORNER_SAMPLED: u32 = 1 << 28;
const EXACT: u32 = (1 << 29) | (1 << 30) | (1 << 31);
/// The PSX's 4×4 ordered dither (GP0 E1h bit 9).
const DITHER: u32 = 1 << 27;
/// Textured, and drawn without the colour's tint (`psx_gpu_render::flags`).
const TEXTURED: u32 = 1 << 22;
const RAW_TEXTURE: u32 = 1 << 23;

/// How far a battle's sky strip is carried on each side: its tiles copied this many periods out
/// (LT-149).
const BATTLE_SKY_PERIODS: i32 = 3;

/// A battle's sky in a widened view (LT-149), for a packet's decoded vertices: copies that carry it
/// into the margins, each a set of vertices in the packet's order with the side it's drawn on. The
/// game's own vertices stay as they are, clipped to its 4:3 as the GPU clips them, so the 4:3 picture
/// is the game's; the copies are drawn only in the margins.
/// - **The strip's tiles** repeat every [`crate::takeover::BATTLE_SKY_PERIOD`] pixels: a tile is
///   copied in place and whole periods left and right, on each side it reaches.
/// - **A fill** (one texel row or column of colour) is carried on past each edge with that edge's
///   texel, from a pixel inside it (the margin's own edge cuts it there).
///
/// The copies are new pixels: interpolated at the pixel centres (not the PSX's planes and corner
/// sample), undithered, so they meet the margin's edge and the picture's top whole. `width` is the
/// game's screen (320 or 512).
fn battle_sky_copies(part: crate::takeover::BattleSky, vertices: &[Vertex], ox: i32, width: i32) -> Vec<(Vec<Vertex>, Side)> {
    use crate::takeover::{BattleSky, BATTLE_SKY_PERIOD};
    let Some(first) = vertices.first() else { return Vec::new() };
    let (x0, x1) = vertices.iter().map(|v| v.pos[0] as i32 - ox).fold((i32::MAX, i32::MIN), |(a, b), x| (a.min(x), b.max(x)));
    let copy = |f: &dyn Fn(&Vertex) -> Vertex| {
        vertices
            .iter()
            .map(|v| {
                let mut c = f(v);
                c.flags &= !(EXACT | CORNER_SAMPLED | DITHER);
                c.fpos = [c.pos[0] as f32, c.pos[1] as f32];
                c
            })
            .collect::<Vec<_>>()
    };
    match part {
        BattleSky::Strip => {
            let mut out = Vec::new();
            for m in -BATTLE_SKY_PERIODS..=BATTLE_SKY_PERIODS {
                let s = m * BATTLE_SKY_PERIOD;
                let shifted = || copy(&|v| Vertex { pos: [(v.pos[0] as i32 + s) as i16, v.pos[1]], ..*v });
                if x0 + s < 0 {
                    out.push((shifted(), Side::Left));
                }
                if x1 + s > width {
                    out.push((shifted(), Side::Right));
                }
            }
            out
        }
        BattleSky::Fill => {
            let edge_u = |x: i32| vertices.iter().find(|v| v.pos[0] as i32 - ox == x).map_or(first.uv[0], |v| v.uv[0]);
            let (ul, ur) = (edge_u(x0), edge_u(x1));
            // `from`'s corners go `out` past the edge, the others to a pixel inside it.
            let to = |v: &Vertex, from: i32, inside: i32, out: i32, u: u16| {
                let x = if v.pos[0] as i32 - ox == from { out } else { inside };
                Vertex { pos: [(x + ox) as i16, v.pos[1]], uv: [u, v.uv[1]], ..*v }
            };
            let reach = SKY_REACH as i32;
            vec![(copy(&|v| to(v, x0, x0 + 1, x0 - reach, ul)), Side::Left), (copy(&|v| to(v, x1, x1 - 1, x1 + reach, ur)), Side::Right)]
        }
    }
}

/// How far a frozen frame's quad is carried past the 4:3 edge it reaches (pixels): wider than any
/// margin (the margin's own columns cut it).
const FROZEN_REACH: i32 = 1024;

/// A frozen frame's quad in a widened view (LT-140), for its decoded vertices: a copy into the
/// margin on each side it reaches (its left edge at the screen's left, its right at `width`). A
/// copy spans the margin from the quad's edge, in the quad's lines and colour; the shader draws it
/// from the native picture by position, so its texture coordinates don't matter. Drawn at the
/// pixel centres, undithered, like the quad's own pixels in the native picture.
fn frozen_copies(vertices: &[Vertex], ox: i32, width: i32) -> Vec<(Vec<Vertex>, Side)> {
    if vertices.is_empty() {
        return Vec::new();
    }
    let (x0, x1) = vertices.iter().map(|v| v.pos[0] as i32 - ox).fold((i32::MAX, i32::MIN), |(a, b), x| (a.min(x), b.max(x)));
    // `edge`'s corners stay; the others go `to`.
    let copy = |edge: i32, to: i32| {
        vertices
            .iter()
            .map(|v| {
                let x = if v.pos[0] as i32 - ox == edge { edge } else { to } + ox;
                let mut c = Vertex { pos: [x as i16, v.pos[1]], frozen: FROZEN_COPY, ..*v };
                c.flags &= !(EXACT | CORNER_SAMPLED | DITHER);
                c.fpos = [c.pos[0] as f32, c.pos[1] as f32];
                c
            })
            .collect::<Vec<_>>()
    };
    let mut out = Vec::new();
    if x0 <= 0 {
        out.push((copy(x0, x0 - FROZEN_REACH), Side::Left));
    }
    if x1 >= width {
        out.push((copy(x1, x1 + FROZEN_REACH), Side::Right));
    }
    out
}

/// How many of its own widths a full-screen quad is copied each way (LT-140).
const SCREEN_PERIODS: i32 = 2;

/// A full-screen quad in a widened view (LT-140, LT-150: the fade a chunk load draws over its frozen
/// frame and then over the live field, a 640 × 448 quad the GPU clips to the screen): for its decoded
/// vertices, when they make an axis-aligned rectangle covering the screen (`width` × `height`),
/// copies of it whole widths left and right on each side they reach, drawn in the margins. Only a
/// quad of one colour ([`one_colour`]) is carried, so the copies carry it on unchanged; they're drawn
/// at the pixel centres, undithered.
fn screen_copies(vertices: &[Vertex], ox: i32, oy: i32, width: i32, height: i32) -> Vec<(Vec<Vertex>, Side)> {
    if vertices.is_empty() {
        return Vec::new();
    }
    let at = |v: &Vertex| [v.pos[0] as i32 - ox, v.pos[1] as i32 - oy];
    let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
    for v in vertices {
        let p = at(v);
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let corners = vertices.iter().all(|v| {
        let p = at(v);
        (p[0] == lo[0] || p[0] == hi[0]) && (p[1] == lo[1] || p[1] == hi[1])
    });
    if !corners || lo[0] > 0 || lo[1] > 0 || hi[0] < width || hi[1] < height {
        return Vec::new();
    }
    let period = hi[0] - lo[0];
    let mut out = Vec::new();
    for m in -SCREEN_PERIODS..=SCREEN_PERIODS {
        let s = m * period;
        let shifted = || {
            vertices
                .iter()
                .map(|v| {
                    let mut c = Vertex { pos: [(v.pos[0] as i32 + s) as i16, v.pos[1]], ..*v };
                    c.flags &= !(EXACT | CORNER_SAMPLED | DITHER);
                    c.fpos = [c.pos[0] as f32, c.pos[1] as f32];
                    c
                })
                .collect::<Vec<_>>()
        };
        if lo[0] + s < 0 {
            out.push((shifted(), Side::Left));
        }
        if hi[0] + s > width {
            out.push((shifted(), Side::Right));
        }
    }
    out
}

/// Whether a primitive's decoded vertices draw one colour all over (LT-150): one vertex colour (or
/// a raw texture), and, textured, every texel its texture coordinates span alike in `vram` (the
/// table's), through its CLUT and texture window as the GPU samples them. A fade or a tint is (the
/// chunk load's samples an 8 × 4 swatch of one grey); a picture isn't, and copies of it whole widths
/// into the margins would repeat it.
fn one_colour(vertices: &[Vertex], vram: &[u16]) -> bool {
    let Some(first) = vertices.first() else { return false };
    let f = first.flags;
    let textured = f & TEXTURED != 0;
    if !(textured && f & RAW_TEXTURE != 0) && vertices.iter().any(|v| v.color[..3] != first.color[..3]) {
        return false;
    }
    if !textured {
        return true;
    }
    if vram.len() < 1024 * 512 {
        return false;
    }
    let (tx, ty, depth) = ((f & 0xF) * 64, ((f >> 4) & 1) * 256, (f >> 5) & 3);
    let (cx, cy) = (((f >> 7) & 0x3F) * 16, (f >> 13) & 0x1FF);
    let w = first.tex_window;
    let (mx, my, wx, wy) = (w & 0xFF, (w >> 8) & 0xFF, (w >> 16) & 0xFF, (w >> 24) & 0xFF);
    let span = |k: usize| vertices.iter().map(|v| u32::from(v.uv[k]).min(255)).fold((u32::MAX, 0), |(a, b), x| (a.min(x), b.max(x)));
    let ((u0, u1), (v0, v1)) = (span(0), span(1));
    let texel = |u: u32, v: u32| -> u16 {
        let (u, v) = ((u & !mx & 0xFF) | (wx & mx), (v & !my & 0xFF) | (wy & my));
        let row = ((ty + v) & 511) as usize * 1024;
        let at = |x: u32| vram[row + ((tx + x) & 1023) as usize];
        let clut = |i: u16| vram[cy as usize * 1024 + ((cx + u32::from(i)) & 1023) as usize];
        match depth {
            0 => clut((at(u / 4) >> ((u % 4) * 4)) & 0xF),
            1 => clut((at(u / 2) >> ((u % 2) * 8)) & 0xFF),
            _ => at(u),
        }
    };
    let t0 = texel(u0, v0);
    (v0..=v1).all(|v| (u0..=u1).all(|u| texel(u, v) == t0))
}

/// A GP0 position's 11-bit coordinate (the low bits of `w`), as the GPU sign-extends it.
fn sext(w: u32) -> i32 {
    ((w as i32) << 21) >> 21
}

/// Decodes `table` at internal scale `scale`: every packet goes through the decoder in the GPU's
/// order, so its state (texture page, blend mode, draw area) is the GPU's; only the packets not set
/// aside give vertices.
pub fn layer(table: &Table, scale: u32) -> Layer {
    layer_with(table, scale, Options::default())
}

/// [`layer`], drawn as `opts` say.
pub fn layer_with(table: &Table, scale: u32, opts: Options) -> Layer {
    layer_from(table, &table.billboards, scale, opts)
}

/// [`layer_with`], with the billboards' corners from `projections`: the table's own moved for a
/// frame between ticks (LT-145, [`crate::billboard::between`]).
pub fn layer_from(table: &Table, projections: &[crate::billboard::Projection], scale: u32, opts: Options) -> Layer {
    let mut t = Translator::new();
    t.restore_environment(table.env);
    t.set_internal_scale(scale);
    // The packets put back (trail sparkles, rebuilt smoke) go through a decoder of their own: the
    // GPU never saw them, so they mustn't change its state for the packets after.
    let mut back = Translator::new();
    back.restore_environment(table.env);
    back.set_internal_scale(scale);
    let billboards = (opts.float || opts.wide).then(|| crate::billboard::Lookup::new(projections)).filter(|l| !l.is_empty());
    let restored: &[(usize, Linked)] = if opts.wide { &table.restored } else { &[] };
    // A battle's sky in a widened view (LT-149), and the game's screen width it's carried past.
    let battle_sky = |p: &Linked| (opts.wide && table.battle && !p.aside).then(|| crate::takeover::battle_sky(p.addr)).flatten();
    let width = table.display[2].max(1) as i32;
    let height = table.display[3].max(1) as i32;
    // A frozen frame's quads (LT-140).
    let frozen = |p: &Linked| !p.aside && table.frozen_quads.contains(&p.addr);
    let mut out = Layer::default();
    let mut index = 0u32;
    let mut next = 0usize;
    let mut packets = table.packets.iter().enumerate().peekable();
    loop {
        // The next packet: one put back before packet i, or packet i.
        let (p, put_back) = match (restored.get(next), packets.peek()) {
            (Some((before, r)), Some((i, _))) if before <= i => {
                next += 1;
                (r, true)
            }
            (Some((_, r)), None) => {
                next += 1;
                (r, true)
            }
            (_, Some(_)) => (packets.next().unwrap().1, false),
            (None, None) => break,
        };
        let dec = if put_back { &mut back } else { &mut t };
        let bucket = p.bucket as f32;
        let sky = opts.wide && table.sky && crate::takeover::SKY_GRADIENT.contains(&p.addr);
        let mut at = 0usize;
        while at < p.words.len() {
            let opcode = (p.words[at] >> 24) as u8;
            let n = gp0_command_word_count(opcode).max(1).min(p.words.len() - at);
            let entry = GpuCmdLogEntry { index, opcode, fifo: GpuCmdFifo::from_slice(&p.words[at..at + n]) };
            index += 1;
            let words = &p.words[at..at + n];
            let command = at;
            at += n;
            out.commands += 1;
            // The draw offset moves a screen position to VRAM (a polygon command doesn't change it).
            let (.., ox, oy) = dec.debug_env();
            let frame = dec.translate(std::slice::from_ref(&entry));
            if p.aside || (opts.no_blob_shadows && p.shadow) {
                continue;
            }
            if frame.vertices.is_empty() {
                out.silent += 1;
                continue;
            }
            let corners = billboards.as_ref().and_then(|b| b.command(p, command, words));
            let base = out.vertices.len() as u32;
            for v in frame.vertices {
                let mut o = Vertex { pos: v.pos, color: v.color, uv: v.uv, flags: v.flags, tex_window: v.tex_window, planes: v.planes, bucket, fpos: [v.pos[0] as f32, v.pos[1] as f32], frozen: if frozen(p) { FROZEN } else { 0 } };
                if let (Some((_, c)), true) = (&corners, opts.float) {
                    let screen = [o.pos[0] as i32 - ox, o.pos[1] as i32 - oy];
                    if let Some((_, f)) = c.iter().find(|(w, _)| [sext(*w), sext(*w >> 16)] == screen) {
                        o.fpos = [f[0] + ox as f32, f[1] + oy as f32];
                    }
                    o.flags = (o.flags & !EXACT) | CORNER_SAMPLED;
                }
                if sky {
                    o.pos[0] += if o.pos[0] as i32 - ox <= 0 { -SKY_REACH } else { SKY_REACH };
                    o.fpos[0] = o.pos[0] as f32;
                    o.flags = (o.flags & !EXACT) | CORNER_SAMPLED;
                }
                out.vertices.push(o);
            }
            let wide = (corners.is_some() || sky) && opts.wide;
            // A battle's sky carried on into the margins (LT-149), drawn as the packet is.
            if let Some(part) = battle_sky(p) {
                for (c, side) in battle_sky_copies(part, &out.vertices[base as usize..], ox, width) {
                    let at = out.vertices.len() as u32;
                    out.vertices.extend(c);
                    out.runs.extend(frame.runs.iter().map(|r| Run { blend: r.kind.index(), first: at + r.start, count: r.count, clip: r.clip, wide: false, margin: Some(side) }));
                }
                out.battle_sky += 1;
            }
            // A full-screen quad of one colour (a fade, a tint: LT-140, LT-150), over a frozen frame
            // or the live field, carried on into the margins. The packets with margin copies of their
            // own (a frozen frame's quads, a battle's sky, the first flight's sky, billboards) aren't.
            if opts.wide && !opts.fades_4_3 && !frozen(p) && !sky && corners.is_none() && battle_sky(p).is_none() {
                let copies = screen_copies(&out.vertices[base as usize..], ox, oy, width, height);
                if !copies.is_empty() && one_colour(&out.vertices[base as usize..], &table.vram) {
                    for (c, side) in copies {
                        let at = out.vertices.len() as u32;
                        out.vertices.extend(c);
                        out.runs.extend(frame.runs.iter().map(|r| Run { blend: r.kind.index(), first: at + r.start, count: r.count, clip: r.clip, wide: false, margin: Some(side) }));
                        out.screen_copies += 1;
                    }
                    if !out.screened.contains(&p.addr) {
                        out.screened.push(p.addr);
                    }
                }
            }
            // A frozen frame carried on into the margins (LT-140), drawn as its quad is.
            if frozen(p) {
                if opts.wide {
                    for (c, side) in frozen_copies(&out.vertices[base as usize..], ox, width) {
                        let at = out.vertices.len() as u32;
                        out.vertices.extend(c);
                        out.runs.extend(frame.runs.iter().map(|r| Run { blend: r.kind.index(), first: at + r.start, count: r.count, clip: r.clip, wide: false, margin: Some(side) }));
                        out.frozen_copies += 1;
                    }
                }
                out.frozen += 1;
            }
            out.billboards += corners.is_some() as u32;
            out.restored += (put_back && corners.is_some()) as u32;
            out.blob_shadows += p.shadow as u32;
            if let Some((caller, c)) = &corners {
                let at = |w: u32, f: [f32; 2]| if opts.float { [f[0] + ox as f32, f[1] + oy as f32] } else { [(sext(w) + ox) as f32, (sext(w >> 16) + oy) as f32] };
                out.quads.push(Quad { caller, corners: c.iter().map(|&(w, f)| at(w, f)).collect(), restored: put_back });
            }
            out.runs.extend(frame.runs.iter().map(|r| Run { blend: r.kind.index(), first: base + r.start, count: r.count, clip: r.clip, wide, margin: None }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_rectangle_decodes_with_its_bucket() {
        // GP0(60h): a monochrome rectangle, colour 0x0000FF, at (10, 20), 8 × 4.
        let table = Table { packets: vec![Linked { addr: 0x8010_0000, bucket: 100, words: vec![0x6000_00FF, (20 << 16) | 10, (4 << 16) | 8], aside: false, shadow: false }], env: [0xE100_0000, 0xE200_0000, 0xE300_0000, 0xE407_FFFF | (511 << 10), 0xE500_0000, 0xE600_0000], ..Table::default() };
        let l = layer(&table, 1);
        assert_eq!(l.commands, 1);
        assert!(!l.vertices.is_empty() && l.vertices.len().is_multiple_of(3));
        assert!(l.vertices.iter().all(|v| v.bucket == 100.0));
        let xs: Vec<i16> = l.vertices.iter().map(|v| v.pos[0]).collect();
        assert_eq!((xs.iter().min(), xs.iter().max()), (Some(&10), Some(&18)));
        assert_eq!(l.runs.iter().map(|r| r.count).sum::<u32>() as usize, l.vertices.len());
        let mut b = Vec::new();
        l.vertices[0].write(&mut b);
        assert_eq!(b.len(), Vertex::BYTES);
    }

    #[test]
    fn a_battle_sky_carries_on_into_the_margins() {
        use crate::takeover::BattleSky;
        let v = |x: i16, y: i16, u: u16| Vertex { pos: [x, y], uv: [u, 0x50], flags: EXACT | DITHER, fpos: [x as f32, y as f32], ..Vertex::default() };
        // A strip tile from x = -8 to 120 (the draw offset 0), two triangles.
        let tile = [v(-8, 0, 0), v(120, 0, 128), v(-8, 80, 0), v(120, 0, 128), v(120, 80, 128), v(-8, 80, 0)];
        let copies = battle_sky_copies(BattleSky::Strip, &tile, 0, 320);
        let sides: Vec<(i16, Side)> = copies.iter().map(|(c, s)| (c[0].pos[0], *s)).collect();
        // In place and a period left on the left (it reaches past 0), a period right and on on the right.
        assert_eq!(sides, vec![(-1160, Side::Left), (-776, Side::Left), (-392, Side::Left), (-8, Side::Left), (376, Side::Right), (760, Side::Right), (1144, Side::Right)]);
        assert!(copies.iter().all(|(c, _)| c.iter().all(|v| v.flags & (EXACT | DITHER | CORNER_SAMPLED) == 0)), "new pixels: interpolated, undithered");
        assert!(copies.iter().all(|(c, _)| c.iter().zip(&tile).all(|(a, b)| a.uv == b.uv)), "the texture as the tile's");
        // A fill from 0 to 320, its texels 0 to 7 across: each edge's texel carried past it.
        let fill = [v(0, 0, 0), v(320, 0, 7), v(0, 40, 0), v(320, 0, 7), v(320, 40, 7), v(0, 40, 0)];
        let f = battle_sky_copies(BattleSky::Fill, &fill, 0, 320);
        assert_eq!(f.len(), 2);
        let span = |c: &[Vertex]| c.iter().fold((i16::MAX, i16::MIN), |(a, b), v| (a.min(v.pos[0]), b.max(v.pos[0])));
        assert_eq!((span(&f[0].0), f[0].1), ((-SKY_REACH, 1), Side::Left));
        assert_eq!((span(&f[1].0), f[1].1), ((319, 320 + SKY_REACH), Side::Right));
        assert!(f[0].0.iter().all(|v| v.uv[0] == 0) && f[1].0.iter().all(|v| v.uv[0] == 7));
    }

    #[test]
    fn a_frozen_frame_carries_on_into_the_margins() {
        // The menu's two quads (`FUN_8018afa0`), as the game links them, and a chunk load's fade (a
        // 640 × 448 raw, subtracting POLY_FT4); the draw area 320 × 224 at the origin.
        let left = vec![0x2C3C_3C3C, 0x0000_0000, 0x0000_0000, 0x0000_00A0, 0x0105_00A0, 0x00E0_0000, 0x0000_E000, 0x00E0_00A0, 0x0000_E0A0];
        let right = vec![0x2C3C_3C3C, 0x0000_00A0, 0x0000_0000, 0x0000_0140, 0x0115_00A0, 0x00E0_00A0, 0x0000_E000, 0x00E0_0140, 0x0000_E0A0];
        let fade = vec![0x2F00_0000, 0x0000_0000, 0x7EC0_D0C0, 0x0000_0280, 0x004E_D0C7, 0x01C0_0000, 0x0000_D3C0, 0x01C0_0280, 0x0000_D3C7];
        let env = [0xE100_0000, 0xE200_0000, 0xE300_0000, 0xE400_0000 | (223 << 10) | 319, 0xE500_0000, 0xE600_0000];
        let (a, b) = (0x801C_7E60u32, 0x801C_7E88u32);
        let linked = |addr: u32, words: &Vec<u32>, bucket: u32| Linked { addr, bucket, words: words.clone(), aside: false, shadow: false };
        // The fade's texture (4-bit, page (896, 0), CLUT (0, 507)): texel 0 everywhere, one grey.
        let mut vram = vec![0u16; 1024 * 512];
        vram[507 * 1024] = 0x2108;
        let table = Table { packets: vec![linked(a, &left, 2), linked(b, &right, 2), linked(0x801C_CF88, &fade, 0)], env, display: [0, 0, 320, 224], frozen_quads: vec![a, b], vram, ..Table::default() };
        let wide = layer_with(&table, 2, Options { wide: true, ..Options::default() });
        assert_eq!((wide.frozen, wide.frozen_copies), (2, 2));
        // The game's quads are marked and stay 4:3; each copy spans its margin, from the quad's edge.
        let marked = wide.vertices.iter().filter(|v| v.frozen == FROZEN).count();
        assert!(marked >= 12 && wide.runs.iter().filter(|r| r.margin.is_none()).all(|r| wide.vertices[r.first as usize..(r.first + r.count) as usize].iter().all(|v| v.frozen != FROZEN_COPY)));
        let span = |side: Side, kind: u32| {
            let vs: Vec<&Vertex> = wide.runs.iter().filter(|r| r.margin == Some(side)).flat_map(|r| &wide.vertices[r.first as usize..(r.first + r.count) as usize]).filter(|v| v.frozen == kind).collect();
            (vs.iter().map(|v| v.pos[0]).min(), vs.iter().map(|v| v.pos[0]).max())
        };
        assert_eq!(span(Side::Left, FROZEN_COPY), (Some(-(FROZEN_REACH as i16)), Some(0)));
        assert_eq!(span(Side::Right, FROZEN_COPY), (Some(320), Some(320 + FROZEN_REACH as i16)));
        // The fade is copied whole widths left and right into both margins, as it is.
        assert!(wide.screen_copies >= 2);
        let fades = |side: Side| wide.runs.iter().filter(|r| r.margin == Some(side)).flat_map(|r| &wide.vertices[r.first as usize..(r.first + r.count) as usize]).filter(|v| v.frozen == 0).count();
        assert!(fades(Side::Left) > 0 && fades(Side::Right) > 0);
        assert_eq!(wide.screened, vec![0x801C_CF88]);
        // At 4:3 nothing is copied, the quads still marked.
        let narrow = layer_with(&table, 2, Options::default());
        assert_eq!((narrow.frozen, narrow.frozen_copies, narrow.screen_copies), (2, 0, 0));
        assert!(narrow.runs.iter().all(|r| r.margin.is_none()));
        // Over the live field (no frozen frame) the fade is carried just the same (LT-150).
        let plain = layer_with(&Table { frozen_quads: Vec::new(), ..table }, 2, Options { wide: true, ..Options::default() });
        assert_eq!((plain.frozen, plain.screen_copies), (0, wide.screen_copies));
        assert!(plain.vertices.iter().all(|v| v.frozen == 0));
    }

    /// LT-150: a full-screen quad is carried into a widened view's margins only when it draws one
    /// colour all over: a fade or a tint, not a picture.
    #[test]
    fn only_a_full_screen_quad_of_one_colour_is_carried() {
        let env = [0xE100_0000, 0xE200_0000, 0xE300_0000, 0xE400_0000 | (223 << 10) | 319, 0xE500_0000, 0xE600_0000];
        let table = |words: Vec<u32>, vram: Vec<u16>| Table { packets: vec![Linked { addr: 0x801C_E670, bucket: 3, words, aside: false, shadow: false }], env, display: [0, 0, 320, 224], vram, ..Table::default() };
        let wide = |t: &Table| layer_with(t, 2, Options { wide: true, ..Options::default() });
        let mut vram = vec![0u16; 1024 * 512];
        vram[507 * 1024] = 0x2108;
        // The battle loading screen's tint: a flat, subtracting quad over the whole screen.
        let tint = vec![0x2A18_180E, 0x0000_0000, 0x0000_0140, 0x00E0_0000, 0x00E0_0140];
        let l = wide(&table(tint.clone(), vram.clone()));
        assert!(l.screen_copies >= 2 && l.screened == vec![0x801C_E670]);
        assert!(l.runs.iter().filter(|r| r.margin.is_some()).all(|r| l.vertices[r.first as usize..(r.first + r.count) as usize].iter().all(|v| v.flags & (EXACT | CORNER_SAMPLED | DITHER) == 0)));
        // Smaller than the screen: not carried.
        let small = vec![0x2A18_180E, 0x0000_0000, 0x0000_0140, 0x00D0_0000, 0x00D0_0140];
        assert_eq!(wide(&table(small, vram.clone())).screen_copies, 0);
        // Shaded across: not one colour.
        let shaded = vec![0x3A00_0000, 0x0000_0000, 0x00FF_FFFF, 0x0000_0140, 0x0000_0000, 0x00E0_0000, 0x00FF_FFFF, 0x00E0_0140];
        assert_eq!(wide(&table(shaded, vram.clone())).screen_copies, 0);
        // The chunk load's fade (a raw 4-bit swatch): carried while its texels are alike, not once
        // one differs (a picture), nor without VRAM to tell.
        let fade = vec![0x2F00_0000, 0x0000_0000, 0x7EC0_D0C0, 0x0000_0280, 0x004E_D0C7, 0x01C0_0000, 0x0000_D3C0, 0x01C0_0280, 0x0000_D3C7];
        assert!(wide(&table(fade.clone(), vram.clone())).screen_copies >= 2);
        let mut picture = vram.clone();
        picture[507 * 1024 + 1] = 0x7FFF;
        picture[(0xD2) * 1024 + 896 + 0xC4 / 4] = 0x0001;
        assert_eq!(wide(&table(fade.clone(), picture)).screen_copies, 0);
        assert_eq!(wide(&table(fade, Vec::new())).screen_copies, 0);
        // At 4:3, nothing.
        assert_eq!(layer_with(&table(tint, vram), 2, Options::default()).screen_copies, 0);
    }

    /// @LT-144/1 (the 2D layer's part): a billboard's corners go sub-pixel above 1× with its texture
    /// and colour interpolated, its runs reach a widened view's margins, and a packet put back is
    /// drawn only in a widened view.
    #[test]
    fn lt144_1_billboards_sub_pixel_and_unclipped_in_a_wide_view() {
        use crate::billboard::{Projection, TRAIL_RETURN};
        use crate::gte::Gte;
        let gte = Gte { rt: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], tr: [0, 0, 1000], h: 892, ofx: 160 << 16, ofy: 112 << 16, exact: None };
        let quad = [[-16, -16, 0], [16, -16, 0], [-16, 16, 0], [16, 16, 0]];
        let (a, b) = (0x801C_E320u32, 0x801C_E348u32);
        let project = |prim: u32, dx: i16| Projection { caller: "FUN_801821ac", ..Projection::of(&gte, quad.map(|v| [v[0] + dx, v[1], v[2]]), [prim + 8, prim + 0x10, prim + 0x18, prim + 0x20], TRAIL_RETURN) };
        let (pa, pb) = (project(a, 0), project(b, -200));
        // A POLY_FT4 (semi-transparent, tinted): command, then xy / uv+CLUT / xy / uv+tpage / xy / uv / xy / uv.
        let ft4 = |p: &Projection| vec![0x2E80_8080, p.corners[0].word, 0x7F83_2020, p.corners[1].word, 0x003F_202F, p.corners[2].word, 0x2F20, p.corners[3].word, 0x2F2F];
        let env = [0xE100_0000, 0xE200_0000, 0xE300_0000, 0xE400_0000 | (239 << 10) | 319, 0xE500_0000, 0xE600_0000];
        let table = Table {
            packets: vec![Linked { addr: a, bucket: 40, words: ft4(&pa), aside: false, shadow: false }],
            restored: vec![(1, Linked { addr: b, bucket: 40, words: ft4(&pb), aside: false, shadow: false })],
            billboards: vec![pa.clone(), pb],
            env,
            ..Table::default()
        };
        // The game's: whole pixels, the PSX's planes, the 4:3 clip, nothing put back.
        let game = layer(&table, 2);
        let n = game.vertices.len();
        assert_eq!(game.billboards, 0);
        assert!(n >= 6);
        assert!(game.vertices.iter().all(|v| v.fpos[0].fract() == 0.0 && v.fpos[1].fract() == 0.0));
        // Above 1× in a widened view: the same quad at its floating-point corners.
        let l = layer_with(&table, 2, Options { float: true, wide: true, ..Options::default() });
        assert_eq!((l.billboards, l.restored), (2, 1));
        assert_eq!(l.vertices.len(), 2 * n, "the quad and the one put back");
        let top_right = l.vertices.iter().map(|v| v.fpos).fold([f32::MIN, f32::MAX], |m, p| [m[0].max(p[0]), m[1].min(p[1])]);
        assert!((top_right[0] - pa.corners[1].float[0]).abs() < 1e-4 && (top_right[1] - pa.corners[1].float[1]).abs() < 1e-4, "{top_right:?}");
        assert!(top_right[0].fract() != 0.0, "sub-pixel");
        assert!(l.vertices.iter().all(|v| v.flags & EXACT == 0 && v.flags & CORNER_SAMPLED != 0));
        assert!(l.runs.iter().all(|r| r.wide));
        assert!(l.quads.iter().any(|q| q.restored && q.corners.iter().all(|c| c[0] < 0.0)), "the one put back, left of the 4:3");
        // Above 1× at 4:3: sub-pixel, but clipped as the game clips, and nothing put back.
        let narrow = layer_with(&table, 2, Options { float: true, ..Options::default() });
        assert_eq!((narrow.billboards, narrow.restored), (1, 0));
        assert!(narrow.runs.iter().all(|r| !r.wide));
    }
}
