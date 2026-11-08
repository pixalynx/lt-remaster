//! Editor encoders: structured edits from the screens turned back into file
//! bytes the project stages (`docs/screens/03-projects-and-saving.md` §5.4).
//! Each one is proven against every retail instance (`tests/edits.rs`):
//! encoding the decoded retail values reproduces the disc bytes.
//!
//! - [`actor`]: actors added to an NPC pack, copies homed in another chunk (screen 10) → `npc/n`.
//! - [`bake`]: walk heights from a chunk's geometry (screen 10).
//! - [`blank`]: the blank slate: a pack, chunk or game emptied of the retail story, and the starter scene.
//! - [`door`]: trigger records (doors) added to an NPC pack's table 2 (screen 10) → `npc/n`.
//! - [`chunk`]: a whole world chunk, geometry, layers and texture page
//!   (screen 10) → `chunk/n`.
//! - [`enemy`]: enemy profile fields (screen 13) → `enemy/n`.
//! - [`pack`]: everything a project changes in an NPC pack, as one set the layout takes.
//! - [`obj`]: a chunk's geometry to and from Wavefront OBJ (screen 10).
//! - [`meshin`]: OBJ and glTF meshes read for the model importer (screen 06).
//! - [`model`]: character models to OBJ and back into their containers (screen 06).
//! - [`movie`]: movies re-made from PNG frames and a WAV, or taken from an STR (screen 14) → `movie/n` + `ovr/1`.
//! - [`anim`]: pose blocks and battle sequences written back (screen 08).
//! - [`prefab`]: construction-kit pieces cut from a chunk and placed in another (screen 10).
//! - [`text`]: dialogue lines (screen 12) → `npc/n`, in place or relocated.
//! - [`placement`]: actor placement words (screen 10) → `npc/n`.
//! - [`land`]: new land painted over a grid chunk's sea (screen 10's Land brush, the island builder) → `chunk/n`.
//! - [`map_screen`]: new land drawn into the world map screen's picture → `sys/16`–`sys/18`.
//! - [`sculpt`]: a grid chunk's ground raised and lowered in step with its neighbours (screen 10) → `chunk/n`.
//! - [`ladder`]: placement ladders with branches added, removed or changed (screen 10) → `npc/n`.
//! - [`remove`]: retail actor records taken out of the game (screen 10, the blank slate) → `npc/n`.
//! - [`spot`]: examine spots added to an NPC pack, beds, signs and chests (screen 10) → `npc/n`.
//! - [`tables`]: game-data records, items to encounter tables (screen 13) → `exe`, `ovr/2`, `ovr/4`, `toy/n`, `sys/9` + `sys/10`.
//! - [`tim`]: palette colours (screen 07) → the file that holds the TIM.
//! - [`teximport`]: PNG ↔ TIM pixels, keeping the rectangle (screen 07).
//! - [`tmdcodec`]: TMD models parsed and written back byte for byte (screen 06).
//! - [`trigger`]: Tier-1 trigger positions (screen 10) → `npc/n`.
//! - [`variants`]: a HackerGC base's chunk variants, a chunk read from a new area while a flag is set (screen 10) → `exe`.
//! - [`script`]: Tier-2 programs from the DSL (screen 11) → `npc/n`, appended
//!   after the grown lines of [`text`].

pub mod actor;
pub mod anim;
pub mod bake;
pub mod character;
pub mod charfile;
pub mod blank;
pub mod boot;
pub mod chunk;
pub mod design;
pub mod door;
pub mod enemy;
pub mod ground;
pub mod land;
pub mod map_screen;
pub mod ladder;
pub mod life;
pub mod lthouse;
pub mod house;
pub mod inside;
pub mod meshin;
pub mod model;
pub mod movie;
pub mod music;
pub mod obj;
pub mod pack;
pub mod prefab;
pub mod props;
pub mod remove;
pub mod scene;
pub mod sculpt;
pub mod seal;
pub mod sight;
pub mod sound;
pub mod spot;
pub mod stock;
pub mod style;
pub mod tables;
pub mod teximport;
pub mod placement;
pub mod script;
pub mod start;
pub mod text;
pub mod walk;
pub mod tim;
pub mod tmdcodec;
pub mod unbake;
pub mod trigger;
pub mod variants;

/// An edit that can't be encoded. `code` is stable for the UI; the message
/// says what to change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EditError {
    pub code: &'static str,
    pub message: String,
}

impl EditError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

pub type Result<T> = std::result::Result<T, EditError>;
