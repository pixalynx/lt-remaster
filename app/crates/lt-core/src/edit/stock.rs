//! The construction kit's stock library (screen 10): ready-made prefabs
//! taken from the retail world, and the modular house parts.
//!
//! The library holds recipes, not game data: where in which chunk each prop
//! stands. The editor cuts them out of the user's own disc on first use
//! ([`make`]) and keeps the result in the project like a harvest. The props
//! were found by surveying every chunk for repeated shapes
//! (`examples/prop_survey.rs`, `docs/research/05-props.md`); each recipe
//! names one instance.
//!
//! - [`Source::Object`]: the whole props centred in a cell, without the
//!   terrain ([`super::props`]); a tree is its crown and its trunk.
//! - [`Source::Area`]: the polygons in a cell rectangle, as a harvest takes
//!   them, for buildings joined to their neighbours.
//! - [`Source::Part`]: a house part made from chunk 78's textures
//!   ([`super::house`]).
//!
//! Textures on shared pages are copied into the prefab, so stock props look
//! the same in every chunk, new areas included.

use super::chunk::Chunk;
use super::house::{self, Part, Roof};
use super::prefab::{self, CellRect, HarvestOptions, Prefab};
use super::props;
use super::{EditError, Result};
use crate::vram::Vram;

/// Bumped when recipes or cutting change what an item comes out as; stock
/// prefabs a project holds from an older revision are made again.
/// 2: whole props leave their ground patch behind.
/// 3: Interiors pieces cut whole (books on a shelf, a chimney, panels in a
/// wall's plane) and sealed ([`super::seal`]).
pub const REVISION: u32 = 3;

/// Interiors items meant to stay open at the top: sealing gives their sides backs instead of a lid.
const OPEN_TOP: &[&str] = &["crate"];

pub const TREES: &str = "Trees";
pub const NATURE: &str = "Rocks & plants";
pub const VILLAGE: &str = "Village";
pub const BUILDINGS: &str = "Buildings";
/// Furniture from the village's rooms (research 13 §4.2), for house insides (screen 23).
pub const INTERIORS: &str = "Interiors";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Object { chunk: u32, at: [u8; 2] },
    Area { chunk: u32, rect: [u8; 4] },
    /// A piece of furniture welded into a room ([`props::furniture`]): the one whose cells match `rect` best.
    Piece { chunk: u32, rect: [u8; 4] },
    Part(Part, Roof),
}

impl Source {
    /// The chunk whose geometry or textures it is made from.
    pub fn chunk(&self) -> u32 {
        match *self {
            Source::Object { chunk, .. } | Source::Area { chunk, .. } | Source::Piece { chunk, .. } => chunk,
            Source::Part(..) => house::SOURCE_CHUNK,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stable id; parts are `log-<part>-<roof>`.
    pub id: String,
    pub name: String,
    pub category: &'static str,
    pub source: Source,
    /// House parts: the part, for showing one roof colour at a time.
    pub part: Option<&'static str>,
    pub roof: Option<&'static str>,
}

/// (id, name, category, chunk, centre cell): whole props.
const OBJECTS: &[(&str, &str, &str, u32, [u8; 2])] = &[
    ("pine", "Pine", TREES, 78, [102, 78]),
    ("pine-pair", "Pine pair", TREES, 78, [110, 76]),
    ("pine-tall", "Tall pine", TREES, 7, [32, 52]),
    ("pine-large", "Large pine", TREES, 43, [90, 6]),
    ("snow-pine", "Snowy pine", TREES, 61, [26, 66]),
    ("snow-pine-large", "Large snowy pine", TREES, 61, [34, 102]),
    ("star-pine", "Pine with a star", TREES, 61, [54, 82]),
    ("fern-large", "Large fern", NATURE, 258, [6, 38]),
    ("dead-tree", "Dead tree", TREES, 173, [50, 54]),
    ("burning-tree", "Burning tree", TREES, 173, [6, 22]),
    ("old-trunk", "Old trunk", TREES, 113, [98, 94]),
    ("fern", "Small fern", NATURE, 258, [2, 42]),
    ("rock", "Rock", NATURE, 111, [54, 62]),
    ("rock-sand", "Rock on sand", NATURE, 5, [118, 82]),
    ("boulder", "Boulder", NATURE, 43, [102, 78]),
    ("boulder-large", "Large boulder", NATURE, 5, [98, 66]),
    ("standing-stone", "Standing stone", NATURE, 129, [50, 102]),
    ("swamp-stump", "Swamp stump", NATURE, 113, [62, 82]),
    ("stumps", "Tree stumps", NATURE, 47, [18, 110]),
    ("flower", "Flower", NATURE, 78, [101, 58]),
    ("grass", "Grass tufts", NATURE, 77, [46, 82]),
    ("well", "Well", VILLAGE, 78, [72, 46]),
    ("barrel", "Barrel", VILLAGE, 78, [92, 47]),
    ("barrels", "Two barrels", VILLAGE, 78, [93, 50]),
    ("tub", "Tub", VILLAGE, 78, [74, 44]),
    ("birdhouse", "Birdhouse", VILLAGE, 78, [30, 33]),
    ("stone-bridge", "Stone bridge", VILLAGE, 78, [24, 38]),
    ("log-bridge", "Log bridge", VILLAGE, 78, [52, 16]),
    ("signpost", "Signpost", VILLAGE, 280, [10, 106]),
    ("stone-well", "Stone well", VILLAGE, 280, [84, 48]),
    ("hay-cart", "Hay cart", VILLAGE, 280, [124, 79]),
    ("planter", "Brick planter", VILLAGE, 48, [16, 46]),
    ("snow-hut", "Snow hut", BUILDINGS, 61, [92, 82]),
    ("round-hut", "Round hut", BUILDINGS, 280, [104, 26]),
    ("totem", "Stone totem", BUILDINGS, 41, [18, 20]),
    ("stone-gate", "Stone gate", BUILDINGS, 41, [30, 20]),
];

/// (id, name, category, chunk, cells c0, r0, c1, r1): buildings by area.
const AREAS: &[(&str, &str, &str, u32, [u8; 4])] = &[
    ("house-blue", "Log house, L-shaped (blue)", BUILDINGS, 78, [52, 24, 67, 35]),
    ("house-red", "Log house (red)", BUILDINGS, 78, [84, 56, 103, 71]),
    ("house-orange", "Log house (orange)", BUILDINGS, 78, [50, 60, 63, 71]),
    ("house-orange-l", "Log house, L-shaped (orange)", BUILDINGS, 78, [40, 46, 51, 59]),
    ("house-orange-long", "Long log house (orange)", BUILDINGS, 78, [72, 16, 90, 27]),
];

/// (id, name, chunk, cells c0, r0, c1, r1): furniture welded into the village's rooms.
const PIECES: &[(&str, &str, u32, [u8; 4])] = &[
    ("bed-quilt", "Double bed, patchwork quilt", 324, [36, 83, 39, 87]),
    ("bed-red", "Bed, red check quilt", 325, [27, 57, 31, 59]),
    ("bed-yellow", "Bed, yellow quilt", 325, [96, 53, 100, 55]),
    ("day-bed", "Day bed with bolster", 326, [60, 65, 65, 67]),
    ("fireplace", "Brick fireplace and stove", 324, [84, 68, 91, 71]),
    ("hearth", "Stone hearth", 325, [76, 61, 79, 63]),
    ("hearth-arched", "Stone hearth, arched", 326, [60, 58, 61, 61]),
    ("shelf", "Open shelf", 324, [44, 86, 47, 87]),
    ("cupboard", "Cupboard", 324, [46, 36, 47, 39]),
    ("cupboard-wide", "Cupboard, wide", 326, [73, 66, 76, 67]),
    ("dresser", "Dresser", 325, [89, 60, 90, 63]),
    ("table-long", "Long table", 324, [41, 76, 47, 78]),
    ("table-red", "Table, red cloth", 324, [32, 76, 35, 77]),
    ("table-rustic", "Table, rustic", 325, [44, 52, 45, 55]),
    ("table-green", "Table, green cloth", 326, [79, 56, 80, 59]),
    ("bench", "Bench", 325, [76, 52, 78, 58]),
    ("bench-small", "Bench, small", 326, [77, 56, 78, 59]),
    ("stool", "Stool", 326, [60, 69, 61, 70]),
    ("chest", "Chest", 324, [90, 64, 91, 65]),
    ("cask", "Barrel", 324, [36, 32, 37, 33]),
    ("sack", "Sack", 324, [36, 45, 37, 46]),
];

/// (id, name, chunk, centre cell, cells w, h): loose things in the village's rooms, whole props.
const LOOSE: &[(&str, &str, u32, [u8; 2], [u8; 2])] = &[
    ("pot", "Pot", 324, [33, 85], [2, 2]),
    ("screen", "Folding screen", 324, [41, 84], [2, 4]),
    ("wood-pile", "Wood pile", 324, [91, 70], [2, 3]),
    ("books", "Books", 324, [33, 77], [3, 3]),
    ("plant", "Plant pot", 324, [91, 64], [2, 2]),
    ("hay-bale", "Hay bale", 327, [45, 78], [3, 2]),
    ("crate", "Open crate", 327, [55, 95], [2, 4]),
];

/// An Interiors item's footprint in cells, unturned, as its recipe has it (the cut can come out a cell smaller).
pub fn interior_size(id: &str) -> Option<[u8; 2]> {
    PIECES
        .iter()
        .find(|p| p.0 == id)
        .map(|p| [p.3[2] - p.3[0] + 1, p.3[3] - p.3[1] + 1])
        .or_else(|| LOOSE.iter().find(|p| p.0 == id).map(|p| p.4))
}

/// The whole library, in shelf order.
pub fn catalogue() -> Vec<Item> {
    let mut out = Vec::new();
    for &(id, name, category, chunk, at) in OBJECTS {
        out.push(Item { id: id.into(), name: name.into(), category, source: Source::Object { chunk, at }, part: None, roof: None });
    }
    for &(id, name, category, chunk, rect) in AREAS {
        out.push(Item { id: id.into(), name: name.into(), category, source: Source::Area { chunk, rect }, part: None, roof: None });
    }
    for &(id, name, chunk, rect) in PIECES {
        out.push(Item { id: id.into(), name: name.into(), category: INTERIORS, source: Source::Piece { chunk, rect }, part: None, roof: None });
    }
    for &(id, name, chunk, at, _) in LOOSE {
        out.push(Item { id: id.into(), name: name.into(), category: INTERIORS, source: Source::Object { chunk, at }, part: None, roof: None });
    }
    for roof in Roof::ALL {
        for p in Part::ALL {
            out.push(Item {
                id: format!("log-{}-{}", p.key(), roof.key()),
                name: p.name().into(),
                category: house::CATEGORY,
                source: Source::Part(p, roof),
                part: Some(p.key()),
                roof: Some(roof.key()),
            });
        }
    }
    out
}

pub fn item(id: &str) -> Option<Item> {
    catalogue().into_iter().find(|i| i.id == id)
}

/// The polygons of the piece of furniture at cells `rect` (`[c0, r0, c1, r1]`) of a room chunk: the room's peeled
/// piece whose cells overlap `rect` most, the peeled parts inside its bounds (a panel not joined at a corner), and
/// the props of their own standing over it (books on a shelf, sticking out a little).
pub fn piece_parts(c: &Chunk, rect: [u8; 4]) -> Option<Vec<(usize, usize)>> {
    let want = rect.map(|v| v as i32);
    let overlap = |o: &props::Object| {
        let h = props::cells_of(o);
        let w = (want[2].min(h[2]) - want[0].max(h[0]) + 1).max(0);
        let d = (want[3].min(h[3]) - want[1].max(h[1]) + 1).max(0);
        w * d
    };
    let all = props::furniture(c);
    let piece = all.iter().max_by_key(|o| (overlap(o), o.prims.len())).filter(|o| overlap(o) > 0)?;
    let within = |o: &props::Object| (0..3).all(|k| o.min[k] >= piece.min[k] - 2 && o.max[k] <= piece.max[k] + 2);
    let over = |o: &props::Object| {
        let (x, z) = o.centre();
        (piece.min[0] as i32..=piece.max[0] as i32).contains(&x) && (piece.min[2] as i32..=piece.max[2] as i32).contains(&z) && o.min[1] >= piece.min[1] - 2
    };
    let loose: Vec<props::Object> = props::objects(c).into_iter().filter(|o| !props::is_room(o) && over(o)).collect();
    Some(all.iter().filter(|o| std::ptr::eq(*o, piece) || within(o)).chain(&loose).flat_map(|o| o.prims.iter().copied()).collect())
}

/// Makes `item` from its source chunk `c` (the retail chunk; for parts,
/// chunk 78) and the chunk's field VRAM (`shared`), which lends the
/// textures of shared pages.
pub fn make(item: &Item, c: &Chunk, shared: Option<&Vram>) -> Result<Prefab> {
    let mut p = match item.source {
        Source::Object { chunk, at } => {
            let opts = HarvestOptions { objects: true, ..HarvestOptions::default() };
            prefab::harvest_in(c, chunk, CellRect { c0: at[0], r0: at[1], c1: at[0], r1: at[1] }, &item.name, &opts, shared)?
        }
        Source::Area { chunk, rect } => {
            let opts = HarvestOptions { far_edges: true, ..HarvestOptions::default() };
            prefab::harvest_in(c, chunk, CellRect { c0: rect[0], r0: rect[1], c1: rect[2], r1: rect[3] }, &item.name, &opts, shared)?
        }
        Source::Piece { chunk, rect } => {
            let prims = piece_parts(c, rect).ok_or_else(|| EditError::new("STOCK_EMPTY", format!("No piece of chunk {chunk}'s rooms stands at cells {}..{} × {}..{}.", rect[0], rect[2], rect[1], rect[3])))?;
            let opts = HarvestOptions { skip_ground: false, prims, ..HarvestOptions::default() };
            prefab::harvest_in(c, chunk, CellRect { c0: 0, r0: 0, c1: 127, r1: 127 }, &item.name, &opts, shared)?
        }
        Source::Part(part, roof) => house::part(part, roof, &c.texture)?,
    };
    if p.prims.is_empty() {
        return Err(EditError::new("STOCK_EMPTY", format!("{} came out empty.", item.name)));
    }
    // Furniture was modelled for the field camera only; turned or stood in the open it needs its hidden sides.
    if item.category == INTERIORS {
        super::seal::seal(&mut p, OPEN_TOP.contains(&item.id.as_str()));
    }
    p.category = item.category.to_string();
    p.stock = Some(item.id.clone());
    p.stock_rev = REVISION;
    Ok(p)
}
