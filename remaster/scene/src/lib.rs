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
//!   live, already-posed vertices.
//! - **Torches:** the flame records (type 5) when `Field_DrawTorchFlames` runs: the village's
//!   light sources.
//! - **VRAM moves:** each `SetDrawMove` (a VRAM-to-VRAM copy the GPU runs with the tick's ordering
//!   table): the field's 13 texture and palette animations, the water among them.
//! - **Character records** as the tick leaves them (LT-119): every NPC's place, facing and
//!   animation state, drawn or not, and the two flags that hold the NPCs' animation and drawing.
//!   The machine's RAM a frame later is already the next tick's.
//!
//! Ids are stable across ticks ([`Group::id`] is the chunk id and the mesh's offset in the chunk;
//! characters are keyed by index; [`Flame::id`] is the torch list's generation and the record
//! index), so a later stage can interpolate.
//! Nothing is written to the machine.

use emulator_core::Cpu;
use lt_emu_hooks::{Action, Call, Handler, Hook, Hooks};
use serde::Serialize;

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
/// Main: libgpu's `SetDrawMove(DR_MOVE *p, RECT *rect, int x, int y)`.
pub const SET_DRAW_MOVE: u32 = 0x801A_8D48;

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

/// One drawn character (`Npc_DrawIfVisible` → `Model_DrawParts`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CharacterDraw {
    /// The character index (0 the Knight, 1 the Baby), stable across ticks.
    pub index: u16,
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
    /// Which entry drew it: 0 = `Model_DrawParts`, 1 = its second entry (0x8019DD68).
    pub entry: u8,
    /// The world matrix (ctx+0x11C) and the GTE's composed model-view matrix.
    pub world: Matrix,
    pub model_view: Matrix,
    /// The live vertices (part record +0x2C, count at +0x38): this tick's pose, already morphed.
    pub vertices: Vec<[i16; 3]>,
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
    /// `Model_DrawParts` calls from elsewhere (effects, menus): not characters.
    pub other_models: u32,
    /// The VRAM copies set up this tick, in order.
    pub moves: Vec<VramMove>,
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

    fn part(&mut self, c: &Call, r: &mut Record, entry: u8) {
        let Some((index, record, model)) = self.npc.filter(|_| within(c.ra(), NPC_DRAW, NPC_DRAW_SIZE)) else {
            r.other_models += 1;
            return;
        };
        let part = c.arg(0);
        let p = c.read(part, 0x3C);
        let verts = u32::from_le_bytes([p[0x2C], p[0x2D], p[0x2E], p[0x2F]]);
        let n = u16::from_le_bytes([p[0x38], p[0x39]]) as usize;
        let raw = if verts != 0 && n <= 0x1000 { c.read(verts, n * 8) } else { Vec::new() };
        let vertices = raw.chunks_exact(8).map(|v| [0, 2, 4].map(|i| i16::from_le_bytes([v[i], v[i + 1]]))).collect();
        let draw = PartDraw {
            record: part,
            entry,
            world: Matrix::from_bytes(&c.read(CTX + 0x11C, 0x20)),
            model_view: Matrix::from_gte(c.cpu()),
            vertices,
        };
        match r.characters.iter_mut().find(|ch| ch.index == index) {
            Some(ch) => ch.parts.push(draw),
            None => {
                let pos = c.read(record, 6);
                let h = |i: usize| i16::from_le_bytes([pos[i], pos[i + 1]]);
                r.characters.push(CharacterDraw { index, record, model, position: [h(0), h(2), h(4)], parts: vec![draw] });
            }
        }
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
        assert_eq!(hooks().hooks().len(), 9);
        assert!(within(0x801E_9E98, NPC_DRAW, NPC_DRAW_SIZE) && !within(NPC_DRAW + NPC_DRAW_SIZE + 4, NPC_DRAW, NPC_DRAW_SIZE));
    }
}
