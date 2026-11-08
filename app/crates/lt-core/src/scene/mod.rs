//! Field cutscenes (screen 20): the catalogue of scenes a pack can play and
//! the scene player that runs one at the field's 30 Hz, ported from the
//! decomp (`Event_Tier2Interpreter`, the walk interpreters, the actor
//! sequencer and the field camera). Spec: `docs/screens/20-cutscenes.md`.
//! [`walk`] runs the same walk interpreter for the World editor's walk paths (LT-79).

pub mod anim;
pub mod catalog;
pub mod math;
pub mod play;
pub mod text;
pub mod vm;
pub mod walk;
pub mod world;

pub use catalog::{catalog, SceneEntry, SceneKind};
pub use vm::Ending;
pub use play::{area_bgm, play, play_inferred, Inferred, Meta, Playback, SceneInput, Start, ACTOR_BYTES, FRAME_HEAD};
