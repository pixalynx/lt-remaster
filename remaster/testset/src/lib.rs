//! Living Tool Remaster's test set (LT-106): fixed starting points in Guardian's Crusade and input
//! recordings from them, which every remaster check runs on (lockstep, render matching, cost).
//!
//! A scene is a machine state (PSoXide's own serde layout, postcard, as `emu-host` saves states:
//! not the disc, not the memory card) plus a tape of pads, one per frame, and the digest of the
//! whole machine after the tape plays. States hold disc data, so the set lives outside git: the
//! `lt-testset` program builds it from the player's disc and memory card into a folder.
//!
//! ```text
//! <set>/manifest.txt            the core, the disc's EXE and the card it was built from
//! <set>/<scene>/state.bin       the machine at the scene's start
//! <set>/<scene>/card.bin        the memory card then (states leave the card out)
//! <set>/<scene>/tape.bin        u16 pads, little-endian, one per frame
//! <set>/<scene>/info.txt        what it is, where it starts, the digest after the tape
//! ```

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use emulator_core::snapshot::{EmulatorState, EmulatorStateRef};
use emulator_core::{fast_boot_disc, Bus, ButtonState, Cpu, Deinterlace};
use psx_iso::Disc;

/// The core build states belong to (the fork commit `Cargo.toml` pins).
pub const CORE: &str = "psoxide-b27fff9";
/// Instructions one frame may run before the core is called stuck.
const STEPS_PER_FRAME_CAP: u64 = 1_000_000;
const RAW_SECTOR: usize = 2352;

/// The field context in the scratchpad and the chunk slot table (MAP_LOADING 1.3).
pub const CTX: u32 = 0x1F80_0000;
pub const SLOTS: u32 = 0x801C_E800;
/// `Character_Struct[0]`, the Knight: s16 x, h, z.
pub const KNIGHT: u32 = 0x8014_6600;
/// `g_EventFlags`: flag n is bit n % 32 of word n / 32.
pub const EVENT_FLAGS: u32 = 0x801C_E810;
pub const MODE_BATTLE: u8 = 2;

/// The PS1 buttons.
pub mod pad {
    pub const START: u16 = 1 << 3;
    pub const UP: u16 = 1 << 4;
    pub const RIGHT: u16 = 1 << 5;
    pub const DOWN: u16 = 1 << 6;
    pub const LEFT: u16 = 1 << 7;
    pub const TRIANGLE: u16 = 1 << 12;
    pub const CROSS: u16 = 1 << 14;
}

/// A read-only raw disc image (2352-byte sectors), read where the core asks.
pub struct Image {
    file: File,
    sectors: u32,
}

impl Image {
    pub fn open(path: &Path) -> Result<Arc<Self>, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        if size == 0 || size % RAW_SECTOR as u64 != 0 {
            return Err(format!("{} isn't a raw disc image", path.display()));
        }
        Ok(Arc::new(Image { file, sectors: (size / RAW_SECTOR as u64) as u32 }))
    }
}

struct Source(Arc<Image>);

impl psx_iso::TrackSource for Source {
    fn len(&self) -> u64 {
        self.0.sectors as u64 * RAW_SECTOR as u64
    }

    fn read_at(&self, offset: u64, out: &mut [u8]) -> bool {
        if offset + out.len() as u64 > self.len() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.0.file.read_exact_at(out, offset).is_ok()
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let (mut off, mut buf) = (offset, &mut out[..]);
            while !buf.is_empty() {
                match self.0.file.seek_read(buf, off) {
                    Ok(0) | Err(_) => return false,
                    Ok(n) => {
                        off += n as u64;
                        buf = &mut buf[n..];
                    }
                }
            }
            true
        }
    }
}

/// The field's state: the current chunk, the four chunk slots, a warp waiting (ctx+0x51 bit 7), a
/// transition (ctx+0xDF bit 7) and the game mode (ctx+0x43).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub cur: u16,
    pub ids: [u16; 4],
    pub loaded: [u8; 4],
    pub warp: u8,
    pub transition: u8,
    pub mode: u8,
}

impl Field {
    pub fn idle(&self) -> bool {
        self.warp < 0x80 && self.transition < 0x80 && self.ids.iter().zip(self.loaded).all(|(&id, ld)| id == 0xFFFF || ld != 0)
    }

    /// Idle on a loaded field chunk, not in battle.
    pub fn on_field(&self) -> bool {
        let k = (self.cur & 3) as usize;
        lt_core::world::valid_chunk(self.cur as u32) && self.ids[k] == self.cur && self.loaded[k] != 0 && self.mode != MODE_BATTLE && self.idle()
    }
}

/// The emulated machine, run a frame at a time as the host program runs it.
pub struct Machine {
    pub cpu: Cpu,
    pub bus: Bus,
    pub frame: u64,
    image: Arc<Image>,
    card: Vec<u8>,
}

impl Machine {
    /// Powers on with `card` in slot 1 (empty for a fresh card).
    pub fn boot(image: &Arc<Image>, card: &[u8]) -> Result<Self, String> {
        let disc = Disc::from_source(Box::new(Source(image.clone())));
        let mut bus = Bus::new_without_bios();
        let mut cpu = Cpu::new();
        fast_boot_disc(&mut bus, &mut cpu, &disc).map_err(|e| format!("the disc didn't boot: {e:?}"))?;
        bus.cdrom.insert_disc(Some(disc));
        bus.attach_digital_pad_port1();
        bus.attach_memcard_port1(card.to_vec());
        Ok(Machine { cpu, bus, frame: 0, image: image.clone(), card: card.to_vec() })
    }

    /// One frame (to the next VBlank) with `pad` held. Sound is made and dropped, as when muted.
    pub fn step(&mut self, pad: u16) -> Result<(), String> {
        self.bus.set_port1_buttons(ButtonState::from_bits(pad));
        let start = self.bus.irq().raise_counts()[0];
        let (_, r) = self.cpu.run(&mut self.bus, STEPS_PER_FRAME_CAP, u64::MAX, |b| b.irq().raise_counts()[0] != start);
        self.bus.run_spu_to_current_cycle();
        self.bus.spu.discard_audio();
        r.map_err(|e| format!("frame {}: {e} (pc={:#010x})", self.frame, self.cpu.pc()))?;
        self.frame += 1;
        Ok(())
    }

    /// `n` frames, pad `pad(i)` on the i-th; returns the pads it held.
    pub fn run(&mut self, n: u64, mut pad: impl FnMut(u64) -> u16) -> Result<Vec<u16>, String> {
        let mut tape = Vec::with_capacity(n as usize);
        for i in 0..n {
            let p = pad(i);
            self.step(p)?;
            tape.push(p);
        }
        Ok(tape)
    }

    /// Up to `max` frames until `done` holds after one; the frame it held on.
    pub fn until(&mut self, max: u64, mut pad: impl FnMut(u64) -> u16, mut done: impl FnMut(&Machine) -> bool) -> Result<Option<u64>, String> {
        for i in 0..max {
            self.step(pad(i))?;
            if done(self) {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    pub fn read(&self, addr: u32, len: usize) -> Vec<u8> {
        let phys = addr & 0x1FFF_FFFF;
        let ram = self.bus.ram();
        if (phys as usize) < 0x80_0000 {
            let at = phys as usize % ram.len();
            if at + len <= ram.len() {
                return ram[at..at + len].to_vec();
            }
        }
        (0..len as u32).map(|i| self.bus.try_read8(addr.wrapping_add(i)).unwrap_or(0)).collect()
    }

    pub fn write(&mut self, addr: u32, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            self.bus.write8_safe(addr + i as u32, b);
        }
    }

    /// Sets story flag `n` (the scenes that need a story state plant it, LT-142).
    pub fn set_flag(&mut self, n: u16) {
        let at = EVENT_FLAGS + (n as u32 / 32) * 4;
        let word = u32::from_le_bytes(self.read(at, 4).try_into().unwrap()) | 1 << (n % 32);
        self.write(at, &word.to_le_bytes());
    }

    /// Clears story flag `n`.
    pub fn clear_flag(&mut self, n: u16) {
        let at = EVENT_FLAGS + (n as u32 / 32) * 4;
        let word = u32::from_le_bytes(self.read(at, 4).try_into().unwrap()) & !(1 << (n % 32));
        self.write(at, &word.to_le_bytes());
    }

    pub fn field(&self) -> Field {
        let ctx = self.read(CTX, 0x100);
        let s = self.read(SLOTS, 12);
        let id = |i: usize| u16::from_le_bytes([s[i * 2], s[i * 2 + 1]]);
        Field {
            cur: u16::from_le_bytes([ctx[0xE8], ctx[0xE9]]),
            ids: [id(0), id(1), id(2), id(3)],
            loaded: [s[8], s[9], s[10], s[11]],
            warp: ctx[0x51],
            transition: ctx[0xDF],
            mode: ctx[0x43],
        }
    }

    pub fn display_size(&self) -> (u32, u32) {
        let (_, w, h) = self.bus.gpu.display_rgba8_with(Deinterlace::Weave);
        (w, h)
    }

    /// The machine as bytes: the core's own serde layout, without the disc and the card.
    pub fn save_state(&self) -> Result<Vec<u8>, String> {
        postcard::to_allocvec(&EmulatorStateRef { cpu: &self.cpu, bus: &self.bus }).map_err(|e| e.to_string())
    }

    /// A machine at `state`, with this disc and the card this machine was booted with.
    pub fn from_state(image: &Arc<Image>, card: &[u8], state: &[u8], frame: u64) -> Result<Self, String> {
        let mut m = Machine::boot(image, card)?;
        let mut st: EmulatorState = postcard::from_bytes(state).map_err(|e| format!("the state doesn't read: {e}"))?;
        st.bus.restore_excluded_from(&mut m.bus);
        st.bus.attach_memcard_port1(m.card.clone());
        m.cpu = st.cpu;
        m.bus = st.bus;
        m.frame = frame;
        Ok(m)
    }

    /// The whole machine as one number: main RAM, the scratchpad, VRAM, the CPU's registers and PC,
    /// and the bus clock. Two runs that end with the same digest ended in the same state.
    pub fn digest(&self) -> u64 {
        let mut h = Fnv::default();
        h.eat(self.bus.ram());
        h.eat(&self.read(CTX, 0x400));
        let (rgba, w, ht) = self.bus.gpu.display_rgba8_with(Deinterlace::Weave);
        h.eat(&rgba);
        h.eat(&[w as u8, ht as u8]);
        for r in self.cpu.gprs() {
            h.eat(&r.to_le_bytes());
        }
        h.eat(&self.cpu.pc().to_le_bytes());
        h.eat(&self.bus.cycles().to_le_bytes());
        h.0
    }

    pub fn image(&self) -> &Arc<Image> {
        &self.image
    }

    /// The memory card as the game has left it.
    pub fn card_now(&mut self) -> Vec<u8> {
        self.bus.memcard_port1_snapshot().unwrap_or_else(|| self.card.clone())
    }
}

#[derive(Clone, Copy)]
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn eat(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ u64::from(x)).wrapping_mul(0x100_0000_01b3);
        }
    }
}

/// One scene of the set, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scene {
    pub name: String,
    pub about: String,
    /// The frame since power-on the state was taken at.
    pub frame: u64,
    pub chunk: u16,
    pub mode: u8,
    /// The memory card at the state (empty: none was ever written).
    pub card: Vec<u8>,
    pub tape: Vec<u16>,
    /// The machine's digest after the tape plays from the state.
    pub digest: u64,
}

impl Scene {
    pub fn dir(set: &Path, name: &str) -> PathBuf {
        set.join(name)
    }

    pub fn save(&self, set: &Path, state: &[u8]) -> Result<(), String> {
        let dir = Self::dir(set, &self.name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("state.bin"), state).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("card.bin"), &self.card).map_err(|e| e.to_string())?;
        let tape: Vec<u8> = self.tape.iter().flat_map(|p| p.to_le_bytes()).collect();
        std::fs::write(dir.join("tape.bin"), tape).map_err(|e| e.to_string())?;
        let info = format!(
            "name {}\nabout {}\nframe {}\nchunk {}\nmode {}\ncard {}\nframes {}\nticks {}\ndigest {:016x}\n",
            self.name,
            self.about,
            self.frame,
            self.chunk,
            self.mode,
            if self.card.is_empty() { "none".to_string() } else { sha1(&self.card) },
            self.tape.len(),
            self.tape.len() / 2,
            self.digest
        );
        std::fs::write(dir.join("info.txt"), info).map_err(|e| e.to_string())
    }

    /// A stored scene and its state's bytes.
    pub fn load(set: &Path, name: &str) -> Result<(Scene, Vec<u8>), String> {
        let dir = Self::dir(set, name);
        let info = std::fs::read_to_string(dir.join("info.txt")).map_err(|e| format!("{name}: {e}"))?;
        let get = |k: &str| info.lines().find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix(' '))).unwrap_or("").to_string();
        let tape = std::fs::read(dir.join("tape.bin")).map_err(|e| e.to_string())?.as_chunks::<2>().0.iter().map(|&b| u16::from_le_bytes(b)).collect();
        let state = std::fs::read(dir.join("state.bin")).map_err(|e| e.to_string())?;
        let scene = Scene {
            name: get("name"),
            about: get("about"),
            frame: get("frame").parse().unwrap_or(0),
            chunk: get("chunk").parse().unwrap_or(0),
            mode: get("mode").parse().unwrap_or(0),
            card: std::fs::read(dir.join("card.bin")).unwrap_or_default(),
            tape,
            digest: u64::from_str_radix(&get("digest"), 16).unwrap_or(0),
        };
        Ok((scene, state))
    }

    /// Plays the tape from the state on a fresh machine; the digest at the end.
    pub fn replay(&self, image: &Arc<Image>, state: &[u8]) -> Result<u64, String> {
        let mut m = Machine::from_state(image, &self.card, state, self.frame)?;
        for &p in &self.tape {
            m.step(p)?;
        }
        Ok(m.digest())
    }
}

/// The scenes in the set, in build order.
pub const SCENES: [&str; 20] = [
    "boot", "cutscene", "village", "menu", "overworld", "battle-1", "interior", "dungeon", "late", "flight", "battle-late", "cave", "tower", "lamps", "door", "gift", "zeppetto",
    "first-flight", "water", "battle-start",
];

/// SHA-1 of a file, for the manifest.
pub fn sha1(bytes: &[u8]) -> String {
    sha1_smol::Sha1::from(bytes).digest().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scene_saves_and_loads() {
        let dir = std::env::temp_dir().join(format!("lt-testset-{}", std::process::id()));
        let s = Scene { name: "x".into(), about: "a test".into(), frame: 42, chunk: 78, mode: 0, card: vec![1, 2, 3], tape: vec![0, 1 << 14, 0], digest: 0xDEAD_BEEF };
        s.save(&dir, b"state").unwrap();
        let (back, state) = Scene::load(&dir, "x").unwrap();
        assert_eq!((back, state), (s, b"state".to_vec()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn idle_needs_every_named_slot_loaded() {
        let f = Field { cur: 78, ids: [0xFFFF, 77, 78, 79], loaded: [0, 1, 1, 1], warp: 0, transition: 0, mode: 0 };
        assert!(f.idle() && f.on_field());
        assert!(!Field { loaded: [0, 1, 0, 1], ..f }.idle());
        assert!(!Field { warp: 0x80, ..f }.idle());
        assert!(!Field { mode: MODE_BATTLE, ..f }.on_field());
    }
}
