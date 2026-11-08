//! Widescreen and a longer draw distance (LT-114, research 16 §4.2).
//!
//! The game draws for its 4:3 screen in three places, and the native view widens each:
//! - **Objects.** `WorldChunk_DrawObjects` turns each map object's culling centre through the
//!   chunk's model-view matrix (MVMVA) and skips it unless `|x| ≤ 0x800` and `-0x200 ≤ z ≤ 0x1000`
//!   in view space. [`object_in`] is that test, exact at the game's reach. Wider, the x limit grows
//!   by the extra screen at the object's depth (`left · z / H`), so an object just past the new edge
//!   has the same slack the game gives one past its own; a longer draw distance scales the z limit.
//! - **Chunks.** The game holds four chunk slots: the current chunk and the neighbours ahead of the
//!   camera. On the overworld grid a wider view also needs the chunks beside those, and a longer
//!   one the rows beyond. They're read off the disc ([`load`], as `remaster_preview.rs` does) and
//!   placed as `Field_ChunkStreamTask` places a neighbour: the view, moved to the chunk's grid
//!   offset. Interiors, dungeons and caves are one chunk each and need none.
//! - **Polygons.** The terrain path's off-screen test takes the wider screen, and the depth test
//!   `crate::gte::FAR` × the distance ([`Reach::sight`]).
//!
//! The 2D layer stays the game's 4:3, in the middle (`crate::composite::Compositor::render`).
//!
//! **Characters past the 4:3 edge stay a gap** (LT-119). `Npc_DrawIfVisible` gives an NPC a packet
//! slot only when its position projects within `-64..=576` (the 512 mode; `-60..=380` at 320) and
//! one of five slots is free; an NPC without one has no posed mesh to draw. [`npc_gap`] counts the
//! NPCs standing in the margins that way.

use std::borrow::Cow;

use lt_assets::{Assets, ChunkUse, Frame, MeshKey};
use lt_core::{overworld, world};
use lt_scene::{Matrix, Record};

use crate::draw::Sight;
use crate::gte::FAR;

/// The widest picture and the longest distance the native view goes to.
pub const MAX_ASPECT: f64 = 32.0 / 9.0;
pub const MAX_DISTANCE: f64 = 4.0;
/// `WorldChunk_DrawObjects`' object test, in view space.
pub const OBJECT_X: i64 = 0x800;
pub const OBJECT_NEAR: i64 = -0x200;
pub const OBJECT_FAR: i64 = 0x1000;
/// A grid chunk's half size: its corners are within ±2048 of its origin (x and z), its heights
/// within about -704..12 (Y down).
const HALF: i32 = 2048;
const HEIGHTS: [i32; 2] = [-768, 64];

/// How much more than the game the native view shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reach {
    /// The picture's width over its height: 4:3 is the game's.
    pub aspect: f64,
    /// The draw distance: 1 is the game's (objects to 0x1000 deep, polygons to 0x3000).
    pub distance: f64,
}

impl Default for Reach {
    fn default() -> Self {
        Reach::GAME
    }
}

impl Reach {
    pub const GAME: Reach = Reach { aspect: 4.0 / 3.0, distance: 1.0 };

    /// Clamped to 4:3 ..= [`MAX_ASPECT`] and 1 ..= [`MAX_DISTANCE`].
    pub fn new(aspect: f64, distance: f64) -> Reach {
        let a = if aspect.is_finite() { aspect } else { 4.0 / 3.0 };
        let d = if distance.is_finite() { distance } else { 1.0 };
        Reach { aspect: a.clamp(4.0 / 3.0, MAX_ASPECT), distance: d.clamp(1.0, MAX_DISTANCE) }
    }

    /// `16:9`, `21:9`, `1.6`… as an aspect.
    pub fn parse_aspect(s: &str) -> Option<f64> {
        match s.split_once(':') {
            Some((w, h)) => Some(w.trim().parse::<f64>().ok()? / h.trim().parse::<f64>().ok()?),
            None => s.trim().parse().ok(),
        }
        .filter(|a| a.is_finite() && *a > 0.0)
    }

    /// Whole pixels added each side of a `width`-wide screen (320 or 512 × 224, shown 4:3) for it to
    /// show this aspect.
    pub fn margin(&self, width: u32) -> u32 {
        (width as f64 * (self.aspect * 0.75 - 1.0) / 2.0).round().max(0.0) as u32
    }

    /// The draw paths' bounds for a `width`-wide screen.
    pub fn sight(&self, width: u32) -> Sight {
        Sight { left: self.margin(width), far: (FAR as f64 * self.distance).round() as u32 }
    }

    /// Nothing past the game's screen.
    pub fn is_game(&self) -> bool {
        self.margin(512) == 0 && self.distance <= 1.0
    }

    fn object_far(&self) -> i64 {
        (OBJECT_FAR as f64 * self.distance).round() as i64
    }
}

/// Whether `WorldChunk_DrawObjects` draws an object whose culling centre is `c` (model space)
/// through `mv`: the game's test with the screen `left` pixels wider each side and the far limit at
/// `far`. `h` is the projection distance. With `left` 0 and `far` [`OBJECT_FAR`] it's the game's.
///
/// Past the game's far limit an object must also be within the screen's height (with the same
/// 0x800 of slack): the field camera looks down, so most of a longer distance is above the top edge.
pub fn object_in(mv: &Matrix, c: [i32; 3], h: u16, left: u32, far: i64) -> bool {
    let p = mv.place(c);
    // MVMVA saturates IR1..3 (a flag the game skips on) outside 16 bits.
    if p.iter().any(|v| !(-0x8000..=0x7FFF).contains(v)) {
        return false;
    }
    let (x, y, z) = (p[0] as i64, p[1] as i64, p[2] as i64);
    let h = h.max(1) as i64;
    let reach_x = OBJECT_X + left as i64 * z.max(0) / h;
    let upright = z <= OBJECT_FAR || y.abs() <= OBJECT_X + 112 * z / h;
    (-reach_x..=reach_x).contains(&x) && (OBJECT_NEAR..=far).contains(&z) && upright
}

/// The chunk the tick's world is centred on: the one drawn at offset 0 (the capture's current chunk
/// can lag a tick behind a seam), else the current one.
pub fn home(r: &Record) -> u16 {
    r.chunks.iter().find(|c| c.offset == [0, 0, 0]).map_or(r.chunk, |c| c.chunk)
}

/// The home chunk's model-view (the view itself) and the screen width, for a tick with a camera.
fn base(r: &Record) -> Option<(Matrix, u16, u32)> {
    let cam = r.camera.as_ref()?;
    let h = home(r);
    let mv = r.chunks.iter().find(|c| c.chunk == h && c.offset == [0, 0, 0]).map_or(cam.view, |c| c.model_view);
    Some((mv, cam.h, if cam.wide { 512 } else { 320 }))
}

/// A neighbour's model-view: the home chunk's moved to `offset`, as `Field_ChunkStreamTask` places
/// one.
fn placed(home: &Matrix, offset: [i32; 3]) -> Matrix {
    Matrix { m: home.m, t: home.place(offset) }
}

/// Grid chunks (with their offsets from home) the game didn't draw that `reach` could show: within
/// a few rows of home, any with a point of its footprint passing [`object_in`] with some slack. None
/// off the grid.
pub fn neighbours(r: &Record, reach: Reach) -> Vec<(u16, [i32; 3])> {
    let Some((mv, h, width)) = base(r) else { return Vec::new() };
    let home = home(r) as u32;
    let (Some((gx, gy)), Some((hx, hz))) = (world::grid_cell(home), overworld::origin(home)) else { return Vec::new() };
    let left = reach.margin(width);
    let far = reach.object_far() + 1024;
    let rows = reach.distance.ceil() as i32 + 1;
    let mut out = Vec::new();
    for dy in -rows..=rows {
        for dx in -(2 * rows + 3)..=(2 * rows + 3) {
            let (x, y) = (gx as i32 + dx, gy as i32 + dy);
            let Some(id) = (x >= 0 && y >= 0).then(|| world::chunk_at(x as u32, y as u32)).flatten() else { continue };
            if r.chunks.iter().any(|c| c.chunk as u32 == id) {
                continue;
            }
            let Some((ox, oz)) = overworld::origin(id) else { continue };
            let offset = [ox - hx, 0, oz - hz];
            let m = placed(&mv, offset);
            // The footprint sampled every 512 units at the top and bottom of its heights, with 512
            // of slack across.
            let seen = (-HALF..=HALF).step_by(512).any(|lx| {
                (-HALF..=HALF).step_by(512).any(|lz| HEIGHTS.iter().any(|&ly| object_in(&m, [lx, ly, lz], h, left + 64, far)))
            });
            if seen {
                out.push((id as u16, offset));
            }
        }
    }
    out
}

/// Reads the chunks `reach` needs that the game didn't draw (once each; [`frame`] draws them).
pub fn load(r: &Record, assets: &mut Assets, reach: Reach) {
    if reach.is_game() {
        return;
    }
    for (id, _) in neighbours(r, reach) {
        assets.load_chunk(id);
    }
}

/// The tick's frame at `reach`: the game's chunks with every object the wider test takes (after
/// the game's own, in its order), the grid neighbours [`load`] read with theirs, and the NPCs
/// without a slot `crowd` posed (LT-119, `crate::crowd`). The game's frame as it is at its own
/// reach.
pub fn frame<'a>(r: &Record, f: &'a Frame, crowd: &[lt_assets::CharacterUse], assets: &Assets, reach: Reach) -> Cow<'a, Frame> {
    let Some((home_mv, h, width)) = base(r).filter(|_| !reach.is_game()) else { return Cow::Borrowed(f) };
    let (left, far) = (reach.margin(width), reach.object_far());
    let mut out = f.clone();
    out.characters.extend(crowd.iter().filter(|c| !f.characters.iter().any(|d| d.index == c.index)).cloned());
    let taken = |mv: &Matrix, centres: &[(u32, [i16; 3])], skip: &[u32]| -> Vec<u32> {
        centres.iter().filter(|(i, c)| !skip.contains(i) && object_in(mv, c.map(|v| v as i32), h, left, far)).map(|&(i, _)| i).collect()
    };
    for c in &mut out.chunks {
        if let Some(a) = assets.chunk(c.chunk) {
            let more = taken(&c.model_view, &a.centres, &c.groups);
            c.groups.extend(more);
        }
    }
    for (id, offset) in neighbours(r, reach) {
        let Some(a) = assets.chunk(id) else { continue };
        let model_view = placed(&home_mv, offset);
        let groups = taken(&model_view, &a.centres, &[]);
        if groups.is_empty() {
            continue;
        }
        out.chunks.push(ChunkUse { chunk: id, mesh: MeshKey::Chunk(a.id, a.file), texture: a.texture, slot: 0xFF, offset, model_view, groups });
    }
    Cow::Owned(out)
}

/// A check's reference (`crate::check::widened`): every object of the game's chunks and of every
/// grid chunk within `rows` rows of home (read here), culled by nothing but the draw paths: what
/// there is to see.
pub fn everything(r: &Record, f: &Frame, assets: &mut Assets, rows: i32) -> Frame {
    let mut out = f.clone();
    let Some((home_mv, _, _)) = base(r) else { return out };
    for c in &mut out.chunks {
        if let Some(a) = assets.chunk(c.chunk) {
            for &(i, _) in &a.centres {
                if !c.groups.contains(&i) {
                    c.groups.push(i);
                }
            }
        }
    }
    let home = home(r) as u32;
    let (Some((gx, gy)), Some((hx, hz))) = (world::grid_cell(home), overworld::origin(home)) else { return out };
    for dy in -rows..=rows {
        for dx in -(2 * rows + 3)..=(2 * rows + 3) {
            let (x, y) = (gx as i32 + dx, gy as i32 + dy);
            let Some(id) = (x >= 0 && y >= 0).then(|| world::chunk_at(x as u32, y as u32)).flatten() else { continue };
            if out.chunks.iter().any(|c| c.chunk as u32 == id) {
                continue;
            }
            let (Some((ox, oz)), Some((mesh, texture))) = (overworld::origin(id), assets.load_chunk(id as u16)) else { continue };
            let offset = [ox - hx, 0, oz - hz];
            let groups = assets.chunk(id as u16).map(|a| a.centres.iter().map(|&(i, _)| i).collect()).unwrap_or_default();
            out.chunks.push(ChunkUse { chunk: id as u16, mesh, texture, slot: 0xFF, offset, model_view: placed(&home_mv, offset), groups });
        }
    }
    out
}

/// Characters a wider view shows that the game doesn't draw, on one tick: NPCs and doors (the
/// overworld's wanderers aren't counted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NpcGap {
    /// Characters the game drew (not the party).
    pub drawn: u32,
    /// Characters `crate::crowd` drew: all of them, and those in the margins.
    pub crowd: u32,
    pub crowd_margins: u32,
    /// Placed characters without a slot that nothing draws, though they're in view: in the margins
    /// (past the game's slot bounds, within the wider screen's), and inside the game's bounds (its
    /// slots all taken). Chests (kind 3) are counted here; the crowd leaves them to the game.
    pub in_margins: u32,
    pub unslotted_in_view: u32,
    /// Drawn characters that `crate::crowd::Actor::drawable` calls undrawable: 0 when it reads the
    /// records right.
    pub misread: u32,
}

impl std::ops::Add for NpcGap {
    type Output = NpcGap;

    fn add(self, b: NpcGap) -> NpcGap {
        NpcGap {
            drawn: self.drawn + b.drawn,
            crowd: self.crowd + b.crowd,
            crowd_margins: self.crowd_margins + b.crowd_margins,
            in_margins: self.in_margins + b.in_margins,
            unslotted_in_view: self.unslotted_in_view + b.unslotted_in_view,
            misread: self.misread + b.misread,
        }
    }
}

/// Counts [`NpcGap`] for a tick from its character records (`Record::actors`), through its
/// camera, at `reach`: `crowd` is what the native view draws besides the game's characters.
pub fn npc_gap(r: &Record, crowd: &[lt_assets::CharacterUse], reach: Reach) -> NpcGap {
    use crate::crowd::npcs;
    let mut g = NpcGap::default();
    let Some(cam) = r.camera.as_ref() else { return g };
    let wide = cam.wide;
    let (x0, x1) = if wide { (-64, 576) } else { (-60, 380) };
    let left = reach.margin(if wide { 512 } else { 320 }) as i64;
    let home = home(r);
    for (i, a) in npcs(r) {
        let drawn = r.characters.iter().any(|d| d.index as usize == i);
        if drawn {
            g.drawn += 1;
            g.misread += u32::from(!a.drawable(home));
            continue;
        }
        if !a.drawable(home) || a.slotted() || r.npc_flags & 4 == 0 {
            continue;
        }
        // Npc_DrawIfVisible's RotTransPers of the position through the view.
        let p = cam.view.place(a.position());
        if p[2] <= 0 {
            continue;
        }
        let sx = cam.offset[0] as i64 + cam.h as i64 * p[0] as i64 / p[2] as i64;
        let sy = cam.offset[1] as i64 + cam.h as i64 * p[1] as i64 / p[2] as i64;
        if !(-40..=300).contains(&sy) {
            continue;
        }
        let inside = (x0..=x1).contains(&sx);
        let margin = !inside && (x0 - left..=x1 + left).contains(&sx);
        let by_crowd = crowd.iter().any(|c| c.index as usize == i);
        g.crowd += u32::from(by_crowd);
        g.crowd_margins += u32::from(by_crowd && margin);
        g.in_margins += u32::from(!by_crowd && margin);
        g.unslotted_in_view += u32::from(!by_crowd && inside);
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn margins_give_the_aspect() {
        // 512 × 224 shown 4:3: 16:9 adds a sixth each side, 21:9 three eighths.
        assert_eq!(Reach::new(16.0 / 9.0, 1.0).margin(512), 85);
        assert_eq!(Reach::new(21.0 / 9.0, 1.0).margin(512), 192);
        assert_eq!(Reach::new(16.0 / 10.0, 1.0).margin(320), 32);
        assert_eq!(Reach::GAME.margin(512), 0);
        assert_eq!(Reach::new(1.0, 0.5), Reach::GAME, "narrower and nearer clamp to the game's");
        assert_eq!(Reach::parse_aspect("21:9"), Some(21.0 / 9.0));
        assert_eq!(Reach::parse_aspect("1.6"), Some(1.6));
        assert!(Reach::GAME.is_game() && !Reach::new(16.0 / 9.0, 1.0).is_game() && !Reach::new(4.0 / 3.0, 2.0).is_game());
    }

    #[test]
    fn the_object_test_is_the_games_at_its_reach() {
        let id = Matrix { m: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], t: [0, 0, 0] };
        assert!(object_in(&id, [0x800, 0, 0x1000], 892, 0, OBJECT_FAR));
        assert!(!object_in(&id, [0x801, 0, 0x800], 892, 0, OBJECT_FAR));
        assert!(!object_in(&id, [0, 0, 0x1001], 892, 0, OBJECT_FAR));
        assert!(object_in(&id, [0, 0, -0x200], 892, 0, OBJECT_FAR) && !object_in(&id, [0, 0, -0x201], 892, 0, OBJECT_FAR));
        // Wider: at 892 deep, 100 more pixels a side reach 100 units further.
        assert!(object_in(&id, [0x800 + 100, 0, 892], 892, 100, OBJECT_FAR) && !object_in(&id, [0x800 + 101, 0, 892], 892, 100, OBJECT_FAR));
        // A longer distance takes deeper objects.
        assert!(object_in(&id, [0, 0, 0x2000], 892, 0, 0x2000));
    }
}
