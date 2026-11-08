//! Guardian's Crusade's memory by name (LT-104, research 16): the struct layouts, globals and
//! every function, generated from the decomp so nobody hand-types an offset.
//!
//! - **Layouts.** `layouts.toml` names the fields. `gen/gen.py` checks each one against the decomp
//!   (the struct field it cites, measured with the decomp's own compiler, or the symbol it names)
//!   and writes `src/generated.rs`. A field the decomp has no type for says so in its doc, and the
//!   live check (`lt-emu` example `gc_sys_live`) reads it from a running game.
//! - **Globals** carry retail addresses. [`Addrs`] maps them for a HackerGC base whose main moved
//!   (its `base.json` `exeLayout.ranges`, the same runs `lt_core::layout::Layout` uses).
//! - **Symbols.** Every function of main and the 25 overlays, with a lookup that tells which
//!   overlay is loaded in a slot from two words of its code ([`loaded_overlays`], [`function_at`]).
//!
//! Reads go through [`Memory`], so the same code reads a live emulator, a RAM image ([`Image`]) or
//! a code hook's call (`lt-emu-hooks`) without copying RAM.

use std::marker::PhantomData;

mod generated;
pub use generated::*;

/// Anything the game's memory can be read from: main RAM in any segment and the scratchpad.
pub trait Memory {
    /// Fills `out` from `addr`. Unmapped bytes read 0.
    fn read(&self, addr: u32, out: &mut [u8]);

    fn u32(&self, addr: u32) -> u32 {
        let mut b = [0; 4];
        self.read(addr, &mut b);
        u32::from_le_bytes(b)
    }
}

impl<F: Fn(u32, &mut [u8])> Memory for F {
    fn read(&self, addr: u32, out: &mut [u8]) {
        self(addr, out)
    }
}

/// A plain RAM image (2 MiB, from 0x80000000), with the scratchpad (1 KiB) beside it.
pub struct Image<'a> {
    pub ram: &'a [u8],
    pub scratchpad: &'a [u8],
}

impl Memory for Image<'_> {
    fn read(&self, addr: u32, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            let a = addr.wrapping_add(i as u32);
            let phys = a & 0x1FFF_FFFF;
            *b = if (0x1F80_0000..0x1F80_0400).contains(&phys) {
                self.scratchpad.get((phys - 0x1F80_0000) as usize).copied().unwrap_or(0)
            } else if phys < 0x80_0000 && !self.ram.is_empty() {
                self.ram[phys as usize % self.ram.len()]
            } else {
                0
            };
        }
    }
}

/// A value read from little-endian bytes.
pub trait Value: Sized {
    const SIZE: usize;
    fn from_le(b: &[u8]) -> Self;
}

macro_rules! int {
    ($($t:ty),*) => {$(
        impl Value for $t {
            const SIZE: usize = std::mem::size_of::<$t>();
            fn from_le(b: &[u8]) -> Self {
                <$t>::from_le_bytes(b[..Self::SIZE].try_into().unwrap())
            }
        }
    )*};
}
int!(u8, i8, u16, i16, u32, i32);

impl<T: Value + Copy + Default, const N: usize> Value for [T; N] {
    const SIZE: usize = T::SIZE * N;
    fn from_le(b: &[u8]) -> Self {
        let mut out = [T::default(); N];
        for (i, v) in out.iter_mut().enumerate() {
            *v = T::from_le(&b[i * T::SIZE..]);
        }
        out
    }
}

/// The GTE's `MATRIX`: a 3 × 3 rotation in 4.12 fixed point, then a translation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Matrix {
    pub m: [[i16; 3]; 3],
    pub t: [i32; 3],
}

impl Value for Matrix {
    const SIZE: usize = 0x20;
    fn from_le(b: &[u8]) -> Self {
        let mut m = [[0i16; 3]; 3];
        for (i, v) in m.iter_mut().flatten().enumerate() {
            *v = <i16 as Value>::from_le(&b[i * 2..]);
        }
        Matrix { m, t: [<i32 as Value>::from_le(&b[0x14..]), <i32 as Value>::from_le(&b[0x18..]), <i32 as Value>::from_le(&b[0x1C..])] }
    }
}

/// `SVECTOR`: x, y, z and a pad, 16-bit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SVector {
    pub x: i16,
    pub y: i16,
    pub z: i16,
    pub pad: i16,
}

impl Value for SVector {
    const SIZE: usize = 8;
    fn from_le(b: &[u8]) -> Self {
        SVector { x: <i16 as Value>::from_le(b), y: <i16 as Value>::from_le(&b[2..]), z: <i16 as Value>::from_le(&b[4..]), pad: <i16 as Value>::from_le(&b[6..]) }
    }
}

/// `VECTOR`: x, y, z and a pad, 32-bit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vector {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub pad: i32,
}

impl Value for Vector {
    const SIZE: usize = 0x10;
    fn from_le(b: &[u8]) -> Self {
        Vector { x: <i32 as Value>::from_le(b), y: <i32 as Value>::from_le(&b[4..]), z: <i32 as Value>::from_le(&b[8..]), pad: <i32 as Value>::from_le(&b[12..]) }
    }
}

/// A field of a struct: its offset and type.
pub struct Field<T> {
    pub offset: u32,
    _t: PhantomData<T>,
}

impl<T> Clone for Field<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Field<T> {}

impl<T: Value> Field<T> {
    pub const fn at(offset: u32) -> Self {
        Field { offset, _t: PhantomData }
    }

    /// The field of the struct at `base`.
    pub fn read(self, mem: &impl Memory, base: u32) -> T {
        let mut b = vec![0u8; T::SIZE];
        mem.read(base.wrapping_add(self.offset), &mut b);
        T::from_le(&b)
    }

    /// The field's address in the struct at `base`.
    pub fn addr(self, base: u32) -> u32 {
        base.wrapping_add(self.offset)
    }
}

/// A global (or a table of them) at a retail address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Global {
    pub name: &'static str,
    pub addr: u32,
    /// Bytes from one entry to the next.
    pub stride: u32,
    pub count: u32,
}

/// Where the globals are in the running game: retail's addresses, or a HackerGC base's.
#[derive(Clone, Debug, Default)]
pub struct Addrs {
    /// `[first, end, delta]`: retail addresses `first..end` are `delta` bytes away in this build.
    ranges: Vec<(u32, u32, i32)>,
}

impl Addrs {
    /// The retail USA disc.
    pub fn retail() -> Self {
        Addrs::default()
    }

    /// A shifted build: its `base.json` `profile.exeLayout.ranges` (what
    /// `lt_core::layout::Layout::ranges` holds).
    pub fn with_ranges(ranges: &[(u32, u32, i32)]) -> Self {
        Addrs { ranges: ranges.to_vec() }
    }

    /// Where retail address `addr` is in this build.
    pub fn map(&self, addr: u32) -> u32 {
        match self.ranges.iter().find(|&&(a, b, _)| a <= addr && addr < b) {
            Some(&(_, _, d)) => addr.wrapping_add_signed(d),
            None => addr,
        }
    }

    /// Entry `i` of `g` in this build.
    pub fn of(&self, g: Global, i: u32) -> u32 {
        assert!(i < g.count, "{}[{i}] past its {} entries", g.name, g.count);
        self.map(g.addr).wrapping_add(i * g.stride)
    }

    /// A function's entry in this build (main's move; an overlay's addresses never do).
    pub fn function(&self, name: &str) -> Option<u32> {
        let s = symbol(name)?;
        Some(match s.unit {
            Unit::Main => self.map(s.addr),
            Unit::Overlay(_) => s.addr,
        })
    }
}

/// A struct field by name (the generated [`FIELDS`] table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub structure: &'static str,
    pub name: &'static str,
    pub offset: u32,
    pub size: u32,
}

/// What lives at `addr` in this build, by name: `CHARACTERS[3].talk`, `CTX.view+0x14`,
/// `CHUNK_DATA[2]+0x1a04`. `None` when no known global holds it.
pub fn describe(addrs: &Addrs, addr: u32) -> Option<String> {
    let addr = if addr & 0x1FFF_FFFF < 0x80_0000 { (addr & 0x1FFF_FFFF) | 0x8000_0000 } else { addr };
    for &(g, ty) in GLOBALS {
        let base = addrs.map(g.addr);
        let span = g.stride.max(1) * g.count;
        if addr < base || addr >= base + span {
            continue;
        }
        let (i, off) = if g.stride == 0 { (0, addr - base) } else { ((addr - base) / g.stride, (addr - base) % g.stride) };
        let at = if g.count > 1 { format!("{}[{i}]", g.name) } else { g.name.to_string() };
        let field = ty.and_then(|t| FIELDS.iter().find(|f| f.structure == t && off >= f.offset && off < f.offset + f.size));
        return Some(match field {
            Some(f) if off == f.offset => format!("{at}.{}", f.name),
            Some(f) => format!("{at}.{}+{:#x}", f.name, off - f.offset),
            None => format!("{at}+{off:#x}"),
        });
    }
    None
}

/// Where a function lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Main,
    /// OVRnn.
    Overlay(u8),
}

/// A function of the retail game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub unit: Unit,
    pub addr: u32,
    pub size: u32,
    pub name: &'static str,
}

/// A code overlay: OVRnn loads at `addr`; the two `(address, word)` pairs are code no other
/// overlay has at those addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overlay {
    pub n: u8,
    pub addr: u32,
    pub size: u32,
    pub signature: [(u32, u32); 2],
}

impl Overlay {
    /// Whether this overlay is the one in RAM now.
    pub fn loaded(&self, mem: &impl Memory) -> bool {
        self.signature.iter().all(|&(a, w)| mem.u32(a) == w)
    }

    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.addr && addr < self.addr + self.size
    }
}

/// The overlays in RAM now.
pub fn loaded_overlays(mem: &impl Memory) -> Vec<u8> {
    OVERLAYS.iter().filter(|o| o.loaded(mem)).map(|o| o.n).collect()
}

/// A function by name (main's first, then the overlays').
pub fn symbol(name: &str) -> Option<&'static Symbol> {
    SYMBOLS.iter().find(|s| s.name == name)
}

/// The function holding code address `pc` (any segment) in the game as `mem` has it now: main's,
/// or that of the overlay loaded where `pc` points. Returns it and the offset into it.
pub fn function_at(mem: &impl Memory, pc: u32) -> Option<(&'static Symbol, u32)> {
    let pc = (pc & 0x1FFF_FFFF) | 0x8000_0000;
    let unit = if pc >= MAIN_TEXT.0 && pc < MAIN_TEXT.1 {
        Unit::Main
    } else {
        Unit::Overlay(OVERLAYS.iter().find(|o| o.contains(pc) && o.loaded(mem))?.n)
    };
    in_unit(unit, pc)
}

/// The function of `unit` holding `pc`, and the offset into it.
pub fn in_unit(unit: Unit, pc: u32) -> Option<(&'static Symbol, u32)> {
    let start = SYMBOLS.partition_point(|s| s.unit != unit);
    let syms = &SYMBOLS[start..];
    let syms = &syms[..syms.partition_point(|s| s.unit == unit)];
    let i = syms.partition_point(|s| s.addr <= pc);
    let s = syms.get(i.checked_sub(1)?)?;
    (pc < s.addr + s.size).then(|| (s, pc - s.addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_sort_by_unit_then_address() {
        let key = |s: &Symbol| (match s.unit { Unit::Main => 0, Unit::Overlay(n) => n }, s.addr);
        assert!(SYMBOLS.windows(2).all(|w| key(&w[0]) < key(&w[1])));
    }

    #[test]
    fn main_functions_look_up_with_offsets() {
        let none = |_: u32, _: &mut [u8]| {};
        let (s, off) = function_at(&none, 0x8019_DD80).unwrap();
        assert_eq!((s.name, off), ("Model_DrawParts", 0x10));
        assert_eq!(function_at(&none, 0xA019_DD70).unwrap().0.name, "Model_DrawParts");
        assert_eq!(symbol("Model_DrawParts").map(|s| s.addr), Some(0x8019_DD70));
        // Past the last of main's functions, nothing.
        assert_eq!(function_at(&none, MAIN_TEXT.1), None);
    }

    #[test]
    fn a_shared_slot_resolves_by_the_loaded_overlay() {
        // OVR02 and OVR04 both load at 0x801DF000; RAM holding OVR04's signature means OVR04.
        let ovr04 = OVERLAYS.iter().find(|o| o.n == 4).unwrap();
        let sig = ovr04.signature;
        let mem = move |addr: u32, out: &mut [u8]| {
            out.fill(0);
            for &(a, w) in &sig {
                if a == addr {
                    out.copy_from_slice(&w.to_le_bytes()[..out.len()]);
                }
            }
        };
        assert_eq!(loaded_overlays(&mem), vec![4]);
        let (s, _) = function_at(&mem, 0x801F_D83C).unwrap();
        assert_eq!((s.name, s.unit), ("Camera_UpdateTask", Unit::Overlay(4)));
        // And with nothing loaded there, no function.
        let none = |_: u32, out: &mut [u8]| out.fill(0);
        assert_eq!(function_at(&none, 0x801F_D83C), None);
    }

    #[test]
    fn fields_and_globals() {
        assert_eq!(Ctx::VIEW.offset, 0x98);
        assert_eq!(Character::Z.offset, 4);
        assert_eq!(CHARACTERS.stride, Character::SIZE);
        assert_eq!(Addrs::retail().of(CHARACTERS, 1), 0x8014_6638);
        let mut ram = vec![0u8; 0x20_0000];
        ram[0x14_6638..0x14_6640].copy_from_slice(&[0x10, 0, 0xF0, 0xFF, 0x20, 0, 0, 0]);
        let img = Image { ram: &ram, scratchpad: &[0; 0x400] };
        let b = Addrs::retail().of(CHARACTERS, 1);
        assert_eq!((Character::X.read(&img, b), Character::H.read(&img, b), Character::Z.read(&img, b)), (16, -16, 32));
    }

    #[test]
    fn addresses_are_named_by_global_and_field() {
        let r = Addrs::retail();
        assert_eq!(describe(&r, 0x8014_6600 + 0x38 * 3 + 0x12).as_deref(), Some("CHARACTERS[3].talk"));
        assert_eq!(describe(&r, 0x1F80_0098 + 0x14).as_deref(), Some("CTX.view+0x14"));
        assert_eq!(describe(&r, 0x8014_6600 + 0x38 + 0x30).as_deref(), Some("CHARACTERS[1]+0x30"));
        assert_eq!(describe(&r, 0x0014_6600).as_deref(), Some("CHARACTERS[0].x"));
        assert_eq!(describe(&r, 0x8000_0000), None);
        let shifted = Addrs::with_ranges(&[(0x801C_E000, 0x801D_0000, 0x800)]);
        assert_eq!(describe(&shifted, 0x801C_F008).as_deref(), Some("CHUNK_SLOTS.loaded"));
    }

    #[test]
    fn a_shifted_build_moves_what_its_ranges_say() {
        let a = Addrs::with_ranges(&[(0x801C_E000, 0x801D_0000, 0x800)]);
        assert_eq!(a.of(CHUNK_SLOTS, 0), 0x801C_F000);
        assert_eq!(a.of(CHARACTERS, 0), 0x8014_6600);
        assert_eq!(a.map(0x1F80_0000), 0x1F80_0000);
    }
}
