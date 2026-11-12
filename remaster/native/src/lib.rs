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
//! - [`billboard`] finds the field's billboards in each table (sparkles, smoke, shadows) for the
//!   2D layer to draw at the native scale and into a widened view's margins (LT-144), and moves
//!   them with the camera between ticks (LT-145); [`smoke`] rebuilds the chimney smoke the game
//!   leaves out of a widened view.
//! - [`crowd`] draws the NPCs the game gives no packet slot (LT-119), posed from their records with
//!   a shadow of the game's animation.
//! - The first flight's sky (LT-143, OVR24) is drawn as chunks are: its cells' NCDS colours and
//!   depth cue run on PSoXide's GTE for the original look ([`draw::SkyShade`]), and the depth cue
//!   fogs the lit look.
//! - [`water`] finds the game's water (level, water-blue polygons) and the lit look draws it as
//!   water: moving waves, the sky reflected, the sun's glints (LT-146), the scene mirrored about its
//!   plane in a pass of its own (LT-147), and shallows, foam and caustics by the shore (LT-148).
//! - Battles (LT-116) are drawn as the field is: the actors, Toys and arena natively through the
//!   battle camera, their lit polygons coloured by the game's own GTE operations with the light the
//!   capture took ([`draw::LightShade`], the arena light by [`draw::arena_colour`]); the lit look by
//!   their real normals under a rig made of the game's battle light ([`light::Rig::battle`]). The
//!   battle transition swirls the native picture of the field away ([`composite`]).
//! - A frozen frame (LT-140: the field menu's, a chunk load's, a battle's) is drawn from the native
//!   picture of that frame, and in a widened view carried on into the margins ([`composite`],
//!   [`layer2d`]). So is a full-screen fade or tint, over a frozen frame or the live field (LT-150,
//!   [`layer2d`]).

pub mod billboard;
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
pub mod smoke;
pub mod takeover;
pub mod water;
pub mod wide;
