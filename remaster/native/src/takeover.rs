//! What the native view takes over from the emulated GPU (LT-111, research 16 §6.2–6.3).
//!
//! **Nothing in the machine changes.** The game's draw code runs as it always does, and the
//! emulated GPU still draws every packet: the GPU's timing feeds back into the CPU (PSoXide clocks
//! the DMA walk, the FIFO and `DrawSync`), so a GPU with less to draw puts interrupts at other
//! instructions and lockstep breaks. Zeroing the terrain and character packets' lengths was tried:
//! the first difference was the kernel's interrupt frame (0x8000E2AC) within two frames.
//!
//! What the native view shows instead: as `main()` hands the tick's ordering table to the GPU
//! (`Dbg_DrawOTag`), the takeover reads it.
//! - Terrain and character packets are set aside: they're drawn natively.
//! - Every other packet is kept with its bucket ([`Linked`]): the 2D layer replays them at N×
//!   ([`crate::layer2d`]).
//!
//! The field's billboards are noted on the way (LT-144): each `RotTransPers4`/`RotAverage4` call by
//! a billboard's function, and `Field_DrawTorchFlames`' anchor tests (`RotTransPers`). The table
//! carries them for the 2D layer ([`crate::billboard`], [`crate::smoke`]).
//!
//! **The battle transition** (`Battle_LoadingScreenTask`, LT-116) copies the frame on screen into
//! VRAM with `MoveImage` and swirls it away, moving columns and rows of the copy about each tick.
//! Its moves are noted ([`Table::moves`]): the native view stands its own picture of that frame in
//! for the copy (`crate::composite`).
//!
//! **A frozen frame** (LT-140): the field menu, a chunk load and battles freeze the frame
//! on screen (`FUN_8018af14`: two `MoveImage`s of 160 columns into VRAM) and show it dimmed under
//! their panels (`FUN_8018afa0`: two textured quads). Those copies are noted too ([`Table::frozen`]):
//! the native view keeps its own widened picture of that frame for them.
//!
//! The GPU's environment, the clear colour and VRAM are noted then too. The GPU has finished the
//! last table (`DrawSync`) and the display shows it, so the last tick's picture is read then as well.
//!
//! **Which packets are terrain and characters:**
//! - **Terrain:** the field packet band from the cursor the tick's first `WorldChunk_DrawObjects`
//!   gets. `Field_ChunkStreamTask` writes the 13 `SetDrawMove`s before it, then the terrain;
//!   nothing else writes the band. The first flight's cells (LT-143) write their own band from the
//!   cursor the tick's first `func_80133914` gets.
//! - **Characters:** each part `Model_DrawParts` draws for `Npc_DrawIfVisible`, or for the two
//!   event tasks that draw an actor themselves ([`lt_scene::character_call`], LT-142), or for a
//!   battle's actors and arena (LT-116). Its polygon records start at part+0x28 and run class by
//!   class (the counts at part+0x00, the record sizes in [`RECORD_BYTES`]); every packet it links is
//!   inside that range. The arena's lit drawer (`FUN_8014c97c`) builds its packets in the same
//!   records.

use lt_emu_hooks::{Action, Call, Hook};
use crate::billboard::{self, Projection, ROT_AVERAGE4, ROT_TRANS_PERS4};
use lt_scene::{DRAW_OBJECTS, DRAW_OBJECTS_WORD, MODEL_DRAW_PARTS, MODEL_DRAW_PARTS_2, SKY_CELL, SKY_CELL_WORD};

/// Main: `Dbg_DrawOTag(u32 *last_entry)`.
pub const DRAW_OTAG: u32 = 0x801A_4CA8;
/// Main: `Dbg_PutDrawEnv(DRAWENV *env)`: isbg at +0x18, then R, G, B.
pub const PUT_DRAW_ENV: u32 = 0x801A_4D18;
/// Main: libgpu's `MoveImage(RECT *rect, int x, int y)` (a VRAM copy run now, outside the table).
pub const MOVE_IMAGE: u32 = 0x801A_4A20;
/// Main: `Battle_LoadingScreenTask`, the battle transition (to `FUN_8018438c`).
pub const LOADING_SCREEN: u32 = 0x8018_3C9C;
pub const LOADING_SCREEN_SIZE: u32 = 0x6F0;
/// Main: `FUN_8018af14(ctx)`, the frame freeze (LT-140): the displayed buffer's left 160 columns
/// to VRAM (320, 0), the right 160 to (320, 256), 224 lines each.
pub const FREEZE_FRAME: u32 = 0x8018_AF14;
pub const FREEZE_FRAME_SIZE: u32 = 0x8C;
/// Main: `Field_ChunkStreamTask`, which makes the same two copies itself (inlined) as a chunk load
/// starts.
pub const CHUNK_STREAM: u32 = 0x8018_B2EC;
pub const CHUNK_STREAM_SIZE: u32 = 0x1DD4;
/// Main: `FUN_8018afa0(owner, buf, level)`, which shows the frozen frame (LT-140): two textured
/// quads (`POLY_FT4`) at `buf`+0x1D8 (screen x 0–160, texture page 0x105) and `buf`+0x200 (x
/// 160–320, page 0x115), linked in bucket 2 and dimmed to 0x7C − 4·`level` (0x80 at level 0).
pub const SHOW_FROZEN: u32 = 0x8018_AFA0;
pub const FROZEN_QUADS: [u32; 2] = [0x1D8, 0x200];
/// Bytes of one of `main()`'s two ordering tables (0xC00 buckets).
pub const OT_BYTES: u32 = 0x3000;
pub const OT_BUCKETS: u32 = OT_BYTES / 4;
/// The field packet bands (`Field_ChunkStreamTask`): 0x8000 a buffer, 0xF000 in mode 3.
pub const FIELD_BANDS: u32 = 0x800C_8000;
/// The first flight's packet bands (`Ovr24_RenderMapTask`, LT-143): 0x20000 a buffer from
/// 0x80060000 (it stops with "PACKET OVER" past that).
pub const SKY_BANDS: u32 = 0x8006_0000;
pub const SKY_BAND: u32 = 0x2_0000;
/// The first flight's sky (`D_80135188`): a POLY_G4 a buffer, 0x24 bytes apart, over the whole
/// screen in the farthest bucket, darker at the top.
pub const SKY_GRADIENT: [u32; 2] = [0x8013_5188, 0x8013_51AC];
/// Bytes of one polygon record per TMD class pass, read from `Model_DrawParts`' loops
/// (`addiu $s0, $s0, n`).
pub const RECORD_BYTES: [u32; 20] = [0x50, 0x64, 0x68, 0x88, 0x4C, 0x60, 0x5C, 0x78, 0x3C, 0x48, 0x54, 0x6C, 0x54, 0x6C, 0x5C, 0x78, 0x34, 0x40, 0x44, 0x58];
const CTX: u32 = 0x1F80_0000;
const END: u32 = 0x00FF_FFFF;

/// A packet in the table: where it is, its bucket (0 the nearest, drawn last) and its words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub addr: u32,
    pub bucket: u32,
    pub words: Vec<u32>,
    /// Terrain or a character: drawn natively. The 2D layer still decodes it, for the GPU state
    /// it leaves (a blob shadow takes its blend mode from the texture page the character's last
    /// polygon set), and draws nothing of it.
    pub aside: bool,
    /// A character's blob shadow (`Npc_DrawIfVisible`'s subtractive quad, or a battle actor's,
    /// LT-116): the lit look draws real shadows instead and leaves these out (LT-112). The flight
    /// shadow shares the field's prims and isn't one (LT-144).
    pub shadow: bool,
}

/// The blob shadows' prims: 0x80147DA0 + buffer × 0x118 + slot × 0x28, seven slots a buffer.
pub const BLOB_SHADOWS: u32 = 0x8014_7DA0;
pub const BLOB_SHADOWS_BYTES: u32 = 2 * 0x118;
/// A battle actor's blob shadow (`BattleFx_DrawActorShadowQuads`, LT-116): two quads in its record,
/// at +0xB0 and +0xF0 (each 0x20 a buffer, both buffers within these 0x80 bytes).
pub const BATTLE_SHADOW: u32 = 0xB0;
pub const BATTLE_SHADOW_BYTES: u32 = 0x80;

/// The battle's sky (`func_8014B4A4`'s POLY_FT4s in the farthest bucket, LT-149): a strip of
/// 128-pixel tiles (`D_8014EB68`, six a buffer; three textures repeating every 384 pixels), the fill
/// below it to line 200 (`D_8014ED48`) and the fill above it (`D_8014EDD8`), each a buffer.
pub const BATTLE_SKY_STRIP: std::ops::Range<u32> = 0x8014_EB68..0x8014_ED48;
pub const BATTLE_SKY_BELOW: std::ops::Range<u32> = 0x8014_ED48..0x8014_ED98;
pub const BATTLE_SKY_ABOVE: std::ops::Range<u32> = 0x8014_EDD8..0x8014_EE28;
/// The strip's textures repeat every this many pixels.
pub const BATTLE_SKY_PERIOD: i32 = 384;

/// Which part of the battle's sky a packet at `addr` is (LT-149).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BattleSky {
    Strip,
    Fill,
}

pub fn battle_sky(addr: u32) -> Option<BattleSky> {
    if BATTLE_SKY_STRIP.contains(&addr) {
        Some(BattleSky::Strip)
    } else if BATTLE_SKY_BELOW.contains(&addr) || BATTLE_SKY_ABOVE.contains(&addr) {
        Some(BattleSky::Fill)
    } else {
        None
    }
}

/// Whether `addr` is a battle actor's blob shadow quad.
pub fn battle_shadow(addr: u32) -> bool {
    use lt_scene::{BATTLE_ACTORS, BATTLE_ACTOR_BYTES, BATTLE_ACTOR_COUNT};
    let Some(o) = addr.checked_sub(BATTLE_ACTORS) else { return false };
    o < BATTLE_ACTOR_COUNT * BATTLE_ACTOR_BYTES && (BATTLE_SHADOW..BATTLE_SHADOW + BATTLE_SHADOW_BYTES).contains(&(o % BATTLE_ACTOR_BYTES))
}

/// A VRAM copy the battle transition or a frame freeze made (`MoveImage`): rect `from` (x, y, w,
/// h) to `to`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageMove {
    pub from: [u16; 4],
    pub to: [u16; 2],
}

/// One tick's ordering table as the GPU gets it.
#[derive(Clone, Debug, Default)]
pub struct Table {
    /// Every packet, in the GPU's order (far to near); the 2D layer draws those not `aside`.
    pub packets: Vec<Linked>,
    /// Packets set aside: drawn natively.
    pub terrain: u32,
    pub characters: u32,
    /// GP0(E1h)–GP0(E6h) as the table starts (`Dbg_PutDrawEnv` has just run).
    pub env: [u32; 6],
    /// The background the draw environment clears to, when it clears (isbg).
    pub clear: Option<[u8; 3]>,
    /// The display area the table's buffer is shown in: x, y, width, height.
    pub display: [u16; 4],
    /// The display is in 24-bit colour (the movies).
    pub bpp24: bool,
    /// The display's screen position (GP1 06h/07h) as a shift in pixels, as PSoXide's picture
    /// shows it: the draw area sits this far into the displayed lines ([`crate::play::on_screen`]).
    pub screen: [i32; 2],
    /// The capture's tick this table was drawn for (the last to end before it), when that tick was
    /// captured.
    pub tick: Option<u64>,
    /// The game isn't showing what its tables draw: the display isn't on the buffer the last
    /// table drew (a movie plays into a buffer of its own). The native view then passes the
    /// displayed picture through.
    pub passthrough: bool,
    /// VRAM as the table starts (1024 × 512 halfwords): what its primitives sample.
    pub vram: Vec<u16>,
    /// The billboards' projections since the table before (LT-144, [`crate::billboard`]).
    pub billboards: Vec<Projection>,
    /// The trail sparkles the game projected and didn't link, each to go before packet `.0`: a
    /// widened view draws them ([`crate::billboard::restore`]).
    pub restored: Vec<(usize, Linked)>,
    /// `Field_DrawTorchFlames`' calls for the table: the smoke's state ([`crate::smoke`]).
    pub torches: Vec<crate::smoke::Torch>,
    /// The first flight's cells drew in it (LT-143): its sky gradient is [`SKY_GRADIENT`]'s.
    pub sky: bool,
    /// The battle transition's VRAM copies since the table before, in order (LT-116): the first
    /// copies the frame on screen, the rest move parts of that copy about.
    pub moves: Vec<ImageMove>,
    /// A frame freeze's VRAM copies since the table before (LT-140): the halves of the frame on
    /// screen, which a menu or a chunk load then shows dimmed.
    pub frozen: Vec<ImageMove>,
    /// The packets that show the frozen frame in this table (`FUN_8018afa0`'s two quads, LT-140).
    pub frozen_quads: Vec<u32>,
    /// A battle's table (ctx+0x43 is 2 as it's drawn): its sky packets are [`battle_sky`]'s (LT-149).
    pub battle: bool,
    /// The draw area (VRAM x0, y0, x1, y1, inclusive) in force where the GPU reaches the first
    /// packet set aside: the native 3D is clipped to its lines as the GPU clips those packets
    /// (LT-116: a battle keeps its world above the status panel, `y` ≤ 175).
    pub clip3d: Option<[u16; 4]>,
}

/// The draw area (GP0 E3h/E4h, inclusive VRAM corners) in force at `table`'s first set-aside
/// packet, starting from the environment the table starts with.
pub fn clip_at_first_aside(table: &Table) -> Option<[u16; 4]> {
    let corner = |w: u32| [(w & 0x3FF) as u16, ((w >> 10) & 0x3FF) as u16];
    let (mut tl, mut br) = (corner(table.env[2]), corner(table.env[3]));
    for p in &table.packets {
        if p.aside {
            return Some([tl[0], tl[1], br[0], br[1]]);
        }
        for &w in &p.words {
            match w >> 24 {
                0xE3 => tl = corner(w),
                0xE4 => br = corner(w),
                _ => {}
            }
        }
    }
    None
}

/// The takeover's hooks, in this order.
pub fn hooks() -> Vec<Hook> {
    vec![
        Hook::observe("takeover: WorldChunk_DrawObjects", DRAW_OBJECTS).guarded(DRAW_OBJECTS_WORD),
        Hook::observe("takeover: Model_DrawParts", MODEL_DRAW_PARTS),
        Hook::observe("takeover: Model_DrawParts (second entry)", MODEL_DRAW_PARTS_2),
        Hook::observe("takeover: Dbg_PutDrawEnv", PUT_DRAW_ENV),
        Hook::observe("takeover: Dbg_DrawOTag", DRAW_OTAG),
        Hook::observe("takeover: RotTransPers4", ROT_TRANS_PERS4),
        Hook::observe("takeover: RotAverage4", ROT_AVERAGE4),
        Hook::observe("takeover: RotTransPers", crate::smoke::ROT_TRANS_PERS),
        Hook::observe("takeover: func_80133914 (OVR24 sky cell)", SKY_CELL).guarded(SKY_CELL_WORD),
        Hook::observe("takeover: FUN_8014c97c (OVR03 lit arena)", lt_scene::ARENA_LIT).guarded(lt_scene::ARENA_LIT_WORD),
        Hook::observe("takeover: MoveImage", MOVE_IMAGE),
        Hook::observe("takeover: FUN_8018afa0 (the frozen frame)", SHOW_FROZEN),
    ]
}

/// The packets a part's polygon records hold: from part+0x28, class by class.
fn part_records(c: &Call, part: u32) -> (u32, u32) {
    let p = c.read(part, 0x2C);
    let records = kseg0(u32::from_le_bytes([p[0x28], p[0x29], p[0x2A], p[0x2B]]));
    let bytes: u32 = (0..20).map(|k| u16::from_le_bytes([p[k * 2], p[k * 2 + 1]]) as u32 * RECORD_BYTES[k]).sum();
    (records, records + bytes)
}

fn kseg0(a: u32) -> u32 {
    (a & 0x1FFF_FFFF) | 0x8000_0000
}

/// A VRAM rect (x, y, width, height in pixels) as the display shows it, RGBA: 15-bit colour, or
/// 24-bit (three bytes a pixel from x × 2 bytes on). Unlike `Gpu::display_rgba8`, the picture
/// isn't slid by the screen position the game sets.
pub fn decode(vram: &[u16], rect: [u16; 4], bpp24: bool) -> Vec<u8> {
    let [x0, y0, w, h] = rect.map(usize::from);
    let mut out = Vec::with_capacity(w * h * 4);
    let half = |x: usize, y: usize| vram[(y & 511) * 1024 + (x & 1023)];
    for y in y0..y0 + h {
        for x in 0..w {
            if bpp24 {
                let byte = |b: usize| {
                    let hw = half(x0 + b / 2, y);
                    if b.is_multiple_of(2) { hw as u8 } else { (hw >> 8) as u8 }
                };
                let b = x * 3;
                out.extend_from_slice(&[byte(b), byte(b + 1), byte(b + 2), 255]);
            } else {
                let p = half(x0 + x, y);
                let c = |v: u16| ((v << 3) | (v >> 2)) as u8;
                out.extend_from_slice(&[c(p & 31), c((p >> 5) & 31), c((p >> 10) & 31), 255]);
            }
        }
    }
    out
}

/// A picture the GPU finished: the display area's pixels (RGBA).
#[derive(Clone, Debug, Default)]
pub struct Shown {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// The takeover's state through a tick.
#[derive(Default)]
pub struct Takeover {
    terrain_from: Option<(u32, u32)>,
    /// The first flight's cells drew since the last table.
    sky: bool,
    parts: Vec<(u32, u32)>,
    clear: Option<[u8; 3]>,
    /// Where the last table drew: x, y of its draw area.
    last_origin: Option<[u16; 2]>,
    /// The billboards' projections since the last table.
    projections: Vec<Projection>,
    /// `Field_DrawTorchFlames`' calls since the last table (their anchor tests), and main's sine
    /// table, read once.
    torches: Vec<crate::smoke::Torch>,
    torch_tick: bool,
    sine: Vec<i16>,
    /// The battle transition's VRAM copies since the last table.
    moves: Vec<ImageMove>,
    /// A frame freeze's VRAM copies since the last table, and the quads showing it.
    frozen: Vec<ImageMove>,
    frozen_quads: Vec<u32>,
    /// The tables handed to the GPU, oldest first (take them with [`Takeover::take`]), each with
    /// the picture the GPU made of the table before it (shown when this one starts).
    pub tables: Vec<(Table, Shown)>,
}

impl Table {
    /// The packets the 2D layer draws.
    pub fn kept(&self) -> impl Iterator<Item = &Linked> {
        self.packets.iter().filter(|p| !p.aside)
    }
}

impl Takeover {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take(&mut self) -> Vec<(Table, Shown)> {
        std::mem::take(&mut self.tables)
    }

    /// Handles hook `i` of [`hooks`] (0-based within them).
    pub fn call(&mut self, i: usize, c: &Call) -> Action {
        match i {
            0 => {
                if self.terrain_from.is_none() {
                    let cursor = kseg0(c.arg(1));
                    let band = if c.read(CTX + 0x43, 1)[0] == 3 { 0xF000 } else { 0x8000 };
                    if cursor >= FIELD_BANDS {
                        let end = FIELD_BANDS + ((cursor - FIELD_BANDS) / band + 1) * band;
                        self.terrain_from = Some((cursor, end));
                    }
                }
            }
            // A character: drawn by the actor loop, or by an event task (LT-142).
            1 | 2 if lt_scene::character_call(c) => self.parts.push(part_records(c, c.arg(0))),
            // The battle arena with its light (LT-116).
            9 => self.parts.push(part_records(c, c.arg(0))),
            // The battle transition's VRAM copies (LT-116), and a frame freeze's (LT-140).
            10 => {
                let ra = c.ra() & 0x1FFF_FFFF;
                let from = |start: u32, size: u32| ra > start & 0x1FFF_FFFF && ra <= (start + size) & 0x1FFF_FFFF;
                let made = || {
                    let r = c.read(c.arg(0), 8);
                    let h = |i: usize| u16::from_le_bytes([r[i], r[i + 1]]);
                    ImageMove { from: [h(0), h(2), h(4), h(6)], to: [c.arg(1) as u16, c.arg(2) as u16] }
                };
                if from(LOADING_SCREEN, LOADING_SCREEN_SIZE) {
                    if self.moves.len() < 64 {
                        self.moves.push(made());
                    }
                } else if (from(FREEZE_FRAME, FREEZE_FRAME_SIZE) || from(CHUNK_STREAM, CHUNK_STREAM_SIZE)) && self.frozen.len() < 8 {
                    self.frozen.push(made());
                }
            }
            // The frozen frame shown (LT-140).
            11 => {
                if self.frozen_quads.len() < 8 {
                    self.frozen_quads.extend(FROZEN_QUADS.map(|o| kseg0(c.arg(1) + o)));
                }
            }
            3 => {
                let env = c.read(c.arg(0) + 0x18, 4);
                self.clear = (env[0] != 0).then_some([env[1], env[2], env[3]]);
            }
            4 => {
                let gpu = &c.bus().gpu;
                let d = gpu.display_area();
                let shown = Shown { w: d.width as u32, h: d.height as u32, rgba: decode(gpu.vram.words(), [d.x, d.y, d.width, d.height], d.bpp24) };
                let mut table = self.walk(c);
                table.env = gpu.draw_environment();
                table.clear = self.clear;
                table.vram = gpu.vram.words().to_vec();
                table.billboards = std::mem::take(&mut self.projections);
                // The flight shadow uses the blob shadows' prims; it isn't one (LT-144).
                for p in table.billboards.iter().filter(|p| p.caller == billboard::FLIGHT_SHADOW) {
                    if let Some(q) = table.packets.iter_mut().find(|q| Some(q.addr) == p.ft4()) {
                        q.shadow = false;
                    }
                }
                table.restored = billboard::restore(&table.billboards, &table.packets, &|a, n| c.read(a, n));
                let torches = std::mem::take(&mut self.torches);
                self.torch_tick = false;
                if !torches.is_empty() {
                    if self.sine.is_empty() {
                        self.sine = c.read(crate::smoke::SINE, crate::smoke::SINE_LEN * 2).as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect();
                    }
                    let (puffs, packets) = crate::smoke::rebuild(&torches, &self.sine, &table.packets);
                    table.billboards.extend(puffs);
                    table.restored.extend(packets);
                    table.restored.sort_by_key(|(after, _)| *after);
                }
                table.torches = torches;
                // The table draws into the buffer the display shows next: the other one.
                let e = table.env;
                let (top, bottom) = (((e[2] >> 10) & 0x3FF) as u16, ((e[3] >> 10) & 0x3FF) as u16);
                table.display = [d.x, top, d.width, d.height.min(bottom.saturating_sub(top) + 1)];
                table.bpp24 = d.bpp24;
                table.screen = [gpu.horizontal_display_offset_px(), gpu.vertical_display_offset_px()];
                // With no table before it, assume the game shows what it draws.
                table.passthrough = self.last_origin.is_some_and(|o| o != [d.x, d.y]);
                self.last_origin = Some([(e[2] & 0x3FF) as u16, top]);
                table.sky = std::mem::take(&mut self.sky);
                table.moves = std::mem::take(&mut self.moves);
                table.frozen = std::mem::take(&mut self.frozen);
                table.frozen_quads = std::mem::take(&mut self.frozen_quads);
                table.clip3d = clip_at_first_aside(&table);
                self.tables.push((table, shown));
                self.terrain_from = None;
                self.parts.clear();
            }
            5 | 6 => {
                // A movie or a menu with no tables for a while: keep the newest.
                if self.projections.len() >= 4096 {
                    self.projections.drain(..2048);
                }
                self.projections.extend(Projection::from_call(c));
            }
            // The first flight's cells (LT-143): their band, from the tick's first cursor.
            8 => {
                self.sky = true;
                let cursor = kseg0(c.arg(2));
                if self.terrain_from.is_none() && cursor >= SKY_BANDS {
                    let end = SKY_BANDS + ((cursor - SKY_BANDS) / SKY_BAND + 1) * SKY_BAND;
                    self.terrain_from = Some((cursor, end));
                }
            }
            7 if billboard::caller(c, c.ra()) == Some("Field_DrawTorchFlames") => {
                // A call's first anchor test notes the state its puffs are built from (its tests go
                // up the record list).
                let rec = crate::smoke::Torch::record(c);
                if !self.torch_tick || self.torches.last().and_then(|t| t.tested.last()).is_some_and(|t| t.0 >= rec) {
                    self.torch_tick = true;
                    self.torches.push(crate::smoke::Torch::at_call(c));
                }
                if let Some(t) = self.torches.last_mut() {
                    t.test(c);
                }
            }
            _ => {}
        }
        Action::Continue
    }

    /// The table from its last entry (the farthest bucket) to the end.
    fn walk(&mut self, c: &Call) -> Table {
        let mut t = Table::default();
        let start = kseg0(c.arg(0));
        let (ot_lo, ot_hi) = (start + 4 - OT_BYTES, start + 4);
        let mut word = c.read_u32(start);
        let mut bucket = OT_BUCKETS - 1;
        let mut guard = 0;
        // The battle's actor records hold their blob shadows (LT-116); the field has chunk data there.
        let battle = c.read(CTX + 0x43, 1)[0] == lt_assets::BATTLE_MODE;
        t.battle = battle;
        while word & END != END && guard < 0x10_0000 {
            guard += 1;
            let addr = kseg0(word & END);
            word = c.read_u32(addr);
            if (ot_lo..ot_hi).contains(&addr) {
                bucket = (addr - ot_lo) / 4;
                continue;
            }
            let terrain = self.terrain_from.is_some_and(|(a, b)| (a..b).contains(&addr));
            let character = !terrain && self.parts.iter().any(|&(a, b)| (a..b).contains(&addr));
            t.terrain += terrain as u32;
            t.characters += character as u32;
            let len = (word >> 24) as usize;
            if len > 0 {
                let words = c.read(addr + 4, len * 4).as_chunks::<4>().0.iter().map(|&b| u32::from_le_bytes(b)).collect();
                let shadow = (BLOB_SHADOWS..BLOB_SHADOWS + BLOB_SHADOWS_BYTES).contains(&addr) || (battle && battle_shadow(addr));
                t.packets.push(Linked { addr, bucket, words, aside: terrain || character, shadow });
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_3d_takes_the_draw_area_set_before_it() {
        let packet = |words: Vec<u32>, aside: bool| Linked { addr: 0, bucket: 0, words, aside, shadow: false };
        // A battle's: the environment's whole screen, then the world's area (to line 175), the
        // native 3D, then the whole screen again for the panel.
        let table = Table {
            env: [0xE100_0000, 0, 0xE300_0000, 0xE403_7D3F, 0, 0],
            packets: vec![packet(vec![0xE300_0000, 0xE402_BD3F], false), packet(vec![0x2C00_0000], true), packet(vec![0xE300_0000, 0xE403_7D3F], false)],
            ..Table::default()
        };
        assert_eq!(clip_at_first_aside(&table), Some([0, 0, 319, 175]));
        let field = Table { packets: vec![packet(vec![0x2C00_0000], true)], ..table.clone() };
        assert_eq!(clip_at_first_aside(&field), Some([0, 0, 319, 223]));
        assert_eq!(clip_at_first_aside(&Table { packets: vec![], ..table }), None);
    }

    #[test]
    fn battle_shadows_are_in_the_actor_records() {
        let actor = |k: u32| lt_scene::BATTLE_ACTORS + k * lt_scene::BATTLE_ACTOR_BYTES;
        assert!(battle_shadow(actor(0) + 0xB0) && battle_shadow(actor(3) + 0x110) && battle_shadow(actor(8) + 0x12C));
        assert!(!battle_shadow(actor(0) + 0xAC) && !battle_shadow(actor(2) + 0x130) && !battle_shadow(actor(9) + 0xB0) && !battle_shadow(0x8000_FFF0));
    }

    #[test]
    fn record_sizes_cover_the_twenty_passes() {
        assert_eq!(RECORD_BYTES.len(), lt_core::mesh::TMD_CLASSES.len());
        assert!(RECORD_BYTES.iter().all(|&b| b % 4 == 0 && b >= 0x34));
    }
}
