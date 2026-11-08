//! Living Tool Remaster's lockstep checker (LT-105): the safety net for the whole remaster.
//!
//! Two machines start from the same test-set scene (`lt-testset`) and play the same input tape.
//! Side A usually runs the original code; side B runs with hooks, and later with native
//! replacements. After every frame (two per game tick) their main RAM and scratchpad are compared,
//! less any [`Mask`].
//!
//! On the first difference the checker plays that frame again, both sides together, one
//! instruction (or one hook call) at a time, watching the byte:
//! - the first moment the two sides disagree names the cause: the store and its function
//!   (`gc-sys`), or the hook that wrote it;
//! - if their control flow splits first (a replace hook skipping a function), that's reported;
//! - either way the report carries the last hook called and the functions run before the cause.
//!
//! The address is named too ([`gc_sys::describe`]: `CHARACTERS[0].talk`, `CTX+0x43`).
//!
//! A mask needs a reason a reviewer can judge ([`Mask::why`]); the report lists every mask given
//! and how many bytes differed inside it.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use emulator_core::{Bus, ButtonState};
use lt_emu_hooks::{Action, Call, Hooks};
use lt_testset::{Image, Machine, Scene};

pub mod gc;

/// Instructions one frame may run before the core is called stuck.
const STEPS_PER_FRAME_CAP: u64 = 1_000_000;
const RAM: usize = 2 * 1024 * 1024;
const SCRATCH: u32 = 0x1F80_0000;
const SCRATCH_LEN: usize = 0x400;
/// Functions kept in a cause's trace.
const TRACE: usize = 8;

/// Memory the checker doesn't compare, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mask {
    pub name: &'static str,
    /// Main RAM (any segment) or the scratchpad.
    pub addr: u32,
    pub len: u32,
    pub why: &'static str,
}

impl Mask {
    fn holds(&self, addr: u32) -> bool {
        let (a, b) = (phys(self.addr), phys(addr));
        a <= b && b < a + self.len
    }
}

/// The physical address, main RAM's mirrors folded.
fn phys(addr: u32) -> u32 {
    let p = addr & 0x1FFF_FFFF;
    if (p as usize) < 0x80_0000 {
        p % RAM as u32
    } else {
        p
    }
}

/// For a B side whose renderer replaces the field's drawing (Stage A, LT-111): the packets it no
/// longer builds and the tables that link them. Addresses from the decomp (`main()`,
/// `Field_ChunkStreamTask`, `Load_KnightBabyAssets_StateMachine`, OVR02's NPC slot allocator).
pub const RENDER_MASKS: &[Mask] = &[
    Mask {
        name: "field packets",
        addr: 0x800C_8000,
        len: 0x1_E000,
        why: "the field's per-frame GPU packet bands (two of 0x8000, or 0xF000 in mode 3, from 0x800C8000): scratch the native renderer stops filling, read by nothing but the GPU",
    },
    Mask {
        name: "ordering tables",
        addr: 0x801C_0960,
        len: 0x6000,
        why: "main()'s two ordering tables (0xC00 buckets each): links to the packets, read only by DrawOTag",
    },
    Mask {
        name: "actor packets",
        addr: 0x800D_8000,
        len: 0x3_9200,
        why: "the Knight's and Baby's prebuilt packets (heap from 0x800D8000) and the seven NPC packet slots (0x800F5000 + (i-2)*0x5A00): the OT link word in each primitive changes when other packets are skipped",
    },
];

/// A handler a side's hooks call.
pub type Handler = Box<dyn FnMut(&Call) -> Action>;

/// Builds one side's hooks, or `None` for a side that runs the original code alone. It's called
/// again for every re-run (the cause search starts again from the scene's state), so a handler
/// with state starts fresh. The cell holds the frame being run, counted from the scene's start.
pub type Setup<'a> = &'a dyn Fn(&Rc<Cell<u64>>) -> Option<(Hooks, Handler)>;

/// A side with no hooks: the original code.
pub fn original(_: &Rc<Cell<u64>>) -> Option<(Hooks, Handler)> {
    None
}

struct Side {
    m: Machine,
    hooks: Option<(Hooks, Handler)>,
    frame: Rc<Cell<u64>>,
}

impl Side {
    fn new(image: &Arc<Image>, scene: &Scene, state: &[u8], setup: Setup) -> Result<Side, String> {
        let mut m = Machine::from_state(image, &scene.card, state, scene.frame)?;
        let frame = Rc::new(Cell::new(0));
        let hooks = setup(&frame);
        if let Some((h, _)) = hooks.as_ref() {
            h.install(&mut m.cpu);
        }
        Ok(Side { m, hooks, frame })
    }

    /// Frame `i` of the tape, run as the host program runs a frame.
    fn frame(&mut self, i: u64, pad: u16) -> Result<(), String> {
        self.frame.set(i);
        let m = &mut self.m;
        match self.hooks.as_mut() {
            Some((h, f)) => {
                m.bus.set_port1_buttons(ButtonState::from_bits(pad));
                h.run_frame(&mut m.cpu, &mut m.bus, f).map_err(|e| format!("frame {i}: {e}"))?;
                m.bus.run_spu_to_current_cycle();
                m.bus.spu.discard_audio();
                m.frame += 1;
                Ok(())
            }
            None => m.step(pad),
        }
    }

    fn byte(&self, addr: u32) -> u8 {
        byte(&self.m.bus, addr)
    }
}

fn byte(bus: &Bus, addr: u32) -> u8 {
    let p = phys(addr);
    if (p as usize) < RAM {
        bus.ram()[p as usize]
    } else {
        bus.try_read8(addr).unwrap_or(0)
    }
}

/// `Foo+0x1c` for a code address, by the decomp's names (the overlay loaded there now).
fn function(bus: &Bus, pc: u32) -> String {
    let mem = |a: u32, out: &mut [u8]| {
        for (i, b) in out.iter_mut().enumerate() {
            *b = byte(bus, a.wrapping_add(i as u32));
        }
    };
    match gc_sys::function_at(&mem, pc) {
        Some((s, off)) => format!("{}+{off:#x}", s.name),
        None => format!("{pc:#010x}"),
    }
}

/// What made the two sides' byte differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cause {
    /// "B: hook Foo wrote it …", "B stored it in Foo+0x1c …", "control flow split …".
    pub what: String,
    /// The last hook called in that frame before the cause, and on which side.
    pub last_hook: Option<String>,
    /// The functions run before it, oldest first (up to eight, the cause's own last).
    pub trace: Vec<String>,
}

/// The first difference between the two sides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    /// The frame (from the scene's start) that ended different, and its game tick (frame / 2).
    pub frame: u64,
    pub tick: u64,
    pub addr: u32,
    pub a: u8,
    pub b: u8,
    /// The address by name, when gc-sys knows it.
    pub field: Option<String>,
    pub cause: Cause,
}

/// One run.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub scene: String,
    /// Frames compared (the whole tape when nothing differed).
    pub frames: u64,
    pub divergence: Option<Divergence>,
    /// Every mask given: its name, the most bytes that differed inside it after one frame, and why.
    pub masks: Vec<(&'static str, u64, &'static str)>,
    /// Calls per hook on side B.
    pub hook_calls: Vec<(String, u64)>,
    /// Side A's machine digest at the end (the test set's own digest, when A runs the original).
    pub digest_a: u64,
}

impl Report {
    pub fn text(&self) -> String {
        let mut s = format!("{}: {} frames ({} ticks) compared", self.scene, self.frames, self.frames / 2);
        match &self.divergence {
            None => s.push_str(", identical after every frame\n"),
            Some(d) => {
                s.push_str(&format!(
                    "\n  FIRST DIFFERENCE after frame {} (tick {}): {:#010x}{}: A {:#04x}, B {:#04x}\n  cause: {}\n",
                    d.frame,
                    d.tick,
                    d.addr,
                    d.field.as_deref().map(|f| format!(" {f}")).unwrap_or_default(),
                    d.a,
                    d.b,
                    d.cause.what,
                ));
                if let Some(h) = &d.cause.last_hook {
                    s.push_str(&format!("  last hook: {h}\n"));
                }
                s.push_str(&format!("  trace: {}\n", d.cause.trace.join(" > ")));
            }
        }
        if self.masks.is_empty() {
            s.push_str("  masks: none\n");
        }
        for (name, n, why) in &self.masks {
            s.push_str(&format!("  mask \"{name}\" used: up to {n} bytes differed in it in a frame. Why: {why}\n"));
        }
        let calls: Vec<String> = self.hook_calls.iter().map(|(h, n)| format!("{h} {n}")).collect();
        if !calls.is_empty() {
            s.push_str(&format!("  B's hook calls: {}\n", calls.join(", ")));
        }
        s
    }
}

/// Plays `scene` on two sides built by `a` and `b`, comparing main RAM and the scratchpad after
/// every frame less `masks`. On the first difference, finds its cause and stops.
pub fn run(image: &Arc<Image>, scene: &Scene, state: &[u8], a: Setup, b: Setup, masks: &[Mask]) -> Result<Report, String> {
    let mut sa = Side::new(image, scene, state, a)?;
    let mut sb = Side::new(image, scene, state, b)?;
    let mut masked = vec![false; RAM];
    let mut masked_scratch = [false; SCRATCH_LEN];
    for m in masks {
        for k in 0..m.len {
            let p = phys(m.addr.wrapping_add(k));
            if (p as usize) < RAM {
                masked[p as usize] = true;
            } else if (SCRATCH..SCRATCH + SCRATCH_LEN as u32).contains(&p) {
                masked_scratch[(p - SCRATCH) as usize] = true;
            }
        }
    }
    let mut report = Report { scene: scene.name.clone(), masks: masks.iter().map(|m| (m.name, 0, m.why)).collect(), ..Report::default() };
    for (i, &pad) in scene.tape.iter().enumerate() {
        let i = i as u64;
        sa.frame(i, pad)?;
        sb.frame(i, pad)?;
        report.frames = i + 1;
        let mut first = None;
        let mut per_mask = vec![0u64; masks.len()];
        let mut tally = |addr: u32| {
            if let Some(j) = masks.iter().position(|m| m.holds(addr)) {
                per_mask[j] += 1;
            }
        };
        let (ra, rb) = (sa.m.bus.ram(), sb.m.bus.ram());
        if ra != rb {
            for (c, (x, y)) in ra.chunks(64).zip(rb.chunks(64)).enumerate() {
                if x == y {
                    continue;
                }
                for k in (0..64).filter(|&k| x[k] != y[k]) {
                    let at = c * 64 + k;
                    if masked[at] {
                        tally(0x8000_0000 | at as u32);
                    } else if first.is_none() {
                        first = Some(0x8000_0000 | at as u32);
                    }
                }
            }
        }
        let (pa, pb) = (sa.m.read(SCRATCH, SCRATCH_LEN), sb.m.read(SCRATCH, SCRATCH_LEN));
        for k in (0..SCRATCH_LEN).filter(|&k| pa[k] != pb[k]) {
            if masked_scratch[k] {
                tally(SCRATCH + k as u32);
            } else if first.is_none() {
                first = Some(SCRATCH + k as u32);
            }
        }
        for (j, n) in per_mask.into_iter().enumerate() {
            report.masks[j].1 = report.masks[j].1.max(n);
        }
        if let Some(addr) = first {
            let cause = find_cause(image, scene, state, a, b, i, addr)?;
            report.divergence = Some(Divergence {
                frame: i,
                tick: i / 2,
                addr,
                a: sa.byte(addr),
                b: sb.byte(addr),
                field: gc_sys::describe(&gc_sys::Addrs::retail(), addr),
                cause,
            });
            break;
        }
    }
    if let Some((h, _)) = sb.hooks.as_ref() {
        report.hook_calls = h.hooks().iter().zip(&h.counts.calls).map(|(k, &n)| (k.name.clone(), n)).collect();
    }
    report.digest_a = sa.m.digest();
    Ok(report)
}

/// Plays both sides again to frame `k`, then steps frame `k` on both together until `addr` differs.
fn find_cause(image: &Arc<Image>, scene: &Scene, state: &[u8], a: Setup, b: Setup, k: u64, addr: u32) -> Result<Cause, String> {
    let mut sides = [Side::new(image, scene, state, a)?, Side::new(image, scene, state, b)?];
    for i in 0..k {
        for s in sides.iter_mut() {
            s.frame(i, scene.tape[i as usize])?;
        }
    }
    let pad = scene.tape[k as usize];
    let mut starts = [0u64; 2];
    for (j, s) in sides.iter_mut().enumerate() {
        s.frame.set(k);
        s.m.bus.set_port1_buttons(ButtonState::from_bits(pad));
        starts[j] = s.m.bus.irq().raise_counts()[0];
    }
    const NAMES: [&str; 2] = ["A", "B"];
    let mut trace: Vec<String> = Vec::new();
    let mut last_hook: Option<String> = None;
    // The PC each side last handled hooks at: its instruction runs next, once.
    let mut handled = [None::<u32>; 2];
    let mut ended = [false; 2];
    let cause = |what: String, last_hook: &Option<String>, trace: &[String]| Ok(Cause { what, last_hook: last_hook.clone(), trace: trace.to_vec() });
    if sides[0].byte(addr) != sides[1].byte(addr) {
        return cause("the sides already differed as the frame began (the end of the last frame's work)".into(), &last_hook, &trace);
    }
    for _ in 0..STEPS_PER_FRAME_CAP {
        // Hooks first, as Cpu::run stops for them.
        for (j, s) in sides.iter_mut().enumerate() {
            let pc = s.m.cpu.pc();
            let Some((h, f)) = s.hooks.as_mut() else { continue };
            if ended[j] || !h.covers(pc) || handled[j] == Some(pc) {
                continue;
            }
            handled[j] = Some(pc);
            let before = h.counts.calls.clone();
            let was = byte(&s.m.bus, addr);
            h.handle(&mut s.m.cpu, &mut s.m.bus, pc, f);
            let called: Vec<&str> = h.hooks().iter().zip(before.iter().zip(&h.counts.calls)).filter(|(_, (x, y))| x != y).map(|(k, _)| k.name.as_str()).collect();
            if called.is_empty() {
                continue;
            }
            last_hook = Some(format!("{}: {}", NAMES[j], called.join(" / ")));
            if byte(&s.m.bus, addr) != was {
                let what = format!("{}: hook {} wrote it (native code standing in at {})", NAMES[j], called.join(" / "), function(&s.m.bus, pc));
                return cause(what, &last_hook, &trace);
            }
        }
        let (pa, pb) = (sides[0].m.cpu.pc(), sides[1].m.cpu.pc());
        if pa != pb || ended[0] != ended[1] {
            // A replace hook (or a real difference) sent them different ways: run each on alone
            // to the frame's end and say what changed the byte on each.
            let split = format!(
                "control flow split before the byte changed (A at {}, B at {})",
                function(&sides[0].m.bus, pa),
                function(&sides[1].m.bus, pb)
            );
            let mut after = Vec::new();
            for (j, s) in sides.iter_mut().enumerate() {
                let w = last_writer(s, addr, starts[j], handled[j])?;
                after.push(format!("{}: {}", NAMES[j], w.unwrap_or_else(|| "left it as it was".into())));
            }
            return cause(format!("{split}; then the last change on each side: {}", after.join("; ")), &last_hook, &trace);
        }
        if ended[0] {
            break;
        }
        let at = function(&sides[0].m.bus, pa);
        let name = at.split('+').next().unwrap_or("").to_string();
        if trace.last() != Some(&name) {
            trace.push(name);
            if trace.len() > TRACE {
                trace.remove(0);
            }
        }
        let was = [sides[0].byte(addr), sides[1].byte(addr)];
        for (j, s) in sides.iter_mut().enumerate() {
            s.m.cpu.step(&mut s.m.bus).map_err(|e| format!("{e} (pc={:#010x})", s.m.cpu.pc()))?;
            handled[j] = None;
            ended[j] = s.m.bus.irq().raise_counts()[0] != starts[j];
        }
        let now = [sides[0].byte(addr), sides[1].byte(addr)];
        if now[0] != now[1] {
            let who: Vec<&str> = (0..2).filter(|&j| now[j] != was[j]).map(|j| NAMES[j]).collect();
            return cause(format!("{} stored it in {at} (pc {pa:#010x})", who.join(" and ")), &last_hook, &trace);
        }
    }
    cause("no instruction in the frame made them differ (a write from DMA or another device)".into(), &last_hook, &trace)
}

/// Runs one side on alone to the end of the frame (`start` is the VBlank count it began with),
/// watching `addr`: the last instruction or hook that changed it, if any.
fn last_writer(s: &mut Side, addr: u32, start: u64, mut handled: Option<u32>) -> Result<Option<String>, String> {
    let mut last = None;
    for _ in 0..STEPS_PER_FRAME_CAP {
        if s.m.bus.irq().raise_counts()[0] != start {
            break;
        }
        let pc = s.m.cpu.pc();
        let was = s.byte(addr);
        if let Some((h, f)) = s.hooks.as_mut() {
            if h.covers(pc) && handled != Some(pc) {
                handled = Some(pc);
                let before = h.counts.calls.clone();
                h.handle(&mut s.m.cpu, &mut s.m.bus, pc, f);
                if byte(&s.m.bus, addr) != was {
                    let called: Vec<&str> = h.hooks().iter().zip(before.iter().zip(&h.counts.calls)).filter(|(_, (x, y))| x != y).map(|(k, _)| k.name.as_str()).collect();
                    last = Some(format!("hook {} wrote {:#04x}", called.join(" / "), s.byte(addr)));
                }
                continue;
            }
        }
        s.m.cpu.step(&mut s.m.bus).map_err(|e| format!("{e} (pc={:#010x})", s.m.cpu.pc()))?;
        handled = None;
        if s.byte(addr) != was {
            last = Some(format!("{} stored {:#04x} (pc {pc:#010x})", function(&s.m.bus, pc), s.byte(addr)));
        }
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_hold_their_bytes_in_any_segment() {
        let m = Mask { name: "t", addr: 0x800C_8000, len: 0x10, why: "a test" };
        assert!(m.holds(0x800C_8000) && m.holds(0x000C_800F) && m.holds(0xA00C_8008));
        assert!(!m.holds(0x800C_8010) && !m.holds(0x800C_7FFF));
        let s = Mask { name: "s", addr: 0x1F80_0043, len: 1, why: "a test" };
        assert!(s.holds(0x1F80_0043) && !s.holds(0x1F80_0044));
    }

    /// @LT-105/4: masks carry a reason, and the report lists every mask with it.
    #[test]
    fn lt105_4_masks_have_reasons_and_the_report_lists_them() {
        assert!(RENDER_MASKS.iter().all(|m| m.why.len() > 20 && m.len > 0));
        let r = Report {
            scene: "x".into(),
            frames: 4,
            masks: RENDER_MASKS.iter().enumerate().map(|(i, m)| (m.name, i as u64, m.why)).collect(),
            ..Report::default()
        };
        let text = r.text();
        for (i, m) in RENDER_MASKS.iter().enumerate() {
            assert!(text.contains(&format!("mask \"{}\" used: up to {i} bytes", m.name)), "{text}");
            assert!(text.contains(m.why), "{text}");
        }
        assert!(Report::default().text().contains("masks: none"));
    }
}
