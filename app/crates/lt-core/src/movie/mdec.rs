//! STR video frames: the v2/v3 bitstream (what PsyQ's `DecDCTvlc` expands
//! on the CPU) and the MDEC chip's dequantisation, IDCT and YCbCr → RGB.
//!
//! A demuxed frame is an 8-byte header — `u16` MDEC code count/2, `u16`
//! 0x3800, `u16` quantisation scale, `u16` version — then a bitstream read
//! as little-endian `u16` words, most significant bit first. Macroblocks run
//! top to bottom, then left to right (16-pixel columns); each holds six
//! 8×8 blocks: two chroma blocks (blue difference, then red — the order the
//! hardware plays retail movies in), then Y1, Y2, Y3, Y4. A block is a DC coefficient (v2: 10
//! bits signed; v3: a differential Huffman code), MPEG-1 AC run/level codes
//! and `10` (end of block). Sources: jPSXdec "PlayStation 1 STR format"
//! §2.2–2.3 and psx-spx "MDEC Decompression"; the integer pipeline below
//! (dequantisation with the hardware's rounding, two 1-D IDCT passes over
//! the standard scale table shifted right by 3, 9-bit wrap then saturation,
//! the reduced-precision green term) follows the MDEC's observed behaviour.

use std::sync::OnceLock;

/// Header magic at bytes 2–3 of every frame.
pub const FRAME_MAGIC: u16 = 0x3800;

/// The quantisation table every STR game uploads (psx-spx), in zig-zag order.
const QUANT: [u8; 64] = [
    0x02, 0x10, 0x10, 0x13, 0x10, 0x13, 0x16, 0x16, 0x16, 0x16, 0x16, 0x16, 0x1A, 0x18, 0x1A, 0x1B, //
    0x1B, 0x1B, 0x1A, 0x1A, 0x1A, 0x1A, 0x1B, 0x1B, 0x1B, 0x1D, 0x1D, 0x1D, 0x22, 0x22, 0x22, 0x1D, //
    0x1D, 0x1D, 0x1B, 0x1B, 0x1D, 0x1D, 0x20, 0x20, 0x22, 0x22, 0x25, 0x26, 0x25, 0x23, 0x23, 0x22, //
    0x23, 0x26, 0x26, 0x28, 0x28, 0x28, 0x30, 0x30, 0x2E, 0x2E, 0x38, 0x38, 0x3A, 0x45, 0x45, 0x53,
];

/// The standard IDCT scale table (psx-spx "set_scale_table"), row = frequency.
const SCALE: [u16; 64] = [
    0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, //
    0x7D8A, 0x6A6D, 0x471C, 0x18F8, 0xE707, 0xB8E3, 0x9592, 0x8275, //
    0x7641, 0x30FB, 0xCF04, 0x89BE, 0x89BE, 0xCF04, 0x30FB, 0x7641, //
    0x6A6D, 0xE707, 0x8275, 0xB8E3, 0x471C, 0x7D8A, 0x18F8, 0x9592, //
    0x5A82, 0xA57D, 0xA57D, 0x5A82, 0x5A82, 0xA57D, 0xA57D, 0x5A82, //
    0x471C, 0x8275, 0x18F8, 0x6A6D, 0x9592, 0xE707, 0x7D8A, 0xB8E3, //
    0x30FB, 0x89BE, 0x7641, 0xCF04, 0xCF04, 0x7641, 0x89BE, 0x30FB, //
    0x18F8, 0xB8E3, 0x6A6D, 0x8275, 0x7D8A, 0x9592, 0x471C, 0xE707,
];

/// Zig-zag index of each coefficient, row-major (row = vertical frequency).
const ZIGZAG: [u8; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, //
    2, 4, 7, 13, 16, 26, 29, 42, //
    3, 8, 12, 17, 25, 30, 41, 43, //
    9, 11, 18, 24, 31, 40, 44, 53, //
    10, 19, 23, 32, 39, 45, 52, 54, //
    20, 22, 33, 38, 46, 51, 55, 60, //
    21, 34, 37, 47, 50, 56, 59, 61, //
    35, 36, 48, 49, 57, 58, 62, 63,
];

/// The 111 MPEG-1 AC codes (without the trailing sign bit): bits, run, level.
/// Plus `10` = end of block and `000001` = escape (6-bit run, 10-bit level).
const AC_CODES: [(&str, u8, u8); 111] = [
    ("11", 0, 1), ("011", 1, 1), ("0100", 0, 2), ("0101", 2, 1), ("00101", 0, 3), ("00110", 4, 1),
    ("00111", 3, 1), ("000100", 7, 1), ("000101", 6, 1), ("000110", 1, 2), ("000111", 5, 1),
    ("0000100", 2, 2), ("0000101", 9, 1), ("0000110", 0, 4), ("0000111", 8, 1), ("00100000", 13, 1),
    ("00100001", 0, 6), ("00100010", 12, 1), ("00100011", 11, 1), ("00100100", 3, 2), ("00100101", 1, 3),
    ("00100110", 0, 5), ("00100111", 10, 1), ("0000001000", 16, 1), ("0000001001", 5, 2),
    ("0000001010", 0, 7), ("0000001011", 2, 3), ("0000001100", 1, 4), ("0000001101", 15, 1),
    ("0000001110", 14, 1), ("0000001111", 4, 2), ("000000010000", 0, 11), ("000000010001", 8, 2),
    ("000000010010", 4, 3), ("000000010011", 0, 10), ("000000010100", 2, 4), ("000000010101", 7, 2),
    ("000000010110", 21, 1), ("000000010111", 20, 1), ("000000011000", 0, 9), ("000000011001", 19, 1),
    ("000000011010", 18, 1), ("000000011011", 1, 5), ("000000011100", 3, 3), ("000000011101", 0, 8),
    ("000000011110", 6, 2), ("000000011111", 17, 1), ("0000000010000", 10, 2), ("0000000010001", 9, 2),
    ("0000000010010", 5, 3), ("0000000010011", 3, 4), ("0000000010100", 2, 5), ("0000000010101", 1, 7),
    ("0000000010110", 1, 6), ("0000000010111", 0, 15), ("0000000011000", 0, 14), ("0000000011001", 0, 13),
    ("0000000011010", 0, 12), ("0000000011011", 26, 1), ("0000000011100", 25, 1), ("0000000011101", 24, 1),
    ("0000000011110", 23, 1), ("0000000011111", 22, 1), ("00000000010000", 0, 31), ("00000000010001", 0, 30),
    ("00000000010010", 0, 29), ("00000000010011", 0, 28), ("00000000010100", 0, 27), ("00000000010101", 0, 26),
    ("00000000010110", 0, 25), ("00000000010111", 0, 24), ("00000000011000", 0, 23), ("00000000011001", 0, 22),
    ("00000000011010", 0, 21), ("00000000011011", 0, 20), ("00000000011100", 0, 19), ("00000000011101", 0, 18),
    ("00000000011110", 0, 17), ("00000000011111", 0, 16), ("000000000010000", 0, 40), ("000000000010001", 0, 39),
    ("000000000010010", 0, 38), ("000000000010011", 0, 37), ("000000000010100", 0, 36),
    ("000000000010101", 0, 35), ("000000000010110", 0, 34), ("000000000010111", 0, 33),
    ("000000000011000", 0, 32), ("000000000011001", 1, 14), ("000000000011010", 1, 13),
    ("000000000011011", 1, 12), ("000000000011100", 1, 11), ("000000000011101", 1, 10),
    ("000000000011110", 1, 9), ("000000000011111", 1, 8), ("0000000000010000", 1, 18),
    ("0000000000010001", 1, 17), ("0000000000010010", 1, 16), ("0000000000010011", 1, 15),
    ("0000000000010100", 6, 3), ("0000000000010101", 16, 2), ("0000000000010110", 15, 2),
    ("0000000000010111", 14, 2), ("0000000000011000", 13, 2), ("0000000000011001", 12, 2),
    ("0000000000011010", 11, 2), ("0000000000011011", 31, 1), ("0000000000011100", 30, 1),
    ("0000000000011101", 29, 1), ("0000000000011110", 28, 1), ("0000000000011111", 27, 1),
];

// Lookup entries, indexed by the next 16 bits: kind in bits 30–31, code
// length in bits 0–4, run in 8–13, level in 16–25.
const KIND_CODE: u32 = 1 << 30;
const KIND_EOB: u32 = 2 << 30;
const KIND_ESC: u32 = 3 << 30;

fn ac_table() -> &'static [u32] {
    static TABLE: OnceLock<Box<[u32]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = vec![0u32; 1 << 16].into_boxed_slice();
        let mut put = |bits: &str, entry: u32| {
            let len = bits.len() as u32;
            let code = u32::from_str_radix(bits, 2).expect("binary code");
            let first = code << (16 - len);
            for v in first..first + (1 << (16 - len)) {
                t[v as usize] = entry | len;
            }
        };
        for (bits, run, level) in AC_CODES {
            put(bits, KIND_CODE | (run as u32) << 8 | (level as u32) << 16);
        }
        put("10", KIND_EOB);
        put("000001", KIND_ESC);
        t
    })
}

/// Zig-zag index → row-major position.
fn zagzig() -> &'static [usize; 64] {
    static T: OnceLock<[usize; 64]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0usize; 64];
        for (pos, &k) in ZIGZAG.iter().enumerate() {
            t[k as usize] = pos;
        }
        t
    })
}

/// The scale table as the hardware uses it: each entry arithmetic-shifted right by 3.
fn idct_matrix() -> &'static [i32; 64] {
    static T: OnceLock<[i32; 64]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0i32; 64];
        for (d, &s) in t.iter_mut().zip(SCALE.iter()) {
            *d = (s as i16 >> 3) as i32;
        }
        t
    })
}

/// Why a frame would not decode. `mb` is the macroblock (column-major order),
/// `block` 0–5 = Cb, Cr, Y1–Y4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdecError {
    /// Fewer than 8 bytes.
    Short,
    /// Bytes 2–3 are not 0x3800.
    BadMagic(u16),
    /// A bitstream version other than 1, 2 or 3.
    Version(u16),
    /// No AC or DC code matches the bits.
    BadCode { mb: usize, block: usize },
    /// More than 63 AC coefficients in one block.
    TooManyCoefficients { mb: usize, block: usize },
    /// The bitstream ended inside the frame.
    PastEnd { mb: usize },
}

impl std::fmt::Display for MdecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MdecError::Short => write!(f, "the frame is shorter than its 8-byte header"),
            MdecError::BadMagic(m) => write!(f, "the frame header magic is {m:#06x}, not 0x3800"),
            MdecError::Version(v) => write!(f, "bitstream version {v} is not supported (only 1–3)"),
            MdecError::BadCode { mb, block } => write!(f, "no code matches in macroblock {mb}, block {block}"),
            MdecError::TooManyCoefficients { mb, block } => {
                write!(f, "macroblock {mb}, block {block} has more than 63 AC coefficients")
            }
            MdecError::PastEnd { mb } => write!(f, "the bitstream ends inside macroblock {mb}"),
        }
    }
}

/// Facts about a decoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    pub qscale: u16,
    pub version: u16,
    /// Bits of bitstream consumed (after the 8-byte header).
    pub bits: usize,
    pub macroblocks: usize,
    /// MDEC run-length codes the bitstream expands to (a DC, each AC, an end per block).
    pub codes: usize,
}

/// Decodes one demuxed frame into `rgba` (width × height × 4 bytes, alpha 255).
pub fn decode(data: &[u8], width: u16, height: u16, rgba: &mut [u8]) -> Result<FrameStats, MdecError> {
    if data.len() < 8 {
        return Err(MdecError::Short);
    }
    let magic = u16::from_le_bytes([data[2], data[3]]);
    if magic != FRAME_MAGIC {
        return Err(MdecError::BadMagic(magic));
    }
    let qscale = u16::from_le_bytes([data[4], data[5]]);
    let version = u16::from_le_bytes([data[6], data[7]]);
    if !(1..=3).contains(&version) {
        return Err(MdecError::Version(version));
    }
    let (w, h) = (width as usize, height as usize);
    assert!(rgba.len() >= w * h * 4, "rgba buffer too small");
    let (mbw, mbh) = (w.div_ceil(16), h.div_ceil(16));
    let stream = &data[8..];
    let mut bits = Bits::new(stream);
    let mut dec = BlockDecoder { qscale: (qscale & 0x3F) as i32, v3: version == 3, prev_dc: [0; 3], codes: 0 };
    let mut blocks = [[0i32; 64]; 6];
    let mut pixels = [[0u8; 3]; 256];
    for mb in 0..mbw * mbh {
        for (b, blk) in blocks.iter_mut().enumerate() {
            let ac = dec.block(&mut bits, b, blk).map_err(|e| match e {
                BlockError::Code => MdecError::BadCode { mb, block: b },
                BlockError::Overrun => MdecError::TooManyCoefficients { mb, block: b },
            })?;
            if bits.consumed() > stream.len() * 8 {
                return Err(MdecError::PastEnd { mb });
            }
            if ac {
                idct(blk);
            } else {
                idct_dc(blk);
            }
        }
        to_rgb(&blocks, &mut pixels);
        // Column-major placement.
        let (x0, y0) = ((mb / mbh) * 16, (mb % mbh) * 16);
        for py in 0..16.min(h.saturating_sub(y0)) {
            let row = ((y0 + py) * w + x0) * 4;
            for px in 0..16.min(w.saturating_sub(x0)) {
                let [r, g, b] = pixels[py * 16 + px];
                let o = row + px * 4;
                rgba[o] = r;
                rgba[o + 1] = g;
                rgba[o + 2] = b;
                rgba[o + 3] = 255;
            }
        }
    }
    Ok(FrameStats { qscale, version, bits: bits.consumed(), macroblocks: mbw * mbh, codes: dec.codes })
}

/// Bits read from little-endian `u16` words, most significant bit first.
/// Past the end it yields zeros (`consumed` then exceeds the data).
struct Bits<'a> {
    data: &'a [u8],
    next: usize,
    buf: u64,
    avail: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut b = Self { data, next: 0, buf: 0, avail: 0 };
        b.refill();
        b
    }

    #[inline]
    fn refill(&mut self) {
        while self.avail <= 48 {
            let w = match self.data.get(self.next..self.next + 2) {
                Some(s) => u16::from_le_bytes([s[0], s[1]]) as u64,
                None => 0,
            };
            self.next += 2;
            self.buf |= w << (48 - self.avail);
            self.avail += 16;
        }
    }

    /// The next `n` bits (1..=32) without consuming them.
    #[inline]
    fn peek(&mut self, n: u32) -> u32 {
        if self.avail < n {
            self.refill();
        }
        (self.buf >> (64 - n)) as u32
    }

    #[inline]
    fn skip(&mut self, n: u32) {
        if self.avail < n {
            self.refill();
        }
        self.buf <<= n;
        self.avail -= n;
    }

    #[inline]
    fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n);
        v
    }

    fn consumed(&self) -> usize {
        self.next * 8 - self.avail as usize
    }
}

enum BlockError {
    Code,
    Overrun,
}

struct BlockDecoder {
    qscale: i32,
    v3: bool,
    /// v3 DC predictors: the two chroma blocks, then Y.
    prev_dc: [i32; 3],
    codes: usize,
}

impl BlockDecoder {
    /// Reads one block and writes its dequantised coefficients (row-major).
    /// Returns whether any AC coefficient is non-zero.
    fn block(&mut self, bits: &mut Bits, b: usize, out: &mut [i32; 64]) -> Result<bool, BlockError> {
        *out = [0; 64];
        let dc = if self.v3 { self.dc_v3(bits, b)? } else { sext(bits.read(10), 10) };
        out[0] = dequant(dc, QUANT[0] as i32);
        let table = ac_table();
        let zz = zagzig();
        let mut k = 0usize;
        let mut ac = false;
        let mut k_codes = 0usize;
        loop {
            let e = table[bits.peek(16) as usize];
            let (run, level) = match e & (3 << 30) {
                KIND_EOB => {
                    bits.skip(2);
                    self.codes += k_codes + 2;
                    return Ok(ac);
                }
                KIND_ESC => {
                    bits.skip(6);
                    let v = bits.read(16);
                    ((v >> 10) as usize, sext(v & 0x3FF, 10))
                }
                KIND_CODE => {
                    bits.skip(e & 0x1F);
                    let level = ((e >> 16) & 0x3FF) as i32;
                    let run = ((e >> 8) & 0x3F) as usize;
                    (run, if bits.read(1) == 1 { -level } else { level })
                }
                _ => return Err(BlockError::Code),
            };
            k += run + 1;
            if k > 63 {
                return Err(BlockError::Overrun);
            }
            let v = ac_dequant(level, self.qscale * QUANT[k] as i32);
            out[zz[k]] = v;
            ac |= v != 0;
            k_codes += 1;
        }
    }

    /// v3 DC: a size code, then that many bits (sign first), ×4, added to the
    /// previous DC of the same kind (either chroma block, or the running luma).
    fn dc_v3(&mut self, bits: &mut Bits, b: usize) -> Result<i32, BlockError> {
        let peek = bits.peek(8);
        let (len, size) = if b < 2 {
            // Chroma: 00→0, 01→1, 10→2, 110→3, 1110→4, …, 11111110→8.
            match peek {
                0x00..=0x3F => (2, 0),
                0x40..=0x7F => (2, 1),
                0x80..=0xBF => (2, 2),
                0xC0..=0xDF => (3, 3),
                0xE0..=0xEF => (4, 4),
                0xF0..=0xF7 => (5, 5),
                0xF8..=0xFB => (6, 6),
                0xFC..=0xFD => (7, 7),
                0xFE => (8, 8),
                _ => return Err(BlockError::Code),
            }
        } else {
            // Luma: 100→0, 00→1, 01→2, 101→3, 110→4, 1110→5, …, 1111110→8.
            match peek {
                0x00..=0x3F => (2, 1),
                0x40..=0x7F => (2, 2),
                0x80..=0x9F => (3, 0),
                0xA0..=0xBF => (3, 3),
                0xC0..=0xDF => (3, 4),
                0xE0..=0xEF => (4, 5),
                0xF0..=0xF7 => (5, 6),
                0xF8..=0xFB => (6, 7),
                0xFC..=0xFD => (7, 8),
                _ => return Err(BlockError::Code),
            }
        };
        bits.skip(len);
        let diff = if size == 0 {
            0
        } else {
            let v = bits.read(size) as i32;
            if v >> (size - 1) == 0 {
                v - ((1 << size) - 1)
            } else {
                v
            }
        };
        let slot = b.min(2);
        let dc = self.prev_dc[slot] + diff * 4;
        self.prev_dc[slot] = dc;
        // The MDEC sees only the low 10 bits.
        Ok(sext((dc & 0x3FF) as u32, 10))
    }
}

#[inline]
fn sext(v: u32, bits: u32) -> i32 {
    ((v << (32 - bits)) as i32) >> (32 - bits)
}

/// DC dequantisation: `coef × q` in the MDEC's ×16 domain, rounded toward
/// zero by half a step; q = 0 means `coef × 2`.
#[inline]
fn dequant(c: i32, q: i32) -> i32 {
    let v = if q != 0 { ((c * q) << 4) + round_bias(c) } else { (c * 2) << 4 };
    v.clamp(-0x4000, 0x3FFF)
}

/// AC dequantisation: `coef × qscale × table / 8` in the ×16 domain.
#[inline]
fn ac_dequant(c: i32, q: i32) -> i32 {
    let v = if q != 0 { (((c * q) >> 3) << 4) + round_bias(c) } else { (c * 2) << 4 };
    v.clamp(-0x4000, 0x3FFF)
}

#[inline]
fn round_bias(c: i32) -> i32 {
    match c {
        0 => 0,
        c if c < 0 => 8,
        _ => -8,
    }
}

/// Sign-extends the low 9 bits, then saturates to −128..127. (Spelled out
/// rather than `clamp`, which debug builds don't inline.)
#[inline(always)]
#[allow(clippy::manual_clamp)]
fn mask9_clamp(v: i32) -> i32 {
    let v = (v << 23) >> 23;
    if v < -128 {
        -128
    } else if v > 127 {
        127
    } else {
        v
    }
}

/// Two 1-D passes over the scale table: columns (kept as 16-bit), then rows
/// (9-bit wrap, saturate). Output is the block's signed samples. All-zero
/// columns and rows are skipped (they contribute nothing). Written with
/// `while` and fixed 8-term sums so debug builds stay fast enough to play.
fn idct(blk: &mut [i32; 64]) {
    let mt = idct_columns();
    let mut tmp = [[0i32; 8]; 8];
    let mut rows = [false; 8];
    let mut h = 0;
    while h < 8 {
        let col = [blk[h], blk[8 + h], blk[16 + h], blk[24 + h], blk[32 + h], blk[40 + h], blk[48 + h], blk[56 + h]];
        if col != [0; 8] {
            let mut y = 0;
            while y < 8 {
                let t = ((dot8(&col, &mt[y]) + 0x4000) >> 15) as i16 as i32;
                tmp[y][h] = t;
                rows[y] |= t != 0;
                y += 1;
            }
        }
        h += 1;
    }
    let mut y = 0;
    while y < 8 {
        let mut x = 0;
        while x < 8 {
            blk[y * 8 + x] = if rows[y] { mask9_clamp((dot8(&tmp[y], &mt[x]) + 0x4000) >> 15) } else { 0 };
            x += 1;
        }
        y += 1;
    }
}

#[inline(always)]
fn dot8(a: &[i32; 8], b: &[i32; 8]) -> i32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3] + a[4] * b[4] + a[5] * b[5] + a[6] * b[6] + a[7] * b[7]
}

/// Column `k` of the scale table (`m[u][k]` for u = 0..8), for [`dot8`].
fn idct_columns() -> &'static [[i32; 8]; 8] {
    static T: OnceLock<[[i32; 8]; 8]> = OnceLock::new();
    T.get_or_init(|| {
        let m = idct_matrix();
        let mut t = [[0i32; 8]; 8];
        for (k, col) in t.iter_mut().enumerate() {
            for (u, v) in col.iter_mut().enumerate() {
                *v = m[u * 8 + k];
            }
        }
        t
    })
}

/// The IDCT of a block with only a DC term: every sample is the same (row 0
/// of the scale table is constant), computed with the same two roundings.
fn idct_dc(blk: &mut [i32; 64]) {
    let m0 = idct_matrix()[0];
    let t = ((blk[0] * m0 + 0x4000) >> 15) as i16 as i32;
    *blk = [mask9_clamp((t * m0 + 0x4000) >> 15); 64];
}

/// One macroblock's six IDCT'd blocks → 16×16 RGB (24-bit MDEC output).
/// The first chroma block drives blue and the second red: what the
/// hardware does with retail STRs (the orange Activision logo, `movie/9`),
/// whatever the docs call the blocks.
fn to_rgb(blocks: &[[i32; 64]; 6], out: &mut [[u8; 3]; 256]) {
    let (cb, cr) = (&blocks[0], &blocks[1]);
    let mut py = 0;
    while py < 16 {
        let mut px = 0;
        while px < 16 {
            let yb = &blocks[2 + (py >> 3) * 2 + (px >> 3)];
            let y = yb[(py & 7) * 8 + (px & 7)];
            let c = (py >> 1) * 8 + (px >> 1);
            let (r_c, b_c) = (cr[c], cb[c]);
            let r = mask9_clamp(y + ((359 * r_c + 0x80) >> 8));
            let g = mask9_clamp(y + ((((-88 * b_c) & !0x1F) + ((-183 * r_c) & !0x07) + 0x80) >> 8));
            let b = mask9_clamp(y + ((454 * b_c + 0x80) >> 8));
            out[py * 16 + px] = [(r + 128) as u8, (g + 128) as u8, (b + 128) as u8];
            px += 1;
        }
        py += 1;
    }
}

/* ───────────────────────── encoding ───────────────────────── */

/// The orthonormal DCT-II basis, `B[u][n] = c(u)·cos((2n+1)uπ/16)`: what
/// the MDEC's IDCT inverts once coefficients are taken in its ×16 domain.
fn dct_basis() -> &'static [[f64; 8]; 8] {
    static T: OnceLock<[[f64; 8]; 8]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0f64; 8]; 8];
        for (u, row) in t.iter_mut().enumerate() {
            let c = if u == 0 { (1.0f64 / 8.0).sqrt() } else { 0.5 };
            for (n, v) in row.iter_mut().enumerate() {
                *v = c * (((2 * n + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos();
            }
        }
        t
    })
}

/// (run, level) → (code bits, length), for the codes in [`AC_CODES`].
fn ac_encode_table() -> &'static std::collections::HashMap<(u8, u8), (u32, u32)> {
    static T: OnceLock<std::collections::HashMap<(u8, u8), (u32, u32)>> = OnceLock::new();
    T.get_or_init(|| AC_CODES.iter().map(|&(bits, run, level)| ((run, level), (u32::from_str_radix(bits, 2).unwrap(), bits.len() as u32))).collect())
}

/// Bits written into little-endian `u16` words, most significant bit first.
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.n += 1;
            if self.n == 16 {
                self.out.extend((self.acc as u16).to_le_bytes());
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.acc <<= 16 - self.n;
            self.out.extend((self.acc as u16).to_le_bytes());
        }
        self.out
    }
}

/// One encoded frame.
#[derive(Debug, Clone)]
pub struct Encoded {
    /// The 8-byte header and the bitstream, padded to 4 bytes.
    pub data: Vec<u8>,
    pub qscale: u16,
    /// The header's code count: MDEC codes rounded up to 64, halved.
    pub codes_word: u16,
}

/// A frame's DCT coefficients, block by block in bitstream order
/// (macroblocks column by column, each Cb, Cr, Y1–Y4), row-major by
/// vertical frequency. Computed once and quantised at any scale.
pub struct Coefficients {
    width: u16,
    height: u16,
    blocks: Vec<[f32; 64]>,
}

/// `rgba` (width × height × 4) → YCbCr 4:2:0 macroblocks → DCT (separable:
/// rows, then columns).
pub fn coefficients(rgba: &[u8], width: u16, height: u16) -> Coefficients {
    let (w, h) = (width as usize, height as usize);
    let (mbw, mbh) = (w.div_ceil(16), h.div_ceil(16));
    let basis = dct_basis();
    let b: [[f32; 8]; 8] = std::array::from_fn(|u| std::array::from_fn(|n| basis[u][n] as f32));
    let px = |x: usize, y: usize| -> [f32; 3] {
        let o = (y.min(h - 1) * w + x.min(w - 1)) * 4;
        [rgba[o] as f32, rgba[o + 1] as f32, rgba[o + 2] as f32]
    };
    let dct = |f: &[[f32; 8]; 8]| -> [f32; 64] {
        // Rows: t[y][u] = Σx f[y][x]·B[u][x]; columns: X[v][u] = Σy t[y][u]·B[v][y].
        let mut t = [[0f32; 8]; 8];
        for y in 0..8 {
            for u in 0..8 {
                t[y][u] = (0..8).map(|x| f[y][x] * b[u][x]).sum();
            }
        }
        let mut out = [0f32; 64];
        for v in 0..8 {
            for u in 0..8 {
                out[v * 8 + u] = (0..8).map(|y| t[y][u] * b[v][y]).sum();
            }
        }
        out
    };
    let mut blocks = Vec::with_capacity(mbw * mbh * 6);
    for mb in 0..mbw * mbh {
        let (x0, y0) = ((mb / mbh) * 16, (mb % mbh) * 16);
        let mut ys = [[0f32; 16]; 16];
        let (mut cb, mut cr) = ([[0f32; 8]; 8], [[0f32; 8]; 8]);
        for yy in 0..16 {
            for xx in 0..16 {
                let [r, g, bl] = px(x0 + xx, y0 + yy);
                ys[yy][xx] = 0.299 * r + 0.587 * g + 0.114 * bl - 128.0;
                cb[yy / 2][xx / 2] += (-0.168_736 * r - 0.331_264 * g + 0.5 * bl) / 4.0;
                cr[yy / 2][xx / 2] += (0.5 * r - 0.418_688 * g - 0.081_312 * bl) / 4.0;
            }
        }
        let luma = |by: usize, bx: usize| -> [[f32; 8]; 8] { std::array::from_fn(|y| std::array::from_fn(|x| ys[by * 8 + y][bx * 8 + x])) };
        // Blue difference first, then red (see `to_rgb`).
        for block in [cb, cr, luma(0, 0), luma(0, 1), luma(1, 0), luma(1, 1)] {
            blocks.push(dct(&block));
        }
    }
    Coefficients { width, height, blocks }
}

/// Encodes `rgba` (width × height × 4) as a version-2 frame at `qscale`
/// (1–63): YCbCr 4:2:0 macroblocks, column by column, each Cb, Cr, Y1–Y4;
/// DCT; DC as 10 bits, AC as MPEG-1 run/level codes or escapes; `10` ends a block.
pub fn encode(rgba: &[u8], width: u16, height: u16, qscale: u16) -> Encoded {
    encode_coefficients(&coefficients(rgba, width, height), qscale)
}

/// The bitstream of already-transformed coefficients at `qscale`.
pub fn encode_coefficients(c: &Coefficients, qscale: u16) -> Encoded {
    let q = qscale.clamp(1, 63);
    let zz = zagzig();
    let table = ac_encode_table();
    let mut bw = BitWriter { out: Vec::new(), acc: 0, n: 0 };
    let mut codes = 0usize;
    for coef in &c.blocks {
        let dc = (coef[0] / QUANT[0] as f32).round().clamp(-512.0, 511.0) as i32;
        bw.put(dc as u32 & 0x3FF, 10);
        codes += 2;
        let mut run = 0u32;
        for k in 1..64 {
            let step = q as f32 * QUANT[k] as f32 / 8.0;
            let level = (coef[zz[k]] / step).round().clamp(-511.0, 511.0) as i32;
            if level == 0 {
                run += 1;
                continue;
            }
            match table.get(&(run as u8, level.unsigned_abs() as u8)).filter(|_| level.unsigned_abs() <= 40) {
                Some(&(bits, len)) => {
                    bw.put(bits, len);
                    bw.put((level < 0) as u32, 1);
                }
                None => {
                    bw.put(0b000001, 6);
                    bw.put(run, 6);
                    bw.put(level as u32 & 0x3FF, 10);
                }
            }
            codes += 1;
            run = 0;
        }
        bw.put(0b10, 2);
    }
    // End of frame: the 10-bit code 0x1FF after the last block, as every
    // retail frame has. The player's VLC expander (`DecDCTvlc2`) stops there
    // and pads the rest of the header's code count; without it, it reads on
    // into the padding.
    bw.put(0x1FF, 10);
    let _ = (c.width, c.height);
    let codes_word = (codes.div_ceil(64) * 64 / 2) as u16;
    let mut data = Vec::with_capacity(8);
    data.extend(codes_word.to_le_bytes());
    data.extend(FRAME_MAGIC.to_le_bytes());
    data.extend(q.to_le_bytes());
    data.extend(2u16.to_le_bytes());
    data.extend(bw.finish());
    while data.len() % 4 != 0 {
        data.push(0);
    }
    Encoded { data, qscale: q, codes_word }
}

/// MDEC codes a frame may expand to: the player's run-length buffers
/// (OVR01 `Fmv_InitStreamBuffers`: 0x80041000 and 0x8005D000) hold 0x1C000
/// bytes each, two bytes a code — so the header's halved count is at most 0x7000.
pub const MAX_CODES_WORD: u16 = 0x7000;

/// The code budget frames are encoded to: the player expands codes on the
/// CPU (`DecDCTvlc2`) once per 1/15 s, and retail's busiest frame needs 0x1B00,
/// so new frames stay within what retail is known to play in time.
pub const CODES_WORD_BUDGET: u16 = 0x1C00;

/// The finest quantisation (lowest scale) whose frame fits `max_bytes` of
/// bitstream and the code budget ([`CODES_WORD_BUDGET`]).
pub fn encode_fit(rgba: &[u8], width: u16, height: u16, max_bytes: usize) -> Option<Encoded> {
    let c = coefficients(rgba, width, height);
    // Bytes fall as the scale rises: a binary search over 1–63 on one transform.
    let (mut lo, mut hi) = (1u16, 63u16);
    let mut best = None;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let e = encode_coefficients(&c, mid);
        if e.data.len() <= max_bytes && e.codes_word <= CODES_WORD_BUDGET {
            best = Some(e);
            if mid == 1 {
                break;
            }
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse = a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as f64 - *y as f64).powi(2)).sum::<f64>() / (a.len() * 3 / 4) as f64;
        10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
    }

    #[test]
    fn encoded_frames_decode_back() {
        let (w, h) = (64u16, 48u16);
        let mut img = vec![0u8; w as usize * h as usize * 4];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let o = (y * w as usize + x) * 4;
                img[o] = (x * 4) as u8;
                img[o + 1] = (y * 5) as u8;
                img[o + 2] = (((x / 8 + y / 8) % 2) * 160 + 40) as u8;
                img[o + 3] = 255;
            }
        }
        let mut back = vec![0u8; img.len()];
        let fine = encode(&img, w, h, 1);
        let st = decode(&fine.data, w, h, &mut back).unwrap();
        assert_eq!((st.version, st.qscale), (2, 1));
        assert_eq!(fine.codes_word as usize, st.codes.div_ceil(64) * 64 / 2);
        assert!(psnr(&img, &back) > 30.0, "q1 {:.1} dB", psnr(&img, &back));
        let coarse = encode(&img, w, h, 12);
        assert!(coarse.data.len() < fine.data.len());
        decode(&coarse.data, w, h, &mut back).unwrap();
        assert!(psnr(&img, &back) > 22.0);
        let fit = encode_fit(&img, w, h, coarse.data.len()).unwrap();
        assert!(fit.data.len() <= coarse.data.len() && fit.qscale <= 12);
    }

    #[test]
    fn tables() {
        let t = ac_table();
        // "11s" = run 0 level 1; "10" = EOB; "000001" = escape.
        assert_eq!(t[0b1100_0000_0000_0000] & !(3 << 30), 2 | 1 << 16);
        assert_eq!(t[0b1000_0000_0000_0000] >> 30, 2);
        assert_eq!(t[0b0000_0100_0000_0000] >> 30, 3);
        assert_eq!(t[0], 0, "sixteen zeros are not a code");
        assert_eq!(zagzig()[1], 1);
        assert_eq!(zagzig()[2], 8);
        assert_eq!(zagzig()[63], 63);
        assert_eq!(idct_matrix()[0], 0x5A82 >> 3);
        assert_eq!(idct_matrix()[12], (0xE707u16 as i16 >> 3) as i32);
    }

    #[test]
    fn dc_only_idct_matches_full() {
        for dc in [-0x4000, -12345, -16, -8, 0, 8, 24, 999, 0x3FFF] {
            let mut a = [0i32; 64];
            a[0] = dc;
            let mut b = a;
            idct(&mut a);
            idct_dc(&mut b);
            assert_eq!(a, b, "dc {dc}");
        }
    }

    #[test]
    fn flat_grey_frame() {
        // 16×16, v2, qscale 1: every block DC 0 then EOB → mid grey.
        let mut d = vec![0x00, 0x00, 0x00, 0x38, 0x01, 0x00, 0x02, 0x00];
        // Six blocks of "0000000000" + "10" = 12 bits each → 72 bits.
        let mut bitstr = String::new();
        for _ in 0..6 {
            bitstr.push_str("000000000010");
        }
        while !bitstr.len().is_multiple_of(16) {
            bitstr.push('0');
        }
        for w in bitstr.as_bytes().chunks(16) {
            let v = u16::from_str_radix(std::str::from_utf8(w).unwrap(), 2).unwrap();
            d.extend_from_slice(&v.to_le_bytes());
        }
        let mut rgba = vec![0u8; 16 * 16 * 4];
        let s = decode(&d, 16, 16, &mut rgba).unwrap();
        assert_eq!((s.version, s.qscale, s.macroblocks, s.bits), (2, 1, 1, 72));
        assert!(rgba.chunks(4).all(|p| p == [128, 128, 128, 255]));
    }
}
