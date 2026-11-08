//! House workbench designs (screen 23, `docs/screens/23-house-workbench.md`):
//! a floor plan of wings on the 128-unit module grid, built into one house
//! prefab in a style ([`super::style`]): the village's log houses, Trisken's
//! half-timber, San Claria's plaster or the stone houses by Amos's inn.
//!
//! - **Wings** are 2 modules deep and 1–8 long, along x or z. Wings touch
//!   but don't overlap: an end against another wing's side joins them (an L,
//!   a T), an end against another's end continues the house, and sides may lie
//!   along each other.
//! - **Walls**: every module edge between the house and the outside gets a
//!   wall per storey (the style's rows, 128 units), log ends where a run of
//!   wall turns, and the style's bands on top of the top storey. A door is an
//!   alcove whose lintel slopes into the module's corners, so it adds no
//!   vertex to the edges it shares with the trim or the storey above.
//! - **Wings of their own height**: a wing may stand fewer storeys than the
//!   house. The taller wing's wall then stands over the lower one's roof, cut
//!   along it, and the lower roof stops against it.
//! - **Roof**: each wing is a prism along its length, rising the style's pitch
//!   per module from the eave to a ridge one module in; hipped, its open ends
//!   slope too. Where a wing's end meets the side of a wing as tall, its
//!   prism runs on into that wing up to its ridge. The roof is the upper
//!   envelope of the prisms: per module the highest of the wings, each the
//!   lowest of its slopes, which makes the valleys of the retail L houses and
//!   the hips of San Claria's. A module one plane covers is a quad; one a
//!   valley or a hip crosses is two triangles split on that diagonal. Gables
//!   fill every outside edge the roof stands above the eave.
//! - Every face goes through one vertex index, so faces that meet share their
//!   corners, and [`weld`] splits any edge another face's corner lies on (no
//!   T-junctions: they crack on the PS1).
//!
//! Plans are in modules: `x` east, `z` north (the editor's top view, north up).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::chunk::Texture;
use super::ground::{self, Material};
use super::inside::{self, Inside};
use super::house::{self, Builder, Ends, Shade, EAST, M, NORTH, ROOF_NORTH, ROOF_SOUTH, SOUTH, WALL, WEST};
use super::prefab::{self as kit, CellRect, PatchCell, Prefab, PrefabPrim, PrimData, Tile, TileOrigin};
use super::style::{self, Ink, Retail, RoofShape, Row, Slot, Style, Tex, Window};
use super::{EditError, Result};

/// The style a design starts in: log house, Knight's village.
pub const STYLE: &str = "log-village";
/// The plan grid's side, in modules.
pub const PLAN: u8 = 16;
/// A house's largest size, in modules.
pub const MAX_SIZE: u8 = 8;
pub const MAX_LENGTH: u8 = 8;
pub const MAX_STOREYS: u8 = 3;
/// The Library shelf designs go on.
pub const CATEGORY: &str = "My houses";
/// Polygons where the cost bar turns amber, and red.
pub const POLYS_AMBER: u32 = 300;
pub const POLYS_RED: u32 = 600;

const ROOF_WEST: Shade = Shade(135);
const ROOF_EAST: Shade = Shade(112);
/// The dark inside of a doorway.
const DOORWAY: [u8; 3] = [14, 10, 8];
/// A recessed window's sill and sides.
const SILL: [u8; 3] = [70, 62, 52];
const REVEAL: [u8; 3] = [34, 28, 24];
/// Height of a door's opening.
const DOOR_TOP: i16 = 112;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    Z,
}

/// Which way a wall faces: north is +z, east +x.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    North,
    East,
    South,
    West,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::North, Side::East, Side::South, Side::West];

    pub fn name(self) -> &'static str {
        match self {
            Side::North => "north",
            Side::East => "east",
            Side::South => "south",
            Side::West => "west",
        }
    }

    /// The module across this side.
    fn step(self, m: [i16; 2]) -> [i16; 2] {
        match self {
            Side::North => [m[0], m[1] + 1],
            Side::East => [m[0] + 1, m[1]],
            Side::South => [m[0], m[1] - 1],
            Side::West => [m[0] - 1, m[1]],
        }
    }

    /// Seen from outside, the sides to the wall's left and right.
    fn left_right(self) -> (Side, Side) {
        match self {
            Side::South => (Side::West, Side::East),
            Side::North => (Side::East, Side::West),
            Side::East => (Side::South, Side::North),
            Side::West => (Side::North, Side::South),
        }
    }

    fn shade(self) -> Shade {
        match self {
            Side::North => NORTH,
            Side::East => EAST,
            Side::South => SOUTH,
            Side::West => WEST,
        }
    }
}

/// One wing: `length` modules along `along`, 2 deep across it, from its min corner `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Wing {
    pub at: [u8; 2],
    pub length: u8,
    pub along: Axis,
    /// Fewer storeys than the house, for a lower wing; `None` stands as tall as the house.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storeys: Option<u8>,
}

impl Wing {
    /// Size in modules, x then z.
    pub fn size(&self) -> (u8, u8) {
        match self.along {
            Axis::X => (self.length, 2),
            Axis::Z => (2, self.length),
        }
    }

    fn modules(&self) -> Vec<[i16; 2]> {
        let (w, h) = self.size();
        (0..h as i16).flat_map(|z| (0..w as i16).map(move |x| [self.at[0] as i16 + x, self.at[1] as i16 + z])).collect()
    }

    /// The two module columns just past each end: (low end, high end).
    fn beyond_ends(&self) -> [[[i16; 2]; 2]; 2] {
        let (x, z, n) = (self.at[0] as i16, self.at[1] as i16, self.length as i16);
        match self.along {
            Axis::X => [[[x - 1, z], [x - 1, z + 1]], [[x + n, z], [x + n, z + 1]]],
            Axis::Z => [[[x, z - 1], [x + 1, z - 1]], [[x, z + n], [x + 1, z + n]]],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WallKind {
    Logs,
    Window,
    Door,
}

/// One module of outside wall set to something other than logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WallSet {
    /// 0 = the ground floor.
    pub storey: u8,
    /// The house module the wall belongs to.
    pub at: [u8; 2],
    pub side: Side,
    pub kind: WallKind,
}

/// A colour slot's palette: a retail one by key, or the design's own (16 BGR555 entries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Paint {
    Retail { key: String },
    Custom { palette: Vec<u16> },
}

impl Paint {
    pub fn retail(key: &str) -> Self {
        Paint::Retail { key: key.to_string() }
    }
}

/// A design's palettes, by the style's colour slots ([`Slot`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Colours {
    /// Roof tiles (and the chimney's brick, for a retail roof).
    pub roof: Paint,
    /// The bands and the gables (a log house's trim).
    pub trim: Paint,
    /// The walls (a log house's logs).
    #[serde(alias = "logs")]
    pub walls: Paint,
}

impl Default for Colours {
    fn default() -> Self {
        Colours { roof: Paint::retail("blue"), trim: Paint::retail("blue"), walls: Paint::retail(LOGS_KEY) }
    }
}

impl Colours {
    /// A style's own retail palettes: the first of each slot.
    pub fn of(st: &Style) -> Self {
        Colours { roof: Paint::retail(st.roofs[0].key), trim: Paint::retail(st.trims[0].key), walls: Paint::retail(st.walls[0].key) }
    }

    fn slot(&self, slot: Slot) -> &Paint {
        match slot {
            Slot::Roof => &self.roof,
            Slot::Trim => &self.trim,
            Slot::Walls => &self.walls,
        }
    }
}

/// The logs' one retail palette.
pub const LOGS_KEY: &str = "village";

fn default_style() -> String {
    STYLE.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HouseDesign {
    pub name: String,
    #[serde(default = "default_style")]
    pub style: String,
    #[serde(default)]
    pub colours: Colours,
    /// Gable or hipped; `None` takes the style's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roof: Option<RoofShape>,
    pub storeys: u8,
    pub wings: Vec<Wing>,
    /// Walls other than logs; the rest are logs.
    #[serde(default)]
    pub walls: Vec<WallSet>,
    /// Roof modules with a chimney.
    #[serde(default)]
    pub chimneys: Vec<[u8; 2]>,
    /// Porches over doors and balconies on upper storeys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extras: Vec<Extra>,
    /// The plot round the house, with its fence, paths, beds and props.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yard: Option<Yard>,
    /// Rooms, furniture, stairs and the cellar (built into a new area when placed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside: Option<Inside>,
}

/// A porch or a balcony on one module of an outside wall (§4.2 `Extra`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extra {
    pub kind: ExtraKind,
    /// 0 for a porch; an upper storey for a balcony.
    pub storey: u8,
    /// The house module the wall belongs to.
    pub at: [u8; 2],
    pub side: Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtraKind {
    /// A lean-to roof on two posts over a ground-floor door, half a module deep.
    Porch,
    /// A plank floor with a rail along an upper storey's wall, 48 deep.
    Balcony,
}

impl Extra {
    /// The module in front of its wall, which it stands over.
    pub fn front(&self) -> [i16; 2] {
        self.side.step([self.at[0] as i16, self.at[1] as i16])
    }
}

/// The plot round the house (§4.2 `Yard`). Plans in modules, props in cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Yard {
    /// Min corner and size in modules; it holds the whole house.
    pub at: [u8; 2],
    pub size: [u8; 2],
    /// A log fence round the plot's edge.
    #[serde(default)]
    pub fence: bool,
    /// Modules of the plot's edge left open: gates.
    #[serde(default)]
    pub gates: Vec<PlotEdge>,
    /// Paths and beds: ground tiles a module each.
    #[serde(default)]
    pub patches: Vec<Patch>,
    #[serde(default)]
    pub props: Vec<YardProp>,
}

/// One module of the plot's edge: the plot module inside it and the side facing out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlotEdge {
    pub at: [u8; 2],
    pub side: Side,
}

/// A ground tile on the plot: a [`Material`] id of the Ground tool (`edit::ground`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    pub at: [u8; 2],
    pub material: u8,
}

/// A stock prop (`edit::stock`) on the plot: its turned footprint's min corner in plan cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YardProp {
    pub item: String,
    pub at: [u8; 2],
    #[serde(default)]
    pub rot: u8,
}

/// The stock props the yard offers, in shelf order.
pub const YARD_PROPS: &[&str] = &["well", "stone-well", "barrel", "barrels", "tub", "hay-cart", "birdhouse", "signpost", "planter", "flower", "grass", "fern", "pine", "rock", "stumps"];

/// Materials a path or bed can take (the Ground tool's, without plain ground).
pub const PATCH_MATERIALS: [Material; 7] = [Material::Dirt, Material::Sand, Material::Rubble, Material::Mossy, Material::Forest, Material::Grass, Material::Water];

/// A plot's largest size, in modules.
pub const MAX_PLOT: u8 = 12;

/* ───────────────────────── Checks ───────────────────────── */

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    /// Errors block saving; warnings don't.
    pub error: bool,
    pub code: String,
    pub message: String,
    /// The module it names, if any.
    pub at: Option<[u8; 2]>,
}

fn problem(error: bool, code: &str, message: String, at: Option<[i16; 2]>) -> Problem {
    Problem { error, code: code.to_string(), message, at: at.map(|m| [m[0].max(0) as u8, m[1].max(0) as u8]) }
}

/// The plan's modules and the wing each belongs to (the first, where wings overlap).
pub fn footprint(d: &HouseDesign) -> BTreeMap<[i16; 2], usize> {
    let mut out = BTreeMap::new();
    for (k, w) in d.wings.iter().enumerate() {
        for m in w.modules() {
            out.entry(m).or_insert(k);
        }
    }
    out
}

/// How a wing's end meets the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// Nothing there: a gable.
    Open,
    /// Against another wing's side: this wing's roof runs on into it, to its ridge.
    Joins(usize),
    /// Against another wing's end, in line: the house continues.
    Continues,
    /// Against a wing of another height (its side or its end): the lower roof stops against the taller wing's wall.
    Abuts(usize),
    /// Only partly against the plan, or against a wing it can't join.
    Bad,
}

fn ends(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>, k: usize) -> [End; 2] {
    let w = &d.wings[k];
    w.beyond_ends().map(|pair| {
        let owners: Vec<Option<usize>> = pair.iter().map(|m| plan.get(m).copied()).collect();
        match (owners[0], owners[1]) {
            (None, None) => End::Open,
            (Some(a), Some(b)) if a == b && a != k => {
                let v = &d.wings[a];
                let across = |x: &Wing| if x.along == Axis::X { x.at[1] } else { x.at[0] };
                if wing_storeys(d, a) != wing_storeys(d, k) && (v.along != w.along || across(v) == across(w)) {
                    End::Abuts(a)
                } else if v.along != w.along {
                    End::Joins(a)
                } else {
                    // In line: the same two modules across.
                    if across(v) == across(w) {
                        End::Continues
                    } else {
                        End::Bad
                    }
                }
            }
            _ => End::Bad,
        }
    })
}

/// The storeys wing `k` stands: its own, or the house's.
pub fn wing_storeys(d: &HouseDesign, k: usize) -> u8 {
    d.wings[k].storeys.unwrap_or(d.storeys).min(d.storeys)
}

/// The design's style.
pub fn style_of(d: &HouseDesign) -> Result<&'static Style> {
    style::style(&d.style).ok_or_else(|| EditError::new("STYLE", format!("Unknown style “{}”.", d.style)))
}

/// Gable or hipped: the design's, or its style's.
pub fn shape_of(d: &HouseDesign, st: &Style) -> RoofShape {
    d.roof.unwrap_or(st.shape)
}

/// Errors (saving is blocked) and warnings, in plan order.
pub fn validate(d: &HouseDesign) -> Vec<Problem> {
    let mut out = Vec::new();
    let Ok(st) = style_of(d) else {
        out.push(problem(true, "STYLE", format!("Unknown style “{}”.", d.style), None));
        return out;
    };
    if !(1..=MAX_STOREYS).contains(&d.storeys) {
        out.push(problem(true, "STOREYS", format!("A house has 1 to {MAX_STOREYS} storeys."), None));
    } else if d.storeys == MAX_STOREYS {
        out.push(problem(false, "STOREYS_TALL", "Three storeys may hide the Knight behind the house from the field camera (retail's tallest house is 288 high).".into(), None));
    }
    if d.wings.is_empty() {
        out.push(problem(true, "NO_WINGS", "Draw a wing to start the plan.".into(), None));
        return out;
    }
    let mut seen: HashMap<[i16; 2], usize> = HashMap::new();
    for (k, w) in d.wings.iter().enumerate() {
        let (sw, sh) = w.size();
        if !(1..=MAX_LENGTH).contains(&w.length) {
            out.push(problem(true, "WING_LENGTH", format!("Wing {} is {} modules long; a wing is 1 to {MAX_LENGTH}.", k + 1, w.length), Some([w.at[0] as i16, w.at[1] as i16])));
        }
        if w.at[0] as u16 + sw as u16 > PLAN as u16 || w.at[1] as u16 + sh as u16 > PLAN as u16 {
            out.push(problem(true, "WING_OFF_PLAN", format!("Wing {} runs off the plan.", k + 1), Some([w.at[0] as i16, w.at[1] as i16])));
        }
        if w.storeys.is_some_and(|n| n == 0 || n > d.storeys) {
            out.push(problem(true, "WING_STOREYS", format!("Wing {} stands {} storeys; a wing stands 1 to the house's {}.", k + 1, w.storeys.unwrap_or(0), d.storeys), Some([w.at[0] as i16, w.at[1] as i16])));
        }
        for m in w.modules() {
            if let Some(&o) = seen.get(&m) {
                out.push(problem(true, "WINGS_OVERLAP", format!("Wings {} and {} overlap. Wings touch; they don't overlap.", o + 1, k + 1), Some(m)));
                break;
            }
            seen.insert(m, k);
        }
    }
    if out.iter().any(|p| p.error) {
        return out;
    }
    let plan = footprint(d);
    let (lo, hi) = bounds(&plan);
    if hi[0] - lo[0] > MAX_SIZE as i16 || hi[1] - lo[1] > MAX_SIZE as i16 {
        out.push(problem(true, "HOUSE_SIZE", format!("The house is {} × {} modules; the most is {MAX_SIZE} × {MAX_SIZE}.", hi[0] - lo[0], hi[1] - lo[1]), None));
    }
    for (k, w) in d.wings.iter().enumerate() {
        let e = ends(d, &plan, k);
        for (i, end) in e.iter().enumerate() {
            if *end == End::Bad {
                let at = w.beyond_ends()[i][0];
                out.push(problem(true, "WING_JOIN", format!("Wing {}'s end meets the plan along only one of its two modules. Line it up with a whole side or end.", k + 1), Some(at)));
            }
        }
        // A one-module arm is fine where it joins or continues another wing; on its own its ridge is shorter than the house is deep.
        if w.length == 1 && !e.iter().any(|x| matches!(x, End::Joins(_) | End::Continues | End::Abuts(_))) {
            let joined_by_others = (0..d.wings.len()).any(|j| j != k && ends(d, &plan, j).iter().any(|e| matches!(e, End::Joins(o) | End::Abuts(o) if *o == k)));
            if !joined_by_others {
                out.push(problem(true, "WING_SHORT", format!("Wing {} is shorter than it is deep. A one-module wing has to join another.", k + 1), Some([w.at[0] as i16, w.at[1] as i16])));
            }
        }
    }
    if parts(&plan) > 1 {
        out.push(problem(false, "PLAN_PARTS", "The plan is in separate pieces: they build as one house with gaps.".into(), None));
    }
    let mut stale = 0;
    for w in &d.walls {
        let m = [w.at[0] as i16, w.at[1] as i16];
        if !plan.contains_key(&m) || plan.contains_key(&w.side.step(m)) || w.storey >= d.storeys {
            stale += 1;
        } else if w.kind == WallKind::Door && w.storey > 0 {
            out.push(problem(true, "DOOR_UPSTAIRS", "A door needs ground under it: doors go on the ground floor.".into(), Some(m)));
        }
    }
    if stale > 0 {
        out.push(problem(false, "WALLS_STALE", format!("{stale} wall setting{} no longer on an outside wall; {} left out.", if stale == 1 { " is" } else { "s are" }, if stale == 1 { "it is" } else { "they are" }), None));
    }
    for c in &d.chimneys {
        if !plan.contains_key(&[c[0] as i16, c[1] as i16]) {
            out.push(problem(true, "CHIMNEY_OFF", "A chimney stands off the roof.".into(), Some([c[0] as i16, c[1] as i16])));
        }
    }
    for slot in Slot::ALL {
        match d.colours.slot(slot) {
            Paint::Custom { palette } if palette.len() != 16 => out.push(problem(true, "PALETTE", format!("The {} colour has {} entries; a palette has 16.", slot.key(), palette.len()), None)),
            Paint::Retail { key } if retail_of(st, slot, key).is_none() => out.push(problem(true, "PALETTE", format!("The {} style has no retail {} colour “{key}”.", st.name, slot.key()), None)),
            _ => {}
        }
    }
    check_extras(d, &plan, &mut out);
    if let Some(y) = &d.yard {
        check_yard(y, &plan, &mut out);
        for e in &d.extras {
            if !on_plot(y, e.front()) {
                out.push(problem(true, "EXTRA_OFF_PLOT", format!("The {} stands off the plot.", e.kind.name()), Some(e.front())));
            }
        }
    }
    if d.inside.is_some() {
        let front: Vec<([i16; 2], Side)> = front_doors(d, &plan);
        inside::check(d, &plan, &front, &inside::item_size, &mut out);
    }
    if !out.iter().any(|p| p.error) {
        if let Err(e) = roof_surface(d, &plan) {
            out.push(problem(true, e.code, e.message, None));
        }
    }
    out
}

impl ExtraKind {
    pub fn name(self) -> &'static str {
        match self {
            ExtraKind::Porch => "porch",
            ExtraKind::Balcony => "balcony",
        }
    }
}

/// Porches go over ground-floor doors, balconies on upper storeys' outside walls, one to a wall.
fn check_extras(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>, out: &mut Vec<Problem>) {
    let kinds = wall_kinds(d);
    let mut seen = BTreeSet::new();
    for e in &d.extras {
        let m = [e.at[0] as i16, e.at[1] as i16];
        let f = e.front();
        let tall = plan.get(&m).map(|&k| wing_storeys(d, k));
        let open = tall.is_some_and(|t| e.storey < t) && !plan.contains_key(&f) && (0..PLAN as i16).contains(&f[0]) && (0..PLAN as i16).contains(&f[1]);
        if !open {
            out.push(problem(true, "EXTRA_OFF", format!("A {} has to stand on an outside wall of the house.", e.kind.name()), Some(m)));
        } else if e.kind == ExtraKind::Porch && (e.storey != 0 || kinds.get(&(0, m, e.side)) != Some(&WallKind::Door)) {
            out.push(problem(true, "PORCH_NO_DOOR", "A porch goes over a ground-floor door.".into(), Some(m)));
        } else if e.kind == ExtraKind::Balcony && e.storey == 0 {
            out.push(problem(true, "BALCONY_GROUND", "A balcony goes on an upper storey.".into(), Some(m)));
        }
        if !seen.insert((e.storey, e.at, e.side)) {
            out.push(problem(true, "EXTRA_TWICE", "Two extras stand on one wall.".into(), Some(m)));
        }
    }
}

/// The ground floor's doors: each module with a door and the side it faces out of.
pub fn front_doors(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>) -> Vec<([i16; 2], Side)> {
    d.walls
        .iter()
        .filter(|w| w.storey == 0 && w.kind == WallKind::Door)
        .map(|w| ([w.at[0] as i16, w.at[1] as i16], w.side))
        .filter(|(m, side)| plan.contains_key(m) && !plan.contains_key(&side.step(*m)))
        .collect()
}

/// The plot's rules: it holds the house and fits the plan; gates, paths, beds
/// and props lie on it (stale ones are left out, with a warning).
fn check_yard(y: &Yard, plan: &BTreeMap<[i16; 2], usize>, out: &mut Vec<Problem>) {
    let at = [y.at[0] as i16, y.at[1] as i16];
    if !(1..=MAX_PLOT).contains(&y.size[0]) || !(1..=MAX_PLOT).contains(&y.size[1]) {
        out.push(problem(true, "PLOT_SIZE", format!("A plot is 1 to {MAX_PLOT} modules on a side."), Some(at)));
        return;
    }
    if y.at[0] as u16 + y.size[0] as u16 > PLAN as u16 || y.at[1] as u16 + y.size[1] as u16 > PLAN as u16 {
        out.push(problem(true, "PLOT_OFF_PLAN", "The plot runs off the plan.".into(), Some(at)));
        return;
    }
    if !plan.keys().all(|m| on_plot(y, *m)) {
        out.push(problem(true, "PLOT_HOUSE", "The plot has to hold the whole house.".into(), Some(at)));
    }
    let stale_gates = y.gates.iter().filter(|g| !on_plot(y, [g.at[0] as i16, g.at[1] as i16]) || on_plot(y, g.side.step([g.at[0] as i16, g.at[1] as i16]))).count();
    let stale_patches = y.patches.iter().filter(|p| !on_plot(y, [p.at[0] as i16, p.at[1] as i16]) || plan.contains_key(&[p.at[0] as i16, p.at[1] as i16])).count();
    if stale_gates + stale_patches > 0 {
        out.push(problem(false, "YARD_STALE", format!("{} gate{} or path tile{} no longer on the plot's open ground; left out.", stale_gates, if stale_gates == 1 { "" } else { "s" }, if stale_patches == 1 { "" } else { "s" }), None));
    }
    for p in &y.patches {
        if !Material::from_id(p.material).is_some_and(|m| PATCH_MATERIALS.contains(&m)) {
            out.push(problem(true, "PATCH_MATERIAL", format!("Material {} isn't one a path or bed can take.", p.material), Some([p.at[0] as i16, p.at[1] as i16])));
        }
    }
    for p in &y.props {
        let cell = [p.at[0] as i16 / 4, p.at[1] as i16 / 4];
        if !YARD_PROPS.contains(&p.item.as_str()) {
            out.push(problem(true, "PROP_UNKNOWN", format!("No yard prop “{}”.", p.item), Some(cell)));
        } else if !on_plot(y, cell) {
            out.push(problem(true, "PROP_OFF_PLOT", format!("A {} stands off the plot.", p.item), Some(cell)));
        }
    }
}

/// Plan module `m` lies on the plot.
fn on_plot(y: &Yard, m: [i16; 2]) -> bool {
    (0..2).all(|a| m[a] >= y.at[a] as i16 && m[a] < y.at[a] as i16 + y.size[a] as i16)
}

/// Min and max corner of the plan's modules (max exclusive).
fn bounds(plan: &BTreeMap<[i16; 2], usize>) -> ([i16; 2], [i16; 2]) {
    let (mut lo, mut hi) = ([i16::MAX; 2], [i16::MIN; 2]);
    for m in plan.keys() {
        for a in 0..2 {
            lo[a] = lo[a].min(m[a]);
            hi[a] = hi[a].max(m[a] + 1);
        }
    }
    (lo, hi)
}

/// Min and max corner (max exclusive) of what the house prefab covers: the
/// plan's modules and the modules its porches and balconies stand over.
pub fn house_bounds(d: &HouseDesign) -> ([i16; 2], [i16; 2]) {
    let plan = footprint(d);
    let mut all = plan.clone();
    for e in &d.extras {
        all.entry(e.front()).or_insert(0);
    }
    bounds(&all)
}

/// Pieces of the plan joined edge to edge.
fn parts(plan: &BTreeMap<[i16; 2], usize>) -> usize {
    let mut seen = BTreeSet::new();
    let mut n = 0;
    for &m in plan.keys() {
        if !seen.insert(m) {
            continue;
        }
        n += 1;
        let mut stack = vec![m];
        while let Some(c) = stack.pop() {
            for s in Side::ALL {
                let o = s.step(c);
                if plan.contains_key(&o) && seen.insert(o) {
                    stack.push(o);
                }
            }
        }
    }
    n
}

/* ───────────────────────── Roof ───────────────────────── */

/// One slope of the roof: a plane rising the style's pitch per module along
/// `var`, from the eave line at `zero` in direction `dir` (+1 or −1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plane {
    wing: usize,
    var: Axis,
    zero: i32,
    dir: i32,
    /// The side it faces (downhill).
    faces: Side,
}

impl Plane {
    /// Height above the eave at (x, z) units.
    fn rise(&self, pitch: i32, x: i32, z: i32) -> i32 {
        let c = if self.var == Axis::X { x } else { z };
        self.dir * (c - self.zero) * pitch / M as i32
    }

    /// The line where it reaches the top, one module in from its eave.
    fn ridge(&self) -> i32 {
        self.zero + self.dir * M as i32
    }
}

/// Wing `k`'s slopes: its two sides, and each open end when the roof is hipped.
fn planes(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>, k: usize, hip: bool) -> Vec<Plane> {
    let w = &d.wings[k];
    let (sw, sh) = w.size();
    let m = M as i32;
    let (x0, z0) = (w.at[0] as i32 * m, w.at[1] as i32 * m);
    let (x1, z1) = (x0 + sw as i32 * m, z0 + sh as i32 * m);
    let p = |var, zero, dir, faces| Plane { wing: k, var, zero, dir, faces };
    let (mut out, hips) = match w.along {
        Axis::X => (vec![p(Axis::Z, z0, 1, Side::South), p(Axis::Z, z1, -1, Side::North)], [p(Axis::X, x0, 1, Side::West), p(Axis::X, x1, -1, Side::East)]),
        Axis::Z => (vec![p(Axis::X, x0, 1, Side::West), p(Axis::X, x1, -1, Side::East)], [p(Axis::Z, z0, 1, Side::South), p(Axis::Z, z1, -1, Side::North)]),
    };
    if hip {
        for (end, q) in ends(d, plan, k).iter().zip(hips) {
            if *end == End::Open {
                out.push(q);
            }
        }
    }
    out
}

/// The roof over one module: the eave it starts from (its wing's), its
/// corners' rise above that (SW, SE, NW, NE), and the faces that make it,
/// each a plane and the corners it covers.
#[derive(Debug, Clone)]
struct RoofModule {
    eave: i16,
    rise: [i32; 4],
    faces: Vec<(Plane, Vec<usize>)>,
}

/// Corner positions of module `m` in units: SW, SE, NW, NE.
fn corners(m: [i16; 2]) -> [[i32; 2]; 4] {
    let (x0, z0) = (m[0] as i32 * M as i32, m[1] as i32 * M as i32);
    let (x1, z1) = (x0 + M as i32, z0 + M as i32);
    [[x0, z0], [x1, z0], [x0, z1], [x1, z1]]
}

/// The roof over every module of the plan, or why it can't be built.
fn roof_surface(d: &HouseDesign, plan: &BTreeMap<[i16; 2], usize>) -> Result<BTreeMap<[i16; 2], RoofModule>> {
    let st = style_of(d)?;
    let pitch = st.pitch as i32;
    let hip = shape_of(d, st) == RoofShape::Hip;
    let slopes: Vec<Vec<Plane>> = (0..d.wings.len()).map(|k| planes(d, plan, k, hip)).collect();
    // A wing's surface is the lowest of its slopes; the roof is the highest of the wings over a point.
    let surface = |k: usize, p: [i32; 2]| slopes[k].iter().map(|q| q.rise(pitch, p[0], p[1])).min().unwrap_or(0);
    // Each wing's prism covers its own modules and, past an end that joins another wing's side, that wing's near half.
    let mut cover: BTreeMap<[i16; 2], Vec<usize>> = BTreeMap::new();
    for (k, w) in d.wings.iter().enumerate() {
        for m in w.modules() {
            cover.entry(m).or_default().push(k);
        }
        for (i, end) in ends(d, plan, k).iter().enumerate() {
            if let End::Joins(_) = end {
                for m in w.beyond_ends()[i] {
                    cover.entry(m).or_default().push(k);
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    let mut at_corner: HashMap<([i32; 2], i16), i32> = HashMap::new();
    for (&m, wings) in &cover {
        let Some(&own) = plan.get(&m) else { continue };
        let eave = st.eave(wing_storeys(d, own));
        let c = corners(m);
        let rise = c.map(|p| wings.iter().map(|&k| surface(k, p)).max().unwrap());
        let planes: Vec<Plane> = wings.iter().flat_map(|&k| slopes[k].iter().copied()).collect();
        let fits = |corner_ids: &[usize]| planes.iter().copied().find(|q| corner_ids.iter().all(|&i| q.rise(pitch, c[i][0], c[i][1]) == rise[i]));
        // One plane over the module, or two triangles split on a diagonal.
        let faces = if let Some(q) = fits(&[0, 1, 2, 3]) {
            vec![(q, vec![0, 1, 2, 3])]
        } else {
            let split = [[[0, 1, 3], [0, 3, 2]], [[1, 3, 2], [1, 2, 0]]].into_iter().find_map(|[a, b]| Some(vec![(fits(&a)?, a.to_vec()), (fits(&b)?, b.to_vec())]));
            split.ok_or_else(|| EditError::new("ROOF_JOIN", format!("The roof over module {}, {} can't be built from these wings yet.", m[0], m[1])))?
        };
        for (i, p) in c.iter().enumerate() {
            match at_corner.insert((*p, eave), rise[i]) {
                Some(r) if r != rise[i] => return Err(EditError::new("ROOF_JOIN", format!("The roof doesn't meet itself at module {}, {}.", m[0], m[1]))),
                _ => {}
            }
        }
        out.insert(m, RoofModule { eave, rise, faces });
    }
    Ok(out)
}

/* ───────────────────────── Build ───────────────────────── */

/// The pages a design's textures come from: the village's (chunk 78: the
/// fence, the hatch, brick chimneys) and its style's.
#[derive(Clone, Copy)]
pub struct Pages<'a> {
    pub village: &'a Texture,
    pub style: &'a Texture,
}

impl<'a> Pages<'a> {
    /// A log house's: the village's page is its style's.
    pub fn village(page: &'a Texture) -> Self {
        Pages { village: page, style: page }
    }
}

/// The CLUT ids a design draws with: each slot's (a retail one or one of the builder's own) and the chimney's.
struct Inks {
    roof: u16,
    trim: u16,
    walls: u16,
    chimney: u16,
}

fn retail_of(st: &Style, slot: Slot, key: &str) -> Option<&'static Retail> {
    st.retail(slot).iter().find(|r| r.key == key)
}

fn inks(b: &mut Builder, st: &Style, c: &Colours) -> Result<Inks> {
    let mut one = |slot: Slot, p: &Paint| -> Result<u16> {
        match p {
            Paint::Retail { key } => retail_of(st, slot, key).map(|r| r.clut).ok_or_else(|| EditError::new("HOUSE_PALETTE", format!("No retail {} colour “{key}” in the {} style.", slot.key(), st.name))),
            // Texels cut with the slot's first retail palette.
            Paint::Custom { palette } => Ok(b.custom(st.retail(slot)[0].clut, palette)),
        }
    };
    let (roof, trim, walls) = (one(Slot::Roof, &c.roof)?, one(Slot::Trim, &c.trim)?, one(Slot::Walls, &c.walls)?);
    let chimney = match &c.roof {
        Paint::Retail { key } => retail_of(st, Slot::Roof, key).map_or(st.roofs[0].chimney, |r| r.chimney),
        Paint::Custom { .. } => st.roofs[0].chimney,
    };
    Ok(Inks { roof, trim, walls, chimney })
}

/// What the house's faces are built with.
struct Kit<'a> {
    pages: Pages<'a>,
    st: &'static Style,
    inks: Inks,
}

impl<'a> Kit<'a> {
    /// Points `b` at `t`'s page and gives the tile it draws (with `paste` copied in).
    fn tile(&self, b: &mut Builder<'a>, t: Tex, paste: Option<([u16; 4], u16)>) -> Result<u16> {
        b.set_page(if t.village { self.pages.village } else { self.pages.style });
        let clut = match t.ink {
            Ink::Slot(Slot::Roof) => self.inks.roof,
            Ink::Slot(Slot::Trim) => self.inks.trim,
            Ink::Slot(Slot::Walls) => self.inks.walls,
            Ink::Clut(c) => c,
            Ink::Chimney => self.inks.chimney,
        };
        b.tile_with(t.rect, clut, paste)
    }
}

/// A rectangle's size in texels.
fn dims(t: Tex) -> (u16, u16) {
    (t.rect[2] - t.rect[0] + 1, t.rect[3] - t.rect[1] + 1)
}

/// Map of the wall kinds set, the rest logs.
fn wall_kinds(d: &HouseDesign) -> HashMap<(u8, [i16; 2], Side), WallKind> {
    d.walls.iter().map(|w| ((w.storey, [w.at[0] as i16, w.at[1] as i16], w.side), w.kind)).collect()
}

/// Texel column for `u` units across a 128-unit module on a rectangle `w` texels wide, mirrored when `flip`.
fn tu(u: i16, w: u16, flip: bool) -> u8 {
    let last = w as i32 - 1;
    let t = ((u as i32 * last + 64) / 128).clamp(0, last);
    (if flip { last - t } else { t }) as u8
}

/// Texel row for `dh` units down from the top of a span `span` high, on a rectangle `h` texels high.
fn tv(dh: i16, h: u16, span: i16) -> u8 {
    let last = h as i32 - 1;
    ((dh as i32 * last + span as i32 / 2) / span as i32).clamp(0, last) as u8
}

/// A wall module's local frame: `u` across from the left as seen from
/// outside, `h` up, `d` into the house. Returns builder coordinates.
fn frame(m: [i16; 2], side: Side) -> impl Fn(i16, i16, i16) -> [i16; 3] {
    let (x0, z0) = (m[0] * M, m[1] * M);
    let (x1, z1) = (x0 + M, z0 + M);
    move |u, h, d| match side {
        Side::South => [x0 + u, h, z0 + d],
        Side::North => [x1 - u, h, z1 - d],
        Side::East => [x1 - d, h, z0 + u],
        Side::West => [x0 + d, h, z1 - u],
    }
}

type At<'f> = &'f dyn Fn(i16, i16, i16) -> [i16; 3];

/// A convex polygon on a wall's frame, its corners `(u, h)` clockwise as seen
/// from outside: one quad when it has four corners, else triangles.
fn face(b: &mut Builder, at: At, pts: &[(i16, i16)], uv: &dyn Fn((i16, i16)) -> [u8; 2], tile: u16, shade: Shade) {
    // Drop repeated corners and corners on a straight edge.
    let mut p: Vec<(i16, i16)> = Vec::new();
    for &q in pts {
        if p.last() != Some(&q) && p.first() != Some(&q) {
            p.push(q);
        }
    }
    let straight = |a: (i16, i16), o: (i16, i16), c: (i16, i16)| (o.0 as i32 - a.0 as i32) * (c.1 as i32 - a.1 as i32) == (o.1 as i32 - a.1 as i32) * (c.0 as i32 - a.0 as i32);
    let mut k = 0;
    while p.len() >= 3 && k < p.len() {
        let n = p.len();
        if straight(p[(k + n - 1) % n], p[k], p[(k + 1) % n]) {
            p.remove(k);
            k = 0;
        } else {
            k += 1;
        }
    }
    let v = |q: (i16, i16)| at(q.0, q.1, 0);
    match p.len() {
        0..=2 => {}
        4 => b.quad_on([v(p[0]), v(p[1]), v(p[3]), v(p[2])], tile, [uv(p[0]), uv(p[1]), uv(p[3]), uv(p[2])], shade),
        n => {
            for i in 1..n - 1 {
                b.tri_on([v(p[0]), v(p[i]), v(p[i + 1])], tile, [uv(p[0]), uv(p[i]), uv(p[i + 1])], shade);
            }
        }
    }
}

/// What of the clockwise polygon `pts` lies on or above the line from
/// (0, `a`) to (`M`, `c`): a wall above a lower roof.
fn cut(pts: &[(i16, i16)], a: i16, c: i16) -> Vec<(i16, i16)> {
    // Scaled by M so it stays whole: ≥ 0 on or above the line.
    let g = |p: (i16, i16)| p.1 as i64 * M as i64 - (a as i64 * M as i64 + (c - a) as i64 * p.0 as i64);
    let mut out = Vec::new();
    for i in 0..pts.len() {
        let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
        let (gp, gq) = (g(p), g(q));
        if gp >= 0 {
            out.push(p);
        }
        if (gp >= 0) != (gq >= 0) && gp != 0 && gq != 0 {
            let t = gp as f64 / (gp - gq) as f64;
            out.push(((p.0 as f64 + t * (q.0 - p.0) as f64).round() as i16, (p.1 as f64 + t * (q.1 - p.1) as f64).round() as i16));
        }
    }
    out
}

/// A panel across the whole module from `lo` up to `hi`, textured with all
/// of `t` (mirrored when `flip`), less what lies under `clip`.
#[allow(clippy::too_many_arguments)]
fn panel<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, at: At, (lo, hi): (i16, i16), t: Tex, flip: bool, clip: Option<(i16, i16)>, shade: Shade) -> Result<()> {
    let tile = kit.tile(b, t, None)?;
    let (w, h) = dims(t);
    let uv = |(u, y): (i16, i16)| [tu(u, w, flip), tv(hi - y, h, hi - lo)];
    let rect = [(0, hi), (M, hi), (M, lo), (0, lo)];
    let pts = match clip {
        Some((a, c)) => cut(&rect, a, c),
        None => rect.to_vec(),
    };
    face(b, at, &pts, &uv, tile, shade);
    Ok(())
}

/// A row's texture for a wall whose log ends sit `ends` (the log style's corners), and whether it is mirrored.
fn row_tex(r: &Row, ends: Ends) -> (Tex, bool) {
    match (ends, r.end, r.both) {
        (Ends::Right, Some(e), _) => (e, false),
        (Ends::Left, Some(e), _) => (e, true),
        (Ends::Both, _, Some(t)) => (t, false),
        _ => (r.plain, false),
    }
}

/// One storey of wall on module `m`'s `side`, with the bands over it when
/// it is the top storey, less what lies under a lower roof (`clip`, the
/// heights at the module's left and right as seen from outside).
#[allow(clippy::too_many_arguments)]
fn wall<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, m: [i16; 2], side: Side, kind: WallKind, ends: Ends, storey: u8, top: bool, clip: Option<(i16, i16)>) -> Result<()> {
    let at = frame(m, side);
    let shade = side.shade();
    let rows = kit.st.rows(storey);
    let h0 = storey as i16 * WALL;
    if kind == WallKind::Door {
        door(b, kit, &at, h0, rows, ends, shade)?;
    } else {
        let mut h = h0;
        for r in rows {
            let (t, flip) = row_tex(r, ends);
            match (kind, r.window, clip) {
                // A wall a lower roof cuts keeps its rows plain.
                (WallKind::Window, Some(w), None) => window(b, kit, &at, (h, h + r.h), r, w, ends, shade)?,
                _ => panel(b, kit, &at, (h, h + r.h), t, flip, clip, shade)?,
            }
            h += r.h;
        }
    }
    if top {
        let mut h = h0 + WALL;
        for band in kit.st.top {
            panel(b, kit, &at, (h, h + band.h), band.tex, false, clip, shade)?;
            h += band.h;
        }
    }
    Ok(())
}

/// A window in row `r`, `lo` to `hi`.
#[allow(clippy::too_many_arguments)]
fn window<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, at: At, (lo, hi): (i16, i16), r: &Row, w: Window, ends: Ends, shade: Shade) -> Result<()> {
    let (t, flip) = row_tex(r, ends);
    match w {
        Window::Row(win) => panel(b, kit, at, (lo, hi), win, r.end.is_some() && ends == Ends::Left, None, shade),
        Window::Paste { src, x } => {
            let tile = kit.tile(b, t, Some((src, x)))?;
            let (tw, th) = dims(t);
            let uv = |(u, y): (i16, i16)| [tu(u, tw, flip), tv(hi - y, th, hi - lo)];
            face(b, at, &[(0, hi), (M, hi), (M, lo), (0, lo)], &uv, tile, shade);
            Ok(())
        }
        Window::Recess => recess(b, kit, at, (lo, hi), (t, flip), shade),
    }
}

/// How deep a recessed window is set into the wall.
const RECESS: i16 = 12;

/// A window opening set into the wall: the row's texture framing a hole 48
/// wide, dark inside. The frame is four quads from the row's corners to the
/// hole's, so the row's edges keep only their corners.
fn recess<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, at: At, (lo, hi): (i16, i16), (t, flip): (Tex, bool), shade: Shade) -> Result<()> {
    let tile = kit.tile(b, t, None)?;
    let (tw, th) = dims(t);
    let uv = |(u, y): (i16, i16)| [tu(u, tw, flip), tv(hi - y, th, hi - lo)];
    let hh = (hi - lo - 24).min(48);
    let (u0, u1) = (40, 88);
    let (y0, y1) = (lo + (hi - lo - hh) / 2, lo + (hi - lo + hh) / 2);
    face(b, at, &[(0, hi), (M, hi), (u1, y1), (u0, y1)], &uv, tile, shade);
    face(b, at, &[(M, hi), (M, lo), (u1, y0), (u1, y1)], &uv, tile, shade);
    face(b, at, &[(M, lo), (0, lo), (u0, y0), (u1, y0)], &uv, tile, shade);
    face(b, at, &[(0, lo), (0, hi), (u0, y1), (u0, y0)], &uv, tile, shade);
    let d = RECESS;
    b.flat([at(u0, y1, d), at(u1, y1, d), at(u0, y0, d), at(u1, y0, d)], DOORWAY);
    b.flat([at(u0, y1, 0), at(u1, y1, 0), at(u0, y1, d), at(u1, y1, d)], DOORWAY);
    b.flat([at(u0, y0, d), at(u1, y0, d), at(u0, y0, 0), at(u1, y0, 0)], SILL);
    b.flat([at(u0, y1, 0), at(u0, y1, d), at(u0, y0, 0), at(u0, y0, d)], REVEAL);
    b.flat([at(u1, y1, d), at(u1, y1, 0), at(u1, y0, d), at(u1, y0, 0)], REVEAL);
    Ok(())
}

/// A face on a log row that tops out at `row_top` (`row_h` high): corners TL,
/// TR, BL, BR as (u, height) on the wall, with their texels.
#[allow(clippy::too_many_arguments)]
fn row_face(b: &mut Builder, at: At, tl: (i16, i16), tr: (i16, i16), bl: (i16, i16), br: (i16, i16), (row_top, row_h): (i16, i16), (tile, t, flip): (u16, Tex, bool), shade: Shade) {
    let (w, h) = dims(t);
    let p = |(u, y): (i16, i16)| at(u, y, 0);
    let uv = |(u, y): (i16, i16)| [tu(u, w, flip), tv(row_top - y, h, row_h)];
    b.quad_on([p(tl), p(tr), p(bl), p(br)], tile, [uv(tl), uv(tr), uv(bl), uv(br)], shade);
}

/// A door: an alcove 64 wide, `DOOR_TOP` high and 32 deep in the middle of
/// the module. The ground floor's two rows either side rise into a lintel
/// that slopes to the module's top corners, so the module's top edge keeps
/// only its corners.
#[allow(clippy::too_many_arguments)]
fn door<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, at: At, h0: i16, rows: &[Row], ends: Ends, shade: Shade) -> Result<()> {
    let (lo, mid, top, door) = (h0, h0 + rows[0].h, h0 + WALL, h0 + DOOR_TOP);
    let ((t0, f0), (t1, f1)) = (row_tex(&rows[0], ends), row_tex(&rows[1], ends));
    let (k0, k1) = (kit.tile(b, t0, None)?, kit.tile(b, t1, None)?);
    let lower = (mid, rows[0].h);
    let upper = (top, rows[1].h);
    // Lower row: a strip each side.
    row_face(b, at, (0, mid), (32, mid), (0, lo), (32, lo), lower, (k0, t0, f0), shade);
    row_face(b, at, (96, mid), (M, mid), (96, lo), (M, lo), lower, (k0, t0, f0), shade);
    // Upper row: the strips rise to the lintel, which slopes to the corners.
    row_face(b, at, (0, top), (32, door), (0, mid), (32, mid), upper, (k1, t1, f1), shade);
    row_face(b, at, (96, door), (M, top), (96, mid), (M, mid), upper, (k1, t1, f1), shade);
    row_face(b, at, (0, top), (M, top), (32, door), (96, door), upper, (k1, t1, f1), shade);
    // The alcove: back, jambs (split where the strips are), the soffit and the step.
    let lit = |s: Shade, by: i16| Shade((s.0 as i16 + by).clamp(0, 255) as u8);
    let jamb = kit.tile(b, kit.st.jamb, None)?;
    let (jw, jh) = dims(kit.st.jamb);
    let whole = |flip: bool| {
        let (l, r) = if flip { (jw as u8 - 1, 0) } else { (0, jw as u8 - 1) };
        [[l, 0], [r, 0], [l, jh as u8 - 1], [r, jh as u8 - 1]]
    };
    for (h_lo, h_hi) in [(lo, mid), (mid, door)] {
        b.flat([at(32, h_hi, 32), at(96, h_hi, 32), at(32, h_lo, 32), at(96, h_lo, 32)], DOORWAY);
        b.quad_on([at(32, h_hi, 0), at(32, h_hi, 32), at(32, h_lo, 0), at(32, h_lo, 32)], jamb, whole(false), lit(shade, 16));
        b.quad_on([at(96, h_hi, 32), at(96, h_hi, 0), at(96, h_lo, 32), at(96, h_lo, 0)], jamb, whole(true), lit(shade, -16));
    }
    b.flat([at(32, door, 0), at(96, door, 0), at(32, door, 32), at(96, door, 32)], DOORWAY);
    // The step floats a unit clear of the jambs and the back, so its corners touch no edge.
    let step = kit.tile(b, kit.st.step, None)?;
    let (sw, sh) = dims(kit.st.step);
    b.quad_on([at(33, lo + 1, 31), at(95, lo + 1, 31), at(33, lo + 1, 0), at(95, lo + 1, 0)], step, [[0, 0], [sw as u8 - 1, 0], [0, sh as u8 - 1], [sw as u8 - 1, sh as u8 - 1]], Shade(102));
    Ok(())
}

fn roof_shade(faces: Side) -> Shade {
    match faces {
        Side::South => ROOF_SOUTH,
        Side::North => ROOF_NORTH,
        Side::West => ROOF_WEST,
        Side::East => ROOF_EAST,
    }
}

/// The roof tile's texels at (x, z) on plane `q`: across the module along
/// the slope's contour, `v` texels down the slope per module from the ridge.
fn roof_uv(q: &Plane, m: [i16; 2], x: i32, z: i32, w: u16, v: u8) -> [u8; 2] {
    let (along, start, across) = match q.var {
        Axis::Z => (x, m[0] as i32 * M as i32, z),
        Axis::X => (z, m[1] as i32 * M as i32, x),
    };
    let last = w as i32 - 1;
    let u = ((along - start) * last + 64) / 128;
    let t = ((across - q.ridge()).abs() * v as i32 + 64) / 128;
    [u.clamp(0, last) as u8, t.clamp(0, 255) as u8]
}

/// What a yard needs besides chunk 78's page.
#[derive(Default)]
pub struct Extras<'a> {
    /// The yard's stock props by id, made from the disc (`stock::make`).
    pub props: HashMap<String, Prefab>,
    /// A field VRAM with the shared ground page (0x08) in it, such as chunk
    /// 78's: paths and beds then get texture tiles of their own (sampled in
    /// place wherever that page is loaded). Without it they sample the page.
    pub ground: Option<&'a crate::vram::Vram>,
}

/// Builds the design into a prefab, ground at 0: the house alone with its
/// footprint at the plan's min corner, or with a yard the whole plot (house,
/// fence, paths, beds and props, put together by [`kit::compose`]). Textured
/// from its style's page and the village's. Fails with the first error
/// [`validate`] finds.
pub fn build(d: &HouseDesign, pages: &Pages, extras: &Extras) -> Result<Prefab> {
    if let Some(p) = validate(d).into_iter().find(|p| p.error) {
        return Err(EditError::new("HOUSE_DESIGN", p.message));
    }
    let house = house_part(d, pages)?;
    let Some(y) = &d.yard else { return Ok(house) };
    let plan = footprint(d);
    let (lo, _) = house_bounds(d);
    let (pw, ph) = (y.size[0] as usize * 4, y.size[1] as usize * 4);
    // Everything goes 8 cells in, so turned props never leave the scratch chunk.
    const IN: i32 = 8;
    let yard = yard_part(d, y, pages.village, extras.ground)?;
    let house_at = ((lo[0] as i32 - y.at[0] as i32) * 4, (lo[1] as i32 - y.at[1] as i32) * 4);
    let mut parts = vec![
        kit::Part { prefab: &yard, col: IN as u8, row: IN as u8, rot: 0, lift: 0 },
        kit::Part { prefab: &house, col: (IN + house_at.0) as u8, row: (IN + house_at.1) as u8, rot: 0, lift: 0 },
    ];
    // Walk cells: the plot open, then the house, the fence and each prop on top.
    let mut heights = yard.heights.clone();
    stamp(&mut heights, pw, &house, house_at.0 as usize, house_at.1 as usize, 0);
    // The house's own modules (an L's empty corner is open ground).
    let house_cells: BTreeSet<(usize, usize)> = plan
        .keys()
        .flat_map(|m| {
            let (c, r) = (((m[0] - y.at[0] as i16) * 4) as usize, ((m[1] - y.at[1] as i16) * 4) as usize);
            (0..16).map(move |i| (c + i % 4, r + i / 4))
        })
        .collect();
    for p in &y.props {
        let prefab = extras.props.get(&p.item).ok_or_else(|| EditError::new("PROP_MISSING", format!("The {} isn't loaded from the disc.", p.item)))?;
        let (fw, fh) = kit::footprint(prefab, p.rot);
        let (c0, r0) = (p.at[0] as i32 - y.at[0] as i32 * 4, p.at[1] as i32 - y.at[1] as i32 * 4);
        if c0 < 0 || r0 < 0 || c0 as usize + fw > pw || r0 as usize + fh > ph {
            return Err(EditError::new("PROP_OFF_PLOT", format!("The {} runs off the plot.", p.item)));
        }
        let cells: Vec<(usize, usize)> = (0..fh).flat_map(|r| (0..fw).map(move |c| (c0 as usize + c, r0 as usize + r))).collect();
        if cells.iter().any(|c| house_cells.contains(c)) {
            return Err(EditError::new("PROP_ON_HOUSE", format!("The {} stands on the house.", p.item)));
        }
        stamp(&mut heights, pw, prefab, c0 as usize, r0 as usize, p.rot);
        parts.push(kit::Part { prefab, col: (IN + c0) as u8, row: (IN + r0) as u8, rot: p.rot, lift: 0 });
    }
    let mut out = kit::compose(&parts, &d.name)?;
    if (out.area.w(), out.area.h()) != (pw, ph) {
        return Err(EditError::new("YARD_BUILD", "The yard's pieces didn't line up with its plot."));
    }
    out.heights = heights;
    out.category = CATEGORY.to_string();
    out.source_chunk = house.source_chunk;
    Ok(out)
}

/// Writes `p`'s walk cells (all but its ground-level ones), turned `rot`
/// quarter turns as [`kit::place`] turns them, into `out` (`w` cells wide)
/// with its footprint's min corner at (`col`, `row`).
fn stamp(out: &mut [PatchCell], w: usize, p: &Prefab, col: usize, row: usize, rot: u8) {
    let (pw, ph) = (p.area.w(), p.area.h());
    for j in 0..ph {
        for i in 0..pw {
            let cell = p.heights[j * pw + i];
            if cell == PatchCell::Rel(0) {
                continue;
            }
            let (ti, tj) = match rot % 4 {
                0 => (i, j),
                1 => (ph - 1 - j, i),
                2 => (pw - 1 - i, ph - 1 - j),
                _ => (j, pw - 1 - i),
            };
            if let Some(o) = out.get_mut((row + tj) * w + col + ti) {
                *o = cell;
            }
        }
    }
}

/// How far paths and beds sit above the ground: inside the 4-unit decal rule
/// (research 12), and clear of a doorstep (1 up), so no edge runs through one.
const DECAL: i16 = 2;

/// The plot's own geometry, the plot as its footprint: the fence with its
/// gates, and the paths and beds as ground tiles 2 units up. Its walk cells
/// close the fence's row (not a gate's) and water.
fn yard_part(d: &HouseDesign, y: &Yard, page: &Texture, ground: Option<&crate::vram::Vram>) -> Result<Prefab> {
    let plan = footprint(d);
    let mut b = Builder::new(page);
    let (ox, oz) = (y.at[0] as i16, y.at[1] as i16);
    // Builder coordinates are the plot's: its min corner at 0.
    b.shift = [-ox * M, 0, -oz * M];
    let (pw, ph) = (y.size[0] as usize * 4, y.size[1] as usize * 4);
    let mut heights = vec![PatchCell::Rel(0); pw * ph];
    // The fence and the hatch are the village's logs: a log house's own colour of them, else the village's.
    let logs = match (&d.colours.walls, d.style == STYLE) {
        (Paint::Custom { palette }, true) => b.custom(house::LOGS, palette),
        _ => house::LOGS,
    };
    // Paths and beds: each patch tile, and the grass that borders it, as the Ground tool draws them.
    let live: Vec<(i32, i32, Material)> = y
        .patches
        .iter()
        .filter(|p| on_plot(y, [p.at[0] as i16, p.at[1] as i16]) && !plan.contains_key(&[p.at[0] as i16, p.at[1] as i16]))
        .filter_map(|p| Some((p.at[0] as i32 - ox as i32, p.at[1] as i32 - oz as i32, Material::from_id(p.material)?)))
        .collect();
    let inside = |c: i32, r: i32| (0..y.size[0] as i32).contains(&c) && (0..y.size[1] as i32).contains(&r) && !plan.contains_key(&[(c + ox as i32) as i16, (r + oz as i32) as i16]);
    let mut tile_of: HashMap<([u8; 2], u16), u16> = HashMap::new();
    for (c, r, tile, clut, turn) in ground::patch_looks(&live, &inside) {
        let (x0, z0) = ((c as i16 + ox) * M, (r as i16 + oz) * M);
        // North-west, north-east, south-west, south-east (top-left, top-right, bottom-left, bottom-right from above).
        let idx = [b.vertex([x0, DECAL, z0 + M]), b.vertex([x0 + M, DECAL, z0 + M]), b.vertex([x0, DECAL, z0]), b.vertex([x0 + M, DECAL, z0])];
        let (u0, v0) = if ground.is_some() { (0u8, 0u8) } else { (tile[0] * 32, tile[1] * 32) };
        let corners = [[u0, v0], [u0 + 31, v0], [u0 + 31, v0 + 31], [u0, v0 + 31]];
        let at = |i: usize| corners[(i + 4 - turn as usize % 4) % 4];
        let uv = [at(0), at(1), at(3), at(2)];
        let tiles = &mut b.tiles;
        let tile_ix = ground.map(|v| {
            *tile_of.entry((tile, clut)).or_insert_with(|| {
                tiles.push(ground_tile(v, tile, clut));
                (tiles.len() - 1) as u16
            })
        });
        b.prims.push(PrefabPrim { prim: PrimData { list: 3, code: 0x2C, n: 4, idx, rgb: [[128; 3]; 4], uv, clut, tpage: ground::DECAL_PAGE }, tile: tile_ix });
    }
    for &(c, r, m) in &live {
        if m == Material::Water {
            for rr in 0..4 {
                for cc in 0..4 {
                    heights[(r as usize * 4 + rr) * pw + c as usize * 4 + cc] = PatchCell::Blocked;
                }
            }
        }
    }
    if y.fence {
        fence(&mut b, y, &plan, logs, &mut heights)?;
    }
    if let Some(h) = d.inside.as_ref().and_then(|i| i.cellar.as_ref()).and_then(|c| c.hatch) {
        if on_plot(y, [h.at[0] as i16, h.at[1] as i16]) {
            hatch(&mut b, h, logs)?;
        }
    }
    if b.pool.len() > u16::MAX as usize {
        return Err(EditError::new("HOUSE_SIZE", "The yard has too many vertices."));
    }
    Ok(Prefab {
        name: format!("{} yard", d.name),
        source_chunk: house::SOURCE_CHUNK,
        area: CellRect { c0: 0, r0: 0, c1: (pw - 1) as u8, r1: (ph - 1) as u8 },
        ground_y: 0,
        pool: std::mem::take(&mut b.pool),
        prims: std::mem::take(&mut b.prims),
        tiles: std::mem::take(&mut b.tiles),
        heights,
        category: CATEGORY.to_string(),
        stock: None,
        stock_rev: 0,
        design: None,
    })
}

/// A ground texture tile (32 × 32 on page 0x08) cut from `v`, sampled in place where that page is loaded.
fn ground_tile(v: &crate::vram::Vram, tile: [u8; 2], clut: u16) -> Tile {
    let (x0, y0) = (tile[0] as usize * 32, tile[1] as usize * 32);
    let hx = (ground::DECAL_PAGE as usize & 0xF) * 64;
    let mut pixels = String::with_capacity(32 * 32);
    for y in y0..y0 + 32 {
        for x in x0..x0 + 32 {
            let hw = v.get(hx + x / 4, y);
            pixels.push(char::from_digit(((hw >> ((x & 3) * 4)) & 0xF) as u32, 16).unwrap());
        }
    }
    let (cx, cy) = ((clut as usize & 0x3F) * 16, clut as usize >> 6);
    let palette = (0..16).map(|i| v.get(cx + i, cy)).collect();
    Tile { w: 32, h: 32, pixels, palette, origin: Some(TileOrigin { page: ground::DECAL_PAGE, x: x0 as u16, y: y0 as u16, clut }) }
}

/// Post size, rail heights and the fence line's inset from the plot's edge.
const POST: i16 = 8;
const POST_TOP: i16 = 54;
const RAILS: [[i16; 2]; 2] = [[18, 26], [38, 46]];
const RAIL: i16 = 4;
const INSET: i16 = 8;
/// One log's band on the logs texture, for the rails.
const RAIL_TEX: [u16; 4] = [0, 0, 63, 7];

/// The log fence round the plot, 8 units in from its edge: a post at every
/// module corner along it and two rails between, except where a gate leaves
/// a module open or the house stands on the plot's edge. Closes the edge row
/// of walk cells it runs along.
fn fence(b: &mut Builder, y: &Yard, plan: &BTreeMap<[i16; 2], usize>, logs: u16, heights: &mut [PatchCell]) -> Result<()> {
    let (x0, z0) = (y.at[0] as i16 * M + INSET, y.at[1] as i16 * M + INSET);
    let (x1, z1) = ((y.at[0] + y.size[0]) as i16 * M - INSET, (y.at[1] + y.size[1]) as i16 * M - INSET);
    let pw = y.size[0] as usize * 4;
    let mut posts: BTreeSet<(i16, i16)> = BTreeSet::new();
    let gate = |m: [i16; 2], s: Side| y.gates.iter().any(|g| [g.at[0] as i16, g.at[1] as i16] == m && g.side == s);
    for side in Side::ALL {
        let n = if matches!(side, Side::North | Side::South) { y.size[0] } else { y.size[1] };
        for k in 0..n as i16 {
            let m = match side {
                Side::South => [y.at[0] as i16 + k, y.at[1] as i16],
                Side::North => [y.at[0] as i16 + k, (y.at[1] + y.size[1]) as i16 - 1],
                Side::West => [y.at[0] as i16, y.at[1] as i16 + k],
                Side::East => [(y.at[0] + y.size[0]) as i16 - 1, y.at[1] as i16 + k],
            };
            if gate(m, side) || plan.contains_key(&m) {
                continue;
            }
            // The run along this module, clipped to the inset line's corners.
            let (a, bb) = match side {
                Side::South | Side::North => ((m[0] * M).max(x0), ((m[0] + 1) * M).min(x1)),
                _ => ((m[1] * M).max(z0), ((m[1] + 1) * M).min(z1)),
            };
            let line = match side {
                Side::South => z0,
                Side::North => z1,
                Side::West => x0,
                Side::East => x1,
            };
            let along_x = matches!(side, Side::South | Side::North);
            let at = |t: i16| if along_x { (t, line) } else { (line, t) };
            posts.insert(at(a));
            posts.insert(at(bb));
            for [r0, r1] in RAILS {
                rail(b, along_x, line, a + POST, bb - POST, r0, r1, logs)?;
            }
            // Its row of walk cells.
            let (c0, r0) = (((m[0] - y.at[0] as i16) * 4) as usize, ((m[1] - y.at[1] as i16) * 4) as usize);
            for i in 0..4 {
                let (c, r) = match side {
                    Side::South => (c0 + i, r0),
                    Side::North => (c0 + i, r0 + 3),
                    Side::West => (c0, r0 + i),
                    Side::East => (c0 + 3, r0 + i),
                };
                heights[r * pw + c] = PatchCell::Blocked;
            }
        }
    }
    for (x, z) in posts {
        post(b, x, z, logs)?;
    }
    Ok(())
}

/// Half the cellar hatch's side: a frame of dark earth round three planks, flat on the ground in its module's middle.
const HATCH: i16 = 36;

/// The cellar hatch on its yard module: the Knight steps onto it (a door box) to go down.
fn hatch(b: &mut Builder, h: super::inside::Hatch, logs: u16) -> Result<()> {
    let (cx, cz) = (h.at[0] as i16 * M + M / 2, h.at[1] as i16 * M + M / 2);
    let (y0, y1) = (DECAL, DECAL + 1);
    b.flat([[cx - HATCH, y0, cz + HATCH], [cx + HATCH, y0, cz + HATCH], [cx - HATCH, y0, cz - HATCH], [cx + HATCH, y0, cz - HATCH]], DOORWAY);
    let inner = HATCH - 6;
    let plank = 2 * inner / 3;
    for k in 0..3 {
        let (z0, z1) = (cz - inner + k * plank + 1, cz - inner + (k + 1) * plank - 1);
        b.quad([[cx - inner, y1, z1], [cx + inner, y1, z1], [cx - inner, y1, z0], [cx + inner, y1, z0]], RAIL_TEX, logs, [[0, 0], [63, 0], [0, 7], [63, 7]], Shade(124))?;
    }
    Ok(())
}

/// A log post 16 units square centred on (x, z): four sides and a top.
fn post(b: &mut Builder, x: i16, z: i16, logs: u16) -> Result<()> {
    let (xa, xb, za, zb, t) = (x - POST, x + POST, z - POST, z + POST, POST_TOP);
    let faces: [([[i16; 3]; 4], Side); 4] = [
        ([[xa, t, za], [xb, t, za], [xa, 0, za], [xb, 0, za]], Side::South),
        ([[xb, t, zb], [xa, t, zb], [xb, 0, zb], [xa, 0, zb]], Side::North),
        ([[xb, t, za], [xb, t, zb], [xb, 0, za], [xb, 0, zb]], Side::East),
        ([[xa, t, zb], [xa, t, za], [xa, 0, zb], [xa, 0, za]], Side::West),
    ];
    for (c, s) in faces {
        b.panel(c, house::tex::JAMB, logs, false, s.shade())?;
    }
    b.panel([[xa, t, zb], [xb, t, zb], [xa, t, za], [xb, t, za]], house::tex::JAMB, logs, false, Shade(120))
}

/// A rail from `a` to `b` along x (or z) on the fence line, between heights `h0`..`h1`: two sides and a top.
#[allow(clippy::too_many_arguments)]
fn rail(b: &mut Builder, along_x: bool, line: i16, a: i16, bb: i16, h0: i16, h1: i16, logs: u16) -> Result<()> {
    if bb <= a {
        return Ok(());
    }
    let u1 = ((bb - a) as i32 * 63 / M as i32).clamp(1, 63) as u8;
    let uv = [[0, 0], [u1, 0], [0, 7], [u1, 7]];
    let (lo, hi) = (line - RAIL, line + RAIL);
    let (faces, top) = if along_x {
        (
            [([[a, h1, lo], [bb, h1, lo], [a, h0, lo], [bb, h0, lo]], Side::South), ([[bb, h1, hi], [a, h1, hi], [bb, h0, hi], [a, h0, hi]], Side::North)],
            [[a, h1, hi], [bb, h1, hi], [a, h1, lo], [bb, h1, lo]],
        )
    } else {
        (
            [([[lo, h1, bb], [lo, h1, a], [lo, h0, bb], [lo, h0, a]], Side::West), ([[hi, h1, a], [hi, h1, bb], [hi, h0, a], [hi, h0, bb]], Side::East)],
            [[lo, h1, bb], [hi, h1, bb], [lo, h1, a], [hi, h1, a]],
        )
    };
    for (c, s) in faces {
        b.quad(c, RAIL_TEX, logs, uv, s.shade())?;
    }
    b.quad(top, RAIL_TEX, logs, uv, Shade(118))
}

/// The house alone: walls, roof, gables and chimneys, its footprint at the plan's min corner.
fn house_part(d: &HouseDesign, pages: &Pages) -> Result<Prefab> {
    let st = style_of(d)?;
    let plan = footprint(d);
    let roof = roof_surface(d, &plan)?;
    let (lo, hi) = house_bounds(d);
    let mut b = Builder::new(pages.style);
    b.shift = [-lo[0] * M, 0, -lo[1] * M];
    let kit = Kit { pages: *pages, st, inks: inks(&mut b, st, &d.colours)? };
    let pitch = st.pitch as i32;
    let kinds = wall_kinds(d);
    let tall = |m: [i16; 2]| plan.get(&m).map(|&k| wing_storeys(d, k));
    let mut doors: Vec<([i16; 2], Side)> = Vec::new();
    // Walls, a storey at a time: on every edge of the plan, and over a lower wing next door.
    for &m in plan.keys() {
        let sm = tall(m).unwrap();
        for side in Side::ALL {
            let n = side.step(m);
            let from = match tall(n) {
                None => 0,
                Some(sn) if sn < sm => sn,
                _ => continue,
            };
            // The lower wing's roof along this edge, at the module's left and right as seen from outside.
            let clip = (from > 0).then(|| {
                let at = frame(m, side);
                let rm = &roof[&n];
                let height = |p: [i16; 3]| corners(n).iter().position(|c| *c == [p[0] as i32, p[2] as i32]).map_or(rm.eave, |i| rm.eave + rm.rise[i] as i16);
                (height(at(0, 0, 0)), height(at(M, 0, 0)))
            });
            let (l, r) = side.left_right();
            // Log ends where the wall's run turns, storey by storey.
            let runs = |s: Side, storey: u8| {
                let o = s.step(m);
                tall(o).is_some_and(|so| so > storey && tall(side.step(o)).is_none_or(|t| t <= storey))
            };
            for s in from..sm {
                let ends = match (runs(l, s), runs(r, s)) {
                    (true, true) => Ends::None,
                    (false, true) => Ends::Left,
                    (true, false) => Ends::Right,
                    (false, false) => Ends::Both,
                };
                let kind = match kinds.get(&(s, m, side)).copied().unwrap_or(WallKind::Logs) {
                    WallKind::Door if s > 0 || from > 0 => WallKind::Logs,
                    k => k,
                };
                if kind == WallKind::Door {
                    doors.push((m, side));
                }
                wall(&mut b, &kit, m, side, kind, ends, s, s + 1 == sm, clip)?;
            }
        }
    }
    // The roof: one face per plane per module.
    let (rw, _) = dims(st.roof);
    for (&m, rm) in &roof {
        let c = corners(m);
        let roof_tile = kit.tile(&mut b, st.roof, None)?;
        for (q, ids) in &rm.faces {
            let p = |i: usize| [c[i][0] as i16, rm.eave + rm.rise[i] as i16, c[i][1] as i16];
            let uv = |i: usize| roof_uv(q, m, c[i][0], c[i][1], rw, st.roof_v);
            let shade = roof_shade(q.faces);
            if ids.len() == 4 {
                // Ridge side first (top-left, top-right), facing up.
                let order: [usize; 4] = match q.faces {
                    Side::South => [2, 3, 0, 1],
                    Side::North => [1, 0, 3, 2],
                    Side::West => [3, 1, 2, 0],
                    Side::East => [0, 2, 1, 3],
                };
                b.quad_on(order.map(p), roof_tile, order.map(uv), shade);
            } else {
                let mut t = [ids[0], ids[1], ids[2]];
                let n = normal(p(t[0]), p(t[1]), p(t[2]));
                if n[1] < 0 {
                    t.swap(1, 2);
                }
                b.tri_on(t.map(p), roof_tile, t.map(uv), shade);
            }
        }
    }
    // Gables: wherever the roof stands above the eave over an outside edge, or over a lower wing.
    let (gw, gh) = dims(st.gable);
    for (&m, rm) in &roof {
        for side in Side::ALL {
            if tall(side.step(m)).is_some_and(|t| t >= tall(m).unwrap()) {
                continue;
            }
            // The edge's two corners, left then right as seen from outside.
            let (a, bb) = match side {
                Side::South => (0, 1),
                Side::North => (3, 2),
                Side::East => (1, 3),
                Side::West => (2, 0),
            };
            let (ra, rb) = (rm.rise[a], rm.rise[bb]);
            if ra == 0 && rb == 0 {
                continue;
            }
            if ra != 0 && rb != 0 {
                return Err(EditError::new("ROOF_JOIN", format!("The roof overhangs an outside wall at module {}, {}.", m[0], m[1])));
            }
            let c = corners(m);
            let p = |i: usize, h: i16| [c[i][0] as i16, h, c[i][1] as i16];
            // Ridge corner up top; the triangle's right angle under it.
            let (high, low) = if ra > rb { (a, bb) } else { (bb, a) };
            let tri = [p(high, rm.eave + rm.rise[high] as i16), p(low, rm.eave), p(high, rm.eave)];
            let uv = [[gw as u8 - 1, 0], [0, gh as u8 - 1], [gw as u8 - 1, gh as u8 - 1]];
            let outward = match side {
                Side::North => [0, 0, 1],
                Side::South => [0, 0, -1],
                Side::East => [1, 0, 0],
                Side::West => [-1, 0, 0],
            };
            let n = normal(tri[0], tri[1], tri[2]);
            let (tri, uv) = if n[0] * outward[0] + n[2] * outward[2] < 0 { ([tri[0], tri[2], tri[1]], [uv[0], uv[2], uv[1]]) } else { (tri, uv) };
            let tile = kit.tile(&mut b, st.gable, None)?;
            b.tri_on(tri, tile, uv, side.shade());
        }
    }
    // Chimneys: a stack from the eave to 32 over the ridge.
    for c in &d.chimneys {
        let m = [c[0] as i16, c[1] as i16];
        let Some(rm) = roof.get(&m) else { continue };
        chimney(&mut b, &kit, [m[0] * M + 32, rm.eave, m[1] * M + 32], rm.eave + pitch as i16 + 32)?;
    }
    // Porches and balconies. A porch's posts are thin and stand in the cells either side of the way to the door,
    // so they close no walk cells: the Knight keeps 32 units from a closed cell, and two would leave him no way in.
    for e in &d.extras {
        let at = frame([e.at[0] as i16, e.at[1] as i16], e.side);
        match e.kind {
            ExtraKind::Porch => porch(&mut b, &kit, &at, e.side)?,
            ExtraKind::Balcony => balcony(&mut b, &kit, &at, e.storey, e.side)?,
        }
    }
    // Close every T-junction a lower wing's roof left against a taller wing's walls.
    b.prims = weld(&b.pool, std::mem::take(&mut b.prims));
    // Walk cells: the house blocks what it covers, except the cells in its doorways.
    let (w, h) = ((hi[0] - lo[0]) as usize * 4, (hi[1] - lo[1]) as usize * 4);
    let mut heights = vec![PatchCell::Rel(0); w * h];
    for &m in plan.keys() {
        let (c0, r0) = ((m[0] - lo[0]) as usize * 4, (m[1] - lo[1]) as usize * 4);
        for r in r0..r0 + 4 {
            for c in c0..c0 + 4 {
                heights[r * w + c] = PatchCell::Blocked;
            }
        }
    }
    for (m, side) in doors {
        let (c0, r0) = ((m[0] - lo[0]) as usize * 4, (m[1] - lo[1]) as usize * 4);
        let cells: [(usize, usize); 2] = match side {
            Side::South => [(c0 + 1, r0), (c0 + 2, r0)],
            Side::North => [(c0 + 1, r0 + 3), (c0 + 2, r0 + 3)],
            Side::West => [(c0, r0 + 1), (c0, r0 + 2)],
            Side::East => [(c0 + 3, r0 + 1), (c0 + 3, r0 + 2)],
        };
        for (c, r) in cells {
            heights[r * w + c] = PatchCell::Rel(0);
        }
    }
    if b.pool.len() > u16::MAX as usize {
        return Err(EditError::new("HOUSE_SIZE", "The house has too many vertices."));
    }
    Ok(Prefab {
        name: d.name.clone(),
        source_chunk: st.chunk,
        area: CellRect { c0: 0, r0: 0, c1: (w - 1) as u8, r1: (h - 1) as u8 },
        ground_y: 0,
        pool: std::mem::take(&mut b.pool),
        prims: std::mem::take(&mut b.prims),
        tiles: std::mem::take(&mut b.tiles),
        heights,
        category: CATEGORY.to_string(),
        stock: None,
        stock_rev: 0,
        design: None,
    })
}

/// How deep a porch and a balcony stand out from the wall.
const PORCH: i16 = 64;
const BALCONY: i16 = 48;
/// A porch roof's edge against the wall (the storey line over the door) and out front.
const PORCH_TOP: i16 = WALL;
const PORCH_LOW: i16 = 96;

/// An upright box on a wall's frame, `u0..u1` across and `d0..d1` out (d
/// negative), `lo` to `hi` high: its four sides with `t` over each, no top.
#[allow(clippy::too_many_arguments)]
fn upright<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, at: At, (u0, u1): (i16, i16), (d0, d1): (i16, i16), (lo, hi): (i16, i16), t: Tex, shade: Shade) -> Result<()> {
    let tile = kit.tile(b, t, None)?;
    let (w, h) = dims(t);
    let uv = [[0, 0], [w as u8 - 1, 0], [0, h as u8 - 1], [w as u8 - 1, h as u8 - 1]];
    let dim = |s: Shade, by: i16| Shade((s.0 as i16 + by).clamp(0, 255) as u8);
    // Out front, toward the wall, and its two ends (corners top-left, top-right, bottom-left, bottom-right as each is seen).
    b.quad_on([at(u0, hi, d0), at(u1, hi, d0), at(u0, lo, d0), at(u1, lo, d0)], tile, uv, shade);
    b.quad_on([at(u1, hi, d1), at(u0, hi, d1), at(u1, lo, d1), at(u0, lo, d1)], tile, uv, dim(shade, -24));
    b.quad_on([at(u0, hi, d1), at(u0, hi, d0), at(u0, lo, d1), at(u0, lo, d0)], tile, uv, dim(shade, 12));
    b.quad_on([at(u1, hi, d0), at(u1, hi, d1), at(u1, lo, d0), at(u1, lo, d1)], tile, uv, dim(shade, -12));
    Ok(())
}

/// A porch over the wall's door: a lean-to roof from the storey line down to
/// `PORCH_LOW` half a module out, its fascia, and two posts at its outer corners.
fn porch<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, at: At, side: Side) -> Result<()> {
    let st = kit.st;
    let (top, low, p) = (PORCH_TOP, PORCH_LOW, -PORCH);
    let roof = kit.tile(b, st.roof, None)?;
    let (rw, _) = dims(st.roof);
    let v = (st.roof_v / 2).max(1);
    b.quad_on([at(0, top, 0), at(M, top, 0), at(0, low, p), at(M, low, p)], roof, [[0, 0], [rw as u8 - 1, 0], [0, v], [rw as u8 - 1, v]], roof_shade(side));
    let plank = kit.tile(b, st.plank, None)?;
    let (pw, ph) = dims(st.plank);
    b.quad_on([at(0, low, p), at(M, low, p), at(0, low - 8, p), at(M, low - 8, p)], plank, [[0, 0], [pw as u8 - 1, 0], [0, ph as u8 - 1], [pw as u8 - 1, ph as u8 - 1]], side.shade());
    for (u0, u1) in [(4, 12), (M - 12, M - 4)] {
        upright(b, kit, at, (u0, u1), (p + 4, p + 12), (0, low - 8), st.post, side.shade())?;
    }
    Ok(())
}

/// A balcony on `storey`'s wall: a plank floor `BALCONY` deep with its
/// fascia, three posts, and a rail along the front and back to the wall at
/// either end.
fn balcony<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, at: At, storey: u8, side: Side) -> Result<()> {
    let st = kit.st;
    let (f, p) = (storey as i16 * WALL, -BALCONY);
    let plank = kit.tile(b, st.plank, None)?;
    let (pw, ph) = dims(st.plank);
    let whole = [[0, 0], [pw as u8 - 1, 0], [0, ph as u8 - 1], [pw as u8 - 1, ph as u8 - 1]];
    b.quad_on([at(0, f, 0), at(M, f, 0), at(0, f, p), at(M, f, p)], plank, whole, Shade(118));
    b.quad_on([at(0, f, p), at(M, f, p), at(0, f - 8, p), at(M, f - 8, p)], plank, whole, side.shade());
    // Everything stands a unit in from the floor's edges and the rails end inside the posts' faces, so no corner
    // lies on another face's edge.
    let (rail, top) = (f + 40, f + 46);
    for (u0, u1) in [(1, 9), (60, 68), (M - 9, M - 1)] {
        upright(b, kit, at, (u0, u1), (p + 1, p + 9), (f, top + 2), st.post, side.shade())?;
    }
    // The front rail between the posts, and a rail back to the wall at each end.
    upright(b, kit, at, (9, M - 9), (p + 3, p + 7), (rail, top), st.post, side.shade())?;
    for (u0, u1) in [(3, 7), (M - 7, M - 3)] {
        upright(b, kit, at, (u0, u1), (p + 9, -1), (rail, top), st.post, side.shade())?;
    }
    Ok(())
}

/// Each chimney's plan module and how high its top stands over the ground the house stands on (units, up), as
/// [`house_part`] builds it: 32 over the ridge.
pub fn chimney_tops(d: &HouseDesign) -> Result<Vec<([u8; 2], i16)>> {
    let pitch = style_of(d)?.pitch as i16;
    let roof = roof_surface(d, &footprint(d))?;
    Ok(d.chimneys.iter().filter_map(|c| roof.get(&[c[0] as i16, c[1] as i16]).map(|rm| (*c, rm.eave + pitch + 32))).collect())
}

/// A 64 × 64 chimney stack, its south-west corner at `at` (x, the bottom, z), up to `top`.
fn chimney<'a>(b: &mut Builder<'a>, kit: &Kit<'a>, at: [i16; 3], top: i16) -> Result<()> {
    let [side_tex, top_tex] = kit.st.chimney;
    let (x, lo, z, hi, w) = (at[0], at[1], at[2], top, 64);
    let side = kit.tile(b, side_tex, None)?;
    let (sw, sh) = dims(side_tex);
    let whole = |w: u16, h: u16| [[0, 0], [w as u8 - 1, 0], [0, h as u8 - 1], [w as u8 - 1, h as u8 - 1]];
    for (c, s) in [
        ([[x, hi, z], [x + w, hi, z], [x, lo, z], [x + w, lo, z]], SOUTH),
        ([[x + w, hi, z], [x + w, hi, z + w], [x + w, lo, z], [x + w, lo, z + w]], EAST),
        ([[x + w, hi, z + w], [x, hi, z + w], [x + w, lo, z + w], [x, lo, z + w]], NORTH),
        ([[x, hi, z + w], [x, hi, z], [x, lo, z + w], [x, lo, z]], WEST),
    ] {
        b.quad_on(c, side, whole(sw, sh), s);
    }
    let flue = kit.tile(b, top_tex, None)?;
    let (fw, fh) = dims(top_tex);
    b.quad_on([[x, hi, z + w], [x + w, hi, z + w], [x, hi, z], [x + w, hi, z]], flue, whole(fw, fh), Shade(120));
    Ok(())
}

/// Splits every polygon edge that another polygon's corner lies on (within ¾
/// of a unit) at that corner, triangulating what it splits: no T-junctions, which
/// crack on the PS1. Polygons with nothing on their edges stay as they are.
fn weld(pool: &[[i16; 3]], prims: Vec<PrefabPrim>) -> Vec<PrefabPrim> {
    let used: Vec<u16> = prims.iter().flat_map(|q| q.prim.idx[..q.prim.n as usize].to_vec()).collect::<BTreeSet<_>>().into_iter().collect();
    let mut out = Vec::with_capacity(prims.len());
    for q in prims {
        let n = q.prim.n as usize;
        // Around the polygon: quads are stored TL, TR, BL, BR.
        let ring: Vec<usize> = if n == 4 { vec![0, 1, 3, 2] } else { vec![0, 1, 2] };
        let mut pts: Vec<(u16, [u8; 2], [u8; 3])> = Vec::new();
        let mut split = false;
        for k in 0..ring.len() {
            let (i, j) = (ring[k], ring[(k + 1) % ring.len()]);
            let (a, c) = (pool[q.prim.idx[i] as usize], pool[q.prim.idx[j] as usize]);
            pts.push((q.prim.idx[i], q.prim.uv[i], q.prim.rgb[i]));
            let dv = [0, 1, 2].map(|t| c[t] as i64 - a[t] as i64);
            let len2 = dv.iter().map(|x| x * x).sum::<i64>();
            let mut on: Vec<(i64, u16)> = used
                .iter()
                .filter(|&&v| v != q.prim.idx[i] && v != q.prim.idx[j])
                .filter_map(|&v| {
                    let e = [0, 1, 2].map(|t| pool[v as usize][t] as i64 - a[t] as i64);
                    let t = dv[0] * e[0] + dv[1] * e[1] + dv[2] * e[2];
                    let cr = [dv[1] * e[2] - dv[2] * e[1], dv[2] * e[0] - dv[0] * e[2], dv[0] * e[1] - dv[1] * e[0]];
                    // Strictly between the ends, and less than ¾ of a unit off the line (a rounded crossing; a door's step floats a whole unit clear).
                    (t > 0 && t < len2 && cr.iter().map(|x| x * x).sum::<i64>() * 16 <= len2 * 9).then_some((t, v))
                })
                .collect();
            on.sort();
            for (t, v) in on {
                let f = t as f64 / len2 as f64;
                let lerp = |x: u8, y: u8| (x as f64 + f * (y as f64 - x as f64)).round() as u8;
                let (ui, uj, ci, cj) = (q.prim.uv[i], q.prim.uv[j], q.prim.rgb[i], q.prim.rgb[j]);
                pts.push((v, [lerp(ui[0], uj[0]), lerp(ui[1], uj[1])], [0, 1, 2].map(|k| lerp(ci[k], cj[k]))));
                split = true;
            }
        }
        if !split {
            out.push(q);
            continue;
        }
        // Clip ears whose triangle has an area and whose new edge has no other corner on it, keeping every corner a corner of some triangle.
        let cross = |a: usize, b: usize, c: usize| {
            let (p, r, s) = (pool[pts[a].0 as usize], pool[pts[b].0 as usize], pool[pts[c].0 as usize]);
            let (u, v) = ([0, 1, 2].map(|k| r[k] as i64 - p[k] as i64), [0, 1, 2].map(|k| s[k] as i64 - p[k] as i64));
            [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
        };
        let area = |a: usize, b: usize, c: usize| cross(a, b, c) != [0, 0, 0];
        let mut ring: Vec<usize> = (0..pts.len()).collect();
        let tri = |a: usize, b: usize, c: usize| {
            let mut p = q;
            p.prim.n = 3;
            p.prim.list = q.prim.list.saturating_sub(if n == 4 { 2 } else { 0 });
            p.prim.code = if n == 4 { q.prim.code - 8 } else { q.prim.code };
            p.prim.idx = [pts[a].0, pts[b].0, pts[c].0, 0];
            p.prim.uv = [pts[a].1, pts[b].1, pts[c].1, [0, 0]];
            p.prim.rgb = [pts[a].2, pts[b].2, pts[c].2, pts[c].2];
            p
        };
        let mut tris = Vec::new();
        while ring.len() > 3 {
            let m = ring.len();
            let ear = |k: usize| {
                let (a, c) = (ring[(k + m - 1) % m], ring[(k + 1) % m]);
                area(a, ring[k], c) && ring.iter().all(|&o| o == a || o == ring[k] || o == c || area(a, o, c))
            };
            let Some(k) = (0..m).find(|&k| ear(k)) else { break };
            tris.push(tri(ring[(k + m - 1) % m], ring[k], ring[(k + 1) % m]));
            ring.remove(k);
        }
        if ring.len() == 3 && area(ring[0], ring[1], ring[2]) {
            tris.push(tri(ring[0], ring[1], ring[2]));
            out.extend(tris);
        } else {
            // It wouldn't split cleanly: keep it whole rather than leave a hole.
            out.push(q);
        }
    }
    out
}

/// (b − a) × (c − a) in builder coordinates (x, height, z).
fn normal(a: [i16; 3], b: [i16; 3], c: [i16; 3]) -> [i32; 3] {
    let (u, v) = ([0, 1, 2].map(|k| b[k] as i32 - a[k] as i32), [0, 1, 2].map(|k| c[k] as i32 - a[k] as i32));
    [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
}

/* ───────────────────────── Cost, palettes, presets ───────────────────────── */

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub polygons: u32,
    /// Of those, the house's and the yard's (fence, paths, beds, props).
    pub house: u32,
    pub yard: u32,
    pub vertices: u32,
    pub tiles: u32,
    /// Palettes of the design's own (each takes a CLUT slot where it's placed).
    pub palettes: u32,
    /// Walk cells it closes.
    pub cells: u32,
    /// Footprint in cells.
    pub w: u32,
    pub h: u32,
    /// Units above the ground.
    pub height: i16,
}

/// What the built prefab `p` costs; with a yard, `pages` build the house alone for the split.
pub fn cost(d: &HouseDesign, p: &Prefab, pages: &Pages) -> Cost {
    let house = if d.yard.is_some() { house_part(d, pages).map_or(0, |h| h.prims.len() as u32) } else { p.prims.len() as u32 };
    Cost {
        polygons: p.prims.len() as u32,
        house,
        yard: (p.prims.len() as u32).saturating_sub(house),
        vertices: p.pool.len() as u32,
        tiles: p.tiles.len() as u32,
        palettes: [&d.colours.roof, &d.colours.trim, &d.colours.walls].iter().filter(|c| matches!(c, Paint::Custom { .. })).count() as u32,
        cells: p.heights.iter().filter(|c| **c == PatchCell::Blocked).count() as u32,
        w: p.area.w() as u32,
        h: p.area.h() as u32,
        height: p.height(),
    }
}

/// A retail palette a colour slot offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetailPalette {
    pub key: String,
    /// 16 BGR555 entries.
    pub palette: Vec<u16>,
    /// The entry the slot's texture shows most: its swatch (a palette of the design's own shows its entry there). A
    /// palette two slots share (Trisken's walls and its blue roof) would otherwise show the other's colour.
    pub main: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Palettes {
    pub roof: Vec<RetailPalette>,
    pub trim: Vec<RetailPalette>,
    pub walls: Vec<RetailPalette>,
}

fn palette_at(page: &Texture, clut: u16) -> Result<Vec<u16>> {
    let (r, e) = page.clut.as_ref().ok_or_else(|| EditError::new("HOUSE_TEXTURE", "The style's page has no palettes."))?;
    let at = ((clut >> 6) as usize).checked_sub(r.y as usize).zip(((clut & 0x3F) as usize * 16).checked_sub(r.x as usize)).map(|(row, col)| row * r.w as usize + col);
    at.and_then(|o| e.get(o..o + 16)).map(|p| p.to_vec()).ok_or_else(|| EditError::new("HOUSE_TEXTURE", format!("CLUT {clut:#06x} isn't in the style's page.")))
}

/// The colour index `rect` of `page` shows most, transparent texels left out (`palette` says which those are).
fn main_index(page: &Texture, rect: [u16; 4], palette: &[u16]) -> usize {
    let mut n = [0u32; 16];
    for y in rect[1]..=rect[3] {
        for x in rect[0]..=rect[2] {
            let b = page.pixels.get(y as usize * page.image.w as usize * 2 + x as usize / 2).copied().unwrap_or(0);
            n[if x & 1 == 0 { b & 15 } else { b >> 4 } as usize] += 1;
        }
    }
    (0..16).filter(|&i| palette.get(i).is_some_and(|&e| e != 0)).max_by_key(|&i| n[i]).unwrap_or(0)
}

/// The retail palettes of each of `st`'s colour slots (what the swatches and the colour dialog start from), from its `page`.
pub fn palettes(st: &Style, page: &Texture) -> Result<Palettes> {
    // What each slot paints most: the roof, the first band, the ground floor's wall.
    let shows = |slot: Slot| match slot {
        Slot::Roof => st.roof.rect,
        Slot::Trim => st.top.first().map_or(st.gable.rect, |b| b.tex.rect),
        Slot::Walls => st.ground[0].plain.rect,
    };
    let of = |slot: Slot| -> Result<Vec<RetailPalette>> {
        st.retail(slot)
            .iter()
            .map(|r| {
                let palette = palette_at(page, r.clut)?;
                let main = main_index(page, shows(slot), &palette);
                Ok(RetailPalette { key: r.key.to_string(), palette, main })
            })
            .collect()
    };
    Ok(Palettes { roof: of(Slot::Roof)?, trim: of(Slot::Trim)?, walls: of(Slot::Walls)? })
}

/// A style as the workbench offers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleInfo {
    pub key: String,
    pub name: String,
    pub town: String,
    pub chunk: u32,
    /// Rise per module of roof run.
    pub pitch: i16,
    /// How high the bands over the top storey stand: the eave is 128 a storey and this.
    pub top: i16,
    /// The roof it starts with.
    pub shape: RoofShape,
    /// Its colour of the walls, for the plan.
    pub palettes: Palettes,
}

/// `st` with its retail palettes, from its `page`.
pub fn style_info(st: &Style, page: &Texture) -> Result<StyleInfo> {
    Ok(StyleInfo { key: st.key.into(), name: st.name.into(), town: st.town.into(), chunk: st.chunk, pitch: st.pitch, top: st.top_h(), shape: st.shape, palettes: palettes(st, page)? })
}

fn set(storey: u8, at: [u8; 2], side: Side, kind: WallKind) -> WallSet {
    WallSet { storey, at, side, kind }
}

fn wing(x: u8, z: u8, length: u8, along: Axis) -> Wing {
    Wing { at: [x, z], length, along, storeys: None }
}

/// `d` moved `dx`, `dz` modules on the plan (its yard and inside stay put).
fn moved(mut d: HouseDesign, dx: u8, dz: u8) -> HouseDesign {
    for w in &mut d.wings {
        w.at = [w.at[0] + dx, w.at[1] + dz];
    }
    for w in &mut d.walls {
        w.at = [w.at[0] + dx, w.at[1] + dz];
    }
    for c in &mut d.chimneys {
        *c = [c[0] + dx, c[1] + dz];
    }
    for e in &mut d.extras {
        e.at = [e.at[0] + dx, e.at[1] + dz];
    }
    d
}

/// The starting points the empty workbench offers.
pub fn presets() -> Vec<(String, HouseDesign)> {
    use Side::*;
    use WallKind::*;
    let design = |name: &str, roof: &str, storeys: u8, wings: Vec<Wing>, walls: Vec<WallSet>, chimneys: Vec<[u8; 2]>| HouseDesign {
        name: name.to_string(),
        style: STYLE.to_string(),
        colours: Colours { roof: Paint::retail(roof), trim: Paint::retail(roof), walls: Paint::retail(LOGS_KEY) },
        roof: None,
        storeys,
        wings,
        walls,
        chimneys,
        extras: Vec::new(),
        yard: None,
        inside: None,
    };
    // The other styles' houses, in their own retail colours.
    let styled = |st: &Style, name: &str, storeys: u8, wings: Vec<Wing>, walls: Vec<WallSet>, chimneys: Vec<[u8; 2]>| HouseDesign { style: st.key.to_string(), colours: Colours::of(st), ..design(name, "orange", storeys, wings, walls, chimneys) };
    // A cottage on a fenced plot: a gate and a path to the door, a vegetable bed, a well and barrels.
    let farm = HouseDesign {
        yard: Some(Yard {
            at: [0, 0],
            size: [5, 4],
            fence: true,
            gates: vec![PlotEdge { at: [1, 0], side: South }],
            patches: vec![Patch { at: [1, 0], material: Material::Dirt as u8 }, Patch { at: [3, 2], material: Material::Forest as u8 }, Patch { at: [4, 2], material: Material::Forest as u8 }],
            props: vec![YardProp { item: "barrels".into(), at: [1, 12], rot: 0 }, YardProp { item: "stone-well".into(), at: [13, 1], rot: 0 }],
        }),
        ..design("Farm cottage", "red", 1, vec![wing(1, 1, 2, Axis::X)], vec![set(0, [1, 1], South, Door), set(0, [2, 1], South, Window), set(0, [1, 2], North, Window)], vec![[2, 2]])
    };
    vec![
        ("cottage".into(), design("Cottage", "orange", 1, vec![wing(0, 0, 2, Axis::X)], vec![set(0, [0, 0], South, Door), set(0, [1, 0], South, Window), set(0, [0, 1], North, Window)], vec![[1, 1]])),
        (
            "long-house".into(),
            design(
                "Long house",
                "red",
                1,
                vec![wing(0, 0, 5, Axis::X)],
                vec![set(0, [2, 0], South, Door), set(0, [0, 0], South, Window), set(0, [1, 0], South, Window), set(0, [3, 0], South, Window), set(0, [4, 0], South, Window), set(0, [1, 1], North, Window), set(0, [3, 1], North, Window)],
                vec![[0, 1]],
            ),
        ),
        (
            "l-house".into(),
            design(
                "L house",
                "blue",
                1,
                vec![wing(0, 0, 4, Axis::X), wing(2, 2, 2, Axis::Z)],
                vec![set(0, [1, 0], South, Door), set(0, [0, 0], South, Window), set(0, [3, 0], South, Window), set(0, [3, 3], East, Window), set(0, [0, 1], West, Window)],
                vec![[0, 1]],
            ),
        ),
        (
            "t-house".into(),
            design(
                "T house",
                "orange",
                1,
                vec![wing(0, 0, 6, Axis::X), wing(2, 2, 2, Axis::Z)],
                vec![set(0, [2, 0], South, Door), set(0, [0, 0], South, Window), set(0, [4, 0], South, Window), set(0, [5, 0], South, Window), set(0, [2, 3], West, Window), set(0, [3, 3], East, Window)],
                vec![[5, 1]],
            ),
        ),
        (
            "two-storey".into(),
            design(
                "Two-storey cottage",
                "blue",
                2,
                vec![wing(0, 0, 3, Axis::X)],
                vec![set(0, [1, 0], South, Door), set(0, [0, 0], South, Window), set(0, [2, 0], South, Window), set(1, [0, 0], South, Window), set(1, [1, 0], South, Window), set(1, [2, 0], South, Window)],
                vec![[2, 1]],
            ),
        ),
        ("farm-cottage".into(), farm),
        (
            "trisken-house".into(),
            // A module up the plan, so the porch has a module to stand over.
            moved(HouseDesign {
                extras: vec![Extra { kind: ExtraKind::Porch, storey: 0, at: [2, 0], side: South }, Extra { kind: ExtraKind::Balcony, storey: 1, at: [1, 0], side: South }],
                ..styled(
                    &style::TIMBER,
                    "Trisken town house",
                    2,
                    vec![wing(0, 0, 4, Axis::X), wing(1, 2, 2, Axis::Z)],
                    vec![set(0, [2, 0], South, Door), set(0, [0, 0], South, Window), set(0, [1, 0], South, Window), set(0, [3, 0], South, Window), set(0, [1, 3], West, Window), set(0, [2, 3], East, Window)],
                    vec![[3, 1]],
                )
            }, 0, 1),
        ),
        (
            "trisken-hall".into(),
            HouseDesign {
                wings: vec![wing(0, 0, 4, Axis::X), Wing { storeys: Some(1), ..wing(4, 0, 2, Axis::X) }],
                ..styled(
                    &style::TIMBER,
                    "Hall with a low wing",
                    2,
                    Vec::new(),
                    vec![set(0, [1, 0], South, Door), set(0, [0, 0], South, Window), set(0, [2, 0], South, Window), set(0, [3, 0], South, Window), set(0, [4, 0], South, Window), set(0, [5, 0], South, Window), set(1, [1, 0], South, Window), set(1, [2, 0], South, Window)],
                    vec![[0, 1]],
                )
            },
        ),
        (
            "claria-house".into(),
            moved(HouseDesign {
                extras: vec![Extra { kind: ExtraKind::Porch, storey: 0, at: [1, 0], side: South }],
                ..styled(
                    &style::PLASTER,
                    "San Claria house",
                    1,
                    vec![wing(0, 0, 3, Axis::X)],
                    vec![set(0, [1, 0], South, Door), set(0, [0, 0], South, Window), set(0, [2, 0], South, Window), set(0, [0, 1], North, Window), set(0, [2, 1], North, Window)],
                    Vec::new(),
                )
            }, 0, 1),
        ),
        (
            "stone-cottage".into(),
            styled(
                &style::STONE,
                "Stone cottage",
                1,
                vec![wing(0, 0, 3, Axis::X)],
                vec![set(0, [1, 0], South, Door), set(0, [0, 0], South, Window), set(0, [2, 0], South, Window), set(0, [1, 1], North, Window)],
                vec![[2, 1]],
            ),
        ),
    ]
}

/// A plan chip: `L · 4 × 3 · 2 storeys`.
pub fn chip(d: &HouseDesign) -> String {
    let plan = footprint(d);
    if plan.is_empty() {
        return "Empty".into();
    }
    let (lo, hi) = bounds(&plan);
    let shape = match d.wings.len() {
        1 => "",
        2 => {
            let joins = (0..2).any(|k| ends(d, &plan, k).iter().any(|e| matches!(e, End::Joins(_) | End::Abuts(_))));
            let (w, h) = (hi[0] - lo[0], hi[1] - lo[1]);
            if joins && plan.len() < (w * h) as usize {
                // An L leaves one corner of its box empty; a T leaves two.
                let empty = |x: i16, z: i16| !plan.contains_key(&[x, z]);
                let corners = [empty(lo[0], lo[1]), empty(hi[0] - 1, lo[1]), empty(lo[0], hi[1] - 1), empty(hi[0] - 1, hi[1] - 1)].iter().filter(|&&c| c).count();
                if corners == 1 {
                    "L · "
                } else {
                    "T · "
                }
            } else {
                ""
            }
        }
        _ => "",
    };
    let storeys = if d.storeys == 1 { "1 storey".to_string() } else { format!("{} storeys", d.storeys) };
    format!("{shape}{} × {} · {storeys}", hi[0] - lo[0], hi[1] - lo[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::chunk::Chunk;
    use crate::edit::{inside, prefab};
    use crate::tim::VramRect;

    /// A page with every palette the styles use (CLUT rows 480–495, the whole width) and some texels.
    fn page() -> Texture {
        let mut c = Chunk::blank(0);
        c.texture.clut = Some((VramRect { x: 0, y: 480, w: 1024, h: 16 }, (0..16 * 1024).map(|i| i as u16 | 0x8000).collect()));
        c.texture.pixels.iter_mut().enumerate().for_each(|(i, b)| *b = (i % 251) as u8);
        c.texture
    }

    fn preset(id: &str) -> HouseDesign {
        presets().into_iter().find(|(k, _)| k == id).unwrap().1
    }

    /// Stand-ins for the yard's stock props: a 2 × 2-cell chimney that closes its cells.
    fn extras(t: &Texture) -> Extras<'static> {
        let mut p = house::part(house::Part::Chimney, house::Roof::Red, t).unwrap();
        p.heights.iter_mut().for_each(|c| *c = PatchCell::Blocked);
        Extras { props: YARD_PROPS.iter().map(|id| (id.to_string(), p.clone())).collect(), ground: None }
    }

    /// Vertex `i`'s position strictly inside the segment `a`–`b`.
    fn inside(a: [i16; 3], b: [i16; 3], p: [i16; 3]) -> bool {
        let (d, e) = ([0, 1, 2].map(|k| b[k] as i64 - a[k] as i64), [0, 1, 2].map(|k| p[k] as i64 - a[k] as i64));
        let cross = [d[1] * e[2] - d[2] * e[1], d[2] * e[0] - d[0] * e[2], d[0] * e[1] - d[1] * e[0]];
        if cross != [0, 0, 0] {
            return false;
        }
        let t = d[0] * e[0] + d[1] * e[1] + d[2] * e[2];
        t > 0 && t < d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
    }

    /// No vertex of the pool lies inside any polygon's edge. Paths and beds
    /// float over the ground, so they are checked among themselves: a tile's
    /// corner touching a wall is a point, not a split edge.
    fn no_t_junctions(p: &Prefab) {
        let decal = |q: &PrefabPrim| q.prim.tpage == ground::DECAL_PAGE;
        for class in [true, false] {
            let of: Vec<&PrefabPrim> = p.prims.iter().filter(|q| decal(q) == class).collect();
            edges_clear(p, &of);
        }
    }

    fn edges_clear(p: &Prefab, prims: &[&PrefabPrim]) {
        let used: BTreeSet<u16> = prims.iter().flat_map(|q| q.prim.idx[..q.prim.n as usize].to_vec()).collect();
        for q in prims {
            let n = q.prim.n as usize;
            // Quads are drawn 0-1-3-2 around.
            let ring: Vec<u16> = if n == 4 { vec![q.prim.idx[0], q.prim.idx[1], q.prim.idx[3], q.prim.idx[2]] } else { q.prim.idx[..3].to_vec() };
            for k in 0..ring.len() {
                let (a, b) = (p.pool[ring[k] as usize], p.pool[ring[(k + 1) % ring.len()] as usize]);
                for &i in &used {
                    assert!(!inside(a, b, p.pool[i as usize]), "vertex {:?} lies inside edge {a:?}–{b:?} of {:?}", p.pool[i as usize], ring.iter().map(|&v| p.pool[v as usize]).collect::<Vec<_>>());
                }
            }
        }
    }

    #[test]
    fn every_preset_builds_without_t_junctions_and_places() {
        let t = page();
        for (id, d) in presets() {
            assert!(validate(&d).iter().all(|p| !p.error), "{id}: {:?}", validate(&d));
            let p = build(&d, &Pages::village(&t), &extras(&t)).unwrap();
            no_t_junctions(&p);
            let mut area = Chunk::new_area(1328);
            prefab::place(&mut area, &p, prefab::Placement { col: 40, row: 40, rot: 1, ground_y: None, write_ground: false, lift: 0 }).unwrap();
            let bytes = area.serialize().unwrap();
            assert_eq!(Chunk::parse(&bytes).unwrap().serialize().unwrap(), bytes, "{id} round-trips");
        }
    }

    #[test]
    fn an_l_roof_has_two_valleys_of_triangles() {
        let d = preset("l-house");
        let p = build(&d, &Pages::village(&page()), &Extras::default()).unwrap();
        // The modules where wing 2's prism runs into wing 1 (x 2–3, z 1) are each split in two.
        let roof_tris = p.prims.iter().filter(|q| q.prim.n == 3 && q.tile.is_some_and(|t| p.tiles[t as usize].w == 64 && p.tiles[t as usize].h == 64)).count();
        assert_eq!(roof_tris, 4);
        let plan = footprint(&d);
        let roof = roof_surface(&d, &plan).unwrap();
        // The ridges meet at (384, 128) at the ridge's height; the inner corner stays at the eave.
        assert_eq!(roof[&[2, 1]].rise, [64, 64, 0, 64]);
        assert_eq!(roof[&[3, 1]].rise, [64, 64, 64, 0]);
    }

    #[test]
    fn a_straight_house_matches_the_builder() {
        // The workbench's long house costs about what the kit's house builder's does.
        let t = page();
        let d = HouseDesign { walls: Vec::new(), chimneys: Vec::new(), ..preset("long-house") };
        let p = build(&d, &Pages::village(&t), &extras(&t)).unwrap();
        let old = house::house(house::HouseSpec { roof: house::Roof::Red, length: 5, door: None, windows: false, chimney: None }, &t).unwrap();
        assert_eq!((p.area.w(), p.area.h()), (old.area.w(), old.area.h()));
        // Walls: 14 modules × 2 rows + trim; roof: 10 quads; gables: 4 triangles.
        assert_eq!(p.prims.len(), 14 * 3 + 10 + 4);
        assert!(p.heights.iter().all(|c| *c == PatchCell::Blocked));
        assert_eq!(p.height(), 224);
    }

    #[test]
    fn storeys_raise_the_roof_and_doors_stay_on_the_ground() {
        let t = page();
        let d = preset("two-storey");
        let p = build(&d, &Pages::village(&t), &extras(&t)).unwrap();
        assert_eq!(p.height(), 2 * 128 + 32 + 64 + 32, "chimney top");
        // The door's two cells stay open; everything else is closed.
        assert_eq!(p.heights.iter().filter(|c| **c == PatchCell::Rel(0)).count(), 2);
        let mut up = d.clone();
        up.walls.push(set(1, [0, 0], Side::South, WallKind::Door));
        assert!(validate(&up).iter().any(|p| p.code == "DOOR_UPSTAIRS" && p.error));
    }

    #[test]
    fn bad_plans_are_named() {
        let base = HouseDesign { walls: Vec::new(), chimneys: Vec::new(), ..preset("cottage") };
        let check = |wings: Vec<Wing>| validate(&HouseDesign { wings, ..base.clone() }).into_iter().filter(|p| p.error).map(|p| p.code).collect::<Vec<_>>();
        assert_eq!(check(vec![]), ["NO_WINGS"]);
        assert_eq!(check(vec![wing(0, 0, 3, Axis::X), wing(1, 0, 2, Axis::Z)]), ["WINGS_OVERLAP"]);
        // An end against one module of another wing.
        assert_eq!(check(vec![wing(0, 0, 3, Axis::X), wing(3, 1, 3, Axis::Z)]), ["WING_JOIN"]);
        // A lone one-module wing; but as an arm it's fine.
        assert_eq!(check(vec![wing(0, 0, 1, Axis::X)]), ["WING_SHORT"]);
        assert!(check(vec![wing(0, 0, 4, Axis::X), wing(1, 2, 1, Axis::Z)]).is_empty());
        assert_eq!(check(vec![wing(0, 0, 8, Axis::X), wing(0, 2, 7, Axis::Z)]), ["HOUSE_SIZE"]);
        // In line, end to end: one long house.
        assert!(check(vec![wing(0, 0, 3, Axis::X), wing(3, 0, 2, Axis::X)]).is_empty());
    }

    #[test]
    fn a_t_and_a_cross_build_and_an_arm_continues_the_roof() {
        let t = page();
        for wings in [
            vec![wing(0, 0, 6, Axis::X), wing(2, 2, 2, Axis::Z)],
            vec![wing(0, 2, 6, Axis::X), wing(2, 4, 2, Axis::Z), wing(2, 0, 2, Axis::Z)],
            vec![wing(0, 0, 4, Axis::X), wing(1, 2, 1, Axis::Z)],
            vec![wing(0, 0, 3, Axis::X), wing(3, 0, 2, Axis::X)],
            vec![wing(0, 0, 3, Axis::X), wing(0, 2, 3, Axis::X)],
        ] {
            let d = HouseDesign { wings: wings.clone(), walls: Vec::new(), chimneys: Vec::new(), ..preset("cottage") };
            let p = build(&d, &Pages::village(&t), &extras(&t)).unwrap_or_else(|e| panic!("{wings:?}: {}", e.message));
            no_t_junctions(&p);
        }
    }

    /// The test plans: an L, a T, a cross, an arm, wings in line and side by side, and a lower wing on each.
    fn plans() -> Vec<Vec<Wing>> {
        let low = |w: Wing| Wing { storeys: Some(1), ..w };
        vec![
            vec![wing(0, 0, 2, Axis::X)],
            vec![wing(0, 0, 4, Axis::X), wing(2, 2, 2, Axis::Z)],
            vec![wing(0, 0, 6, Axis::X), wing(2, 2, 2, Axis::Z)],
            vec![wing(0, 2, 6, Axis::X), wing(2, 4, 2, Axis::Z), wing(2, 0, 2, Axis::Z)],
            vec![wing(0, 0, 4, Axis::X), wing(1, 2, 1, Axis::Z)],
            vec![wing(0, 0, 3, Axis::X), wing(3, 0, 2, Axis::X)],
            vec![wing(0, 0, 3, Axis::X), wing(0, 2, 3, Axis::X)],
            vec![wing(0, 0, 3, Axis::X), low(wing(3, 0, 2, Axis::X))],
            vec![wing(0, 0, 4, Axis::X), low(wing(1, 2, 2, Axis::Z))],
            vec![low(wing(0, 0, 4, Axis::X)), wing(1, 2, 3, Axis::Z)],
            vec![wing(0, 2, 6, Axis::X), low(wing(2, 4, 2, Axis::Z)), low(wing(2, 0, 2, Axis::Z))],
        ]
    }

    #[test]
    fn every_style_and_roof_builds_every_plan_without_t_junctions() {
        let t = page();
        for st in style::ALL {
            for shape in [RoofShape::Gable, RoofShape::Hip] {
                for storeys in [1, 2, 3] {
                    for wings in plans() {
                        let mut d = HouseDesign { style: st.key.into(), colours: Colours::of(st), roof: Some(shape), storeys, wings: wings.clone(), walls: Vec::new(), chimneys: Vec::new(), ..preset("cottage") };
                        // A door on the first south wall, windows on the rest at every storey, a chimney on the first module.
                        let plan = footprint(&d);
                        let south: Vec<[u8; 2]> = plan.keys().filter(|m| !plan.contains_key(&Side::South.step(**m))).map(|m| [m[0] as u8, m[1] as u8]).collect();
                        d.walls.push(set(0, south[0], Side::South, WallKind::Door));
                        d.walls.extend(south[1..].iter().flat_map(|&m| (0..storeys).map(move |s| set(s, m, Side::South, WallKind::Window))));
                        d.chimneys.push(plan.keys().next().map(|m| [m[0] as u8, m[1] as u8]).unwrap());
                        let p = build(&d, &Pages::village(&t), &extras(&t)).unwrap_or_else(|e| panic!("{} {shape:?} {storeys} {wings:?}: {}", st.key, e.message));
                        no_t_junctions(&p);
                        let mut area = Chunk::new_area(1328);
                        prefab::place(&mut area, &p, prefab::Placement { col: 40, row: 40, rot: 3, ground_y: None, write_ground: false, lift: 0 }).unwrap();
                        assert!(area.serialize().is_ok());
                    }
                }
            }
        }
    }

    #[test]
    fn every_style_stacks_its_storeys_to_128() {
        for st in style::ALL {
            for rows in [st.ground, st.upper] {
                assert_eq!(rows.iter().map(|r| r.h).sum::<i16>(), WALL, "{}", st.key);
                assert!(rows.iter().filter(|r| r.window.is_some()).count() <= 1, "{}", st.key);
            }
            // A door's alcove spans the ground floor's two rows.
            assert_eq!(st.ground.len(), 2, "{}", st.key);
            assert!(st.top_h() > 0 && st.top_h() + st.pitch <= 176, "{}: a lower roof has to stay under the storey over it", st.key);
            for slot in Slot::ALL {
                assert!(!st.retail(slot).is_empty(), "{} {slot:?}", st.key);
            }
        }
    }

    #[test]
    fn a_hip_roof_slopes_every_open_end() {
        let t = page();
        let d = HouseDesign { roof: Some(RoofShape::Hip), chimneys: Vec::new(), ..preset("cottage") };
        let p = build(&d, &Pages::village(&t), &Extras::default()).unwrap();
        let roof: Vec<&PrefabPrim> = p.prims.iter().filter(|q| q.prim.idx[..q.prim.n as usize].iter().any(|&i| p.pool[i as usize][1] < -160)).collect();
        // A 2 × 2 wing hipped both ends is a pyramid: every module split on its diagonal, and no gables.
        assert_eq!(roof.len(), 8);
        assert!(roof.iter().all(|q| q.prim.n == 3 && q.tile.is_some_and(|k| p.tiles[k as usize].h == 64)));
        assert_eq!(p.height(), 160 + 64);
        // A longer wing keeps a ridge between its hips.
        let long = HouseDesign { roof: Some(RoofShape::Hip), walls: Vec::new(), chimneys: Vec::new(), ..preset("long-house") };
        let plan = footprint(&long);
        let r = roof_surface(&long, &plan).unwrap();
        assert_eq!(r[&[0, 0]].rise, [0, 0, 0, 64], "the west hip");
        assert_eq!(r[&[2, 0]].rise, [0, 0, 64, 64], "the ridge");
        assert_eq!(r[&[2, 0]].faces.len(), 1);
    }

    #[test]
    fn a_lower_wing_stops_against_a_taller_wall() {
        let t = page();
        let d = preset("trisken-hall");
        let plan = footprint(&d);
        assert_eq!(ends(&d, &plan, 1)[0], End::Abuts(0));
        let r = roof_surface(&d, &plan).unwrap();
        // The low wing's roof starts at its own eave and stays its own.
        assert_eq!(r[&[4, 0]].eave, 160);
        assert_eq!(r[&[3, 0]].eave, 288);
        let p = build(&d, &Pages::village(&t), &Extras::default()).unwrap();
        no_t_junctions(&p);
        // The hall's east end stands over the low roof: faces on x = 512 from the low eave up to the hall's.
        let on_end: Vec<&PrefabPrim> = p.prims.iter().filter(|q| q.prim.idx[..q.prim.n as usize].iter().all(|&i| p.pool[i as usize][0] == 512)).collect();
        let lo = on_end.iter().flat_map(|q| q.prim.idx[..q.prim.n as usize].iter().map(|&i| -p.pool[i as usize][1])).min().unwrap();
        assert!(lo >= 128 && !on_end.is_empty(), "nothing hidden under the low roof is drawn: lowest {lo}");
        // Upstairs is the hall alone.
        assert!(inside::storey_cells(&d, &plan, 1).iter().all(|c| c[0] < 8));
        assert!(validate(&HouseDesign { wings: vec![wing(0, 0, 4, Axis::X), Wing { storeys: Some(3), ..wing(4, 0, 2, Axis::X) }], ..d.clone() }).iter().any(|p| p.code == "WING_STOREYS"));
    }

    #[test]
    fn porches_and_balconies_stand_on_their_walls() {
        let t = page();
        for st in style::ALL {
            let d = HouseDesign { style: st.key.into(), colours: Colours::of(st), ..preset("trisken-house") };
            let p = build(&d, &Pages::village(&t), &extras(&t)).unwrap_or_else(|e| panic!("{}: {}", st.key, e.message));
            no_t_junctions(&p);
            // The porch stands over the module in front of the door (z −1): the footprint grows a module south.
            assert_eq!((p.area.w(), p.area.h()), (16, 20), "{}", st.key);
            let cell = |c: usize, r: usize| p.heights[r * 16 + c];
            // Under it the whole module is open, posts and all: the way to the door is as wide as the door.
            assert!((8..12).all(|c| (0..4).all(|r| cell(c, r) == PatchCell::Rel(0))));
            assert_eq!(cell(8, 4), PatchCell::Blocked, "the house");
            let with_yard = HouseDesign { yard: Some(Yard { at: [0, 1], size: [5, 5], fence: false, gates: Vec::new(), patches: Vec::new(), props: Vec::new() }), ..d.clone() };
            assert!(validate(&with_yard).iter().any(|p| p.code == "EXTRA_OFF_PLOT"), "the plot starts at the house");
        }
        let base = preset("trisken-house");
        let with = |e: Extra| validate(&HouseDesign { extras: vec![e], ..base.clone() }).into_iter().filter(|p| p.error).map(|p| p.code).collect::<Vec<_>>();
        assert_eq!(with(Extra { kind: ExtraKind::Porch, storey: 0, at: [0, 1], side: Side::South }), ["PORCH_NO_DOOR"]);
        assert_eq!(with(Extra { kind: ExtraKind::Balcony, storey: 0, at: [0, 1], side: Side::South }), ["BALCONY_GROUND"]);
        assert_eq!(with(Extra { kind: ExtraKind::Balcony, storey: 1, at: [1, 2], side: Side::North }), ["EXTRA_OFF"], "the arm stands there");
        assert_eq!(with(Extra { kind: ExtraKind::Balcony, storey: 2, at: [0, 1], side: Side::South }), ["EXTRA_OFF"], "no third storey");
        assert_eq!(with(Extra { kind: ExtraKind::Balcony, storey: 1, at: [0, 0], side: Side::South }), ["EXTRA_OFF"], "off the plan's edge");
    }

    #[test]
    fn old_designs_keep_their_logs_colour() {
        let j = r#"{"name":"Old","storeys":1,"wings":[{"at":[0,0],"length":2,"along":"x"}],"colours":{"roof":{"kind":"retail","key":"red"},"trim":{"kind":"retail","key":"red"},"logs":{"kind":"retail","key":"village"}}}"#;
        let d: HouseDesign = serde_json::from_str(j).unwrap();
        assert_eq!(d.colours.walls, Paint::retail("village"));
        assert_eq!(d.style, STYLE);
        assert!(validate(&d).iter().all(|p| !p.error));
    }

    #[test]
    fn custom_colours_take_their_own_palettes() {
        let t = page();
        let mut d = preset("cottage");
        let pal: Vec<u16> = (0..16).map(|i| 0x8000 | i * 0x421).collect();
        d.colours.roof = Paint::Custom { palette: pal.clone() };
        let p = build(&d, &Pages::village(&t), &extras(&t)).unwrap();
        assert!(p.tiles.iter().any(|x| x.palette == pal));
        assert_eq!(cost(&d, &p, &Pages::village(&t)).palettes, 1);
        d.colours.walls = Paint::Custom { palette: vec![0; 3] };
        assert!(validate(&d).iter().any(|p| p.code == "PALETTE"));
    }

    #[test]
    fn designs_round_trip_as_json() {
        for (_, d) in presets() {
            let j = serde_json::to_string(&d).unwrap();
            assert_eq!(serde_json::from_str::<HouseDesign>(&j).unwrap(), d);
        }
        assert_eq!(chip(&preset("l-house")), "L · 4 × 4 · 1 storey");
        assert_eq!(chip(&preset("t-house")), "T · 6 × 4 · 1 storey");
        assert_eq!(chip(&preset("two-storey")), "3 × 2 · 2 storeys");
    }

    #[test]
    fn a_yard_fences_its_plot_with_a_gate_and_lays_its_paths() {
        let t = page();
        let d = preset("farm-cottage");
        let p = build(&d, &Pages::village(&t), &extras(&t)).unwrap();
        // The whole plot: 5 × 4 modules.
        assert_eq!((p.area.w(), p.area.h()), (20, 16));
        let cell = |c: usize, r: usize| p.heights[r * 20 + c];
        // The fence closes the plot's edge row; the gate (module 1 on the south) and the door's alcove stay open.
        assert_eq!(cell(0, 0), PatchCell::Blocked);
        assert_eq!(cell(19, 15), PatchCell::Blocked);
        assert!((4..8).all(|c| cell(c, 0) == PatchCell::Rel(0)), "the gate");
        assert_eq!(cell(5, 4), PatchCell::Rel(0), "the door's alcove");
        assert_eq!(cell(4, 5), PatchCell::Blocked, "the house");
        // Open ground stays open; the props close theirs (barrels at cell 1, 12; the well at 13, 1).
        assert_eq!(cell(2, 2), PatchCell::Rel(0));
        assert_eq!(cell(1, 12), PatchCell::Blocked);
        assert_eq!(cell(14, 2), PatchCell::Blocked);
        // Paths and beds are ground tiles on the shared page, 2 units up.
        let decals = p.prims.iter().filter(|q| q.prim.tpage == ground::DECAL_PAGE && q.tile.is_none()).count();
        assert!(decals >= 3, "{decals} decals");
        assert!(p.prims.iter().filter(|q| q.prim.tpage == ground::DECAL_PAGE).all(|q| q.prim.idx[..4].iter().all(|&i| p.pool[i as usize][1] == -DECAL)));
        no_t_junctions(&p);
        // The cost splits the house from the yard.
        let c = cost(&d, &p, &Pages::village(&t));
        assert_eq!(c.house + c.yard, c.polygons);
        assert!(c.yard > 0 && c.house > 0, "{c:?}");
        let mut area = Chunk::new_area(1328);
        prefab::place(&mut area, &p, prefab::Placement { col: 30, row: 30, rot: 2, ground_y: None, write_ground: false, lift: 0 }).unwrap();
        assert!(area.serialize().is_ok());
    }

    #[test]
    fn a_fence_costs_about_eleven_quads_a_module() {
        let t = page();
        let mut d = preset("farm-cottage");
        d.yard = Some(Yard { at: [0, 0], size: [4, 4], fence: true, gates: Vec::new(), patches: Vec::new(), props: Vec::new() });
        let with = build(&d, &Pages::village(&t), &extras(&t)).unwrap().prims.len();
        let house = build(&HouseDesign { yard: None, ..d.clone() }, &Pages::village(&t), &extras(&t)).unwrap().prims.len();
        // 16 modules of fence round the plot: 6 rail quads each, and 16 posts of 5.
        assert_eq!(with - house, 16 * 6 + 16 * 5);
    }

    #[test]
    fn bad_yards_are_named() {
        let t = page();
        let base = preset("farm-cottage");
        let with = |f: &dyn Fn(&mut Yard)| {
            let mut d = base.clone();
            f(d.yard.as_mut().unwrap());
            validate(&d).into_iter().filter(|p| p.error).map(|p| p.code).collect::<Vec<_>>()
        };
        assert!(with(&|y| y.size = [2, 2]).contains(&"PLOT_HOUSE".to_string()));
        assert_eq!(with(&|y| y.size = [13, 4]), ["PLOT_SIZE"]);
        assert_eq!(with(&|y| y.props.push(YardProp { item: "dragon".into(), at: [1, 1], rot: 0 })), ["PROP_UNKNOWN"]);
        assert_eq!(with(&|y| y.patches.push(Patch { at: [0, 3], material: 0 })), ["PATCH_MATERIAL"]);
        // A prop on the house is refused when the house is built.
        let mut d = base.clone();
        d.yard.as_mut().unwrap().props.push(YardProp { item: "barrel".into(), at: [5, 5], rot: 0 });
        assert_eq!(build(&d, &Pages::village(&t), &extras(&t)).unwrap_err().code, "PROP_ON_HOUSE");
        // Without the props' prefabs the yard can't be built.
        assert_eq!(build(&base, &Pages::village(&t), &Extras::default()).unwrap_err().code, "PROP_MISSING");
    }
}
