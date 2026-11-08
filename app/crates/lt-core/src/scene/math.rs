//! The field's fixed-point helpers and EXE tables, ported exactly: the sin
//! table (`D_8017602C`, a quarter wave that is not quite `floor`), the GC
//! `ratan2` and its table (`D_80175828`), `Angle_StepToward`, the polar step
//! `Vector_FromAngleScale2D`, and the Knight's and Baby's field animation
//! tables. Angles are 12-bit (0x1000 = 360°); facing 0 looks along +z.

use crate::container::le32;
use crate::{PsxExe, Result};

use super::anim::Sequence;

const SIN_TABLE: u32 = 0x8017_602C;
const ATAN_TABLE: u32 = 0x8017_5828;
const KNIGHT_SETS: u32 = 0x801B_7448;
const BABY_SET: u32 = 0x801B_7490;
/// Knight-vs-NPC radius² by size index (`D_801B74B0`).
const RADIUS2: u32 = 0x801B_74B0;
/// NPC/Baby-vs-Knight box half-size by size index (`D_801B74B8`).
const BOX: u32 = 0x801B_74B8;

/// Read once from the disc's EXE.
#[derive(Debug, Clone)]
pub struct Tables {
    sin: Vec<i16>,
    atan: Vec<i16>,
    /// 3 sets × 6 animations (`ctx+0xDE & 2` → set 1, `& 4` → set 2).
    pub knight: Vec<Sequence>,
    /// 8 animations.
    pub baby: Vec<Sequence>,
    pub radius2: [u16; 4],
    pub boxes: [u8; 4],
    /// Item names (`#ddd`) and Living Toy names (`~dd`) as the game stores them.
    pub items: Vec<Vec<u8>>,
    pub toys: Vec<Vec<u8>>,
}

impl Tables {
    pub fn from_exe(exe: &PsxExe) -> Result<Self> {
        let halves = |va: u32, n: usize| -> Result<Vec<i16>> {
            Ok(exe.slice(va, n * 2)?.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect())
        };
        let seq = |va: u32| -> Result<Sequence> {
            let n = exe.slice(va, 1)?[0] as usize;
            let b = exe.slice(va + 1, n * 2)?;
            Ok(b.chunks_exact(2).map(|c| (c[0], c[1])).collect())
        };
        let table = |va: u32, n: usize| -> Result<Vec<Sequence>> {
            let ptrs = exe.slice(va, n * 4)?;
            (0..n).map(|i| seq(le32(ptrs, i * 4))).collect()
        };
        let r = halves(exe.va(RADIUS2), 4)?;
        let b = exe.slice(exe.va(BOX), 4)?;
        let cstr = |va: u32| -> Vec<u8> {
            (0..64u32).map_while(|k| exe.slice(va + k, 1).ok().map(|c| c[0]).filter(|&c| c != 0)).collect()
        };
        let names = |table: u32, n: usize, stride: usize| -> Vec<Vec<u8>> {
            (0..n).map(|i| exe.slice(table + (i * stride) as u32, 4).map(|p| cstr(le32(p, 0))).unwrap_or_default()).collect()
        };
        Ok(Self {
            sin: halves(exe.va(SIN_TABLE), 1025)?,
            atan: halves(exe.va(ATAN_TABLE), 1025)?,
            knight: table(exe.va(KNIGHT_SETS), 18)?,
            baby: table(exe.va(BABY_SET), 8)?,
            radius2: [r[0] as u16, r[1] as u16, r[2] as u16, r[3] as u16],
            boxes: [b[0], b[1], b[2], b[3]],
            items: names(exe.va(crate::gamedata::items::ITEM_TABLE), crate::gamedata::items::ITEM_COUNT, crate::gamedata::items::ITEM_SIZE),
            toys: names(exe.va(crate::gamedata::toys::ROSTER), crate::gamedata::toys::ROSTER_ROWS, crate::gamedata::toys::ROW_SIZE),
        })
    }

    /// `FUN_801a1040`: 4096·sin(a).
    pub fn sin(&self, a: i32) -> i32 {
        let i = (a & 0x3FF) as usize;
        let v = if a & 0x400 != 0 { self.sin[1024 - i] } else { self.sin[i] } as i32;
        if a & 0x800 != 0 {
            -v
        } else {
            v
        }
    }

    /// `FUN_801a103c`: sin(a + 0x400).
    pub fn cos(&self, a: i32) -> i32 {
        self.sin(a + 0x400)
    }

    /// `Vector_FromAngleScale2D`: (dx, dz) = (sin·s, cos·s) / 4096, truncated toward 0.
    pub fn polar(&self, a: i32, s: i32) -> (i32, i32) {
        (self.sin(a) * s / 4096, self.cos(a) * s / 4096)
    }

    /// `FUN_801a0e00`: the angle of (x, y) from +x toward +y, in [−0x800, 0x800].
    pub fn ratan2(&self, a0: i32, a1: i32) -> i32 {
        if a0 | a1 == 0 {
            return 0;
        }
        let (mut x, mut y, mut t0, mut neg, mut v0) = (a0, a1, 0, false, 0x800);
        if y <= 0 {
            y = -y;
            neg = !neg;
            v0 = -0x800;
        }
        if x < 0 {
            x = -x;
            neg = !neg;
            t0 = v0;
        }
        let ratio = if x < y {
            neg = !neg;
            t0 = v0 >> 1;
            (x << 12) / y
        } else {
            (y << 12) / x
        };
        let v = self.atan[((ratio >> 2) as usize).min(1024)] as i32;
        t0 + if neg { -v } else { v }
    }

    /// The GC facing from `from` toward `to` (x, z): `ratan2(dx, −dz) + 0x400`.
    pub fn facing(&self, from: (i32, i32), to: (i32, i32)) -> i32 {
        self.ratan2(to.0 - from.0, from.1 - to.1) + 0x400
    }
}

/// `Angle_StepToward`: shortest way, clamped to `step` per call (`step` 0 snaps).
pub fn step_toward(p: &mut i16, target: i32, step: i32) {
    if step == 0 {
        *p = target as i16;
        return;
    }
    let mut d = (target - *p as i32) & 0xFFF;
    if d >= 0x801 {
        d -= 0x1000;
    }
    if d < -step {
        d = -step;
    }
    if d > step {
        d = step;
    }
    *p = (*p as i32 + d) as i16;
}

/// True when `a` and `b` are the same 12-bit angle.
pub fn same_angle(a: i32, b: i32) -> bool {
    (a - b) & 0xFFF == 0
}
