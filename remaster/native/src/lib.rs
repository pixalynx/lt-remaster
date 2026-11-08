//! Living Tool Remaster's native renderer, Stage A3 (LT-110; research 16 §6).
//!
//! A tick captured by `lt-scene` and resolved to assets by `lt-assets` is drawn unlit, with its
//! baked colours, through the game's own camera:
//! - the view and model matrices the GTE had;
//! - the projection distance H (892 in the field);
//! - the screen offset (257 or 160, 112);
//! - 512 or 320 wide. The 1.6 x-stretch of the 512 mode is in the view matrix's first row already.
//!
//! - [`gte`] projects with the GTE's own arithmetic and the draw paths' polygon tests (or in floating
//!   point, to see what precision alone changes).
//! - [`draw`] turns a tick into screen triangles.
//! - [`raster`] draws them with wgpu into a colour image and a coverage mask.
//! - [`gate`] compares that mask with the emulated frame's.
//! - [`takeover`] reads each tick's ordering table as the GPU gets it and sets terrain and
//!   characters aside; [`layer2d`] decodes the rest; [`composite`] draws the native view: native
//!   3D, then the 2D layer by bucket depth (LT-111). [`play`] runs a scene with it.
//! - [`light`] lights the native 3D (LT-112): world space, normals, the area's rig, a shadow map,
//!   and the lamps the art paints; the original look stays one switch away.
//! - [`interp`] blends the last two ticks for frames in between (LT-113): 60 and 120 Hz with the
//!   game still at 30.
//! - [`wide`] widens the view past 4:3 and the draw distance past the game's (LT-114): the game's
//!   object test widened, neighbour chunks off the disc, the 2D layer kept 4:3 in the middle.
//! - [`crowd`] draws the NPCs the game gives no packet slot (LT-119), posed from their records with
//!   a shadow of the game's animation.

pub mod check;
pub mod composite;
pub mod crowd;
pub mod draw;
pub mod gate;
pub mod gte;
pub mod interp;
pub mod layer2d;
pub mod light;
pub mod play;
pub mod raster;
pub mod takeover;
pub mod wide;
