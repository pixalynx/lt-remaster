//! Guardian's Crusade disc and asset formats: the USA release (SLUS-00811), and the EU
//! (SLES-01793) and Japanese (SLPS-01531) ones read-only ([`release`]).
//!
//! Everything here is read-only against the retail image: builds write a new
//! image (see `docs/screens/15-file-patcher.md`). The layout facts come from the
//! decomp; see `docs/CONTEXT.md` and `docs/research/01-disc-filesystem-build.md`.

pub mod audio;
pub mod bps;
pub mod base;
pub mod build;
pub mod catalogue;
pub mod cdrom;
pub mod container;
pub mod cue;
pub mod disc;
pub mod edit;
pub mod engine;
pub mod error;
pub mod exe;
pub mod extract;
pub mod flags;
pub mod flyover;
pub mod fs;
pub mod gamedata;
pub mod identity;
pub mod iso9660;
pub mod layout;
pub mod mesh;
pub mod movie;
pub mod npc;
pub mod overlay;
pub mod overworld;
pub mod pack;
pub mod placement;
pub mod pose;
pub mod project;
pub mod raster;
pub mod release;
pub mod save;
pub mod savestate;
pub mod scene;
pub mod story;
pub mod text;
pub mod textbox;
pub mod tier2;
pub mod tim;
pub mod tmd;
pub mod toy;
pub mod trigger;
pub mod vram;
pub mod world;
pub mod worldmap;

pub use disc::{DiscImage, SectorSource};
pub use error::{LtError, Result};
pub use exe::PsxExe;
pub use fs::{Entry, FileTable, Kind};
