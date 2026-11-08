//! LT-108's checks: capture over a test-set scene, against `draw_hooks.rs`'s log, ids and cost.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use emulator_core::ButtonState;
use lt_emu_hooks::Hooks;
use lt_testset::{Image, Machine, Scene};
use serde_json::Value;

use crate::{hooks, Capture, Matrix, Record, DRAW_OBJECTS, DRAW_OBJECTS_SIZE, NPC_DRAW, NPC_DRAW_SIZE};

/// One frame with the capture's hooks.
pub fn frame(m: &mut Machine, h: &mut Hooks, cap: &mut Capture, pad: u16) -> Result<(), String> {
    cap.frame = m.frame;
    m.bus.set_port1_buttons(ButtonState::from_bits(pad));
    h.run_frame(&mut m.cpu, &mut m.bus, cap).map_err(|e| format!("frame {}: {e}", m.frame))?;
    m.bus.run_spu_to_current_cycle();
    m.bus.spu.discard_audio();
    m.frame += 1;
    Ok(())
}

/// Plays a scene's tape with the capture on; its records and the time the frames took.
pub fn capture_scene(image: &Arc<Image>, scene: &Scene, state: &[u8]) -> Result<(Vec<Record>, Duration), String> {
    let mut m = Machine::from_state(image, &scene.card, state, scene.frame)?;
    let mut h = hooks();
    h.install(&mut m.cpu);
    let mut cap = Capture::new();
    let t = Instant::now();
    for &pad in &scene.tape {
        frame(&mut m, &mut h, &mut cap, pad)?;
    }
    Ok((cap.take(), t.elapsed()))
}

/// The same tape without hooks: the time the frames took.
pub fn plain_scene(image: &Arc<Image>, scene: &Scene, state: &[u8]) -> Result<Duration, String> {
    let mut m = Machine::from_state(image, &scene.card, state, scene.frame)?;
    let t = Instant::now();
    for &pad in &scene.tape {
        m.step(pad)?;
    }
    Ok(t.elapsed())
}

/// `draw_hooks.rs`'s input: START pulses through the opening, then ✕ (compat_tour's tape).
pub fn draw_hooks_tape(f: u64) -> u16 {
    const START: u16 = 1 << 3;
    const CROSS: u16 = 1 << 14;
    if f % 120 >= 8 {
        0
    } else if f < 2300 {
        START
    } else {
        CROSS
    }
}

/// As `draw_hooks.rs` runs: power-on, `boot` frames of its tape, then `observe` frames of no
/// input, captured.
pub fn capture_from_boot(image: &Arc<Image>, boot: u64, observe: u64) -> Result<Vec<Record>, String> {
    let mut m = Machine::boot(image, &[])?;
    for f in 0..boot {
        m.step(draw_hooks_tape(f))?;
    }
    let mut h = hooks();
    h.install(&mut m.cpu);
    let mut cap = Capture::new();
    for _ in 0..observe {
        frame(&mut m, &mut h, &mut cap, 0)?;
    }
    Ok(cap.take())
}

/// The `draw_hooks.rs` arguments [`compare`] reads (after `<disc> <out> <boot> <observe>`).
pub const DRAW_HOOKS_ARGS: &[&str] = &[
    "TickStart=0x80177E48",
    "TickEnd=0x80177E50",
    "Gte_SetRotTransMatrix=0x801A1338:a0:0x20",
    "WorldChunk_DrawObjects=0x801E080C",
    "WorldChunk_DrawObjectMesh=0x801E090C",
    "Npc_DrawIfVisible=0x801E9E90",
    "Gte_SetComposedTransform=0x801A11CC:a1:0x20",
    "Model_DrawParts=0x8019DD70:a0:0x3C:a0@2c:0x1000",
    "Model_DrawParts2=0x8019DD68:a0:0x3C:a0@2c:0x1000",
    "Field_DrawTorchFlames=0x801EB4AC",
];

/// What a `draw_hooks.rs` log says about one tick.
#[derive(Debug, Default, PartialEq, Eq)]
struct Expected {
    frame: u64,
    view: Option<Matrix>,
    chunks: Vec<(u32, Vec<(u32, u32)>)>,
    /// (index, part record, world matrix, vertices).
    parts: Vec<(u16, u32, Matrix, Vec<[i16; 3]>)>,
    torch_pass: bool,
}

fn hex(v: &Value, k: &str) -> Vec<u8> {
    let s = v[k].as_str().unwrap_or("");
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()).collect()
}

fn word(v: &Value, k: &str) -> u32 {
    u32::from_str_radix(v[k].as_str().unwrap_or("0").trim_start_matches("0x"), 16).unwrap_or(0)
}

fn parse(log: &str) -> Result<Vec<Expected>, String> {
    let field_task = gc_sys::symbol("Field_ChunkStreamTask").ok_or("gc-sys has no Field_ChunkStreamTask")?;
    let within = |ra: u32, a: u32, n: u32| ra > a && ra <= a + n;
    let mut ticks = Vec::new();
    let mut cur: Option<Expected> = None;
    let (mut npc, mut world) = (None::<u16>, Matrix::default());
    for line in log.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("draw_hooks log: {e}"))?;
        let (hook, ra) = (v["hook"].as_str().unwrap_or(""), word(&v, "ra"));
        if hook == "TickStart" {
            cur = Some(Expected { frame: v["frame"].as_u64().unwrap_or(0), ..Expected::default() });
            npc = None;
            continue;
        }
        let Some(e) = cur.as_mut() else { continue };
        match hook {
            "TickEnd" => ticks.push(cur.take().unwrap()),
            "Gte_SetRotTransMatrix" if word(&v, "a0") == 0x1F80_0098 && within(ra, field_task.addr, field_task.size) && e.chunks.is_empty() => {
                e.view = Some(Matrix::from_bytes(&hex(&v, "a0_bytes")));
            }
            "WorldChunk_DrawObjects" => e.chunks.push((word(&v, "a0"), Vec::new())),
            "WorldChunk_DrawObjectMesh" if within(ra, DRAW_OBJECTS, DRAW_OBJECTS_SIZE) => {
                if let Some(c) = e.chunks.last_mut() {
                    c.1.push((word(&v, "a0"), word(&v, "a1")));
                }
            }
            "Npc_DrawIfVisible" => npc = Some(word(&v, "a1") as u16),
            "Gte_SetComposedTransform" if within(ra, NPC_DRAW, NPC_DRAW_SIZE) => world = Matrix::from_bytes(&hex(&v, "a1_bytes")),
            "Model_DrawParts" | "Model_DrawParts2" if within(ra, NPC_DRAW, NPC_DRAW_SIZE) => {
                let rec = hex(&v, "a0_bytes");
                let n = u16::from_le_bytes([rec[0x38], rec[0x39]]) as usize;
                let verts = hex(&v, "a0@2c_bytes");
                let vertices = verts.chunks_exact(8).take(n).map(|b| [0, 2, 4].map(|i| i16::from_le_bytes([b[i], b[i + 1]]))).collect();
                if let Some(i) = npc {
                    e.parts.push((i, word(&v, "a0"), world, vertices));
                }
            }
            "Field_DrawTorchFlames" => e.torch_pass = true,
            _ => {}
        }
    }
    Ok(ticks)
}

/// Records against a `draw_hooks.rs` log of the same frames ([`DRAW_HOOKS_ARGS`]): ticks
/// compared, and every difference.
pub fn compare(records: &[Record], log: &str) -> Result<(usize, Vec<String>), String> {
    let expected = parse(log)?;
    let mut diffs = Vec::new();
    if expected.len() != records.len() {
        diffs.push(format!("{} ticks in the log, {} records", expected.len(), records.len()));
    }
    for (e, r) in expected.iter().zip(records) {
        let t = r.tick;
        let mut say = |what: String| diffs.push(format!("tick {t} (frame {}): {what}", r.frame));
        if e.frame != r.frame {
            say(format!("starts at frame {} in the log", e.frame));
        }
        if e.view != r.camera.as_ref().map(|c| c.view) {
            say(format!("view {:?} in the log, {:?} captured", e.view, r.camera.as_ref().map(|c| c.view)));
        }
        let chunks: Vec<(u32, Vec<(u32, u32)>)> = r
            .chunks
            .iter()
            .map(|c| (0x8001_0000 + c.slot as u32 * 0x2_E000, c.groups.iter().map(|g| (g.mesh, g.object)).collect()))
            .collect();
        if e.chunks != chunks {
            say(format!("chunks/groups differ: log {:?}, captured {:?}", e.chunks.iter().map(|c| (c.0, c.1.len())).collect::<Vec<_>>(), chunks.iter().map(|c| (c.0, c.1.len())).collect::<Vec<_>>()));
        }
        let parts: Vec<(u16, u32, Matrix, Vec<[i16; 3]>)> =
            r.characters.iter().flat_map(|c| c.parts.iter().map(move |p| (c.index, p.record, p.world, p.vertices.clone()))).collect();
        let (mut a, mut b) = (e.parts.clone(), parts);
        a.sort_by_key(|p| (p.0, p.1));
        b.sort_by_key(|p| (p.0, p.1));
        if a.len() != b.len() {
            say(format!("{} character parts in the log, {} captured", a.len(), b.len()));
        }
        for (x, y) in a.iter().zip(&b) {
            let part = format!("character {} part {:#010x}", y.0, y.1);
            if (x.0, x.1) != (y.0, y.1) {
                say(format!("character {} part {:#010x} in the log, {part} captured", x.0, x.1));
            } else if x.2 != y.2 {
                say(format!("{part}: world {:?} in the log, {:?} captured", x.2, y.2));
            } else if x.3 != y.3 {
                let k = x.3.iter().zip(&y.3).position(|(u, v)| u != v).unwrap_or(x.3.len().min(y.3.len()));
                say(format!("{part}: vertex {k} is {:?} in the log, {:?} captured ({} and {} vertices)", x.3.get(k), y.3.get(k), x.3.len(), y.3.len()));
            }
        }
        if e.torch_pass != r.torch_pass {
            say(format!("torch pass {} in the log", e.torch_pass));
        }
    }
    Ok((expected.len().min(records.len()), diffs))
}

/// LT-108 AC1 over records: each has the camera, `chunks` chunk draws (or at least one) with
/// groups, and every drawn character with a matrix and vertices. Returns the problems.
pub fn completeness(records: &[Record], chunks: Option<usize>) -> Vec<String> {
    let mut p = Vec::new();
    for r in records {
        let mut say = |what: &str| p.push(format!("tick {} (frame {}): {what}", r.tick, r.frame));
        if r.camera.is_none() {
            say("no camera");
        }
        if chunks.map_or(r.chunks.is_empty(), |n| r.chunks.len() != n) {
            say(&format!("{} chunk draws", r.chunks.len()));
        }
        if r.chunks.iter().all(|c| c.groups.is_empty()) {
            say("no groups drawn");
        }
        if r.characters.is_empty() {
            say("no characters");
        }
        for c in &r.characters {
            if c.parts.iter().any(|q| q.vertices.is_empty() || q.world == Matrix::default()) {
                say(&format!("character {} without vertices or a world matrix", c.index));
            }
        }
    }
    p
}

/// LT-108 AC3: every id means one thing across all ticks (a group id one chunk and mesh; a
/// character index one record; a flame id one position), and how often ids carry over from one tick
/// to the next. Returns (problems, groups seen again / groups, characters seen again / characters).
pub fn stability(records: &[Record]) -> (Vec<String>, (usize, usize), (usize, usize)) {
    let mut p = Vec::new();
    let mut groups: HashMap<u64, (u16, u32)> = HashMap::new();
    let mut chars: HashMap<u16, u32> = HashMap::new();
    let mut flames: HashMap<u32, [i16; 3]> = HashMap::new();
    let (mut g_again, mut g_all, mut c_again, mut c_all) = (0, 0, 0, 0);
    let mut last_groups: Vec<u64> = Vec::new();
    let mut last_chars: Vec<u16> = Vec::new();
    for r in records {
        let mut these = Vec::new();
        for c in &r.chunks {
            for g in &c.groups {
                let base = 0x8001_0000 + c.slot as u32 * 0x2_E000;
                let what = (c.chunk, g.mesh - base);
                if *groups.entry(g.id).or_insert(what) != what {
                    p.push(format!("tick {}: group id {:#x} names {:?}, earlier {:?}", r.tick, g.id, what, groups[&g.id]));
                }
                g_all += 1;
                g_again += last_groups.contains(&g.id) as usize;
                these.push(g.id);
            }
        }
        last_groups = these;
        for c in &r.characters {
            if *chars.entry(c.index).or_insert(c.record) != c.record {
                p.push(format!("tick {}: character {} at record {:#x}, earlier {:#x}", r.tick, c.index, c.record, chars[&c.index]));
            }
            c_all += 1;
            c_again += last_chars.contains(&c.index) as usize;
        }
        last_chars = r.characters.iter().map(|c| c.index).collect();
        for f in &r.flames {
            if *flames.entry(f.id).or_insert(f.position) != f.position {
                p.push(format!("tick {}: flame {:#x} moved", r.tick, f.id));
            }
        }
    }
    (p, (g_again, g_all), (c_again, c_all))
}
