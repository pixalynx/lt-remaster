//! The inside of a house workbench design (screen 23 §5.4, research 13):
//! rooms per storey from inner walls, their finishes and tints, furniture
//! with actions, stairs between storeys and an optional cellar.
//!
//! - **Grids.** Inner walls lie on the plan's half-module lines (64 units
//!   outside). A storey's half-cells are its plan modules split in four; the
//!   cellar's come from its own outline. Walls between half-cells split a
//!   storey into rooms (a doorway still separates two rooms; it only lets the
//!   Knight through).
//! - **Scale.** The inside is built larger than the outside, as retail's are
//!   (research 13 §2): a half-module is 2, 3 or 4 inside cells at 1×, 1.5× or
//!   2×. Furniture stands on inside cells, counted from the plan's corner.
//! - **Storeys** are numbered from the ground floor (0); the cellar is −1.
//! - **Areas.** Each storey is built into a new area of its own, and stairs are door pairs between them. A door
//!   names its NPC pack only when it leads to another chunk (`ScanFieldEventTriggers`), and a new area has no
//!   entry in the game's pack table, so stairs within one area would bring the Knight back to it with no actors
//!   and no examine spots (seen in PCSX-Redux, 2026-10-27).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::design::{Axis, HouseDesign, Problem, Side};

/// Inside scale, in percent of the outside, and the default (research 13 §2).
pub const SCALES: [u8; 3] = [100, 150, 200];
pub const DEFAULT_SCALE: u8 = 150;
/// A storey's inside, walls included, at most this many cells a side (the south-west quarter of its area).
pub const STOREY_CELLS: i16 = 62;
/// The cellar's largest outline, in modules.
pub const MAX_CELLAR: u8 = 8;
/// The Knight examines from floor this close to a spot's centre (`spot::reach`).
pub const REACH: i32 = 112;
/// Shops 0–22 (opcode 0x07, screen 13).
pub const SHOPS: u8 = 23;
/// Inside cell side, in units.
pub const CELL: i32 = 32;

/// The inside of a design: every storey's inner walls, room settings and furniture, the stairs and the cellar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inside {
    /// Percent of the outside: 100, 150 or 200.
    #[serde(default = "default_scale")]
    pub scale: u8,
    #[serde(default)]
    pub walls: Vec<InnerWall>,
    /// Settings of the rooms that have any; the rest take their storey's defaults.
    #[serde(default)]
    pub rooms: Vec<RoomSet>,
    #[serde(default)]
    pub furniture: Vec<Furniture>,
    #[serde(default)]
    pub stairs: Vec<Stair>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cellar: Option<Cellar>,
    /// A copy of a retail door actor in the front doorway (an actor slot and, on screen, an NPC draw slot).
    #[serde(default)]
    pub door_panel: bool,
    /// The people who live here (§12.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub residents: Vec<Resident>,
    /// The song that plays inside: `bgm/n` (a streamed song on a HackerGC base when one replaces it); `None` is the
    /// house tune every retail interior plays (bgm/1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music: Option<u8>,
    /// Doors that open only once a flag is set, by the story or with a key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locks: Vec<Lock>,
}

fn default_scale() -> u8 {
    DEFAULT_SCALE
}

impl Default for Inside {
    fn default() -> Self {
        Inside { scale: DEFAULT_SCALE, walls: Vec::new(), rooms: Vec::new(), furniture: Vec::new(), stairs: Vec::new(), cellar: None, door_panel: false, residents: Vec::new(), music: None, locks: Vec::new() }
    }
}

/// One half-module of inner wall on a storey: from grid point `at` (half-modules) one step along `along`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InnerWall {
    pub storey: i8,
    pub at: [u8; 2],
    pub along: Axis,
    /// A doorway: the Knight walks through, the rooms stay two.
    #[serde(default)]
    pub door: bool,
}

impl InnerWall {
    /// The two half-cells it lies between.
    pub fn sides(&self) -> [[i16; 2]; 2] {
        let [x, z] = [self.at[0] as i16, self.at[1] as i16];
        match self.along {
            Axis::X => [[x, z - 1], [x, z]],
            Axis::Z => [[x - 1, z], [x, z]],
        }
    }
}

/// A room's settings, for the room holding half-cell `at` on its storey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSet {
    pub storey: i8,
    pub at: [u8; 2],
    #[serde(default)]
    pub name: String,
    pub floor: String,
    pub walls: String,
    #[serde(default)]
    pub tint: Tint,
    /// Brighter near the windows (Gouraud: +12 bytes a quad).
    #[serde(default)]
    pub window_light: bool,
    /// The template it was started from: not a link (§5.4.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// Vertex colour scale for every polygon in a room (128 = 1.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Tint {
    #[default]
    Daylight,
    Warm,
    Cool,
    Dim,
    Custom { rgb: [u8; 3] },
}

impl Tint {
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Tint::Daylight => [128, 128, 128],
            Tint::Warm => [140, 120, 96],
            Tint::Cool => [100, 112, 136],
            Tint::Dim => [84, 84, 88],
            Tint::Custom { rgb } => rgb,
        }
    }
}

/// A piece of furniture: an Interiors stock item at inside cell `at` (its turned footprint's min corner,
/// counted from the plan's corner at this scale) on `storey`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Furniture {
    pub storey: i8,
    pub item: String,
    pub at: [u8; 2],
    #[serde(default)]
    pub rot: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    /// A fireplace with its fire lit: firelight in its room, and smoke from the chimney over it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lit: bool,
}

/// The Interiors pieces a fire can be lit in.
pub const FIRES: [&str; 3] = ["fireplace", "hearth", "hearth-arched"];

/// A locked door (§12.2): stepping into it, the Knight goes through only once `flag` is set; until then he's told
/// it's locked and turns back. With a `key`, holding it unlocks the door (sets the flag) and uses the key up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lock {
    pub door: LockedDoor,
    pub flag: u16,
    /// The item that opens it, or none: the story opens it by setting the flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<u16>,
}

/// Which door a [`Lock`] is on: a front door (its module and side) or a stair (locked at its foot: coming down is
/// always open). Leaving the house is never locked. The cellar hatch can't be: it stands on open ground, so there is
/// nowhere beside it to keep the way in out of the Knight's reach ([`super::life::lock_dsl`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum LockedDoor {
    Front { at: [u8; 2], side: Side },
    Stair { storey: i8, at: [u8; 2] },
}

/// The songs a house can play: bgm/1–60 but the flight's (bgm/57), which loads into the other bank.
pub fn song_ok(n: u8) -> bool {
    (1..super::music::BGM_FILES).contains(&n) && n != super::music::FLIGHT_BGM
}

/// The lock on `link`, if it has one.
pub fn lock_of<'a>(i: &'a Inside, link: &DoorLink) -> Option<&'a Lock> {
    i.locks.iter().find(|l| match (l.door, link) {
        (LockedDoor::Front { at, side }, DoorLink::Front { module, side: s, .. }) => *s == side && module[0] == at[0] as i16 && module[1] == at[1] as i16,
        (LockedDoor::Stair { storey, at }, DoorLink::Stair { storey: s, at: a, .. }) => *s == storey && *a == at,
        _ => false,
    })
}

/// What pressing ✕ in front of a piece does: an examine spot (screen 23 §5.4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Action {
    /// Rest at the inn price of nothing: heal, save, wake beside the bed on `wake`'s side.
    Rest { wake: Side },
    /// Give `item` once, setting `flag`.
    Chest { item: u16, flag: u16 },
    /// One line of dialogue.
    Examine { line: String },
    /// Say `line`, then open shop `shop`.
    Shop { shop: u8, line: String },
}

/// Someone who lives in the house (§12.2): a copy of a retail character standing on an inside cell of `storey`,
/// who turns to the Knight and says a line when spoken to, and may walk a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resident {
    pub storey: i8,
    /// The inside cell they stand on, counted from the plan's corner at this scale (as furniture's `at`).
    pub at: [u8; 2],
    /// Which way they face, in eighths of a turn (0 = north, 2 = east).
    #[serde(default)]
    pub facing: u8,
    /// Whose model and poses: actor `slot` of `npc/pack` on the retail disc.
    pub pack: u16,
    pub slot: u16,
    /// The name the dialogue box shows; none shows no name.
    #[serde(default)]
    pub name: String,
    /// What they say (dialogue markup).
    pub line: String,
    /// Inside cells they walk to in turn, then back the same way, over and over; empty: they stand.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route: Vec<[u8; 2]>,
}

/// Waypoints a route has at most: each adds legs both ways to a walk of at most 60 words.
pub const MAX_ROUTE: usize = 6;
/// How fast residents walk: what retail walkers mostly use, 4 units a frame (2 on each axis on a diagonal), so a
/// leg of whole cells ends exactly on a cell.
pub const RESIDENT_SPEED: u16 = 4;
/// Frames a resident stands still at each end of its route.
pub const RESIDENT_PAUSE: u16 = 90;

/// A resident's walk (`edit::walk`) from inside cell `start` through `route` and back the same way, then round
/// again: each leg diagonal first, then straight, at [`RESIDENT_SPEED`], with a pause at each end. Coming back
/// retraces every leg backwards, so the steps cancel exactly and the loop never drifts. `None` when it is longer
/// than a walk can be.
pub fn resident_walk(start: [i32; 2], route: &[[i32; 2]]) -> Option<Vec<u16>> {
    if route.is_empty() {
        return Some(Vec::new());
    }
    // Per step at speed 4: 4 units along an axis, sin(45°) · 4 = 2.83 truncated to 2 on each axis diagonally.
    let (axis, diag) = (32 / RESIDENT_SPEED as i32, 32 / 2);
    let mut moves: Vec<(u16, i32)> = Vec::new();
    let leg = |from: [i32; 2], to: [i32; 2], moves: &mut Vec<(u16, i32)>| {
        let (dx, dz) = (to[0] - from[0], to[1] - from[1]);
        let m = dx.abs().min(dz.abs());
        if m > 0 {
            let h = match (dx > 0, dz > 0) {
                (true, true) => 1,
                (true, false) => 3,
                (false, false) => 5,
                (false, true) => 7,
            };
            moves.push((h, m * diag));
        }
        let (rx, rz) = (dx - dx.signum() * m, dz - dz.signum() * m);
        if rx != 0 {
            moves.push((if rx > 0 { 2 } else { 6 }, rx.abs() * axis));
        }
        if rz != 0 {
            moves.push((if rz > 0 { 0 } else { 4 }, rz.abs() * axis));
        }
    };
    let mut at = start;
    for &p in route {
        leg(at, p, &mut moves);
        at = p;
    }
    let out = moves.len();
    let back: Vec<(u16, i32)> = moves.iter().rev().map(|&(h, n)| ((h + 4) % 8, n)).collect();
    let speed = 0x8000 | RESIDENT_SPEED;
    let pause = [0x8000, 0x2000 | RESIDENT_PAUSE, speed];
    let mut words = vec![speed];
    let push = |words: &mut Vec<u16>, (h, mut n): (u16, i32)| {
        words.push(0x1000 | h);
        while n > 0 {
            let s = n.min(super::walk::MAX_STEPS as i32);
            words.push(0x2000 | s as u16);
            n -= s;
        }
    };
    for &mv in &moves[..out] {
        push(&mut words, mv);
    }
    words.extend(pause);
    for mv in back {
        push(&mut words, mv);
    }
    words.extend(pause);
    words.push(super::walk::LOOP);
    (words.len() <= super::walk::MAX_WORDS).then_some(words)
}

/// The inside cells a walk from `a` to `b` crosses, as [`resident_walk`] takes it (diagonal first, then straight),
/// `a` and `b` among them.
pub fn route_cells(a: [i32; 2], b: [i32; 2]) -> Vec<[i32; 2]> {
    let mut out = vec![a];
    let mut p = a;
    while p != b {
        // Both axes while both differ (the diagonal), then the one left.
        p = [p[0] + (b[0] - p[0]).signum(), p[1] + (b[1] - p[1]).signum()];
        out.push(p);
    }
    out
}

/// A stair between storey `storey` (its foot) and the one above (its head), at half-cell `at`: one half-module
/// wide and two long, rising toward `rises`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stair {
    pub storey: i8,
    pub at: [u8; 2],
    pub rises: Side,
}

impl Stair {
    /// Its two half-cells: the foot, then the head.
    pub fn cells(&self) -> [[i16; 2]; 2] {
        let a = [self.at[0] as i16, self.at[1] as i16];
        [a, step(a, self.rises)]
    }
}

/// A storey below the ground floor with its own outline (module rectangles), reached by stairs and optionally a
/// hatch from the yard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cellar {
    /// `[x, z, w, h]` in plan modules.
    pub outline: Vec<[u8; 4]>,
    /// A hatch on the plot: the yard module it stands in, and the side of it the Knight steps down from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hatch: Option<Hatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hatch {
    pub at: [u8; 2],
    pub side: Side,
}

fn step(m: [i16; 2], s: Side) -> [i16; 2] {
    match s {
        Side::North => [m[0], m[1] + 1],
        Side::East => [m[0] + 1, m[1]],
        Side::South => [m[0], m[1] - 1],
        Side::West => [m[0] - 1, m[1]],
    }
}

/* ───────────────────────── Finishes, templates ───────────────────────── */

/// Floor or wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Surface {
    Floor,
    Wall,
}

/// A finish: a texture rectangle on a retail interior's page (research 13 §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finish {
    pub id: &'static str,
    pub name: &'static str,
    pub surface: Surface,
    /// "village" or "plaster".
    pub style: &'static str,
    pub chunk: u32,
    pub tpage: u16,
    pub clut: u16,
    /// `[u0, v0, u1, v1]`, inclusive, in the page's texels.
    pub uv: [u8; 4],
}

const fn fin(id: &'static str, name: &'static str, surface: Surface, style: &'static str, chunk: u32, tpage: u16, clut: u16, uv: [u8; 4]) -> Finish {
    Finish { id, name, surface, style, chunk, tpage, clut, uv }
}

/// The finishes of the Village (324–327) and Plaster (330) styles.
pub const FINISHES: &[Finish] = &[
    fin("logs", "Logs", Surface::Wall, "village", 324, 0x000a, 0x7802, [0, 0, 63, 95]),
    fin("logs-beam", "Logs with a beam", Surface::Wall, "village", 324, 0x000a, 0x7802, [192, 128, 255, 223]),
    fin("panels-dark", "Dark wood panels", Surface::Wall, "village", 327, 0x001d, 0x79c9, [64, 32, 127, 127]),
    fin("planks", "Planks", Surface::Floor, "village", 324, 0x000a, 0x7982, [64, 0, 127, 63]),
    fin("rug-green", "Check rug, green", Surface::Floor, "village", 324, 0x001a, 0x7b03, [128, 64, 191, 127]),
    fin("rug-red", "Check rug, red", Surface::Floor, "village", 324, 0x001a, 0x7943, [128, 64, 191, 127]),
    fin("earth", "Packed earth", Surface::Floor, "village", 327, 0x001d, 0x7a49, [128, 160, 191, 223]),
    fin("plaster", "Plaster", Surface::Wall, "plaster", 330, 0x000c, 0x7806, [0, 0, 63, 63]),
    fin("planks-dark", "Dark planks", Surface::Floor, "plaster", 330, 0x000c, 0x7886, [0, 96, 63, 159]),
    fin("carpet-yellow", "Lattice carpet, yellow", Surface::Floor, "plaster", 330, 0x000c, 0x7ac6, [64, 96, 159, 191]),
    fin("carpet-green", "Lattice carpet, green", Surface::Floor, "plaster", 330, 0x000c, 0x7b06, [64, 96, 159, 191]),
    fin("carpet-purple", "Lattice carpet, purple", Surface::Floor, "plaster", 330, 0x000c, 0x7b46, [64, 96, 159, 191]),
];

pub fn finish(id: &str) -> Option<&'static Finish> {
    FINISHES.iter().find(|f| f.id == id)
}

/// A room's defaults: the ground floor and up take planks and logs in daylight; the cellar earth, dark panels, dim.
pub fn default_room(storey: i8) -> (&'static str, &'static str, Tint) {
    if storey < 0 {
        ("earth", "panels-dark", Tint::Dim)
    } else {
        ("planks", "logs", Tint::Daylight)
    }
}

/// A starting point for a room (§5.4.2): it sets the room once and furnishes it, largest piece first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub floor: &'static str,
    pub walls: &'static str,
    pub tint: Tint,
    /// Interiors stock ids, largest first, each with the action it takes (`""`, `"rest"`, `"chest"`, `"examine"`, `"shop"`).
    pub furniture: &'static [(&'static str, &'static str)],
}

pub const TEMPLATES: &[Template] = &[
    Template { id: "kitchen", name: "Kitchen", floor: "planks", walls: "logs", tint: Tint::Warm, furniture: &[("fireplace", ""), ("table-long", ""), ("bench", ""), ("cupboard", ""), ("stool", ""), ("cask", ""), ("pot", "examine")] },
    Template { id: "bedroom", name: "Bedroom", floor: "planks", walls: "logs", tint: Tint::Warm, furniture: &[("bed-quilt", "rest"), ("shelf", ""), ("chest", "")] },
    Template { id: "shop", name: "Shop", floor: "planks", walls: "logs", tint: Tint::Daylight, furniture: &[("table-long", "shop"), ("shelf", ""), ("dresser", ""), ("crate", "")] },
    Template { id: "inn-room", name: "Inn room", floor: "planks", walls: "logs", tint: Tint::Warm, furniture: &[("bed-red", "rest"), ("bed-yellow", "rest"), ("table-rustic", ""), ("stool", "")] },
    Template { id: "hall", name: "Hall", floor: "rug-green", walls: "logs", tint: Tint::Daylight, furniture: &[("table-red", ""), ("bench", "")] },
    Template { id: "storeroom", name: "Storeroom", floor: "earth", walls: "logs", tint: Tint::Dim, furniture: &[("shelf", ""), ("cask", ""), ("crate", "chest"), ("sack", "")] },
    Template { id: "cellar-store", name: "Cellar store", floor: "earth", walls: "panels-dark", tint: Tint::Dim, furniture: &[("shelf", ""), ("cask", ""), ("crate", ""), ("sack", ""), ("sack", "")] },
];

pub fn template(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.id == id)
}

/// An Interiors item's footprint in cells, unturned (`None`: not an Interiors item).
pub fn item_size(id: &str) -> Option<[u8; 2]> {
    super::stock::item(id).filter(|i| i.category == super::stock::INTERIORS).and_then(|_| super::stock::interior_size(id))
}

/* ───────────────────────── Rooms ───────────────────────── */

/// A room: the half-cells one storey's walls enclose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Room {
    pub storey: i8,
    /// Its first half-cell (row by row from the south-west): the room's key.
    pub key: [i16; 2],
    pub cells: Vec<[i16; 2]>,
    /// Index into `Inside::rooms`, when it has settings.
    pub set: Option<usize>,
}

/// Half-cells per inside cell side: a half-module is `k` inside cells.
pub fn cells_per_half(scale: u8) -> i16 {
    (scale as i16 / 50).max(2)
}

/// The half-cells of `storey`: the modules of the wings that stand that high for the ground floor and up, the cellar's outline below.
pub fn storey_cells(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>, storey: i8) -> BTreeSet<[i16; 2]> {
    let modules: Vec<[i16; 2]> = if storey >= 0 {
        if storey as u8 >= d.storeys {
            return BTreeSet::new();
        }
        plan.iter().filter(|(_, &k)| super::design::wing_storeys(d, k) > storey as u8).map(|(m, _)| *m).collect()
    } else {
        let Some(c) = d.inside.as_ref().and_then(|i| i.cellar.as_ref()) else { return BTreeSet::new() };
        c.outline.iter().flat_map(|r| (0..r[3] as i16).flat_map(move |j| (0..r[2] as i16).map(move |i| [r[0] as i16 + i, r[1] as i16 + j]))).collect()
    };
    modules.iter().flat_map(|m| (0..4).map(move |q| [m[0] * 2 + q % 2, m[1] * 2 + q / 2])).collect()
}

/// The storeys the inside has: the cellar (when it has one), then the ground floor up.
pub fn storeys(d: &HouseDesign) -> Vec<i8> {
    let cellar = d.inside.as_ref().is_some_and(|i| i.cellar.is_some());
    (if cellar { -1 } else { 0 }..d.storeys as i8).collect()
}

/// The inner walls of `storey` that stand between two of its half-cells (others are stale).
fn live_walls<'a>(i: &'a Inside, cells: &'a BTreeSet<[i16; 2]>, storey: i8) -> impl Iterator<Item = &'a InnerWall> + 'a {
    i.walls.iter().filter(move |w| w.storey == storey && w.sides().iter().all(|c| cells.contains(c)))
}

/// Every room of every storey, cellar first, each storey's rooms in key order.
pub fn rooms(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>) -> Vec<Room> {
    let Some(i) = &d.inside else { return Vec::new() };
    let mut out = Vec::new();
    for s in storeys(d) {
        let cells = storey_cells(d, plan, s);
        let walls: BTreeSet<([i16; 2], [i16; 2])> = live_walls(i, &cells, s).flat_map(|w| {
            let [a, b] = w.sides();
            [(a, b), (b, a)]
        }).collect();
        let mut seen: BTreeSet<[i16; 2]> = BTreeSet::new();
        // Row by row from the south-west, so a room's key is its first cell.
        let mut order: Vec<[i16; 2]> = cells.iter().copied().collect();
        order.sort_by_key(|c| (c[1], c[0]));
        for &start in &order {
            if !seen.insert(start) {
                continue;
            }
            let mut room = vec![start];
            let mut queue = VecDeque::from([start]);
            while let Some(c) = queue.pop_front() {
                for side in Side::ALL {
                    let n = step(c, side);
                    if cells.contains(&n) && !walls.contains(&(c, n)) && seen.insert(n) {
                        room.push(n);
                        queue.push_back(n);
                    }
                }
            }
            room.sort_by_key(|c| (c[1], c[0]));
            let set = i.rooms.iter().position(|r| r.storey == s && room.contains(&[r.at[0] as i16, r.at[1] as i16]));
            out.push(Room { storey: s, key: start, cells: room, set });
        }
    }
    out
}

/// A room's settings, its own or its storey's defaults: (floor, walls, tint, window light, name).
pub fn room_look<'a>(i: &'a Inside, r: &Room) -> (&'a str, &'a str, Tint, bool, &'a str) {
    match r.set.map(|k| &i.rooms[k]) {
        Some(s) => (&s.floor, &s.walls, s.tint, s.window_light, &s.name),
        None => {
            let (f, w, t) = default_room(r.storey);
            (f, w, t, false, "")
        }
    }
}

/* ───────────────────────── Checks ───────────────────────── */

fn problem(error: bool, code: &str, message: String, at: Option<[i16; 2]>) -> Problem {
    Problem { error, code: code.to_string(), message, at: at.map(|m| [m[0].max(0) as u8, m[1].max(0) as u8]) }
}

/// The inside's rules, after the plan checks out. `front` is the ground floor's door modules and the side each
/// faces, `items` the Interiors footprints by stock id (unturned, in cells).
pub fn check(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>, front: &[([i16; 2], Side)], items: &dyn Fn(&str) -> Option<[u8; 2]>, out: &mut Vec<Problem>) {
    let Some(i) = &d.inside else { return };
    if !SCALES.contains(&i.scale) {
        out.push(problem(true, "INSIDE_SCALE", "Inside scale is 1×, 1.5× or 2×.".into(), None));
        return;
    }
    let k = cells_per_half(i.scale);
    for s in storeys(d) {
        let cells = storey_cells(d, plan, s);
        let (lo, hi) = cells.iter().fold(([i16::MAX; 2], [i16::MIN; 2]), |(lo, hi), c| ([lo[0].min(c[0]), lo[1].min(c[1])], [hi[0].max(c[0] + 1), hi[1].max(c[1] + 1)]));
        if !cells.is_empty() && ((hi[0] - lo[0]) * k > STOREY_CELLS || (hi[1] - lo[1]) * k > STOREY_CELLS) {
            out.push(problem(true, "INSIDE_SIZE", format!("{} is {} × {} cells inside at {}×; a storey's inside is at most {STOREY_CELLS} a side. Try a smaller inside scale.", storey_name(s), (hi[0] - lo[0]) * k, (hi[1] - lo[1]) * k, scale_text(i.scale)), None));
        }
    }
    if front.is_empty() {
        out.push(problem(true, "INSIDE_NO_DOOR", "The inside needs a way in: give the ground floor a door.".into(), None));
    }
    if let Some(c) = &i.cellar {
        if c.outline.is_empty() {
            out.push(problem(true, "CELLAR_EMPTY", "Draw the cellar's outline (Rooms · Outline), or switch the cellar off.".into(), None));
        }
        let cells = storey_cells(d, plan, -1);
        let (lo, hi) = cells.iter().fold(([i16::MAX; 2], [i16::MIN; 2]), |(lo, hi), c| ([lo[0].min(c[0]), lo[1].min(c[1])], [hi[0].max(c[0] + 1), hi[1].max(c[1] + 1)]));
        if !cells.is_empty() && ((hi[0] - lo[0]) / 2 > MAX_CELLAR as i16 || (hi[1] - lo[1]) / 2 > MAX_CELLAR as i16) {
            out.push(problem(true, "CELLAR_SIZE", format!("The cellar is at most {MAX_CELLAR} × {MAX_CELLAR} modules."), None));
        }
        if !i.stairs.iter().any(|s| s.storey == -1) && c.hatch.is_none() {
            out.push(problem(true, "CELLAR_NO_WAY", "Nothing reaches the cellar: add stairs down from the ground floor or a hatch in the yard.".into(), None));
        }
        if let Some(h) = c.hatch {
            let m = [h.at[0] as i16, h.at[1] as i16];
            let open = |m: [i16; 2]| d.yard.as_ref().is_some_and(|y| (0..2).all(|a| m[a] >= y.at[a] as i16 && m[a] < (y.at[a] + y.size[a]) as i16)) && !plan.contains_key(&m);
            if !open(m) {
                out.push(problem(true, "HATCH_OFF_PLOT", "The cellar hatch stands on the plot's open ground.".into(), Some(m)));
            } else if !open(step(m, h.side)) {
                out.push(problem(true, "HATCH_SIDE", format!("The Knight steps onto the cellar hatch from its {} side: that module must be the plot's open ground too.", h.side.name()), Some(m)));
            }
            if !c.outline.iter().any(|r| m[0] >= r[0] as i16 && m[0] < (r[0] + r[2]) as i16 && m[1] >= r[1] as i16 && m[1] < (r[1] + r[3]) as i16) {
                out.push(problem(true, "HATCH_NO_CELLAR", "The cellar hatch leads straight down: the cellar's outline must reach under it.".into(), Some(m)));
            }
        }
    }
    if let Some(w) = i.walls.iter().find(|w| blocks_door(w, front)) {
        let [a, _] = w.sides();
        out.push(problem(true, "INNER_WALL_DOOR", "An inner wall runs through the front door: keep the door's two half-modules and the landing inside it open.".into(), Some([a[0] / 2, a[1] / 2])));
    }
    let stale = i.walls.iter().filter(|w| !w.sides().iter().all(|c| storey_cells(d, plan, w.storey).contains(c))).count();
    if stale > 0 {
        out.push(problem(false, "INSIDE_STALE", format!("{stale} inner wall{} no longer inside the house; left out.", if stale == 1 { " is" } else { "s are" }), None));
    }
    for st in &i.stairs {
        let [foot, head] = st.cells();
        let below = storey_cells(d, plan, st.storey);
        let above = storey_cells(d, plan, st.storey + 1);
        let m = [foot[0] / 2, foot[1] / 2];
        if st.storey + 1 >= d.storeys as i8 || !below.contains(&foot) || !below.contains(&head) || !above.contains(&foot) || !above.contains(&head) {
            out.push(problem(true, "STAIR_OFF", format!("Stairs from {} need both their half-modules inside it and inside the storey above.", storey_name(st.storey).to_lowercase()), Some(m)));
        }
    }
    for f in &i.furniture {
        let Some(size) = items(&f.item) else {
            out.push(problem(true, "FURNITURE_UNKNOWN", format!("No Interiors item “{}”.", f.item), None));
            continue;
        };
        let cells = storey_cells(d, plan, f.storey);
        let [w, h] = if f.rot % 2 == 1 { [size[1], size[0]] } else { size };
        // Every inside cell it covers lies on one of the storey's half-cells.
        let inside = (0..h as i16).all(|r| (0..w as i16).all(|c| cells.contains(&[(f.at[0] as i16 + c) / k, (f.at[1] as i16 + r) / k])));
        if !inside {
            out.push(problem(true, "FURNITURE_OFF", format!("A piece ({}) stands outside {}.", f.item, storey_name(f.storey).to_lowercase()), Some([f.at[0] as i16 / k / 2, f.at[1] as i16 / k / 2])));
        }
        match &f.action {
            Some(Action::Chest { flag, .. }) if *flag == 0 || *flag == 0xF8 => {
                out.push(problem(true, "CHEST_FLAG", "A chest needs a free story flag of its own (not 0 or 0xF8).".into(), None));
            }
            Some(Action::Shop { shop, .. }) if *shop >= SHOPS => {
                out.push(problem(true, "SHOP_ID", format!("Shop {shop} doesn't exist; shops are 0 to {}.", SHOPS - 1), None));
            }
            Some(Action::Examine { line }) | Some(Action::Shop { line, .. }) if line.trim().is_empty() => {
                out.push(problem(true, "ACTION_LINE", "An examine line or a shop's greeting needs words.".into(), None));
            }
            _ => {}
        }
    }
    let clear = clear_cells(d, front);
    for f in &i.furniture {
        let Some(size) = items(&f.item) else { continue };
        let [w, h] = if f.rot % 2 == 1 { [size[1], size[0]] } else { size };
        let covers = |c: [i16; 2]| {
            let (x0, z0) = (c[0] * k, c[1] * k);
            (f.at[0] as i16) < x0 + k && x0 < f.at[0] as i16 + w as i16 && (f.at[1] as i16) < z0 + k && z0 < f.at[1] as i16 + h as i16
        };
        if let Some((_, _, what)) = clear.iter().find(|(s, c, _)| *s == f.storey && covers(*c)) {
            out.push(problem(true, "FURNITURE_IN_WAY", format!("A piece ({}) stands in the way of {what}.", f.item), Some([f.at[0] as i16 / k / 2, f.at[1] as i16 / k / 2])));
        }
    }
    let mut flags: BTreeMap<u16, usize> = BTreeMap::new();
    for f in &i.furniture {
        if let Some(Action::Chest { flag, .. }) = f.action {
            *flags.entry(flag).or_default() += 1;
        }
    }
    if flags.values().any(|&n| n > 1) {
        out.push(problem(true, "CHEST_FLAG_SHARED", "Two chests share a flag: opening one would empty both.".into(), None));
    }
    for r in &i.rooms {
        if finish(&r.floor).is_none_or(|f| f.surface != Surface::Floor) || finish(&r.walls).is_none_or(|f| f.surface != Surface::Wall) {
            out.push(problem(true, "ROOM_FINISH", format!("A room has an unknown finish ({} / {}).", r.floor, r.walls), Some([r.at[0] as i16 / 2, r.at[1] as i16 / 2])));
        }
    }
    // Rooms the Knight can't reach from the front door: through doorways, and up and down the stairs.
    let all = rooms(d, plan);
    let reached = reachable(d, plan, &all, front);
    let lost = all.iter().enumerate().filter(|(n, _)| !reached.contains(n)).count();
    if lost > 0 && !front.is_empty() {
        out.push(problem(false, "ROOM_UNREACHED", format!("{lost} room{} can't be reached from the front door: add a doorway or stairs.", if lost == 1 { "" } else { "s" }), None));
    }
    check_residents(d, plan, front, items, out);
    if let Some(n) = i.music.filter(|&n| !song_ok(n)) {
        out.push(problem(true, "MUSIC_SONG", format!("bgm/{n} isn't a song a house can play: pick one of bgm/1 to bgm/60, but not the flight's (bgm/57)."), None));
    }
    for l in &i.locks {
        let (door, at) = match l.door {
            LockedDoor::Front { at, side } => (front.iter().any(|&(m, s)| s == side && m == [at[0] as i16, at[1] as i16]).then_some("the front door"), Some([at[0] as i16, at[1] as i16])),
            LockedDoor::Stair { storey, at } => (i.stairs.iter().any(|s| s.storey == storey && s.at == at).then_some("the stairs"), Some([at[0] as i16 / 2, at[1] as i16 / 2])),
        };
        let Some(door) = door else {
            out.push(problem(false, "LOCK_STALE", "A lock is on a door the house no longer has; left out.".into(), at));
            continue;
        };
        if l.flag == 0 || l.flag == 0xF8 {
            out.push(problem(true, "LOCK_FLAG", format!("The lock on {door} needs a free story flag of its own (not 0 or 0xF8): the door opens once it's set."), at));
        }
        if l.key.is_some_and(|k| k == 0) {
            out.push(problem(true, "LOCK_KEY", format!("The lock on {door} names no key: pick an item, or leave it to the story."), at));
        }
    }
    let mut seen = BTreeSet::new();
    if i.locks.iter().any(|l| !seen.insert(format!("{:?}", l.door))) {
        out.push(problem(true, "LOCK_TWICE", "A door has two locks: keep one.".into(), None));
    }
    for f in i.furniture.iter().filter(|f| f.lit) {
        if !FIRES.contains(&f.item.as_str()) {
            out.push(problem(true, "FIRE_NOT_HEARTH", format!("Only a fireplace or a hearth can be lit, not the {}.", f.item), None));
            continue;
        }
        let Some(size) = items(&f.item) else { continue };
        let [w, h] = if f.rot % 2 == 1 { [size[1], size[0]] } else { size };
        let k = cells_per_half(i.scale);
        let m = [(f.at[0] as i16 + w as i16 / 2) / k / 2, (f.at[1] as i16 + h as i16 / 2) / k / 2];
        if chimney_over(d, m).is_none() {
            out.push(problem(false, "FIRE_NO_CHIMNEY", "A lit fire has no chimney over it, so no smoke rises outside: add a chimney on its module or one beside it.".into(), Some(m)));
        }
    }
}

/// The chimney a fire on plan module `m` sends its smoke up: one on `m`, else the nearest beside it.
pub fn chimney_over(d: &HouseDesign, m: [i16; 2]) -> Option<[u8; 2]> {
    d.chimneys
        .iter()
        .filter(|c| (c[0] as i16 - m[0]).abs() <= 1 && (c[1] as i16 - m[1]).abs() <= 1)
        .min_by_key(|c| (c[0] as i16 - m[0]).abs() + (c[1] as i16 - m[1]).abs())
        .copied()
}

/// The chimneys whose smoke rises, from the lit fires under them, each once.
pub fn smoking(d: &HouseDesign, items: &dyn Fn(&str) -> Option<[u8; 2]>) -> Vec<[u8; 2]> {
    let Some(i) = &d.inside else { return Vec::new() };
    let k = cells_per_half(i.scale);
    let mut out = Vec::new();
    for f in i.furniture.iter().filter(|f| f.lit && FIRES.contains(&f.item.as_str())) {
        let Some(size) = items(&f.item) else { continue };
        let [w, h] = if f.rot % 2 == 1 { [size[1], size[0]] } else { size };
        let m = [(f.at[0] as i16 + w as i16 / 2) / k / 2, (f.at[1] as i16 + h as i16 / 2) / k / 2];
        if let Some(c) = chimney_over(d, m).filter(|c| !out.contains(c)) {
            out.push(c);
        }
    }
    out
}

/// The inside cells furniture covers on `storey`.
fn furniture_cells(i: &Inside, storey: i8, items: &dyn Fn(&str) -> Option<[u8; 2]>) -> BTreeSet<[i32; 2]> {
    let mut out = BTreeSet::new();
    for f in i.furniture.iter().filter(|f| f.storey == storey) {
        let Some(size) = items(&f.item) else { continue };
        let [w, h] = if f.rot % 2 == 1 { [size[1], size[0]] } else { size };
        for r in 0..h as i32 {
            for c in 0..w as i32 {
                out.insert([f.at[0] as i32 + c, f.at[1] as i32 + r]);
            }
        }
    }
    out
}

/// The residents' rules: on a storey's floor clear of furniture and of the ways through, a line to say, and a route
/// along open floor that a walk can hold. Where the built room has walls the Knight's probe keeps him from, the
/// build checks again ([`build`]).
fn check_residents(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>, front: &[([i16; 2], Side)], items: &dyn Fn(&str) -> Option<[u8; 2]>, out: &mut Vec<Problem>) {
    let Some(i) = &d.inside else { return };
    if i.residents.is_empty() {
        return;
    }
    let k = cells_per_half(i.scale) as i32;
    let all = storeys(d);
    let clear = clear_cells(d, front);
    let module = |c: [u8; 2]| Some([(c[0] as i32 / k / 2) as i16, (c[1] as i32 / k / 2) as i16]);
    for (n, r) in i.residents.iter().enumerate() {
        let who = if r.name.trim().is_empty() { format!("Resident {}", n + 1) } else { r.name.trim().to_string() };
        if !all.contains(&r.storey) {
            out.push(problem(true, "RESIDENT_STOREY", format!("{who} lives on a storey the house doesn't have."), None));
            continue;
        }
        if r.line.trim().is_empty() {
            out.push(problem(true, "RESIDENT_LINE", format!("{who} needs a line to say."), module(r.at)));
        } else if let Err(e) = super::text::parse_markup(&r.line) {
            out.push(problem(true, "RESIDENT_LINE", format!("{who}'s line: {}", e.message), module(r.at)));
        }
        let cells = storey_cells(d, plan, r.storey);
        let walls: BTreeSet<([i16; 2], [i16; 2])> = live_walls(i, &cells, r.storey)
            .filter(|w| !w.door)
            .flat_map(|w| {
                let [a, b] = w.sides();
                [(a, b), (b, a)]
            })
            .collect();
        let taken = furniture_cells(i, r.storey, items);
        let half = |c: [i32; 2]| [(c[0].div_euclid(k)) as i16, (c[1].div_euclid(k)) as i16];
        let on_floor = |c: [i32; 2]| c[0] >= 0 && c[1] >= 0 && cells.contains(&half(c)) && !taken.contains(&c);
        // A step from one cell to the next crosses no inner wall: straight, the wall between their half-cells;
        // diagonally across a corner, one of the two ways round it.
        let open = |a: [i32; 2], b: [i32; 2]| {
            let (p, q) = (half(a), half(b));
            let free = |x: [i16; 2], y: [i16; 2]| x == y || !walls.contains(&(x, y));
            if p[0] != q[0] && p[1] != q[1] {
                let (u, v) = ([q[0], p[1]], [p[0], q[1]]);
                (cells.contains(&u) && free(p, u) && free(u, q)) || (cells.contains(&v) && free(p, v) && free(v, q))
            } else {
                free(p, q)
            }
        };
        let at = [r.at[0] as i32, r.at[1] as i32];
        if !on_floor(at) {
            out.push(problem(true, "RESIDENT_OFF_FLOOR", format!("{who} stands on furniture or off {}: put them on open floor.", storey_name(r.storey).to_lowercase()), module(r.at)));
            continue;
        }
        if let Some((_, _, what)) = clear.iter().find(|(s, c, _)| *s == r.storey && *c == half(at)) {
            out.push(problem(true, "RESIDENT_IN_WAY", format!("{who} stands in the way of {what}."), module(r.at)));
        }
        if r.route.len() > MAX_ROUTE {
            out.push(problem(true, "RESIDENT_ROUTE_LONG", format!("{who}'s route has {} stops; a route has at most {MAX_ROUTE}.", r.route.len()), module(r.at)));
            continue;
        }
        let mut from = at;
        let mut blocked = false;
        for &p in &r.route {
            let to = [p[0] as i32, p[1] as i32];
            let path = route_cells(from, to);
            if !path.iter().all(|&c| on_floor(c)) || !path.windows(2).all(|w| open(w[0], w[1])) {
                out.push(problem(true, "RESIDENT_ROUTE_BLOCKED", format!("{who}'s route runs through a wall or furniture: walk it round by adding a stop."), module(p)));
                blocked = true;
                break;
            }
            from = to;
        }
        if !blocked && resident_walk(at, &r.route.iter().map(|p| [p[0] as i32, p[1] as i32]).collect::<Vec<_>>()).is_none() {
            out.push(problem(true, "RESIDENT_ROUTE_LONG", format!("{who}'s route is longer than a walk can hold ({} words): take a stop out.", super::walk::MAX_WORDS), module(r.at)));
        }
    }
    for s in all {
        let here: Vec<&Resident> = i.residents.iter().filter(|r| r.storey == s).collect();
        let drawn = crate::placement::DRAWN_AT_ONCE as usize;
        if here.len() > drawn {
            out.push(problem(
                false,
                "RESIDENTS_DRAWN",
                format!("{} has {} residents; the game draws {drawn} people at once, so the rest stand there unseen until one leaves the screen.", storey_name(s), here.len()),
                None,
            ));
        }
        let mut seen = BTreeSet::new();
        if here.iter().any(|r| !seen.insert(r.at)) {
            out.push(problem(true, "RESIDENT_SHARED", format!("Two residents stand on one cell of {}.", storey_name(s).to_lowercase()), None));
        }
    }
}

/// Inner wall `w` stands in a front door's way: between its two half-cells, between the two landing half-cells inside
/// them (where the Knight arrives), or between a door half-cell and its landing.
pub fn blocks_door(w: &InnerWall, front: &[([i16; 2], Side)]) -> bool {
    if w.storey != 0 {
        return false;
    }
    let [p, q] = w.sides();
    front.iter().any(|&(m, side)| {
        let [a, b] = door_cells(m, side);
        let (la, lb) = (step(a, opposite(side)), step(b, opposite(side)));
        [(a, b), (la, lb), (a, la), (b, lb)].iter().any(|&(x, y)| (p == x && q == y) || (p == y && q == x))
    })
}

/// Half-cells furniture leaves clear, by storey, with what keeps them so: a front door's half-cells and the landing
/// inside it, a doorway's two half-cells, a stair's foot and the landing before it, the landing beyond its head
/// upstairs, and the cellar hatch's module.
pub fn clear_cells(d: &HouseDesign, front: &[([i16; 2], Side)]) -> Vec<(i8, [i16; 2], &'static str)> {
    let Some(i) = &d.inside else { return Vec::new() };
    let mut clear: Vec<(i8, [i16; 2], &'static str)> = Vec::new();
    for &(m, side) in front {
        for c in door_cells(m, side) {
            clear.push((0, c, "the front door"));
            clear.push((0, step(c, opposite(side)), "the front door"));
        }
    }
    for w in i.walls.iter().filter(|w| w.door) {
        for c in w.sides() {
            clear.push((w.storey, c, "a doorway"));
        }
    }
    if let Some(h) = i.cellar.as_ref().and_then(|c| c.hatch) {
        for q in 0..4 {
            clear.push((-1, [h.at[0] as i16 * 2 + q % 2, h.at[1] as i16 * 2 + q / 2], "the hatch's ladder"));
        }
    }
    for st in &i.stairs {
        let [foot, head] = st.cells();
        clear.push((st.storey, foot, "the stairs"));
        clear.push((st.storey, step(foot, opposite(st.rises)), "the stairs"));
        clear.push((st.storey + 1, step(head, st.rises), "the stairs"));
    }
    clear
}

/// Indices into `rooms` the front door reaches.
fn reachable(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>, rooms: &[Room], front: &[([i16; 2], Side)]) -> BTreeSet<usize> {
    let Some(i) = &d.inside else { return BTreeSet::new() };
    let room_of = |s: i8, c: [i16; 2]| rooms.iter().position(|r| r.storey == s && r.cells.contains(&c));
    let mut out = BTreeSet::new();
    let mut queue = VecDeque::new();
    for (m, side) in front {
        // The door's half-cells on the inside of its wall.
        for c in door_cells(*m, *side) {
            if let Some(r) = room_of(0, c) {
                if out.insert(r) {
                    queue.push_back(r);
                }
            }
        }
    }
    let hatch_room = i.cellar.as_ref().and_then(|c| c.hatch).and_then(|_| rooms.iter().position(|r| r.storey == -1));
    if let Some(r) = hatch_room {
        if out.insert(r) {
            queue.push_back(r);
        }
    }
    let _ = plan;
    while let Some(r) = queue.pop_front() {
        let room = &rooms[r];
        let mut next = Vec::new();
        for w in i.walls.iter().filter(|w| w.storey == room.storey && w.door) {
            let [a, b] = w.sides();
            if room.cells.contains(&a) {
                next.extend(room_of(room.storey, b));
            } else if room.cells.contains(&b) {
                next.extend(room_of(room.storey, a));
            }
        }
        for st in &i.stairs {
            let [foot, head] = st.cells();
            if st.storey == room.storey && (room.cells.contains(&foot) || room.cells.contains(&head)) {
                next.extend(room_of(st.storey + 1, head));
            }
            if st.storey + 1 == room.storey && (room.cells.contains(&foot) || room.cells.contains(&head)) {
                next.extend(room_of(st.storey, foot));
            }
        }
        for n in next {
            if out.insert(n) {
                queue.push_back(n);
            }
        }
    }
    out
}

/// The two half-cells just inside an outside door on module `m`'s `side`.
pub fn door_cells(m: [i16; 2], side: Side) -> [[i16; 2]; 2] {
    let (x, z) = (m[0] * 2, m[1] * 2);
    match side {
        Side::South => [[x, z], [x + 1, z]],
        Side::North => [[x, z + 1], [x + 1, z + 1]],
        Side::West => [[x, z], [x, z + 1]],
        Side::East => [[x + 1, z], [x + 1, z + 1]],
    }
}

pub fn storey_name(s: i8) -> String {
    match s {
        -1 => "The cellar".into(),
        0 => "The ground floor".into(),
        1 => "The upper floor".into(),
        n => format!("Floor {}", n + 1),
    }
}

fn scale_text(s: u8) -> String {
    if s % 100 == 0 {
        format!("{}", s / 100)
    } else {
        format!("{}", s as f32 / 100.0)
    }
}

/* ───────────────────────── Templates ───────────────────────── */

/// What a template did: the inside with the room set and furnished, and the items left out.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Furnished {
    pub inside: Inside,
    /// Items that found no place against a wall clear of the ways through.
    pub left: Vec<String>,
}

/// The inside cells a piece at `at` turned `rot` covers, `size` unturned.
fn covered(at: [i32; 2], size: [u8; 2], rot: u8) -> impl Iterator<Item = [i32; 2]> {
    let [w, h] = if rot % 2 == 1 { [size[1], size[0]] } else { size };
    (0..h as i32).flat_map(move |r| (0..w as i32).map(move |c| [at[0] + c, at[1] + r]))
}

/// "an open crate", "a pot".
fn with_article(name: &str) -> String {
    let n = name.to_lowercase();
    let a = if n.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
    format!("{a} {n}")
}

/// Room `key` on `storey` started from template `t` (§5.4.2): its floor, walls and tint set, its furniture replaced
/// by the template's, largest first. Each piece stands against the room's walls (north and west first, the walls
/// the field camera faces before it turns), never on a door's, doorway's, stair's or hatch's half-cells, and only
/// where the room's free floor stays in one piece, so the ways between its doorways stay open. What doesn't fit is
/// left out. A chest takes the first of `flags` no other chest has; an examine piece gets a line naming it. `None`:
/// no such room.
pub fn apply_template(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>, storey: i8, key: [i16; 2], t: &Template, items: &dyn Fn(&str) -> Option<[u8; 2]>, flags: &[u16]) -> Option<Furnished> {
    let mut i = d.inside.clone()?;
    let k = cells_per_half(i.scale) as i32;
    let room = rooms(d, plan).into_iter().find(|r| r.storey == storey && r.key == key)?;
    let half_of = |f: &Furniture| -> Option<[i16; 2]> {
        let [w, h] = items(&f.item).map(|s| if f.rot % 2 == 1 { [s[1], s[0]] } else { s })?;
        Some([((f.at[0] as i32 + w as i32 / 2) / k) as i16, ((f.at[1] as i32 + h as i32 / 2) / k) as i16])
    };
    // The room's settings, its name kept.
    let set = room.set.map(|n| i.rooms[n].clone());
    let name = set.as_ref().map(|r| r.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| t.name.to_string());
    let room_set = RoomSet { storey, at: [key[0] as u8, key[1] as u8], name, floor: t.floor.into(), walls: t.walls.into(), tint: t.tint, window_light: set.as_ref().is_some_and(|r| r.window_light), template: Some(t.id.into()) };
    match room.set {
        Some(n) => i.rooms[n] = room_set,
        None => i.rooms.push(room_set),
    }
    i.furniture.retain(|f| !(f.storey == storey && half_of(f).is_some_and(|h| room.cells.contains(&h))));

    let cells: BTreeSet<[i32; 2]> = room.cells.iter().flat_map(|h| (0..k).flat_map(move |r| (0..k).map(move |c| [h[0] as i32 * k + c, h[1] as i32 * k + r]))).collect();
    let mut taken: BTreeSet<[i32; 2]> = i
        .furniture
        .iter()
        .filter(|f| f.storey == storey)
        .filter_map(|f| Some(covered([f.at[0] as i32, f.at[1] as i32], items(&f.item)?, f.rot).collect::<Vec<_>>()))
        .flatten()
        .collect();
    let blocked: BTreeSet<[i32; 2]> = clear_cells(&HouseDesign { inside: Some(i.clone()), ..d.clone() }, &super::design::front_doors(d, plan))
        .into_iter()
        .filter(|(s, _, _)| *s == storey)
        .flat_map(|(_, h, _)| (0..k).flat_map(move |r| (0..k).map(move |c| [h[0] as i32 * k + c, h[1] as i32 * k + r])))
        .collect();
    // The room's free floor stays one piece with `rect` taken too.
    let whole = |taken: &BTreeSet<[i32; 2]>, rect: &BTreeSet<[i32; 2]>| {
        let free: Vec<[i32; 2]> = cells.iter().copied().filter(|c| !taken.contains(c) && !rect.contains(c)).collect();
        let Some(&first) = free.first() else { return true };
        let mut seen = BTreeSet::from([first]);
        let mut queue = VecDeque::from([first]);
        while let Some(c) = queue.pop_front() {
            for n in [[c[0] + 1, c[1]], [c[0] - 1, c[1]], [c[0], c[1] + 1], [c[0], c[1] - 1]] {
                if cells.contains(&n) && !taken.contains(&n) && !rect.contains(&n) && seen.insert(n) {
                    queue.push_back(n);
                }
            }
        }
        seen.len() == free.len()
    };
    let (lo, hi) = cells.iter().fold(([i32::MAX; 2], [i32::MIN; 2]), |(lo, hi), c| ([lo[0].min(c[0]), lo[1].min(c[1])], [hi[0].max(c[0]), hi[1].max(c[1])]));
    let mut used: BTreeSet<u16> = i.furniture.iter().filter_map(|f| if let Some(Action::Chest { flag, .. }) = f.action { Some(flag) } else { None }).collect();
    let mut left = Vec::new();
    for &(item, act) in t.furniture {
        let Some(size) = items(item) else {
            left.push(item.to_string());
            continue;
        };
        let mut best: Option<(i32, [i32; 2], u8)> = None;
        for rot in if size[0] == size[1] { 0..1 } else { 0..2 } {
            let [w, h] = if rot % 2 == 1 { [size[1] as i32, size[0] as i32] } else { [size[0] as i32, size[1] as i32] };
            for z in (lo[1]..=hi[1] - h + 1).rev() {
                for x in lo[0]..=hi[0] - w + 1 {
                    let rect: BTreeSet<[i32; 2]> = covered([x, z], size, rot).collect();
                    if !rect.iter().all(|c| cells.contains(c) && !taken.contains(c) && !blocked.contains(c)) {
                        continue;
                    }
                    let wall = |c: [i32; 2]| !cells.contains(&c);
                    let (north, west) = ((x..x + w).filter(|&c| wall([c, z + h])).count(), (z..z + h).filter(|&r| wall([x - 1, r])).count());
                    let (east, south) = ((z..z + h).filter(|&r| wall([x + w, r])).count(), (x..x + w).filter(|&c| wall([c, z - 1])).count());
                    let score = (4 * north + 3 * west + 2 * east + south) as i32;
                    if score == 0 || best.is_some_and(|b| b.0 >= score) || !whole(&taken, &rect) {
                        continue;
                    }
                    best = Some((score, [x, z], rot));
                }
            }
        }
        let Some((_, at, rot)) = best else {
            left.push(item.to_string());
            continue;
        };
        let rect: Vec<[i32; 2]> = covered(at, size, rot).collect();
        let action = match act {
            "rest" => {
                // Wake on the side with the most open floor beside the bed.
                let [w, h] = if rot % 2 == 1 { [size[1] as i32, size[0] as i32] } else { [size[0] as i32, size[1] as i32] };
                let open = |pts: Vec<[i32; 2]>| pts.into_iter().filter(|c| cells.contains(c) && !taken.contains(c)).count();
                let sides = [
                    (Side::South, open((at[0]..at[0] + w).map(|c| [c, at[1] - 1]).collect())),
                    (Side::East, open((at[1]..at[1] + h).map(|r| [at[0] + w, r]).collect())),
                    (Side::North, open((at[0]..at[0] + w).map(|c| [c, at[1] + h]).collect())),
                    (Side::West, open((at[1]..at[1] + h).map(|r| [at[0] - 1, r]).collect())),
                ];
                let wake = sides.iter().max_by_key(|s| s.1).map_or(Side::South, |s| s.0);
                Some(Action::Rest { wake })
            }
            "chest" => {
                let flag = flags.iter().copied().find(|f| !used.contains(f)).unwrap_or(0);
                used.insert(flag);
                Some(Action::Chest { item: 1, flag })
            }
            "examine" => Some(Action::Examine { line: format!("It's {}.", with_article(&super::stock::item(item).map_or(item.to_string(), |s| s.name))) }),
            "shop" => Some(Action::Shop { shop: 0, line: "Welcome! Take a look.".into() }),
            _ => None,
        };
        taken.extend(rect);
        i.furniture.push(Furniture { storey, item: item.into(), at: [at[0] as u8, at[1] as u8], rot, action, lit: false });
    }
    Some(Furnished { inside: i, left })
}

/// A finish's tile as a PNG, cut from its chunk's texture page (`chunk`: the chunk file).
pub fn finish_swatch(f: &Finish, chunk: &[u8]) -> Option<Vec<u8>> {
    let mut v = crate::vram::Vram::new();
    v.paint_tim(chunk.get(crate::world::TIM_OFFSET..)?);
    let (w, h) = ((f.uv[2] - f.uv[0]) as u32 + 1, (f.uv[3] - f.uv[1]) as u32 + 1);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = match v.texel(f.tpage, f.clut, f.uv[0] + x as u8, f.uv[1] + y as u8) {
                crate::vram::Texel::Colour(c) => [crate::vram::c5(c), crate::vram::c5(c >> 5), crate::vram::c5(c >> 10), 255],
                _ => [0, 0, 0, 0],
            };
            rgba.extend_from_slice(&c);
        }
    }
    super::obj::encode_png(w, h, &rgba)
}

/* ───────────────────────── Build ───────────────────────── */

/// Wall height over the floor, in three 64-unit bands (research 13 §3.1). Walls between rooms stand two bands
/// high, so the field camera (45° down) still shows the Knight a cell beyond one.
const WALL_H: i16 = 192;
const INNER_BANDS: i16 = 2;
const BAND: i16 = 64;
/// The floor's geometry height (PSX Y, as in 324–326) and the walk heights over it.
pub const FLOOR_Y: i16 = -32;
const WALK_FLOOR: i16 = FLOOR_Y - 1;
const WALK_WALL: i16 = FLOOR_Y - 1 - WALL_H;
/// Brightness of floors and walls before the tint, like retail's darker walls: walls across z (north, south)
/// darker than walls across x.
const FLOOR_SHADE: f32 = 1.0;
const NORTH_SHADE: f32 = 0.78;
const WEST_SHADE: f32 = 0.86;
/// A door's dark opening, and a stairwell's.
const DARK: [u8; 3] = [14, 10, 8];
/// The hatch ladder's wood, before the room's tint.
const LADDER: [u8; 3] = [112, 78, 46];

/// Where half-cells land in a storey's area: the same on every storey, so a stair's foot and head have the same
/// coordinates in the areas below and above.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// Inside cells a half-module.
    pub k: i32,
    /// The lowest half-cell of every storey, a cell in from the area's south-west corner.
    pub lo: [i32; 2],
}

impl Layout {
    pub fn new(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>) -> Layout {
        let scale = d.inside.as_ref().map_or(DEFAULT_SCALE, |i| i.scale);
        let mut lo = [i32::MAX; 2];
        for s in storeys(d) {
            for c in storey_cells(d, plan, s) {
                lo = [lo[0].min(c[0] as i32), lo[1].min(c[1] as i32)];
            }
        }
        Layout { k: cells_per_half(scale) as i32, lo: if lo[0] == i32::MAX { [0, 0] } else { lo } }
    }

    fn origin(&self) -> [i32; 2] {
        [1 - self.lo[0] * self.k, 1 - self.lo[1] * self.k]
    }

    /// The area cell at half-cell `h`'s south-west corner.
    pub fn cell(&self, h: [i16; 2]) -> [i32; 2] {
        let o = self.origin();
        [o[0] + h[0] as i32 * self.k, o[1] + h[1] as i32 * self.k]
    }

    /// The area cell of inside cell `at` (counted from the plan's corner).
    pub fn inside(&self, at: [u8; 2]) -> [i32; 2] {
        let o = self.origin();
        [o[0] + at[0] as i32, o[1] + at[1] as i32]
    }
}

/// A door box in a storey's area (chunk-local units): it fires within 32 units, and the Knight arriving here
/// lands 96 units from it toward `facing` (0 = +z, 2 = +x, 4 = −z, 6 = −x).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoorPoint {
    pub area: u32,
    pub x: i16,
    pub z: i16,
    pub facing: i16,
}

/// The door pairs an inside needs once placed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum DoorLink {
    /// The outside door on `module`'s `side`, to `inside`; the outside end comes from where the house stands.
    Front { module: [i16; 2], side: Side, inside: DoorPoint },
    /// A stair (at half-cell `at`): its foot on `storey` to its head on the one above.
    Stair { storey: i8, at: [u8; 2], lower: DoorPoint, upper: DoorPoint },
    /// The cellar hatch on yard module `at`, stepped onto from `side`, to the foot of its ladder in the cellar.
    Hatch { at: [u8; 2], side: Side, inside: DoorPoint },
}

/// An examine spot an inside needs: an action at a piece of furniture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotPlan {
    /// The area of the piece's storey.
    pub area: u32,
    pub item: String,
    /// The area cell the Knight examines: the piece's cell nearest floor he can stand on.
    pub col: u8,
    pub row: u8,
    /// Which way the Knight faces it (0–4095, 0 = +z), or a bed's wake side.
    pub dir: u16,
    pub action: Action,
}

/// A resident as the game gets them: a copy of `npc/pack` actor `slot` homed on `area` at (`x`, `h`, `z`) (area
/// units), facing `dir` (0–4095), saying `line`, walking `walk` (`edit::walk` words; empty stands).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidentPlan {
    /// Its index in the design's residents.
    pub index: usize,
    pub area: u32,
    pub x: i16,
    pub h: i16,
    pub z: i16,
    pub dir: u16,
    pub pack: u16,
    pub slot: u16,
    pub name: String,
    pub line: String,
    pub walk: Vec<u16>,
}

impl ResidentPlan {
    /// Their talk program, as screen 11's DSL: they turn to the Knight and say the line under their name.
    pub fn dsl(&self) -> String {
        let quoted = |m: &str| format!("\"{}\"", m.replace('\\', "\\\\").replace('"', "\\\""));
        match self.name.trim() {
            "" => format!("face_knight\nsay {}\n", quoted(&self.line)),
            name => format!("face_knight\nspeaker {}\nsay {}\nno_speaker\n", quoted(name), quoted(&self.line)),
        }
    }
}

/// One storey of a built inside: the chunk of its area and what went into it.
#[derive(Debug, Clone)]
pub struct BuiltStorey {
    pub storey: i8,
    pub area: u32,
    pub chunk: super::chunk::Chunk,
    pub polygons: u32,
    pub tiles: u32,
    pub palettes: u32,
    /// Each piece of furniture's draw groups in `chunk`, by its index in the design's furniture.
    pub pieces: Vec<(usize, std::ops::Range<usize>)>,
}

/// The built inside: a chunk per storey, and the doors and spots to link once placed.
#[derive(Debug, Clone)]
pub struct BuiltInside {
    pub storeys: Vec<BuiltStorey>,
    pub doors: Vec<DoorLink>,
    pub spots: Vec<SpotPlan>,
    pub residents: Vec<ResidentPlan>,
    /// What the built rooms refuse that the plan let through (a resident on a cell the walls took).
    pub problems: Vec<Problem>,
}

pub fn facing_of(side: Side) -> i16 {
    match side {
        Side::North => 0,
        Side::East => 2,
        Side::South => 4,
        Side::West => 6,
    }
}

fn opposite(side: Side) -> Side {
    match side {
        Side::North => Side::South,
        Side::South => Side::North,
        Side::East => Side::West,
        Side::West => Side::East,
    }
}

/// A ladder rising from the floor at (`cx`, `cz`) toward the ceiling, a half-cell toward `side` (where the hatch is
/// stepped onto, so the Knight climbs walking back that way): two rails and five rungs, facing the camera.
fn ladder(b: &mut super::house::Builder, cx: i16, cz: i16, side: Side, rgb: [u8; 3]) {
    let (w, rail, top) = (24i16, 4i16, WALL_H);
    let off = 16;
    // A plane across z (seen from the south) or across x (seen from the east).
    let across_z = matches!(side, Side::North | Side::South);
    let at = if across_z { cz + if side == Side::North { off } else { -off } } else { cx + if side == Side::East { off } else { -off } };
    let mut quad = |a: i16, bb: i16, y0: i16, y1: i16| {
        let pts = if across_z {
            [[cx + a, y1, at], [cx + bb, y1, at], [cx + a, y0, at], [cx + bb, y0, at]]
        } else {
            [[at, y1, cz + a], [at, y1, cz + bb], [at, y0, cz + a], [at, y0, cz + bb]]
        };
        b.flat(pts, rgb);
    };
    quad(-w - rail, -w + rail, 0, top);
    quad(w - rail, w + rail, 0, top);
    for k in 1..=5 {
        let y = k * top / 6;
        quad(-w + rail, w - rail, y - 3, y + 3);
    }
}

/// A finish's tile on its page: the rectangle relative to the page's texture, its CLUT, width and height.
fn finish_tile(f: &Finish, page: &super::chunk::Texture) -> super::Result<([u16; 4], u16, u8, u8)> {
    let img = page.image;
    let row0 = ((f.tpage >> 4) & 1) * 256;
    let bad = || super::EditError::new("INSIDE_FINISH", format!("Finish “{}” isn't on chunk {}'s page.", f.id, f.chunk));
    if (f.tpage & 0xF) * 64 != img.x {
        return Err(bad());
    }
    let (y0, y1) = ((row0 + f.uv[1] as u16).checked_sub(img.y).ok_or_else(bad)?, (row0 + f.uv[3] as u16).checked_sub(img.y).ok_or_else(bad)?);
    if y1 >= img.h {
        return Err(bad());
    }
    Ok(([f.uv[0] as u16, y0, f.uv[2] as u16, y1], f.clut, f.uv[2] - f.uv[0], f.uv[3] - f.uv[1]))
}

/// Builds the inside of `d`, each storey into its own new area (`areas`, by storey): floors and walls in their
/// finishes (on every side, facing in, so the field's culling opens the room to the camera from any side),
/// doorways, stairs, furniture from `items` (Interiors prefabs by id), tints and window light, and the walk cells.
/// `pages` holds the retail interiors' textures by chunk (324, 327, 330), `vram` an area's field VRAM.
pub fn build(
    d: &HouseDesign,
    plan: &BTreeMap<[i16; 2], usize>,
    areas: &BTreeMap<i8, u32>,
    pages: &std::collections::HashMap<u32, super::chunk::Texture>,
    items: &std::collections::HashMap<String, super::prefab::Prefab>,
    vram: &dyn Fn(u32) -> Option<crate::vram::Vram>,
) -> super::Result<BuiltInside> {
    use super::chunk::Chunk;
    use super::house::Builder;
    use super::prefab::{self as kit, CellRect, PackCache, PatchCell, Placement, Prefab};
    use super::EditError;

    let i = d.inside.as_ref().ok_or_else(|| EditError::new("INSIDE_NONE", "The design has no inside."))?;
    let lay = Layout::new(d, plan);
    let k = lay.k;
    let all = rooms(d, plan);
    let first = pages.values().next().ok_or_else(|| EditError::new("INSIDE_PAGES", "No interior pages to cut finishes from."))?;
    let area_of = |s: i8| areas.get(&s).copied().ok_or_else(|| EditError::new("INSIDE_AREAS", format!("{} has no area to build into.", storey_name(s))));
    let unit = |c: i32| (c * CELL) as i16;
    let front = super::design::front_doors(d, plan);
    // Window light: the windows of each storey, as points in area units.
    let windows = |s: i8| -> Vec<[f32; 2]> {
        if s < 0 {
            return Vec::new();
        }
        d.walls
            .iter()
            .filter(|w| w.storey as i8 == s && w.kind == super::design::WallKind::Window)
            .map(|w| {
                let [a, c] = door_cells([w.at[0] as i16, w.at[1] as i16], w.side);
                let (p, q) = (lay.cell(a), lay.cell(c));
                let mid = |v: i32, u: i32| ((v + u) as f32 / 2.0 + k as f32 / 2.0) * CELL as f32;
                let edge = |cell: i32, side_far: bool| (cell + if side_far { k } else { 0 }) as f32 * CELL as f32;
                match w.side {
                    Side::South => [mid(p[0], q[0]), edge(p[1], false)],
                    Side::North => [mid(p[0], q[0]), edge(p[1], true)],
                    Side::West => [edge(p[0], false), mid(p[1], q[1])],
                    Side::East => [edge(p[0], true), mid(p[1], q[1])],
                }
            })
            .collect()
    };
    let light = |wins: &[[f32; 2]], x: f32, z: f32| -> f32 {
        let d = wins.iter().map(|w| ((w[0] - x).powi(2) + (w[1] - z).powi(2)).sqrt()).fold(f32::MAX, f32::min);
        if d == f32::MAX {
            return 1.0;
        }
        let (near, far) = (2.0 * k as f32 * CELL as f32, 6.0 * k as f32 * CELL as f32);
        1.15 - 0.35 * ((d - near) / (far - near)).clamp(0.0, 1.0)
    };
    // Firelight: warmer and brighter near a lit fire, fading out over three modules.
    let glow = |fires: &[[f32; 2]], x: f32, z: f32| -> [f32; 3] {
        let d = fires.iter().map(|f| ((f[0] - x).powi(2) + (f[1] - z).powi(2)).sqrt()).fold(f32::MAX, f32::min);
        if d == f32::MAX {
            return [1.0; 3];
        }
        let t = 1.0 - (d / (6.0 * k as f32 * CELL as f32)).clamp(0.0, 1.0);
        [1.0 + 0.30 * t, 1.0 + 0.12 * t, 1.0 - 0.10 * t]
    };
    let colour = |tint: [u8; 3], f: f32, g: [f32; 3]| [0, 1, 2].map(|c| (tint[c] as f32 * f * g[c]).round().clamp(0.0, 255.0) as u8);

    let mut built = Vec::new();
    let mut spots = Vec::new();
    let mut residents = Vec::new();
    let mut problems = Vec::new();
    let mut actions: Vec<(SpotPlan, [i32; 2], [i32; 2])> = Vec::new();
    for s in storeys(d) {
        let area = area_of(s)?;
        let mut b = Builder::new(first);
        let cells = storey_cells(d, plan, s);
        let wins = windows(s);
        let walls: BTreeMap<([i16; 2], [i16; 2]), bool> = live_walls(i, &cells, s)
            .flat_map(|w| {
                let [a, c] = w.sides();
                [((a, c), w.door), ((c, a), w.door)]
            })
            .collect();
        // The lit fires: where each burns (area units) and the half-cell it stands on, so its room takes the light.
        let fires: Vec<([f32; 2], [i16; 2])> = i
            .furniture
            .iter()
            .filter(|f| f.storey == s && f.lit && FIRES.contains(&f.item.as_str()))
            .filter_map(|f| {
                let (fw, fh) = kit::footprint(items.get(&f.item)?, f.rot);
                let at = lay.inside(f.at);
                let c = [at[0] * CELL + fw as i32 * CELL / 2, at[1] * CELL + fh as i32 * CELL / 2];
                Some(([c[0] as f32, c[1] as f32], [((f.at[0] as i32 + fw as i32 / 2) / k) as i16, ((f.at[1] as i32 + fh as i32 / 2) / k) as i16]))
            })
            .collect();
        let warm_in = |r: &Room| -> Vec<[f32; 2]> { fires.iter().filter(|(_, h)| r.cells.contains(h)).map(|(p, _)| *p).collect() };
        for r in all.iter().filter(|r| r.storey == s) {
            let (floor, wall, tint, windowed, _) = room_look(i, r);
            let (ff, fw) = (finish(floor).ok_or_else(|| EditError::new("ROOM_FINISH", format!("No finish {floor}.")))?, finish(wall).ok_or_else(|| EditError::new("ROOM_FINISH", format!("No finish {wall}.")))?);
            let tint = tint.rgb();
            let warm = warm_in(r);
            // Gouraud when the light changes across the room: from the windows, or from a fire.
            let lit = windowed || !warm.is_empty();
            let rgb4 = |pts: [[i16; 3]; 4], shade: f32| -> [[u8; 3]; 4] {
                pts.map(|p| {
                    let (x, z) = (p[0] as f32, p[2] as f32);
                    colour(tint, shade * if windowed { light(&wins, x, z) } else { 1.0 }, glow(&warm, x, z))
                })
            };
            for &h in &r.cells {
                let c0 = lay.cell(h);
                let (x0, z0, x1, z1) = (unit(c0[0]), unit(c0[1]), unit(c0[0] + k), unit(c0[1] + k));
                // The floor: the finish's whole tile over the half-cell.
                let page = &pages[&ff.chunk];
                b.set_page(page);
                let (rect, clut, w, hh) = finish_tile(ff, page)?;
                let pts = [[x0, 0, z1], [x1, 0, z1], [x0, 0, z0], [x1, 0, z0]];
                b.quad_rgb(pts, rect, clut, [[0, 0], [w, 0], [0, hh], [w, hh]], rgb4(pts, FLOOR_SHADE), lit)?;
                // Walls on every edge that leaves the room, facing in, as retail rooms are boxed: the field culls
                // whichever face away from the camera, so the room stays cut away however L1 and R1 turn it.
                for side in [Side::North, Side::West, Side::South, Side::East] {
                    let n = step(h, side);
                    if r.cells.contains(&n) {
                        continue;
                    }
                    let inner = cells.contains(&n);
                    let door = if inner {
                        walls.get(&(h, n)).copied().unwrap_or(false)
                    } else {
                        s == 0 && front.iter().any(|&(m, fs)| fs == side && door_cells(m, fs).contains(&h))
                    };
                    // Corners left to right as seen from inside the room, looking at the wall.
                    let (a, c) = match side {
                        Side::North => ([x0, z1], [x1, z1]),
                        Side::West => ([x0, z0], [x0, z1]),
                        Side::South => ([x1, z0], [x0, z0]),
                        Side::East => ([x1, z1], [x1, z0]),
                    };
                    let shade = if matches!(side, Side::North | Side::South) { NORTH_SHADE } else { WEST_SHADE };
                    let wp = &pages[&fw.chunk];
                    b.set_page(wp);
                    let (rect, clut, w, hh) = finish_tile(fw, wp)?;
                    // Walls between rooms are lower, and their doorways are plain gaps; the outline keeps its lintel.
                    let bands = if inner { INNER_BANDS } else { 3 };
                    for band in 0..bands {
                        if door && (inner || band < 2) {
                            continue;
                        }
                        let (y0, y1) = (band * BAND, (band + 1) * BAND);
                        // Texture rows top to bottom: the top band shows the tile's first third.
                        let (v0, v1) = (((2 - band) as u16 * (hh as u16 + 1) / 3) as u8, (((3 - band) as u16 * (hh as u16 + 1)) / 3 - 1) as u8);
                        let pts = [[a[0], y1, a[1]], [c[0], y1, c[1]], [a[0], y0, a[1]], [c[0], y0, c[1]]];
                        b.quad_rgb(pts, rect, clut, [[0, v0], [w, v0], [0, v1], [w, v1]], rgb4(pts, shade), lit)?;
                    }
                    if door && !inner {
                        // The front door: the dark way out.
                        b.flat([[a[0], 2 * BAND, a[1]], [c[0], 2 * BAND, c[1]], [a[0], 0, a[1]], [c[0], 0, c[1]]], DARK);
                    }
                    // A wall between rooms gets its top, black, half a cell deep behind it (corners north-west,
                    // north-east, south-west, south-east, facing up); with the other room's, the wall's thickness. The
                    // outline has none, as retail rooms have none: beyond it is the void, and the top of a near wall
                    // the camera culls would hang across the room.
                    if !inner || door {
                        continue;
                    }
                    let t = bands * BAND;
                    let top = match side {
                        Side::North => [[x0, t, z1 + 16], [x1, t, z1 + 16], [x0, t, z1], [x1, t, z1]],
                        Side::West => [[x0 - 16, t, z1], [x0, t, z1], [x0 - 16, t, z0], [x0, t, z0]],
                        Side::South => [[x0, t, z0], [x1, t, z0], [x0, t, z0 - 16], [x1, t, z0 - 16]],
                        Side::East => [[x1, t, z1], [x1 + 16, t, z1], [x1, t, z0], [x1 + 16, t, z0]],
                    };
                    b.flat(top, [0, 0, 0]);
                }
            }
        }
        // Stairs: steps up on their storey, a dark well on the one above.
        for st in i.stairs.iter().filter(|st| st.storey == s || st.storey + 1 == s) {
            let [foot, head] = st.cells();
            let lowest = [foot[0].min(head[0]), foot[1].min(head[1])];
            let c0 = lay.cell(lowest);
            let (sx, sz) = if matches!(st.rises, Side::North | Side::South) { (k, 2 * k) } else { (2 * k, k) };
            let (x0, z0, x1, z1) = (unit(c0[0]), unit(c0[1]), unit(c0[0] + sx), unit(c0[1] + sz));
            if st.storey + 1 == s {
                b.flat([[x0, 1, z1], [x1, 1, z1], [x0, 1, z0], [x1, 1, z0]], DARK);
                continue;
            }
            let r = all.iter().find(|r| r.storey == s && r.cells.contains(&foot));
            let (floor, _, tint, _, _) = r.map(|r| room_look(i, r)).unwrap_or(("planks", "logs", Tint::Daylight, false, ""));
            let ff = finish(floor).unwrap_or(&FINISHES[3]);
            let page = &pages[&ff.chunk];
            b.set_page(page);
            let (rect, clut, w, hh) = finish_tile(ff, page)?;
            // Four steps from the foot to the head, each a tread and a riser.
            let steps = 4i16;
            for n in 0..steps {
                let rise = (n + 1) * (WALL_H / steps);
                let (a, c) = (n as f32 / steps as f32, (n + 1) as f32 / steps as f32);
                let lerp = |p: i16, q: i16, t: f32| (p as f32 + (q - p) as f32 * t).round() as i16;
                let (tx0, tz0, tx1, tz1) = match st.rises {
                    Side::North => (x0, lerp(z0, z1, a), x1, lerp(z0, z1, c)),
                    Side::South => (x0, lerp(z1, z0, c), x1, lerp(z1, z0, a)),
                    Side::East => (lerp(x0, x1, a), z0, lerp(x0, x1, c), z1),
                    Side::West => (lerp(x1, x0, c), z0, lerp(x1, x0, a), z1),
                };
                let tread = [[tx0, rise, tz1], [tx1, rise, tz1], [tx0, rise, tz0], [tx1, rise, tz0]];
                b.quad_rgb(tread, rect, clut, [[0, 0], [w, 0], [0, hh / 4], [w, hh / 4]], [colour(tint.rgb(), 1.0, [1.0; 3]); 4], false)?;
                let (y0, y1) = (rise - WALL_H / steps, rise);
                let riser = match st.rises {
                    Side::North => [[tx0, y1, tz0], [tx1, y1, tz0], [tx0, y0, tz0], [tx1, y0, tz0]],
                    Side::South => [[tx1, y1, tz1], [tx0, y1, tz1], [tx1, y0, tz1], [tx0, y0, tz1]],
                    Side::East => [[tx0, y1, tz1], [tx0, y1, tz0], [tx0, y0, tz1], [tx0, y0, tz0]],
                    Side::West => [[tx1, y1, tz0], [tx1, y1, tz1], [tx1, y0, tz0], [tx1, y0, tz1]],
                };
                b.quad_rgb(riser, rect, clut, [[0, 0], [w, 0], [0, hh / 4], [w, hh / 4]], [colour(tint.rgb(), 0.8, [1.0; 3]); 4], false)?;
            }
        }

        // The hatch's ladder in the cellar: at the far edge of the box under the hatch, facing the way in.
        if let (true, Some(h)) = (s < 0, i.cellar.as_ref().and_then(|c| c.hatch)) {
            let c0 = lay.cell([h.at[0] as i16 * 2 + 1, h.at[1] as i16 * 2 + 1]);
            let (cx, cz) = (unit(c0[0]), unit(c0[1]));
            let r = all.iter().find(|r| r.storey == s && r.cells.contains(&[h.at[0] as i16 * 2, h.at[1] as i16 * 2]));
            let t = r.map_or(Tint::Dim, |r| room_look(i, r).2).rgb();
            let rgb = [0, 1, 2].map(|c| (LADDER[c] as u32 * t[c] as u32 / 128).min(255) as u8);
            ladder(&mut b, cx, cz, h.side, rgb);
        }

        // The storey's rooms as one prefab over the whole chunk, placed on the floor height.
        let shell = Prefab {
            name: d.name.clone(),
            source_chunk: area,
            area: CellRect { c0: 0, r0: 0, c1: 127, r1: 127 },
            ground_y: FLOOR_Y,
            pool: b.pool,
            prims: b.prims,
            tiles: b.tiles,
            heights: vec![PatchCell::Rel(0); 128 * 128],
            category: String::new(),
            stock: None,
            stock_rev: 0,
            design: None,
        };
        let mut c = Chunk::blank(area);
        c.full_page();
        let mut cache = match vram(area) {
            Some(v) => PackCache::with_vram(v),
            None => PackCache::default(),
        };
        let at = Placement { col: 0, row: 0, rot: 0, ground_y: Some(FLOOR_Y), write_ground: false, lift: 0 };
        let placed = kit::place_with(&mut c, &shell, at, &mut cache)?;
        let (mut polygons, mut tiles, mut palettes) = (placed.polygons, placed.tiles, placed.cluts_new);

        // Walk cells: void, then the floor, the wall ring round it, inner walls on their far side, stairs.
        c.heights = vec![-1; 128 * 128];
        c.zones = vec![1; 128 * 128];
        let set = |c: &mut Chunk, col: i32, row: i32, h: i16| {
            if (0..128).contains(&col) && (0..128).contains(&row) {
                c.heights[(row * 128 + col) as usize] = h;
            }
        };
        let mut floor: BTreeSet<[i32; 2]> = BTreeSet::new();
        for &h in &cells {
            let c0 = lay.cell(h);
            for j in 0..k {
                for q in 0..k {
                    floor.insert([c0[0] + q, c0[1] + j]);
                }
            }
        }
        for f in &floor {
            set(&mut c, f[0], f[1], WALK_FLOOR);
        }
        for f in &floor {
            for (dx, dz) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let n = [f[0] + dx, f[1] + dz];
                if !floor.contains(&n) {
                    set(&mut c, n[0], n[1], WALK_WALL);
                }
            }
        }
        for w in live_walls(i, &cells, s).filter(|w| !w.door) {
            // The row of cells just beyond the wall's line, on its far (north or west) side.
            let [_, far] = w.sides();
            let c0 = lay.cell(far);
            for q in 0..k {
                match w.along {
                    Axis::X => set(&mut c, c0[0] + q, c0[1], WALK_WALL),
                    Axis::Z => set(&mut c, c0[0], c0[1] + q, WALK_WALL),
                }
            }
        }
        for st in &i.stairs {
            let [foot, head] = st.cells();
            let (cell, h) = if st.storey == s { (head, WALK_WALL) } else if st.storey + 1 == s { (foot, -1) } else { continue };
            let c0 = lay.cell(cell);
            for j in 0..k {
                for q in 0..k {
                    set(&mut c, c0[0] + q, c0[1] + j, h);
                }
            }
        }

        // Furniture, tinted with its room and lit by its windows.
        let mut pieces = Vec::new();
        for (fi, f) in i.furniture.iter().enumerate().filter(|(_, f)| f.storey == s) {
            let Some(p) = items.get(&f.item) else {
                return Err(EditError::new("FURNITURE_MISSING", format!("The {} isn't loaded from the disc.", f.item)));
            };
            let at = lay.inside(f.at);
            let (fw, fh) = kit::footprint(p, f.rot);
            let centre = [at[0] * CELL + fw as i32 * CELL / 2, at[1] * CELL + fh as i32 * CELL / 2];
            let half = [((f.at[0] as i32 + fw as i32 / 2) / k) as i16, ((f.at[1] as i32 + fh as i32 / 2) / k) as i16];
            let room = all.iter().find(|r| r.storey == s && r.cells.contains(&half));
            let (tint, lit) = room.map(|r| room_look(i, r)).map_or((Tint::Daylight, false), |l| (l.2, l.3));
            let factor = if lit { light(&wins, centre[0] as f32, centre[1] as f32) } else { 1.0 };
            let g = room.map_or([1.0; 3], |r| glow(&warm_in(r), centre[0] as f32, centre[1] as f32));
            let mut p = p.clone();
            let t = tint.rgb();
            for q in &mut p.prims {
                for v in &mut q.prim.rgb {
                    for ch in 0..3 {
                        v[ch] = (v[ch] as f32 * t[ch] as f32 / 128.0 * factor * g[ch]).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
            if !(0..=127).contains(&at[0]) || !(0..=127).contains(&at[1]) {
                return Err(EditError::new("FURNITURE_OFF", format!("A piece ({}) stands outside the inside area.", f.item)));
            }
            let g0 = c.groups.len();
            let placed = kit::place_with(&mut c, &p, Placement { col: at[0] as u8, row: at[1] as u8, rot: f.rot, ground_y: Some(FLOOR_Y), write_ground: false, lift: 0 }, &mut cache)?;
            pieces.push((fi, g0..c.groups.len()));
            polygons += placed.polygons;
            tiles += placed.tiles;
            palettes += placed.cluts_new;
            if let Some(action) = &f.action {
                let dir = match action {
                    Action::Rest { wake } => facing_of(*wake) as u16 * 512,
                    _ => 0,
                };
                actions.push((SpotPlan { area, item: f.item.clone(), col: (centre[0] / CELL) as u8, row: (centre[1] / CELL) as u8, dir, action: action.clone() }, at, [fw as i32, fh as i32]));
            }
        }
        // Each action's spot: the cell of its piece nearest floor the Knight can stand on (a bed's wake side
        // first), so a deep piece, or one with another in front of it, is still in reach. None in reach: the
        // centre, and the placement says so.
        let walk = crate::world::walk::Walk::new(&c.heights, false, &[]);
        for (mut sp, at, size) in actions.drain(..) {
            let centre = [at[0] * CELL + size[0] * CELL / 2, at[1] * CELL + size[1] * CELL / 2];
            let wake = match &sp.action {
                Action::Rest { wake } => Some(*wake),
                _ => None,
            };
            let on_side = |cell: [i32; 2], side: Side| match side {
                Side::North => cell[1] == at[1] + size[1] - 1,
                Side::South => cell[1] == at[1],
                Side::East => cell[0] == at[0] + size[0] - 1,
                Side::West => cell[0] == at[0],
            };
            let best = (0..size[1])
                .flat_map(|j| (0..size[0]).map(move |q| [at[0] + q, at[1] + j]))
                .filter_map(|cell| {
                    let (x, z) = (cell[0] * CELL + CELL / 2, cell[1] * CELL + CELL / 2);
                    let ((px, pz), _) = super::spot::reach(&walk, x - 2048, z - 2048)?;
                    let gap = (px as i32 + 2048 - x).pow(2) + (pz as i32 + 2048 - z).pow(2);
                    let off = (x - centre[0]).pow(2) + (z - centre[1]).pow(2);
                    Some(((wake.is_some_and(|w| !on_side(cell, w)), gap, off), cell))
                })
                .min();
            if let Some((_, cell)) = best {
                (sp.col, sp.row) = (cell[0] as u8, cell[1] as u8);
            }
            spots.push(sp);
        }
        // Residents: on floor (walls and stairs are raised cells), every cell of the route too. People aren't kept
        // from walls as the Knight is, so a cell beside one will do.
        for (index, r) in i.residents.iter().enumerate().filter(|(_, r)| r.storey == s) {
            let who = if r.name.trim().is_empty() { format!("Resident {}", index + 1) } else { r.name.trim().to_string() };
            let cell = |c: [u8; 2]| lay.inside(c);
            let centre = |c: [i32; 2]| ((c[0] * CELL + CELL / 2 - 2048) as i16, (c[1] * CELL + CELL / 2 - 2048) as i16);
            let at = cell(r.at);
            let (x, z) = centre(at);
            let module = Some([(r.at[0] as i32 / k / 2) as u8, (r.at[1] as i32 / k / 2) as u8]);
            let Ok(h) = walk.stand(x, z) else {
                problems.push(Problem { error: true, code: "RESIDENT_OFF_FLOOR".into(), message: format!("{who} stands where the built room has a wall or stairs: move them onto the floor."), at: module });
                continue;
            };
            let route: Vec<[i32; 2]> = r.route.iter().map(|&p| cell(p)).collect();
            let mut from = at;
            let mut clear = true;
            for &p in &route {
                if !route_cells(from, p).iter().all(|&c| walk.stand(centre(c).0, centre(c).1).is_ok()) {
                    clear = false;
                }
                from = p;
            }
            if !clear {
                problems.push(Problem { error: true, code: "RESIDENT_ROUTE_BLOCKED".into(), message: format!("{who}'s route crosses a wall or stairs in the built room: add a stop to walk round it."), at: module });
                continue;
            }
            let Some(walk_words) = resident_walk(at, &route) else { continue };
            residents.push(ResidentPlan {
                index,
                area,
                x,
                h,
                z,
                dir: (r.facing as u16 % 8) * 512,
                pack: r.pack,
                slot: r.slot,
                name: r.name.trim().to_string(),
                line: r.line.clone(),
                walk: walk_words,
            });
        }
        built.push(BuiltStorey { storey: s, area, chunk: c, polygons, tiles, palettes, pieces });
    }

    // Door boxes: the front doors just inside their wall, the stairs at their foot and head.
    let world = |u: i32| (u - 2048) as i16;
    let mut doors = Vec::new();
    let ground = area_of(0)?;
    for &(m, side) in &front {
        let [a, b2] = door_cells(m, side);
        let (p, q) = (lay.cell(a), lay.cell(b2));
        let mid = |u: i32, v: i32| (u + v + k) * CELL / 2;
        let inward = 16;
        let (x, z) = match side {
            Side::South => (mid(p[0], q[0]), p[1] * CELL + inward),
            Side::North => (mid(p[0], q[0]), (p[1] + k) * CELL - inward),
            Side::West => (p[0] * CELL + inward, mid(p[1], q[1])),
            Side::East => ((p[0] + k) * CELL - inward, mid(p[1], q[1])),
        };
        doors.push(DoorLink::Front { module: m, side, inside: DoorPoint { area: ground, x: world(x), z: world(z), facing: facing_of(opposite(side)) } });
    }
    if let Some(h) = i.cellar.as_ref().and_then(|c| c.hatch) {
        let c0 = lay.cell([h.at[0] as i16 * 2 + 1, h.at[1] as i16 * 2 + 1]);
        let inside = DoorPoint { area: area_of(-1)?, x: world(c0[0] * CELL), z: world(c0[1] * CELL), facing: facing_of(opposite(h.side)) };
        doors.push(DoorLink::Hatch { at: h.at, side: h.side, inside });
    }
    // Stairs from the ground floor up first, then down to the cellar: each links a storey that already has a way in.
    let mut stairs: Vec<&Stair> = i.stairs.iter().collect();
    stairs.sort_by_key(|st| (st.storey < 0, st.storey));
    for st in stairs {
        let [foot, head] = st.cells();
        let centre = |h: [i16; 2]| {
            let c0 = lay.cell(h);
            (world(c0[0] * CELL + k * CELL / 2), world(c0[1] * CELL + k * CELL / 2))
        };
        let (lx, lz) = centre(foot);
        let (ux, uz) = centre(head);
        doors.push(DoorLink::Stair {
            storey: st.storey,
            at: st.at,
            lower: DoorPoint { area: area_of(st.storey)?, x: lx, z: lz, facing: facing_of(opposite(st.rises)) },
            upper: DoorPoint { area: area_of(st.storey + 1)?, x: ux, z: uz, facing: facing_of(st.rises) },
        });
    }
    Ok(BuiltInside { storeys: built, doors, spots, residents, problems })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::design::{presets, HouseDesign};

    fn plan_of(d: &HouseDesign) -> BTreeMap<[i16; 2], usize> {
        let mut out = BTreeMap::new();
        for (k, w) in d.wings.iter().enumerate() {
            let (sw, sh) = if w.along == Axis::X { (w.length, 2) } else { (2, w.length) };
            for z in 0..sh as i16 {
                for x in 0..sw as i16 {
                    out.entry([w.at[0] as i16 + x, w.at[1] as i16 + z]).or_insert(k);
                }
            }
        }
        out
    }

    fn two_storey() -> HouseDesign {
        presets().into_iter().find(|(k, _)| k == "two-storey").unwrap().1
    }

    #[test]
    fn templates_furnish_against_the_walls_and_keep_the_ways_clear() {
        let mut d = two_storey();
        let walls = (0..4u8).map(|z| InnerWall { storey: 0, at: [2, z], along: Axis::Z, door: z == 1 }).collect();
        d.inside = Some(Inside { walls, ..Inside::default() });
        let plan = plan_of(&d);
        let sizes = |id: &str| crate::edit::stock::interior_size(id);
        let front = [([1, 0], Side::South)];
        for (id, cell) in [("kitchen", [3, 0]), ("bedroom", [0, 0])] {
            let room = rooms(&d, &plan).into_iter().find(|r| r.storey == 0 && r.cells.contains(&cell)).unwrap();
            let f = apply_template(&d, &plan, 0, room.key, template(id).unwrap(), &sizes, &[0x130, 0x131]).unwrap();
            let set = f.inside.rooms.iter().find(|r| r.template.as_deref() == Some(id)).unwrap();
            assert_eq!((set.floor.as_str(), set.walls.as_str()), (template(id).unwrap().floor, template(id).unwrap().walls));
            assert!(f.inside.furniture.len() >= 2, "{id}: {:?} left out {:?}", f.inside.furniture, f.left);
            // Every piece in the room, none in the way, the chests on flags of their own.
            let mut done = d.clone();
            done.inside = Some(f.inside.clone());
            let mut out = Vec::new();
            check(&done, &plan, &front, &sizes, &mut out);
            assert!(out.iter().all(|p| !p.error), "{id}: {out:?}");
            d = done;
        }
        let i = d.inside.as_ref().unwrap();
        assert!(i.furniture.iter().any(|f| matches!(f.action, Some(Action::Rest { .. }))));
        // Applied again, the room's furniture is replaced, not doubled.
        let room = rooms(&d, &plan).into_iter().find(|r| r.storey == 0 && r.cells.contains(&[0, 0])).unwrap();
        let again = apply_template(&d, &plan, 0, room.key, template("bedroom").unwrap(), &sizes, &[0x130, 0x131]).unwrap();
        assert_eq!(again.inside.furniture.len(), i.furniture.len());
    }

    #[test]
    fn walls_split_a_storey_into_rooms_and_doorways_join_them() {
        // The two-storey cottage is 3 × 2 modules: 6 × 4 half-cells a storey.
        let mut d = two_storey();
        let plan = plan_of(&d);
        // A wall across the ground floor at x = 2 half-modules, with a doorway in its second half-module.
        let walls = (0..4u8).map(|z| InnerWall { storey: 0, at: [2, z], along: Axis::Z, door: z == 1 }).collect();
        d.inside = Some(Inside { walls, ..Inside::default() });
        let r = rooms(&d, &plan);
        let ground: Vec<&Room> = r.iter().filter(|r| r.storey == 0).collect();
        assert_eq!(ground.len(), 2);
        assert_eq!((ground[0].cells.len(), ground[1].cells.len()), (8, 16));
        assert_eq!(r.iter().filter(|r| r.storey == 1).count(), 1);
        // The front door (module 1, south) opens into the second room; the doorway reaches the first.
        let front = [([1, 0], Side::South)];
        let all = rooms(&d, &plan);
        assert_eq!(reachable(&d, &plan, &all, &front).len(), 2, "the upper floor has no stairs yet");
        d.inside.as_mut().unwrap().stairs.push(Stair { storey: 0, at: [4, 1], rises: Side::North });
        assert_eq!(reachable(&d, &plan, &all, &front).len(), 3);
    }

    #[test]
    fn bad_insides_are_named() {
        let d0 = two_storey();
        let plan = plan_of(&d0);
        let items = |id: &str| (id == "bed-quilt").then_some([4u8, 5u8]);
        let front = [([1, 0], Side::South)];
        let codes = |f: &dyn Fn(&mut Inside)| {
            let mut d = d0.clone();
            let mut i = Inside::default();
            f(&mut i);
            d.inside = Some(i);
            let mut out = Vec::new();
            check(&d, &plan, &front, &items, &mut out);
            out.into_iter().filter(|p| p.error).map(|p| p.code).collect::<Vec<_>>()
        };
        assert_eq!(codes(&|_| {}), Vec::<String>::new());
        assert_eq!(codes(&|i| i.scale = 120), ["INSIDE_SCALE"]);
        assert_eq!(codes(&|i| i.stairs.push(Stair { storey: 1, at: [0, 0], rises: Side::North })), ["STAIR_OFF"]);
        assert_eq!(codes(&|i| i.furniture.push(Furniture { storey: 0, item: "dragon".into(), at: [0, 0], rot: 0, action: None, lit: false })), ["FURNITURE_UNKNOWN"]);
        // 3 inside cells a half-module at 1.5×: the 6 × 4 half-cell storey is 18 × 12 cells.
        assert_eq!(codes(&|i| i.furniture.push(Furniture { storey: 0, item: "bed-quilt".into(), at: [15, 0], rot: 0, action: None, lit: false })), ["FURNITURE_OFF"]);
        assert_eq!(codes(&|i| i.furniture.push(Furniture { storey: 0, item: "bed-quilt".into(), at: [0, 0], rot: 0, action: Some(Action::Chest { item: 3, flag: 0 }), lit: false })), ["CHEST_FLAG"]);
        assert_eq!(codes(&|i| i.cellar = Some(Cellar { outline: vec![[0, 0, 3, 2]], hatch: None })), ["CELLAR_NO_WAY"]);
        // The door on module (1, 0) south has half-cells (2, 0) and (3, 0): a wall between them splits it.
        assert_eq!(codes(&|i| i.walls.push(InnerWall { storey: 0, at: [3, 0], along: Axis::Z, door: false })), ["INNER_WALL_DOOR"]);
        assert_eq!(codes(&|i| i.walls.push(InnerWall { storey: 1, at: [3, 0], along: Axis::Z, door: false })), Vec::<String>::new());
        // A hatch needs the plot: on the house, then on a plot two modules east of it.
        let hatch = |at: [u8; 2], side: Side, outline: [u8; 4]| move |i: &mut Inside| i.cellar = Some(Cellar { outline: vec![outline], hatch: Some(Hatch { at, side }) });
        assert_eq!(codes(&hatch([1, 0], Side::East, [0, 0, 4, 2])), ["HATCH_OFF_PLOT"]);
        let yard = |d: &mut HouseDesign| d.yard = Some(super::super::design::Yard { at: [0, 0], size: [5, 2], fence: false, gates: Vec::new(), patches: Vec::new(), props: Vec::new() });
        let yard_codes = |f: &dyn Fn(&mut Inside)| {
            let mut d = d0.clone();
            yard(&mut d);
            let mut i = Inside::default();
            f(&mut i);
            d.inside = Some(i);
            let mut out = Vec::new();
            check(&d, &plan, &front, &items, &mut out);
            out.into_iter().filter(|p| p.error).map(|p| p.code).collect::<Vec<_>>()
        };
        assert_eq!(yard_codes(&hatch([3, 1], Side::East, [0, 0, 4, 2])), Vec::<String>::new());
        assert_eq!(yard_codes(&hatch([3, 1], Side::West, [0, 0, 4, 2])), ["HATCH_SIDE"]);
        assert_eq!(yard_codes(&hatch([4, 1], Side::West, [0, 0, 4, 2])), ["HATCH_NO_CELLAR"]);
        let mut d = d0.clone();
        d.inside = Some(Inside::default());
        let mut out = Vec::new();
        check(&d, &plan, &[], &items, &mut out);
        assert!(out.iter().any(|p| p.code == "INSIDE_NO_DOOR"));
    }
}
