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
//! The GPU's environment, the clear colour and VRAM are noted then too. The GPU has finished the
//! last table (`DrawSync`) and the display shows it, so the last tick's picture is read then as well.
//!
//! **Which packets are terrain and characters:**
//! - **Terrain:** the field packet band from the cursor the tick's first `WorldChunk_DrawObjects`
//!   gets. `Field_ChunkStreamTask` writes the 13 `SetDrawMove`s before it, then the terrain;
//!   nothing else writes the band.
//! - **Characters:** each part `Model_DrawParts` draws for `Npc_DrawIfVisible`. Its polygon records
//!   start at part+0x28 and run class by class (the counts at part+0x00, the record sizes in
//!   [`RECORD_BYTES`]); every packet it links is inside that range.

use lt_emu_hooks::{Action, Call, Hook};
use lt_scene::{DRAW_OBJECTS, DRAW_OBJECTS_WORD, MODEL_DRAW_PARTS, MODEL_DRAW_PARTS_2, NPC_DRAW, NPC_DRAW_SIZE};

/// Main: `Dbg_DrawOTag(u32 *last_entry)`.
pub const DRAW_OTAG: u32 = 0x801A_4CA8;
/// Main: `Dbg_PutDrawEnv(DRAWENV *env)`: isbg at +0x18, then R, G, B.
pub const PUT_DRAW_ENV: u32 = 0x801A_4D18;
/// Bytes of one of `main()`'s two ordering tables (0xC00 buckets).
pub const OT_BYTES: u32 = 0x3000;
pub const OT_BUCKETS: u32 = OT_BYTES / 4;
/// The field packet bands (`Field_ChunkStreamTask`): 0x8000 a buffer, 0xF000 in mode 3.
pub const FIELD_BANDS: u32 = 0x800C_8000;
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
    /// A character's blob shadow (`Npc_DrawIfVisible`'s subtractive quad): the lit look draws real
    /// shadows instead and leaves these out (LT-112).
    pub shadow: bool,
}

/// The blob shadows' prims: 0x80147DA0 + buffer × 0x118 + slot × 0x28, seven slots a buffer.
pub const BLOB_SHADOWS: u32 = 0x8014_7DA0;
pub const BLOB_SHADOWS_BYTES: u32 = 2 * 0x118;

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
}

/// The takeover's hooks, in this order.
pub fn hooks() -> Vec<Hook> {
    vec![
        Hook::observe("takeover: WorldChunk_DrawObjects", DRAW_OBJECTS).guarded(DRAW_OBJECTS_WORD),
        Hook::observe("takeover: Model_DrawParts", MODEL_DRAW_PARTS),
        Hook::observe("takeover: Model_DrawParts (second entry)", MODEL_DRAW_PARTS_2),
        Hook::observe("takeover: Dbg_PutDrawEnv", PUT_DRAW_ENV),
        Hook::observe("takeover: Dbg_DrawOTag", DRAW_OTAG),
    ]
}

fn within(ra: u32, start: u32, size: u32) -> bool {
    let (ra, start) = (ra & 0x1FFF_FFFF, start & 0x1FFF_FFFF);
    ra > start && ra <= start + size
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
    parts: Vec<(u32, u32)>,
    clear: Option<[u8; 3]>,
    /// Where the last table drew: x, y of its draw area.
    last_origin: Option<[u16; 2]>,
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
            1 | 2 if within(c.ra(), NPC_DRAW, NPC_DRAW_SIZE) => {
                let part = c.read(c.arg(0), 0x2C);
                let records = kseg0(u32::from_le_bytes([part[0x28], part[0x29], part[0x2A], part[0x2B]]));
                let bytes: u32 = (0..20).map(|k| u16::from_le_bytes([part[k * 2], part[k * 2 + 1]]) as u32 * RECORD_BYTES[k]).sum();
                self.parts.push((records, records + bytes));
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
                // The table draws into the buffer the display shows next: the other one.
                let e = table.env;
                let (top, bottom) = (((e[2] >> 10) & 0x3FF) as u16, ((e[3] >> 10) & 0x3FF) as u16);
                table.display = [d.x, top, d.width, d.height.min(bottom.saturating_sub(top) + 1)];
                table.bpp24 = d.bpp24;
                table.screen = [gpu.horizontal_display_offset_px(), gpu.vertical_display_offset_px()];
                // With no table before it, assume the game shows what it draws.
                table.passthrough = self.last_origin.is_some_and(|o| o != [d.x, d.y]);
                self.last_origin = Some([(e[2] & 0x3FF) as u16, top]);
                self.tables.push((table, shown));
                self.terrain_from = None;
                self.parts.clear();
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
                let shadow = (BLOB_SHADOWS..BLOB_SHADOWS + BLOB_SHADOWS_BYTES).contains(&addr);
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
    fn record_sizes_cover_the_twenty_passes() {
        assert_eq!(RECORD_BYTES.len(), lt_core::mesh::TMD_CLASSES.len());
        assert!(RECORD_BYTES.iter().all(|&b| b % 4 == 0 && b >= 0x34));
    }
}
