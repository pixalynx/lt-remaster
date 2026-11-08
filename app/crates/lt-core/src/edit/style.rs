//! House styles for the workbench (screen 23 phase 4,
//! `docs/research/14-house-styles.md`): which textures a design's walls,
//! bands, door, windows, roof, gables and chimneys are cut from, and the
//! retail palettes each colour slot offers.
//!
//! Every retail town builds on the 128-unit module (`examples/style_survey.rs`),
//! so a style is only a table: the generator ([`super::design`]) is the same.
//! A storey is 128 units of rows (two log rows, or a beam band and a timber
//! frame), and the bands over the top storey take the eave up to where the
//! roof starts. Textures map the way the towns map them: a module's width to
//! the rectangle's width, a row's height to its height, and one module of
//! roof run to `roof_v` texels down the slope.

use serde::{Deserialize, Serialize};

use super::house::tex;

/// A design's three colour slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Roof,
    Trim,
    Walls,
}

impl Slot {
    pub const ALL: [Slot; 3] = [Slot::Roof, Slot::Trim, Slot::Walls];

    pub fn key(self) -> &'static str {
        match self {
            Slot::Roof => "roof",
            Slot::Trim => "trim",
            Slot::Walls => "walls",
        }
    }
}

/// What draws a texture: a colour slot's palette, one palette of the page, or
/// the chimney palette that goes with the roof's retail colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    Slot(Slot),
    Clut(u16),
    Chimney,
}

/// A rectangle `[x0, y0, x1, y1]` (inclusive) on the style's page, or on the
/// village's (chunk 78) when `village`, and what draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tex {
    pub rect: [u16; 4],
    pub ink: Ink,
    pub village: bool,
}

const fn own(rect: [u16; 4], ink: Ink) -> Tex {
    Tex { rect, ink, village: false }
}

const fn village(rect: [u16; 4], ink: Ink) -> Tex {
    Tex { rect, ink, village: true }
}

const WALLS: Ink = Ink::Slot(Slot::Walls);
const TRIM: Ink = Ink::Slot(Slot::Trim);
const ROOF: Ink = Ink::Slot(Slot::Roof);

/// How a row shows a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The row is drawn with another texture that has the window in it (the
    /// log window has log ends on its right, mirrored when the ends are on the left).
    Row(Tex),
    /// The window `src` (as tall as the row's texture, the same palette) is
    /// pasted into the row's own texture at texel column `x`.
    Paste { src: [u16; 4], x: u16 },
    /// A dark opening set into the wall, framed by the row's own texture.
    Recess,
}

/// One row of a storey's wall, `h` units high.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub h: i16,
    pub plain: Tex,
    /// Log ends on the right, and on both sides (the log style's corners).
    pub end: Option<Tex>,
    pub both: Option<Tex>,
    /// The row a Window wall shows its window in; the storey's other rows stay plain.
    pub window: Option<Window>,
}

const fn row(h: i16, plain: Tex) -> Row {
    Row { h, plain, end: None, both: None, window: None }
}

const fn window_row(h: i16, plain: Tex, window: Window) -> Row {
    Row { h, plain, end: None, both: None, window: Some(window) }
}

/// A band over the top storey (trim, a frieze, an eave), `h` units high.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Band {
    pub h: i16,
    pub tex: Tex,
}

/// A retail palette a slot offers: its key, its CLUT on the style's page, and
/// (for a roof) the chimney's CLUT on the village's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retail {
    pub key: &'static str,
    pub clut: u16,
    pub chimney: u16,
}

const fn retail(key: &'static str, clut: u16) -> Retail {
    Retail { key, clut, chimney: 0x7846 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoofShape {
    /// Gable ends: each open end of a wing is a gable wall.
    Gable,
    /// Hipped: each open end slopes down to the eave too.
    Hip,
}

#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub key: &'static str,
    pub name: &'static str,
    /// Where it comes from, for the inspector.
    pub town: &'static str,
    /// The chunk whose page the textures are cut from.
    pub chunk: u32,
    /// The ground floor's rows and every upper storey's, bottom up, 128 units each.
    pub ground: &'static [Row],
    pub upper: &'static [Row],
    /// Bands over the top storey, bottom up.
    pub top: &'static [Band],
    /// The door alcove's sides (a column, as tall as a row) and its step.
    pub jamb: Tex,
    pub step: Tex,
    /// One module of roof: across the rectangle along the ridge, `roof_v`
    /// texels down the slope.
    pub roof: Tex,
    pub roof_v: u8,
    /// Rise per module of run, from the eave to the ridge one module in.
    pub pitch: i16,
    pub shape: RoofShape,
    /// A half gable: eave at the bottom left, the ridge at the top right.
    pub gable: Tex,
    /// The chimney's sides and its top.
    pub chimney: [Tex; 2],
    /// A porch's or balcony's posts and rails, and its floor.
    pub post: Tex,
    pub plank: Tex,
    /// Retail palettes per slot; the first is the default (and what a palette of the design's own is cut with).
    pub roofs: &'static [Retail],
    pub trims: &'static [Retail],
    pub walls: &'static [Retail],
}

impl Style {
    pub fn retail(&self, slot: Slot) -> &'static [Retail] {
        match slot {
            Slot::Roof => self.roofs,
            Slot::Trim => self.trims,
            Slot::Walls => self.walls,
        }
    }

    /// Height of the bands over the top storey.
    pub fn top_h(&self) -> i16 {
        self.top.iter().map(|b| b.h).sum()
    }

    /// The eave over `storeys` storeys: where the roof starts.
    pub fn eave(&self, storeys: u8) -> i16 {
        storeys as i16 * 128 + self.top_h()
    }

    /// A storey's rows: the ground floor's or an upper one's.
    pub fn rows(&self, storey: u8) -> &'static [Row] {
        if storey == 0 {
            self.ground
        } else {
            self.upper
        }
    }
}

/* ───────────────────────── The styles ───────────────────────── */

const LOG_ROW: Row = Row {
    h: 64,
    plain: own(tex::LOGS_PLAIN, WALLS),
    end: Some(own(tex::LOGS_END, WALLS)),
    both: Some(own(tex::LOGS_BOTH, WALLS)),
    window: None,
};
const LOG_ROWS: [Row; 2] = [LOG_ROW, Row { window: Some(Window::Row(own(tex::WINDOW, WALLS))), ..LOG_ROW }];
const BRICK: [Tex; 2] = [village(tex::BRICK, Ink::Chimney), village(tex::FLUE, Ink::Chimney)];

/// Log house, Knight's village (chunk 78): the houses `edit::house` already builds.
pub const LOG: Style = Style {
    key: "log-village",
    name: "Log house",
    town: "Knight's village",
    chunk: 78,
    ground: &LOG_ROWS,
    upper: &LOG_ROWS,
    top: &[Band { h: 32, tex: own(tex::TRIM, TRIM) }],
    jamb: own(tex::JAMB, Ink::Clut(0x7806)),
    step: own(tex::STEP, Ink::Clut(0x7806)),
    roof: own(tex::ROOF, ROOF),
    roof_v: 63,
    pitch: 64,
    shape: RoofShape::Gable,
    gable: own(tex::GABLE, TRIM),
    chimney: BRICK,
    post: own(tex::JAMB, Ink::Clut(0x7806)),
    plank: own(tex::STEP, Ink::Clut(0x7806)),
    roofs: &[retail("orange", 0x7806), Retail { key: "blue", clut: 0x7A46, chimney: 0x7A86 }, retail("red", 0x7A06)],
    trims: &[retail("orange", 0x7886), retail("blue", 0x7A46), retail("red", 0x7A06)],
    walls: &[retail("village", 0x7806)],
};

/// Half-timber, Trisken (chunk 272): plaster and timber below a beam, a timber frame above, tiled roofs.
pub const TIMBER: Style = Style {
    key: "timber-trisken",
    name: "Half-timber",
    town: "Trisken",
    chunk: 272,
    ground: &[row(64, own([0, 224, 63, 255], WALLS)), window_row(64, own([0, 192, 63, 223], WALLS), Window::Row(own([192, 224, 255, 255], Ink::Clut(0x7942))))],
    // The frame's nail heads are the roof's colour, as in Trisken.
    upper: &[row(32, own([128, 160, 191, 175], TRIM)), window_row(96, own([128, 112, 191, 159], ROOF), Window::Recess)],
    top: &[Band { h: 32, tex: own([128, 160, 191, 175], TRIM) }],
    jamb: own([176, 192, 191, 223], WALLS),
    step: own([64, 0, 95, 23], Ink::Clut(0x7842)),
    roof: own([0, 0, 63, 47], ROOF),
    roof_v: 47,
    pitch: 96,
    shape: RoofShape::Gable,
    // Its edge is the roof's: in the roof's palette, as in Trisken.
    gable: own([0, 48, 63, 111], ROOF),
    chimney: BRICK,
    post: own([176, 192, 191, 223], WALLS),
    plank: own([128, 160, 191, 175], TRIM),
    roofs: &[retail("orange", 0x79C2), Retail { key: "blue", clut: 0x7802, chimney: 0x7A86 }],
    trims: &[retail("timber", 0x7802)],
    walls: &[retail("trisken", 0x7802)],
};

/// Plaster, San Claria (chunk 39): plaster walls under a fish frieze, thatched hip roofs.
pub const PLASTER: Style = Style {
    key: "plaster-claria",
    name: "Plaster",
    town: "San Claria",
    chunk: 39,
    ground: &[row(64, own([0, 224, 63, 255], WALLS)), window_row(64, own([0, 192, 63, 223], WALLS), Window::Paste { src: [0, 128, 31, 159], x: 16 })],
    upper: &[row(64, own([0, 224, 63, 255], WALLS)), window_row(64, own([0, 192, 63, 223], WALLS), Window::Paste { src: [0, 128, 31, 159], x: 16 })],
    top: &[Band { h: 32, tex: own([64, 224, 191, 254], TRIM) }, Band { h: 16, tex: own([0, 72, 63, 79], ROOF) }],
    jamb: own([64, 160, 79, 191], WALLS),
    step: own([80, 216, 111, 223], WALLS),
    roof: own([0, 0, 63, 62], ROOF),
    roof_v: 62,
    pitch: 64,
    shape: RoofShape::Hip,
    gable: own([0, 192, 63, 223], WALLS),
    chimney: BRICK,
    post: own([64, 160, 79, 191], WALLS),
    plank: own([80, 216, 111, 223], WALLS),
    roofs: &[retail("thatch", 0x7808)],
    trims: &[retail("frieze", 0x7888)],
    walls: &[retail("claria", 0x7888)],
};

/// Stone and plaster, Amos's inn (chunk 7): a stone ground floor, plaster above, a steep tiled roof.
pub const STONE: Style = Style {
    key: "stone-amos",
    name: "Stone and plaster",
    town: "Amos's inn",
    chunk: 7,
    ground: &[row(64, own([0, 0, 63, 31], WALLS)), window_row(64, own([0, 32, 63, 63], WALLS), Window::Row(own([128, 32, 191, 63], WALLS)))],
    upper: &[row(64, own([64, 0, 127, 31], WALLS)), window_row(64, own([64, 0, 127, 31], WALLS), Window::Row(own([128, 0, 191, 31], WALLS)))],
    top: &[Band { h: 32, tex: own([0, 32, 63, 47], TRIM) }],
    jamb: own([32, 128, 47, 159], Ink::Clut(0x7908)),
    step: own([0, 0, 63, 7], WALLS),
    roof: own([0, 73, 63, 127], ROOF),
    roof_v: 54,
    pitch: 128,
    shape: RoofShape::Gable,
    gable: own([128, 64, 159, 127], WALLS),
    chimney: BRICK,
    post: own([32, 128, 47, 159], Ink::Clut(0x7908)),
    plank: own([0, 32, 63, 47], TRIM),
    roofs: &[retail("red", 0x7848)],
    trims: &[retail("timber", 0x7808)],
    walls: &[retail("amos", 0x7808)],
};

pub const ALL: [&Style; 4] = [&LOG, &TIMBER, &PLASTER, &STONE];

pub fn style(key: &str) -> Option<&'static Style> {
    ALL.into_iter().find(|s| s.key == key)
}
