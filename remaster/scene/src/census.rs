//! LT-118: every way the engine draws 3D, found from the code and measured on the test set.
//!
//! **Found.** Every 3D vertex goes through the GTE's perspective transform, `RTPS` or `RTPT`
//! (COP2 commands 0x01 and 0x30): there is no other way to project on the PS1. [`scan`] reads
//! main's code and all 25 overlays off the disc and lists each function (gc-sys's symbols, from the
//! decomp) that holds one, with its other GTE work (lighting `NC*`, depth `AVSZ*`, `MVMVA`, …).
//! libgte's own wrappers (`RotTransPers*`) are in the list too; their callers are the paths.
//!
//! **Measured.** [`Census`] hooks the entry of every one of them (observe only) and counts each
//! call by the function that made it (its return address, named with gc-sys for the overlay loaded
//! there). Overlays share load addresses (OVR02/OVR04, OVR06–22 and 25, OVR05/23), so a hook
//! counts only when the code loaded at its address is that function's entry.

use std::collections::BTreeMap;

use std::sync::Arc;

use emulator_core::ButtonState;
use gc_sys::{Symbol, Unit, OVERLAYS, SYMBOLS};
use lt_emu_hooks::{Action, Call, Hook, Hooks};
use lt_testset::{Image, Machine, Scene};

/// GTE commands by their low six bits (COP2 `imm25`: `0x4A000000 | sf/mx/v/cv/lm | cmd`).
pub const COMMANDS: [(u32, &str); 22] = [
    (0x01, "RTPS"),
    (0x06, "NCLIP"),
    (0x0C, "OP"),
    (0x10, "DPCS"),
    (0x11, "INTPL"),
    (0x12, "MVMVA"),
    (0x13, "NCDS"),
    (0x14, "CDP"),
    (0x16, "NCDT"),
    (0x1B, "NCCS"),
    (0x1C, "CC"),
    (0x1E, "NCS"),
    (0x20, "NCT"),
    (0x28, "SQR"),
    (0x29, "DCPL"),
    (0x2A, "DPCT"),
    (0x2D, "AVSZ3"),
    (0x2E, "AVSZ4"),
    (0x30, "RTPT"),
    (0x3D, "GPF"),
    (0x3E, "GPL"),
    (0x3F, "NCCT"),
];

/// The GTE command an instruction word is, if it is one.
pub fn command(word: u32) -> Option<&'static str> {
    if word >> 25 != 0x25 {
        return None;
    }
    COMMANDS.iter().find(|(c, _)| *c == word & 0x3F).map(|(_, n)| *n)
}

/// A function that projects vertices, and the GTE commands it holds.
#[derive(Clone, Debug)]
pub struct GteFunction {
    pub symbol: &'static Symbol,
    pub commands: BTreeMap<&'static str, u32>,
    /// Its first instruction word (what a hook at its entry finds when it is loaded).
    pub entry: u32,
}

impl GteFunction {
    /// Where it lives: "main" or "OVRnn".
    pub fn unit(&self) -> String {
        unit_name(self.symbol.unit)
    }

    /// Its GTE commands, most first, e.g. "RTPT 12, AVSZ3 4".
    pub fn summary(&self) -> String {
        let mut v: Vec<(&&str, &u32)> = self.commands.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        v.iter().map(|(n, c)| format!("{n} {c}")).collect::<Vec<_>>().join(", ")
    }
}

pub fn unit_name(u: Unit) -> String {
    match u {
        Unit::Main => "main".into(),
        Unit::Overlay(n) => format!("OVR{n:02}"),
    }
}

/// Every function that projects (`RTPS`/`RTPT`): `code(unit, addr, size)` gives a function's bytes
/// (main's from the EXE, an overlay's from its file).
pub fn scan(code: impl Fn(Unit, u32, u32) -> Option<Vec<u8>>) -> Vec<GteFunction> {
    let mut out = Vec::new();
    for s in SYMBOLS {
        let Some(bytes) = code(s.unit, s.addr, s.size) else { continue };
        let words: Vec<u32> = bytes.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
        let mut commands = BTreeMap::new();
        for &w in &words {
            if let Some(n) = command(w) {
                *commands.entry(n).or_insert(0u32) += 1;
            }
        }
        if commands.contains_key("RTPS") || commands.contains_key("RTPT") {
            out.push(GteFunction { symbol: s, commands, entry: words.first().copied().unwrap_or(0) });
        }
    }
    out
}

/// The game's code as the disc has it: main's EXE and each overlay's file.
pub struct Code {
    pub exe: lt_core::PsxExe,
    pub overlays: BTreeMap<u8, Vec<u8>>,
}

impl Code {
    /// Reads the overlays with `file` (`ovr/n`).
    pub fn new(exe: lt_core::PsxExe, file: impl Fn(&str) -> Option<Vec<u8>>) -> Self {
        let overlays = OVERLAYS.iter().filter_map(|o| Some((o.n, file(&format!("ovr/{}", o.n))?))).collect();
        Code { exe, overlays }
    }

    /// `size` bytes of `unit`'s code from `addr`.
    pub fn bytes(&self, unit: Unit, addr: u32, size: u32) -> Option<&[u8]> {
        match unit {
            Unit::Main => self.exe.slice(addr, size as usize).ok(),
            Unit::Overlay(n) => {
                let o = OVERLAYS.iter().find(|o| o.n == n)?;
                let at = addr.checked_sub(o.addr)? as usize;
                self.overlays.get(&n)?.get(at..at + size as usize)
            }
        }
    }

    /// The function at code address `pc` as RAM (`read`) has it: main's, or that of the overlay
    /// whose file holds the same 16 bytes there (overlays share load addresses; this needs no
    /// signature words).
    pub fn function_at(&self, read: impl Fn(u32, usize) -> Vec<u8>, pc: u32) -> Option<(&'static Symbol, u32)> {
        let pc = (pc & 0x1FFF_FFFF) | 0x8000_0000;
        if pc >= gc_sys::MAIN_TEXT.0 && pc < gc_sys::MAIN_TEXT.1 {
            return gc_sys::in_unit(Unit::Main, pc);
        }
        let at = pc & !3;
        let ram = read(at.saturating_sub(8), 16);

        OVERLAYS
            .iter()
            .filter(|o| o.contains(at))
            .find(|o| self.bytes(Unit::Overlay(o.n), at.saturating_sub(8), 16).is_some_and(|b| b == ram.as_slice()))
            .and_then(|o| gc_sys::in_unit(Unit::Overlay(o.n), pc))
    }
}

/// The functions whose code holds a `jal` to `target` (a static call; calls through a pointer
/// don't show). A `jal` reaches code at the same top address bits, so an overlay's call to an
/// address in a slot it shares is a call into its own code there, or into whatever is loaded.
pub fn static_callers(code: &Code, target: u32) -> Vec<&'static Symbol> {
    let want = (0x03u32 << 26) | ((target & 0x0FFF_FFFF) >> 2);
    SYMBOLS
        .iter()
        .filter(|s| code.bytes(s.unit, s.addr, s.size).is_some_and(|b| b.as_chunks::<4>().0.iter().any(|w| u32::from_le_bytes(*w) == want)))
        .collect()
}

/// Where `addr` is stored as a word in the code files (main's EXE, the overlays), a function
/// pointer or a jump table, or built in a register by `lui` then `addiu`/`ori` within eight
/// instructions (a call through `jalr`). Each as (unit, address).
pub fn pointers_to(code: &Code, addr: u32) -> Vec<(Unit, u32)> {
    let mut out = Vec::new();
    // `addiu` sign-extends its low half, so its `lui` carries one more when that half is negative.
    let (lo, hi_addiu, hi_ori) = (addr & 0xFFFF, (addr.wrapping_add(0x8000)) >> 16, addr >> 16);
    let mut search = |unit: Unit, base: u32, bytes: &[u8]| {
        let words: Vec<u32> = bytes.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
        for (i, &w) in words.iter().enumerate() {
            if w == addr {
                out.push((unit, base + 4 * i as u32));
                continue;
            }
            // lui rt, hi
            if w >> 26 != 0x0F {
                continue;
            }
            let (rt, hi) = ((w >> 16) & 31, w & 0xFFFF);
            let built = words[i + 1..(i + 9).min(words.len())].iter().any(|&n| {
                let (op, rs, imm) = (n >> 26, (n >> 21) & 31, n & 0xFFFF);
                rs == rt && imm == lo && ((op == 0x09 && hi == hi_addiu) || (op == 0x0D && hi == hi_ori))
            });
            if built {
                out.push((unit, base + 4 * i as u32));
            }
        }
    };
    let exe = &code.exe;
    search(Unit::Main, exe.t_addr, exe.bytes.get(0x800..).unwrap_or(&[]));
    for o in OVERLAYS {
        if let Some(b) = code.overlays.get(&o.n) {
            search(Unit::Overlay(o.n), o.addr, b);
        }
    }
    out
}

/// [`scan`] over the disc's EXE and overlay files.
pub fn scan_code(code: &Code) -> Vec<GteFunction> {
    scan(|unit, addr, size| code.bytes(unit, addr, size).map(<[u8]>::to_vec))
}

/// Entry stubs: tiny functions (16 bytes at most) whose first instruction branches or jumps into
/// one of `functions`, so a call through them skips that function's own entry (main's
/// `FUN_8019dd68` branches past `Model_DrawParts`' first instruction: the battle arena and some
/// NPC draws come in that way). Each as (stub, index into `functions`).
pub fn entry_stubs(code: &Code, functions: &[GteFunction]) -> Vec<(&'static Symbol, usize)> {
    let mut out = Vec::new();
    for s in SYMBOLS.iter().filter(|s| s.size <= 16) {
        let Some(b) = code.bytes(s.unit, s.addr, 4) else { continue };
        let w = u32::from_le_bytes(b.try_into().expect("4 bytes"));
        let target = match w >> 26 {
            // b offset (beq zero, zero)
            0x04 if (w >> 16) & 0x3FF == 0 => s.addr.wrapping_add(4).wrapping_add(((w & 0xFFFF) as i16 as i32 as u32) << 2),
            // j target
            0x02 => (s.addr.wrapping_add(4) & 0xF000_0000) | ((w & 0x03FF_FFFF) << 2),
            _ => continue,
        };
        if let Some(i) = functions.iter().position(|f| f.symbol.unit == s.unit && target > f.symbol.addr && target < f.symbol.addr + f.symbol.size) {
            out.push((s, i));
        }
    }
    out
}

/// Calls to each projecting function, by caller, as the game runs.
pub struct Census {
    pub functions: Vec<GteFunction>,
    /// Entry stubs into them ([`entry_stubs`]): calls there count for the function they enter.
    pub stubs: Vec<(&'static Symbol, usize)>,
    /// The hook table: one hook per distinct entry address (several overlays' functions can share
    /// one).
    addrs: Vec<u32>,
    /// Function index → caller name → calls.
    pub calls: Vec<BTreeMap<String, u64>>,
}

impl Census {
    pub fn new(functions: Vec<GteFunction>, stubs: Vec<(&'static Symbol, usize)>) -> Self {
        let mut addrs: Vec<u32> = functions.iter().map(|f| f.symbol.addr).chain(stubs.iter().map(|(s, _)| s.addr)).collect();
        addrs.sort_unstable();
        addrs.dedup();
        let calls = vec![BTreeMap::new(); functions.len()];
        Census { functions, stubs, addrs, calls }
    }

    /// The census for every projecting function of `code`, with their entry stubs.
    pub fn of(code: &Code) -> Self {
        let functions = scan_code(code);
        let stubs = entry_stubs(code, &functions);
        Census::new(functions, stubs)
    }

    pub fn hooks(&self) -> Hooks {
        Hooks::new(self.addrs.iter().map(|&a| Hook::observe(format!("{a:#010x}"), a)).collect()).expect("the census's hook table")
    }

    /// A hook's call: counted for the function loaded at its address, if one of ours starts there,
    /// by the function that called it (named from `code`).
    pub fn call(&mut self, code: &Code, c: &Call) {
        let read = |a: u32, n: usize| c.read(a, n);
        let Some((sym, 0)) = code.function_at(read, c.pc) else { return };
        let (i, via) = match self.functions.iter().position(|f| std::ptr::eq(f.symbol, sym)) {
            Some(i) => (i, String::new()),
            None => match self.stubs.iter().find(|(s, _)| std::ptr::eq(*s, sym)) {
                Some(&(s, i)) => (i, format!(" via {}", s.name)),
                None => return,
            },
        };
        let caller = match code.function_at(read, c.ra().wrapping_sub(8)) {
            Some((s, _)) => format!("{} ({}){via}", s.name, unit_name(s.unit)),
            None => format!("{:#010x}{via}", c.ra()),
        };
        *self.calls[i].entry(caller).or_insert(0) += 1;
    }

    /// Clears the counts (between scenes).
    pub fn reset(&mut self) {
        for c in &mut self.calls {
            c.clear();
        }
    }
}

/// Plays `frames` frames of `m` with `pad(i)` held, counting into `census`.
pub fn play(m: &mut Machine, code: &Code, census: &mut Census, frames: u64, mut pad: impl FnMut(u64) -> u16) -> Result<(), String> {
    let mut h = census.hooks();
    h.install(&mut m.cpu);
    for i in 0..frames {
        m.bus.set_port1_buttons(ButtonState::from_bits(pad(i)));
        h.run_frame(&mut m.cpu, &mut m.bus, &mut |c: &Call| {
            census.call(code, c);
            Action::Continue
        })
        .map_err(|e| format!("frame {}: {e}", m.frame))?;
        m.bus.run_spu_to_current_cycle();
        m.bus.spu.discard_audio();
        m.frame += 1;
    }
    Hooks::uninstall(&mut m.cpu);
    Ok(())
}

/// A test set scene's tape, counted.
pub fn scene(image: &Arc<Image>, code: &Code, scene: &Scene, state: &[u8], census: &mut Census) -> Result<(), String> {
    let mut m = Machine::from_state(image, &scene.card, state, scene.frame)?;
    let tape = scene.tape.clone();
    play(&mut m, code, census, tape.len() as u64, |i| tape[i as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gte_commands_are_told_apart() {
        assert_eq!(command(0x4A28_0030), Some("RTPT"));
        assert_eq!(command(0x4A18_0001), Some("RTPS"));
        assert_eq!(command(0x4B68_002E), Some("AVSZ4"));
        assert_eq!(command(0x4A00_0012 | 0x0048_0000), Some("MVMVA"));
        assert_eq!(command(0x27BD_FFE0), None, "addiu sp");
        assert_eq!(command(0x4800_0000), None, "mfc2");
    }

    #[test]
    fn an_address_built_in_a_register_counts_as_a_pointer() {
        // lui t0, 0x8019 / addiu t0, t0, -0x39a4 (0x8018c65c) / jalr t0; and a stored word.
        let words = [0x3C08_8019u32, 0x2508_C65C, 0x0100_F809, 0x8018_C65C];
        let mut exe_bytes = vec![0u8; 0x800];
        exe_bytes[..8].copy_from_slice(b"PS-X EXE");
        exe_bytes[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        exe_bytes.extend(words.iter().flat_map(|w| w.to_le_bytes()));
        let code = Code { exe: lt_core::PsxExe::parse(0, exe_bytes).unwrap(), overlays: BTreeMap::new() };
        assert_eq!(pointers_to(&code, 0x8018_C65C), vec![(Unit::Main, 0x8001_0000), (Unit::Main, 0x8001_000C)]);
        assert!(pointers_to(&code, 0x8018_C660).is_empty());
    }

    #[test]
    fn a_function_that_projects_is_found() {
        let main = SYMBOLS.iter().find(|s| s.unit == Unit::Main).unwrap();
        let code = |u: Unit, a: u32, _: u32| -> Option<Vec<u8>> {
            (u == Unit::Main && a == main.addr).then(|| [0x27BD_FFE0u32, 0x4A28_0030, 0x4B68_002E, 0x03E0_0008].iter().flat_map(|w| w.to_le_bytes()).collect())
        };
        let found = scan(code);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].entry, found[0].summary().as_str()), (0x27BD_FFE0, "AVSZ4 1, RTPT 1"));
    }
}
