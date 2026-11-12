//! Living Tool Remaster's scene capture (LT-108, Stage A1; research 16 §4.2–4.3).
//!
//! Observe hooks on the engine's draw functions turn each game tick (one pass of `main()`'s loop,
//! 30 Hz) into one [`Record`]: everything a native renderer needs to draw that frame.
//! - **Camera:** the view matrix (ctx+0x98, set by `Field_ChunkStreamTask`), the projection H, the
//!   screen offset and the 320/512 mode, read when the first chunk is drawn.
//! - **Terrain:** each `WorldChunk_DrawObjects` call (a chunk slot, with its world offset and the
//!   GTE's model-view matrix) and each `WorldChunk_DrawObjectMesh` group it draws.
//! - **Characters:** each `Model_DrawParts` call made by `Npc_DrawIfVisible`, keyed by character
//!   index: the world matrix (ctx+0x11C), the model-view matrix, the part record and a copy of its
//!   live, already-posed vertices. Two field event tasks draw an actor themselves the same way
//!   (LT-142): `Event_ActorApproachKnightTask` (Mr. O'Neal jumping to the Knight) and
//!   `Event_ActorFlickerInTask` (Zeppetto flickering out); their calls are characters too
//!   ([`Drawer`]).
//! - **Torches:** the flame records (type 5) when `Field_DrawTorchFlames` runs: the village's
//!   light sources.
//! - **The first flight's sky (LT-143):** OVR24 draws its sea and islands as map cells with its own
//!   drawer (`func_80133914`, the chunk mesh format, lit by NCDS and fogged): each cell drawn, its
//!   model-view matrix and place, and the GTE's lighting registers ([`SkyCell`], [`Record::sky_gte`]).
//!   Its task draws the area's model 2 at the Knight, the Baby's glow (an additive effect, left on
//!   the 2D layer): recorded as a character ([`Drawer::Flight`]), not set aside.
//! - **VRAM moves:** each `SetDrawMove` (a VRAM-to-VRAM copy the GPU runs with the tick's ordering
//!   table): the field's 13 texture and palette animations, the water among them.
//! - **Character records** as the tick leaves them (LT-119): every NPC's place, facing and
//!   animation state, drawn or not, and the two flags that hold the NPCs' animation and drawing.
//!   The machine's RAM a frame later is already the next tick's.
//! - **Battles (LT-116):** OVR03 draws the actors and Toys (`FUN_80144b48`) and the arena
//!   (`func_8014B4A4`, through `Model_DrawParts`' second entry; `FUN_8014c97c` while a battle light
//!   is set) with the same part records. Each is a character ([`Drawer::Battle`], [`Drawer::Arena`]),
//!   with the GTE's light registers at the call ([`GteLight`]): battle is where the game lights by
//!   normals, and those registers hold its light (directions, colours, the back colour) with every
//!   flash. The battle camera is the view at ctx+0x98 and the GTE's H (400) and offset.
//!
//! Ids are stable across ticks ([`Group::id`] is the chunk id and the mesh's offset in the chunk;
//! characters are keyed by index; [`Flame::id`] is the torch list's generation and the record
//! index), so a later stage can interpolate.
//! Nothing is written to the machine.

use emulator_core::Cpu;
use lt_emu_hooks::{Action, Call, Handler, Hook, Hooks};
use serde::Serialize;

pub mod census;
pub mod check;

/// `main()`'s loop: `jal ExecTaskTree` (the tick's tasks start) and `jal ProcessLoadRequestQueue`
/// right after it (they're done). Main is fixed, so the words only guard against another build.
pub const TICK_START: u32 = 0x8017_7E48;
pub const TICK_START_WORD: u32 = 0x0C05_ED80;
pub const TICK_END: u32 = 0x8017_7E50;
pub const TICK_END_WORD: u32 = 0x0C06_66BB;
/// OVR02 (the field), each with its first word there.
pub const DRAW_OBJECTS: u32 = 0x801E_080C;
pub const DRAW_OBJECTS_WORD: u32 = 0x27BD_FFE0;
pub const DRAW_OBJECT_MESH: u32 = 0x801E_090C;
pub const DRAW_OBJECT_MESH_WORD: u32 = 0xACF6_0204;
pub const NPC_DRAW: u32 = 0x801E_9E90;
pub const NPC_DRAW_WORD: u32 = 0x27BD_FFB8;
pub const NPC_DRAW_SIZE: u32 = 0x6B0;
pub const TORCH_FLAMES: u32 = 0x801E_B4AC;
pub const TORCH_FLAMES_WORD: u32 = 0x27BD_FF70;
/// Main: both entries of the part drawer (the second skips `li $at, 1`).
pub const MODEL_DRAW_PARTS: u32 = 0x8019_DD70;
pub const MODEL_DRAW_PARTS_2: u32 = 0x8019_DD68;
pub const DRAW_OBJECTS_SIZE: u32 = 0x100;
/// OVR02's event tasks that draw an actor with `Model_DrawParts(pose, ctx, 1)` (LT-142): Tier-2
/// opcode 0x23 runs them, as tasks 0 and 6 of `D_801FC40C`. Each with its first word and size
/// (to the next function).
pub const APPROACH_KNIGHT: u32 = 0x801E_249C;
pub const APPROACH_KNIGHT_WORD: u32 = 0x27BD_FFD0;
pub const APPROACH_KNIGHT_SIZE: u32 = 0x41C;
pub const FLICKER_IN: u32 = 0x801E_2A3C;
pub const FLICKER_IN_WORD: u32 = 0x27BD_FFD8;
pub const FLICKER_IN_SIZE: u32 = 0x180;
/// The records they move, as fixed in their code: the approach's is `CHARACTERS[22]`, or
/// `CHARACTERS[3]` while the current chunk is Zeppetto's house (0x15A); the flicker's is
/// `CHARACTERS[2]`. The pose they draw is the record's slot's part record (+0x31).
const APPROACHING: u32 = 0x8014_6AD0;
const APPROACHING_AT_ZEPPETTOS: u32 = 0x8014_66A8;
const ZEPPETTOS: u32 = 0x15A;
const FLICKERING: u32 = 0x8014_6670;
/// `D_801FD688`: the area's models (TMD addresses), by a record's +0x16, as the actor loop hands
/// `Npc_DrawIfVisible` its model.
const MODELS: u32 = 0x801F_D688;
/// OVR24, the first flight's sky scene (LT-143). `func_80133914(lists, pool, cursor, ot)` draws one
/// map cell: the hand-written sibling of `WorldChunk_DrawObjectMesh`. `func_80132074`, the scene's
/// task, draws the area's model 2 (`D_801FD690`, built into `D_801350C0`; it asks for a second
/// part, which is empty).
pub const SKY_CELL: u32 = 0x8013_3914;
pub const SKY_CELL_WORD: u32 = 0x27BD_FFD8;
pub const SKY_TASK: u32 = 0x8013_2074;
pub const SKY_TASK_WORD: u32 = 0x27BD_FFA8;
pub const SKY_TASK_SIZE: u32 = 0x18A0;
const SKY_MODEL: u32 = 0x801F_D690;
const SKY_PARTS: u32 = 0x8013_50C0;
/// The character index the flight task's parts take: past the records (0x50 and up are the
/// overworld's wanderers, which end before 0x57).
pub const SKY_MODEL_INDEX: u16 = 0x100;
/// `func_80134A4C`'s cell window (`D_801351E0`: 25 × s16 type, s8 column, s8 row), the map's
/// offset per column or row (`D_80135048`, s16: the wrap, ±9,216 or 0) and the cell types'
/// records (16 bytes each from 0x8001000C: lists, pool, culling centre). Every type shares one
/// vertex pool in map coordinates, so a cell at offset 0 is drawn through the view itself.
const SKY_WINDOW: u32 = 0x8013_51E0;
const SKY_OFFSETS: u32 = 0x8013_5048;
const SKY_RECORDS: u32 = 0x8001_000C;
/// The one normal every cell's NCDS lights with (`D_80135058`).
const SKY_NORMAL: u32 = 0x8013_5058;
/// A sky cell's id in the chunk id space: past every chunk, its row and column in the map.
pub const SKY_CHUNKS: u16 = 0xF000;
/// Main: libgpu's `SetDrawMove(DR_MOVE *p, RECT *rect, int x, int y)`.
pub const SET_DRAW_MOVE: u32 = 0x801A_8D48;
/// OVR03 (battle presentation, at 0x80139000; LT-116): the actors' drawer (`FUN_80144b48`, from
/// `Battle_SetupActorsTask`), the arena's task (`func_8014B4A4`, root mode 8's) and the arena drawer
/// it calls while a battle light is set (`FUN_8014c97c`, hand asm), each with its first word and
/// size (to the next function). OVR03 loads in battle only, so the words keep the hooks quiet
/// elsewhere.
pub const BATTLE_ACTOR: u32 = 0x8014_4B48;
pub const BATTLE_ACTOR_WORD: u32 = 0x27BD_FF10;
pub const BATTLE_ACTOR_SIZE: u32 = 0xC88;
pub const BATTLE_ARENA: u32 = 0x8014_B4A4;
pub const BATTLE_ARENA_WORD: u32 = 0x27BD_FFC0;
pub const BATTLE_ARENA_SIZE: u32 = 0x14D8;
pub const ARENA_LIT: u32 = 0x8014_C97C;
pub const ARENA_LIT_WORD: u32 = 0x18E0_017E;
/// The battle actors (gc-sys `D_801B74C4`, 0x80010000): nine records of 0x40C bytes, each drawn
/// with the part record at +0x400.
pub const BATTLE_ACTORS: u32 = 0x8001_0000;
pub const BATTLE_ACTOR_BYTES: u32 = 0x40C;
pub const BATTLE_ACTOR_COUNT: u32 = 9;
/// The arena's light source (`D_8014E540`, `Battle_SetLightSource`): x, y, z (s16, the offset the
/// drawer adds to each vertex), the side flag, the strength (s32 at +8), r, g, b and "set this
/// tick" at +0xC..+0xF.
pub const ARENA_LIGHT: u32 = 0x8014_E540;
/// The battle's lights (OVR03 data): the world's light matrix (`D_8014E440`, each row a light's
/// direction; an actor's light matrix is it times the actor's rotation) and the area's base colour
/// matrix with its back colour (`D_8014E300[1]`, a MATRIX whose translation is the back colour;
/// `Battle_InitAreaLighting` sets it from the area's entry in `D_8014E460`). Flashes blend an actor's
/// own copy (actor+0x70) away from the base and back.
pub const BATTLE_WORLD_LIGHTS: u32 = 0x8014_E440;
pub const BATTLE_BASE_COLOURS: u32 = 0x8014_E320;
/// The character indices battle draws take: an actor's is [`BATTLE_INDEX`] + its slot, the arena's
/// [`ARENA_INDEX`] (past the field's records and the first flight's 0x100).
pub const BATTLE_INDEX: u16 = 0x200;
pub const ARENA_INDEX: u16 = 0x2FF;

const CTX: u32 = 0x1F80_0000;
const CHUNK_DATA: u32 = 0x8001_0000;
const CHUNK_STRIDE: u32 = 0x2_E000;
const CHUNK_SLOT_IDS: u32 = 0x801C_E800;
/// The character records (gc-sys `CHARACTERS`): 87 of 0x38 bytes.
pub const CHARACTERS: u32 = 0x8014_6600;
pub const CHARACTER_BYTES: usize = 0x38;
pub const CHARACTER_COUNT: usize = 87;
/// `D_801CE80C`: non-zero holds every NPC's animation; `D_801D732C`: bit 2 draws the NPCs;
/// `D_801FDAEC`: how many records the actor loop runs (the rest are left from another area).
const ANIM_HOLD: u32 = 0x801C_E80C;
const NPC_FLAGS: u32 = 0x801D_732C;
const ACTOR_COUNT: u32 = 0x801F_DAEC;
/// The field's object records (`D_801FD638`, a pointer) and their count (`D_801FD758`); torches
/// are type 5.
const FIELD_OBJECTS: u32 = 0x801F_D638;
const FIELD_OBJECT_COUNT: u32 = 0x801F_D758;
const TORCH: u8 = 5;
/// `Field_ChunkStreamTask`'s neighbour offsets: ±2 slots (`D_8017544C`, `D_8017545C`, VECTORs),
/// and ±1 slot (±0x800, 0, ±0x1000).
const NEIGHBOUR_2: [u32; 2] = [0x8017_544C, 0x8017_545C];

/// A GTE matrix: rotation (4.12) and translation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Matrix {
    pub m: [[i16; 3]; 3],
    pub t: [i32; 3],
}

impl Matrix {
    /// A `MATRIX` in memory (0x20 bytes).
    pub fn from_bytes(b: &[u8]) -> Matrix {
        let h = |i: usize| i16::from_le_bytes([b[i * 2], b[i * 2 + 1]]);
        let w = |i: usize| i32::from_le_bytes([b[0x14 + i * 4], b[0x15 + i * 4], b[0x16 + i * 4], b[0x17 + i * 4]]);
        Matrix { m: [[h(0), h(1), h(2)], [h(3), h(4), h(5)], [h(6), h(7), h(8)]], t: [w(0), w(1), w(2)] }
    }

    /// The GTE's rotation and translation registers now.
    pub fn from_gte(cpu: &Cpu) -> Matrix {
        let g = cpu.cop2();
        let c = |i: u8| g.read_control(i);
        let (lo, hi) = (|v: u32| v as u16 as i16, |v: u32| (v >> 16) as u16 as i16);
        Matrix {
            m: [[lo(c(0)), hi(c(0)), lo(c(1))], [hi(c(1)), lo(c(2)), hi(c(2))], [lo(c(3)), hi(c(3)), lo(c(4))]],
            t: [c(5) as i32, c(6) as i32, c(7) as i32],
        }
    }

    /// `ApplyMatrixLV(self, v) + t`, as `Field_ChunkStreamTask` places a neighbour. Also the GTE's
    /// MVMVA on `v` (rotation, translation, `sf = 1`), as `WorldChunk_DrawObjects` culls an object.
    pub fn place(&self, v: [i32; 3]) -> [i32; 3] {
        let mut out = [0; 3];
        for (r, o) in out.iter_mut().enumerate() {
            let s: i64 = (0..3).map(|c| self.m[r][c] as i64 * v[c] as i64).sum();
            *o = (s >> 12) as i32 + self.t[r];
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Camera {
    pub view: Matrix,
    /// The projection plane distance (GTE H; 892 in the field).
    pub h: u16,
    /// The screen offset (GTE OFX, OFY), in pixels.
    pub offset: [i32; 2],
    /// 512 × 224 free roam (else 320 × 224).
    pub wide: bool,
}

/// One `WorldChunk_DrawObjects` call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ChunkDraw {
    /// The chunk id (stable while the chunk is drawn, whichever slot holds it).
    pub chunk: u16,
    pub slot: u8,
    /// Where the chunk sits in the world relative to the current one: 0, or a neighbour's offset.
    pub offset: [i32; 3],
    /// The GTE's model-view matrix for the call (the view, moved to the neighbour).
    pub model_view: Matrix,
    pub groups: Vec<Group>,
}

/// One `WorldChunk_DrawObjectMesh` call: a group (a map object) of a chunk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Group {
    /// The chunk id × 2³² + the mesh's offset in the chunk's data.
    pub id: u64,
    /// The mesh and the object it was handed (absolute addresses).
    pub mesh: u32,
    pub object: u32,
}

/// What drew a character.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Drawer {
    /// The field's actor loop, through `Npc_DrawIfVisible`.
    #[default]
    Npc,
    /// `Event_ActorApproachKnightTask`: the actor jumps from the Knight to its place, turning.
    ApproachKnight,
    /// `Event_ActorFlickerInTask`: the actor shown on some ticks only, for 30 ticks.
    FlickerIn,
    /// OVR24's first-flight task (`func_80132074`): the area's model 2 at the Knight, the Baby's
    /// glow. Every polygon of it is semi-transparent (added): an effect, which the 2D layer draws
    /// with the PS1's blend. Recorded, not set aside ([`character_call`] leaves it out).
    Flight,
    /// A battle actor or Toy (OVR03 `FUN_80144b48`, LT-116).
    Battle,
    /// The battle arena (OVR03 `func_8014B4A4`, or `FUN_8014c97c` with the arena light, LT-116).
    Arena,
}

/// The event task a `Model_DrawParts` call comes from, and the record it draws (LT-142).
pub fn event_actor(c: &Call) -> Option<(Drawer, u32)> {
    if within(c.ra(), APPROACH_KNIGHT, APPROACH_KNIGHT_SIZE) && c.read_u32(APPROACH_KNIGHT) == APPROACH_KNIGHT_WORD {
        let at_zeppettos = c.read_u32(CTX + 0xE8) == ZEPPETTOS;
        Some((Drawer::ApproachKnight, if at_zeppettos { APPROACHING_AT_ZEPPETTOS } else { APPROACHING }))
    } else if within(c.ra(), FLICKER_IN, FLICKER_IN_SIZE) && c.read_u32(FLICKER_IN) == FLICKER_IN_WORD {
        Some((Drawer::FlickerIn, FLICKERING))
    } else {
        None
    }
}

/// Whether a `Model_DrawParts` call is the first flight's task's (LT-143).
pub fn flight_part(c: &Call) -> bool {
    within(c.ra(), SKY_TASK, SKY_TASK_SIZE) && c.read_u32(SKY_TASK) == SKY_TASK_WORD
}

/// Whether a `Model_DrawParts` call is the field's actor loop's: from `Npc_DrawIfVisible`, with
/// OVR02 there. OVR04 (battle) loads at the same address, and its effects call `Model_DrawParts`
/// too, so the address alone isn't enough (LT-116).
pub fn npc_call(c: &Call) -> bool {
    within(c.ra(), NPC_DRAW, NPC_DRAW_SIZE) && c.read_u32(NPC_DRAW) == NPC_DRAW_WORD
}

/// The battle drawer a `Model_DrawParts` call comes from (LT-116): an actor's or the arena's, with
/// OVR03 loaded.
pub fn battle_call(c: &Call) -> Option<Drawer> {
    if within(c.ra(), BATTLE_ACTOR, BATTLE_ACTOR_SIZE) && c.read_u32(BATTLE_ACTOR) == BATTLE_ACTOR_WORD {
        Some(Drawer::Battle)
    } else if within(c.ra(), BATTLE_ARENA, BATTLE_ARENA_SIZE) && c.read_u32(BATTLE_ARENA) == BATTLE_ARENA_WORD {
        Some(Drawer::Arena)
    } else {
        None
    }
}

/// Whether a `Model_DrawParts` call draws a character the native view takes over: from
/// `Npc_DrawIfVisible`, an event task, or a battle's actors and arena (LT-116). (The first flight's
/// glow and the battle effects stay on the 2D layer.)
pub fn character_call(c: &Call) -> bool {
    npc_call(c) || event_actor(c).is_some() || battle_call(c).is_some()
}

/// The GTE's lighting registers at a battle draw (LT-116), as `Model_DrawParts` lights with them:
/// - `llm`: the light matrix, each row a light's direction in the model's space (4.12; the world's
///   lights, `D_8014E440`, times the model's rotation);
/// - `lcm`: the colour matrix, each column a light's colour (4.12);
/// - `bk`: the back colour (0x1000 = 1.0, colour 255).
///
/// NCS gives a corner `bk + lcm · max(0, llm · n)`, and the GPU modulates its texture by it (128 is
/// 1.0). A flash is a change to `lcm` and `bk`, per actor (`Battle_BlendActorLight`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GteLight {
    pub llm: [[i16; 3]; 3],
    pub lcm: [[i16; 3]; 3],
    pub bk: [i32; 3],
}

impl GteLight {
    /// The registers from control words 8..=20 (as `SkyGte::control` holds them).
    pub fn from_control(c: &[u32]) -> GteLight {
        let (lo, hi) = (|v: u32| v as u16 as i16, |v: u32| (v >> 16) as u16 as i16);
        let m = |b: usize| [[lo(c[b]), hi(c[b]), lo(c[b + 1])], [hi(c[b + 1]), lo(c[b + 2]), hi(c[b + 2])], [lo(c[b + 3]), hi(c[b + 3]), lo(c[b + 4])]];
        GteLight { llm: m(8), bk: [c[13] as i32, c[14] as i32, c[15] as i32], lcm: m(16) }
    }

    /// The GTE's now.
    pub fn from_gte(cpu: &Cpu) -> GteLight {
        let g = cpu.cop2();
        let c: Vec<u32> = (0..21).map(|i| g.read_control(i)).collect();
        Self::from_control(&c)
    }

    /// Its control words 8..=20, to load a GTE with.
    pub fn control(&self) -> [(u8, u32); 13] {
        let pair = |a: i16, b: i16| a as u16 as u32 | (b as u16 as u32) << 16;
        let m = |m: &[[i16; 3]; 3]| [pair(m[0][0], m[0][1]), pair(m[0][2], m[1][0]), pair(m[1][1], m[1][2]), pair(m[2][0], m[2][1]), m[2][2] as u16 as u32];
        let (l, c) = (m(&self.llm), m(&self.lcm));
        [(8, l[0]), (9, l[1]), (10, l[2]), (11, l[3]), (12, l[4]), (13, self.bk[0] as u32), (14, self.bk[1] as u32), (15, self.bk[2] as u32), (16, c[0]), (17, c[1]), (18, c[2]), (19, c[3]), (20, c[4])]
    }
}

/// The arena's light source as `FUN_8014c97c` drew with it (LT-116, `D_8014E540`): the offset it adds
/// to each of the arena's vertices (the light at minus it), its strength and colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ArenaLight {
    pub offset: [i16; 3],
    pub strength: i32,
    pub rgb: [u8; 3],
}

/// One map cell OVR24 draws (`func_80133914`, LT-143).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SkyCell {
    /// Its column and row in the map, and its type: which of the area's cell meshes it is.
    pub cell: [u8; 2],
    pub kind: u16,
    /// Its primitive lists (the chunk group format) and vertex pool, as `func_80133914` gets them.
    pub lists: u32,
    pub pool: u32,
    /// Where it sits in the world (x, 0, z), and the GTE's model-view matrix (the view moved there).
    pub offset: [i32; 3],
    pub model_view: Matrix,
}

impl SkyCell {
    /// Its id among the chunks: [`SKY_CHUNKS`] with its row and column.
    pub fn id(&self) -> u16 {
        SKY_CHUNKS | (self.cell[1] as u16) << 4 | self.cell[0] as u16
    }
}

/// The GTE as OVR24 lights its cells (LT-143): every control register at the tick's first cell
/// (light, colour and back colour matrices, the far colour and the depth cue), and the normal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SkyGte {
    pub control: [u32; 32],
    pub normal: [i16; 3],
}

/// One drawn character (`Npc_DrawIfVisible`, or an event task, → `Model_DrawParts`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CharacterDraw {
    /// The character index (0 the Knight, 1 the Baby), stable across ticks.
    pub index: u16,
    pub by: Drawer,
    /// Its record (`CHARACTERS[index]`) and its position there.
    pub record: u32,
    /// The model it's drawn with: a TMD in RAM, its pointers relocated (`Npc_DrawIfVisible`'s
    /// third argument). The party's is a leftover NPC model (lt-assets matches them by shape).
    pub model: u32,
    pub position: [i16; 3],
    pub parts: Vec<PartDraw>,
}

/// One part record drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PartDraw {
    /// The part record (0x3C bytes; gc-sys `Part`).
    pub record: u32,
    /// Which entry drew it: 0 = `Model_DrawParts`, 1 = its second entry (0x8019DD68), which keys a
    /// polygon on its farthest corner; 2 = the arena's lit drawer (`FUN_8014c97c`, LT-116), which
    /// does too.
    pub entry: u8,
    /// The world matrix (ctx+0x11C) and the GTE's composed model-view matrix.
    pub world: Matrix,
    pub model_view: Matrix,
    /// The live vertices (part record +0x2C, count at +0x38): this tick's pose, already morphed.
    pub vertices: Vec<[i16; 3]>,
    /// A battle draw's light (LT-116): the GTE's registers it was lit with, and the arena light
    /// when `FUN_8014c97c` drew it (entry 2).
    pub light: Option<GteLight>,
    pub arena_light: Option<ArenaLight>,
}

/// A torch flame: a light source.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Flame {
    /// The torch list's generation × 2¹⁶ + its index in the field's object records. The
    /// generation steps when the list's torches change (another area loaded), so an id is one
    /// torch for as long as it exists.
    pub id: u32,
    pub index: u16,
    pub position: [i16; 3],
}

/// One `SetDrawMove`: the GPU copies VRAM rect `from` (x, y, w, h in halfwords) to `to` when it
/// runs the tick's ordering table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VramMove {
    pub from: [u16; 4],
    pub to: [u16; 2],
}

/// One game tick.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Record {
    /// Ticks captured before this one.
    pub tick: u64,
    /// The frame (vblank) the tick started in, as the driver counts them ([`Capture::frame`]).
    pub frame: u64,
    /// ctx: play time in vblanks, the current chunk, the game mode.
    pub play_time: u32,
    pub chunk: u16,
    pub mode: u8,
    pub camera: Option<Camera>,
    pub chunks: Vec<ChunkDraw>,
    pub characters: Vec<CharacterDraw>,
    /// Whether the torch pass ran, and the torches it had.
    pub torch_pass: bool,
    pub flames: Vec<Flame>,
    /// `Model_DrawParts` calls from elsewhere (battles, OVR24's sky scene): not characters.
    pub other_models: u32,
    /// The VRAM copies set up this tick, in order.
    pub moves: Vec<VramMove>,
    /// OVR24's sky cells drawn this tick, and the GTE that lit them (LT-143).
    pub sky: Vec<SkyCell>,
    pub sky_gte: Option<SkyGte>,
    /// A battle's lights this tick (LT-116): the world's light directions ([`BATTLE_WORLD_LIGHTS`])
    /// as `llm`, and the area's base colours and back colour ([`BATTLE_BASE_COLOURS`]).
    pub battle_light: Option<GteLight>,
    /// The character records at the tick's end ([`CHARACTER_COUNT`] × [`CHARACTER_BYTES`]).
    #[serde(skip)]
    pub actors: Vec<u8>,
    /// `D_801CE80C`, `D_801D732C` and `D_801FDAEC` at the tick's end.
    pub anim_hold: u32,
    pub npc_flags: u32,
    pub actor_count: u32,
}

/// The hooks: observe only.
pub fn hooks() -> Hooks {
    Hooks::new(vec![
        Hook::observe("tick start", TICK_START).guarded(TICK_START_WORD),
        Hook::observe("tick end", TICK_END).guarded(TICK_END_WORD),
        Hook::observe("WorldChunk_DrawObjects", DRAW_OBJECTS).guarded(DRAW_OBJECTS_WORD),
        Hook::observe("WorldChunk_DrawObjectMesh", DRAW_OBJECT_MESH).guarded(DRAW_OBJECT_MESH_WORD),
        Hook::observe("Npc_DrawIfVisible", NPC_DRAW).guarded(NPC_DRAW_WORD),
        Hook::observe("Field_DrawTorchFlames", TORCH_FLAMES).guarded(TORCH_FLAMES_WORD),
        Hook::observe("Model_DrawParts", MODEL_DRAW_PARTS),
        Hook::observe("Model_DrawParts (second entry)", MODEL_DRAW_PARTS_2),
        Hook::observe("SetDrawMove", SET_DRAW_MOVE),
        Hook::observe("func_80133914 (OVR24 sky cell)", SKY_CELL).guarded(SKY_CELL_WORD),
        Hook::observe("FUN_8014c97c (OVR03 lit arena)", ARENA_LIT).guarded(ARENA_LIT_WORD),
    ])
    .expect("the capture's hook table")
}

/// The handler: builds records as the hooks fire.
#[derive(Default)]
pub struct Capture {
    /// The driver sets this to the frame about to run, for [`Record::frame`].
    pub frame: u64,
    /// Finished records, oldest first. Take them as they come ([`Capture::take`]).
    pub records: Vec<Record>,
    cur: Option<Record>,
    ticks: u64,
    /// The character `Npc_DrawIfVisible` was last called for: index, record and model.
    npc: Option<(u16, u32, u32)>,
    /// The torch list last seen (a hash of its torches) and its generation.
    torch_list: Option<u64>,
    torch_generation: u32,
}

fn phys(a: u32) -> u32 {
    a & 0x1FFF_FFFF
}

fn within(ra: u32, start: u32, size: u32) -> bool {
    let (ra, start) = (phys(ra), phys(start));
    ra > start && ra <= start + size
}

impl Capture {
    pub fn new() -> Self {
        Self::default()
    }

    /// The records finished since the last call.
    pub fn take(&mut self) -> Vec<Record> {
        std::mem::take(&mut self.records)
    }

    fn tick_start(&mut self, c: &Call) {
        let ctx = c.read(CTX, 0x100);
        self.cur = Some(Record {
            tick: self.ticks,
            frame: self.frame,
            play_time: u32::from_le_bytes([ctx[0xEC], ctx[0xED], ctx[0xEE], ctx[0xEF]]),
            chunk: u16::from_le_bytes([ctx[0xE8], ctx[0xE9]]),
            mode: ctx[0x43],
            ..Record::default()
        });
        self.npc = None;
    }

    fn chunk(&mut self, c: &Call, r: &mut Record) {
        let base = phys(c.arg(0)) | 0x8000_0000;
        if !(CHUNK_DATA..CHUNK_DATA + 4 * CHUNK_STRIDE).contains(&base) {
            return;
        }
        let slot = ((base - CHUNK_DATA) / CHUNK_STRIDE) as u8;
        let chunk = c.read_u16(CHUNK_SLOT_IDS + slot as u32 * 2);
        let model_view = Matrix::from_gte(c.cpu());
        let view = Matrix::from_bytes(&c.read(CTX + 0x98, 0x20));
        if r.camera.is_none() {
            let g = c.cpu().cop2();
            r.camera = Some(Camera {
                view,
                h: g.read_control(26) as u16,
                offset: [(g.read_control(24) as i32) >> 16, (g.read_control(25) as i32) >> 16],
                wide: c.read(CTX + 0x4C, 1)[0] != 0,
            });
        }
        // The world offset whose placement under the view gives this call's translation.
        let mut candidates = vec![[0, 0, 0], [-0x800, 0, 0x1000], [-0x800, 0, -0x1000], [0x800, 0, 0x1000], [0x800, 0, -0x1000]];
        for &v in &NEIGHBOUR_2 {
            let b = c.read(v, 12);
            let w = |i: usize| i32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
            candidates.push([w(0), w(1), w(2)]);
        }
        let dist = |o: [i32; 3]| {
            let p = view.place(o);
            (0..3).map(|i| (p[i] - model_view.t[i]).unsigned_abs() as u64).sum::<u64>()
        };
        let offset = candidates.into_iter().min_by_key(|&o| dist(o)).unwrap_or_default();
        r.chunks.push(ChunkDraw { chunk, slot, offset, model_view, groups: Vec::new() });
    }

    fn group(&mut self, c: &Call, r: &mut Record) {
        if !within(c.ra(), DRAW_OBJECTS, DRAW_OBJECTS_SIZE) {
            return;
        }
        let Some(d) = r.chunks.last_mut() else { return };
        let base = CHUNK_DATA + d.slot as u32 * CHUNK_STRIDE;
        let (mesh, object) = (phys(c.arg(0)) | 0x8000_0000, phys(c.arg(1)) | 0x8000_0000);
        d.groups.push(Group { id: ((d.chunk as u64) << 32) | mesh.wrapping_sub(base) as u64, mesh, object });
    }

    /// The battle camera (LT-116): the view at ctx+0x98 and the GTE's H and offset, at the tick's
    /// first battle draw. Battle draws at 320 × 224.
    fn battle_camera(&mut self, c: &Call, r: &mut Record) {
        if r.camera.is_none() {
            let g = c.cpu().cop2();
            r.camera = Some(Camera {
                view: Matrix::from_bytes(&c.read(CTX + 0x98, 0x20)),
                h: g.read_control(26) as u16,
                offset: [(g.read_control(24) as i32) >> 16, (g.read_control(25) as i32) >> 16],
                wide: false,
            });
        }
        if r.battle_light.is_none() {
            let (world, base) = (Matrix::from_bytes(&c.read(BATTLE_WORLD_LIGHTS, 0x20)), Matrix::from_bytes(&c.read(BATTLE_BASE_COLOURS, 0x20)));
            r.battle_light = Some(GteLight { llm: world.m, lcm: base.m, bk: base.t });
        }
    }

    /// Which battle actor a part is: the slot whose record draws it (+0x400).
    fn battle_actor(c: &Call, part: u32) -> Option<u32> {
        (0..BATTLE_ACTOR_COUNT).find(|k| phys(c.read_u32(BATTLE_ACTORS + k * BATTLE_ACTOR_BYTES + 0x400)) == phys(part))
    }

    fn part(&mut self, c: &Call, r: &mut Record, entry: u8) {
        let battle = battle_call(c);
        let drawn = match self.npc.filter(|_| within(c.ra(), NPC_DRAW, NPC_DRAW_SIZE)) {
            Some(npc) => Some((Drawer::Npc, npc)),
            None if flight_part(c) => Some((Drawer::Flight, (SKY_MODEL_INDEX, SKY_PARTS, phys(c.read_u32(SKY_MODEL)) | 0x8000_0000))),
            // A battle actor: its slot's index, its record, no model pointer (lt-assets finds the
            // model by its parts' shapes).
            None if battle == Some(Drawer::Battle) => {
                self.battle_camera(c, r);
                Self::battle_actor(c, c.arg(0)).map(|k| (Drawer::Battle, (BATTLE_INDEX + k as u16, BATTLE_ACTORS + k * BATTLE_ACTOR_BYTES, 0)))
            }
            None if battle == Some(Drawer::Arena) => {
                self.battle_camera(c, r);
                Some((Drawer::Arena, (ARENA_INDEX, 0, 0)))
            }
            None => event_actor(c).map(|(by, record)| {
                let index = ((record - CHARACTERS) / CHARACTER_BYTES as u32) as u16;
                let model = c.read_u32(MODELS + c.read(record + 0x16, 1)[0] as u32 * 4);
                (by, (index, record, phys(model) | 0x8000_0000))
            }),
        };
        let Some((by, (index, record, model))) = drawn else {
            r.other_models += 1;
            return;
        };
        let part = c.arg(0);
        let p = c.read(part, 0x3C);
        // A part with no polygons draws nothing (the flight task's second: model 2 has one object).
        if p[..0x28].iter().all(|&b| b == 0) {
            return;
        }
        let verts = u32::from_le_bytes([p[0x2C], p[0x2D], p[0x2E], p[0x2F]]);
        let n = u16::from_le_bytes([p[0x38], p[0x39]]) as usize;
        let raw = if verts != 0 && n <= 0x1000 { c.read(verts, n * 8) } else { Vec::new() };
        let vertices = raw.chunks_exact(8).map(|v| [0, 2, 4].map(|i| i16::from_le_bytes([v[i], v[i + 1]]))).collect();
        let battle = matches!(by, Drawer::Battle | Drawer::Arena);
        let draw = PartDraw {
            record: part,
            entry,
            world: Matrix::from_bytes(&c.read(CTX + 0x11C, 0x20)),
            model_view: Matrix::from_gte(c.cpu()),
            vertices,
            light: battle.then(|| GteLight::from_gte(c.cpu())),
            arena_light: (entry == 2).then(|| {
                let l = c.read(ARENA_LIGHT, 0x10);
                let h = |i: usize| i16::from_le_bytes([l[i], l[i + 1]]);
                ArenaLight { offset: [h(0), h(2), h(4)], strength: i32::from_le_bytes([l[8], l[9], l[10], l[11]]), rgb: [l[12], l[13], l[14]] }
            }),
        };
        match r.characters.iter_mut().find(|ch| ch.index == index && ch.by == by) {
            Some(ch) => ch.parts.push(draw),
            None => {
                // The flight's parts and the arena have no record: their place is the world matrix's.
                let pos = if matches!(by, Drawer::Flight | Drawer::Arena) { draw.world.t.iter().flat_map(|&v| (v as i16).to_le_bytes()).collect() } else { c.read(record, 6) };
                let h = |i: usize| i16::from_le_bytes([pos[i], pos[i + 1]]);
                r.characters.push(CharacterDraw { index, by, record, model, position: [h(0), h(2), h(4)], parts: vec![draw] });
            }
        }
    }

    /// A sky cell: the camera if no chunk gave it, the GTE's lighting at the tick's first, and which
    /// of the window's 25 cells this is (its type's record holds these lists, and its place under
    /// the view is the GTE's translation, as `func_80134A4C` sets it).
    fn sky_cell(&mut self, c: &Call, r: &mut Record) {
        let view = Matrix::from_bytes(&c.read(CTX + 0x98, 0x20));
        let g = c.cpu().cop2();
        if r.camera.is_none() {
            r.camera = Some(Camera {
                view,
                h: g.read_control(26) as u16,
                offset: [(g.read_control(24) as i32) >> 16, (g.read_control(25) as i32) >> 16],
                wide: c.read(CTX + 0x4C, 1)[0] != 0,
            });
        }
        if r.sky_gte.is_none() {
            let n = c.read(SKY_NORMAL, 6);
            r.sky_gte = Some(SkyGte {
                control: std::array::from_fn(|i| g.read_control(i as u8)),
                normal: [0, 2, 4].map(|i| i16::from_le_bytes([n[i], n[i + 1]])),
            });
        }
        let model_view = Matrix::from_gte(c.cpu());
        let (lists, pool) = (c.arg(0), c.arg(1));
        let window = c.read(SKY_WINDOW, 25 * 4);
        // Eight offsets, but the window reaches rows 8 and 9: the game reads on into the normal
        // after the table (row 9 is moved 6,000 down the map), and so does this.
        let offsets = c.read(SKY_OFFSETS, 0x40);
        let off = |i: u8| offsets.get(i as usize * 2..i as usize * 2 + 2).map_or(0, |b| i16::from_le_bytes([b[0], b[1]]) as i32);
        let found = window.as_chunks::<4>().0.iter().find_map(|e| {
            let kind = u16::from_le_bytes([e[0], e[1]]);
            let (x, y) = (e[2], e[3]);
            if c.read_u32(SKY_RECORDS + kind as u32 * 16) != lists {
                return None;
            }
            let o = [off(x), 0, -off(y)];
            let t = if o == [0, 0, 0] { view.t } else { view.place(o) };
            (t == model_view.t).then_some((kind, [x, y], o))
        });
        let (kind, cell, offset) = found.unwrap_or((0xFFFF, [0xFF, 0xFF], [0; 3]));
        r.sky.push(SkyCell { cell, kind, lists, pool, offset, model_view });
    }

    fn torches(&mut self, c: &Call, r: &mut Record) {
        r.torch_pass = true;
        let list = c.read_u32(FIELD_OBJECTS);
        let n = c.read_u16(FIELD_OBJECT_COUNT).min(0x400);
        if list == 0 {
            return;
        }
        let recs = c.read(list, n as usize * 0x10);
        let torches: Vec<(u16, [i16; 3])> = recs
            .chunks_exact(0x10)
            .enumerate()
            .filter(|(_, rec)| rec[8] == TORCH)
            .map(|(i, rec)| (i as u16, [0, 2, 4].map(|k| i16::from_le_bytes([rec[k], rec[k + 1]]))))
            .collect();
        // FNV-1a over the list's address and its torches (not their animation phase).
        let mut hash = 0xCBF2_9CE4_8422_2325u64;
        for b in list.to_le_bytes().into_iter().chain(torches.iter().flat_map(|(i, p)| i.to_le_bytes().into_iter().chain(p.iter().flat_map(|v| v.to_le_bytes())))) {
            hash = (hash ^ b as u64).wrapping_mul(0x100_0000_01B3);
        }
        if self.torch_list != Some(hash) {
            self.torch_list = Some(hash);
            self.torch_generation += 1;
        }
        let generation = self.torch_generation << 16;
        r.flames.extend(torches.into_iter().map(|(index, position)| Flame { id: generation | index as u32, index, position }));
    }
}

impl Handler for Capture {
    fn call(&mut self, c: &Call) -> Action {
        match c.hook {
            0 => self.tick_start(c),
            1 => {
                if let Some(mut r) = self.cur.take() {
                    r.actors = c.read(CHARACTERS, CHARACTER_COUNT * CHARACTER_BYTES);
                    r.anim_hold = c.read_u32(ANIM_HOLD);
                    r.npc_flags = c.read_u32(NPC_FLAGS);
                    r.actor_count = c.read_u32(ACTOR_COUNT);
                    self.records.push(r);
                    self.ticks += 1;
                }
            }
            hook => {
                let Some(mut r) = self.cur.take() else { return Action::Continue };
                match hook {
                    2 => self.chunk(c, &mut r),
                    3 => self.group(c, &mut r),
                    4 => self.npc = Some((c.arg(1) as u16, c.arg(0), phys(c.arg(2)) | 0x8000_0000)),
                    5 => self.torches(c, &mut r),
                    6 => self.part(c, &mut r, 0),
                    7 => self.part(c, &mut r, 1),
                    8 => {
                        let rect = c.read(c.arg(1), 8);
                        let h = |i: usize| u16::from_le_bytes([rect[i], rect[i + 1]]);
                        r.moves.push(VramMove { from: [h(0), h(2), h(4), h(6)], to: [c.arg(2) as u16, c.arg(3) as u16] });
                    }
                    9 => self.sky_cell(c, &mut r),
                    // The arena with its light (LT-116): the same part, drawn by hand asm.
                    10 => self.part(c, &mut r, 2),
                    _ => {}
                }
                self.cur = Some(r);
            }
        }
        Action::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices_read_as_the_gte_lays_them_out() {
        let mut b = vec![0u8; 0x20];
        for (i, v) in [4096i16, 0, 0, 0, 4096, 0, 0, 0, 4096].iter().enumerate() {
            b[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        for (i, v) in [100i32, -200, 3000].iter().enumerate() {
            b[0x14 + i * 4..0x18 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        let m = Matrix::from_bytes(&b);
        assert_eq!(m.m, [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]]);
        assert_eq!(m.t, [100, -200, 3000]);
        assert_eq!(m.place([0x800, 0, -0x1000]), [100 + 0x800, -200, 3000 - 0x1000]);
    }

    #[test]
    fn the_hook_table_holds() {
        assert_eq!(hooks().hooks().len(), 11);
        assert_eq!(BATTLE_ACTOR + BATTLE_ACTOR_SIZE, 0x8014_57D0, "BabyForm_RestoreStats follows");
        assert_eq!(BATTLE_ARENA + BATTLE_ARENA_SIZE, ARENA_LIT, "FUN_8014c97c follows");
        assert!(within(0x801E_9E98, NPC_DRAW, NPC_DRAW_SIZE) && !within(NPC_DRAW + NPC_DRAW_SIZE + 4, NPC_DRAW, NPC_DRAW_SIZE));
        // The event tasks' calls return to 0x801E2890 (the approach) and 0x801E2B54 (the flicker).
        assert!(within(0x801E_2890, APPROACH_KNIGHT, APPROACH_KNIGHT_SIZE) && within(0x801E_2B54, FLICKER_IN, FLICKER_IN_SIZE));
        assert_eq!(APPROACH_KNIGHT + APPROACH_KNIGHT_SIZE, 0x801E_28B8, "Event_KnightJumpTask follows");
        assert_eq!(FLICKER_IN + FLICKER_IN_SIZE, 0x801E_2BBC, "NamingScreen_CommitNameTask follows");
        assert_eq!(((APPROACHING - CHARACTERS) / 0x38, (APPROACHING_AT_ZEPPETTOS - CHARACTERS) / 0x38, (FLICKERING - CHARACTERS) / 0x38), (22, 3, 2));
    }
}
