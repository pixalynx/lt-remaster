//! The emulator thread: Guardian's Crusade's own code on the patched PSoXide core, in real time.
//!
//! Frames are paced by the emulated clock, as the host program does (`emu-host/host/src/runner.rs`):
//! the next frame is due when the emulated time this one covered has passed (PSoXide's NTSC field
//! is 59.29 Hz, never a fixed 59.94). Sound goes out from this thread, which stays the clock.
//! The engine's draw functions are watched by observe hooks (`lt-emu-hooks`); the counts show in
//! the debug overlay.
//!
//! **Two ways to run.** On its own (`lt-remaster <disc>`) this thread boots the disc and runs it
//! ([`spawn`]). With `--host` (LT-117) it serves Living Tool's Play screen instead ([`serve`]):
//! the host program's protocol and runner (`lt_emu_host`) with this program's frame ([`Draws`]) as
//! its stepper, so the Play screen opens its project's disc here (its build's sectors laid over
//! the player's image), loads the machine state it was playing, and keeps syncing, warping and
//! reading RAM as it does with `lt-emu-host`. Either way a frame is [`Draws::frame`].
//!
//! **The native view (LT-111).** The native view's hooks (`lt_native::play::Player`: the scene
//! capture and the takeover) come first in the hook table. Each tick the game draws comes out of a
//! frame with its ordering table; the emulator thread hands it, with a copy of RAM, to the native
//! thread, which resolves it to assets and composites it at `--scale` × (terrain and characters
//! native, the 2D layer emulated), lit (LT-112) or in the game's original look. It composites on the
//! window's own GPU device and the window draws the result from there: nothing is read back. At
//! `--fps` 60 or 120 it draws 2 or 4 frames a tick, blending the last two ticks (LT-113), one tick
//! behind. The emulator never waits: when the native thread is still busy the tick is dropped, and
//! counted. A dropped tick's VRAM copies (a frame freeze's, LT-140, and the battle transition's) are
//! carried on to the next tick that gets through, so the native view still takes its picture of the
//! frame the game copied. At a wider aspect or a longer distance (LT-114, [`Shared::reach`]) it reads the
//! neighbour chunks that needs as each tick is resolved, and draws past the 4:3 edges.
//!
//! **The disc's assets.** The native thread reads its assets off the disc the core reads
//! ([`Shared::disc`]: the image with the project's sectors over it), and reads them again whenever
//! that disc or its patches change (a Sync on the Play screen): an edited chunk or actor shows
//! natively as soon as the game reads it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use emulator_core::{fast_boot_disc, Bus, ButtonState, Cpu, Deinterlace};
use lt_emu_hooks::{Action, Handler, Hook, Hooks};
use lt_emu_host::audio::{AudioOut, Control};
use lt_emu_host::disc::{Image, Overlay, Source};
use lt_native::light::Lights;
use lt_native::play::{Drawn, Player};
use psx_iso::Disc;

/// The CPU clock, for emulated time.
pub const CPU_HZ: f64 = 33_868_800.0;
/// A frame's emulated time is clamped to this range when pacing (a mode switch can make one odd).
const FRAME_MIN: Duration = Duration::from_micros(8_000);
const FRAME_MAX: Duration = Duration::from_micros(40_000);
/// A frame that starts more than one field after it was due is late.
const LATE: Duration = Duration::from_micros(16_900);

/// The engine functions the overlay counts (research 16 §4.3). OVR02's are guarded by its words,
/// so OVR04 (battle) in the same slot never trips them.
fn watched() -> Vec<Hook> {
    vec![
        Hook::observe("terrain groups", 0x801E_090C).guarded(0xACF6_0204),
        Hook::observe("characters", 0x801E_9E90).guarded(0x27BD_FFB8),
        Hook::observe("models", 0x8019_DD70),
    ]
}

/// Pictures the window may fall behind by before the oldest is dropped.
pub const QUEUE: usize = 6;

/// One picture of the emulated display.
pub struct Picture {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Frames since boot.
    pub n: u64,
}

/// A picture of the native view.
pub enum NativeFrame {
    /// Composited on the window's GPU: the texture (`w` × `h`) and where it sits in the emulated
    /// picture's frame (`canvas`, shifted by the screen position `at`), in native pixels.
    Gpu { texture: wgpu::Texture, w: u32, h: u32, canvas: [u32; 2], at: [i32; 2] },
    /// A movie: the displayed picture enlarged, framed already.
    Pixels(Picture),
}

/// Over the last second.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub frames: u64,
    pub fps: f64,
    pub field_hz: f64,
    pub emu_ms: f64,
    pub emu_ms_max: f64,
    /// Frames that started more than a frame late since boot (on its own; `--host` paces in the
    /// host program's runner, which doesn't count them).
    pub late: u64,
    pub audio_underruns: u64,
    pub audio_queued_ms: f64,
    pub audio_device: String,
    pub hooks: Vec<(String, u64)>,
    /// The native view: ticks drawn and frames drawn (several a tick between ticks, LT-113), the
    /// last second's frames a second, its mean and longest time a frame (resolve and composite),
    /// and ticks dropped because the native thread was busy.
    pub native_ticks: u64,
    pub native_frames: u64,
    pub native_fps: f64,
    pub native_ms: f64,
    pub native_ms_max: f64,
    pub native_dropped: u64,
    pub native_error: Option<String>,
    /// The disc's assets: how many times they were read (once, then after each change to the
    /// disc or its patches), how long the last read took, and the chunk the last tick stood on
    /// with an FNV-1a of its bytes as the native view read them (a Sync's edit shows here).
    pub assets_loads: u64,
    pub assets_ms: f64,
    pub native_chunk: Option<(u16, u64)>,
    /// The characters the last native tick drew: the game's (index and position), and those it
    /// posed from their records (LT-119: index).
    pub native_characters: Vec<(u16, [i16; 3])>,
    pub native_crowd: Vec<u16>,
    /// The field context's chunk (ctx+0xE8) and mode (ctx+0x43).
    pub chunk: u16,
    pub mode: u8,
    pub halted: Option<String>,
}

/// What the window and the emulator thread share.
pub struct Shared {
    /// The pad held now (PS1 button bits), from the window's keys and (on its own) controllers.
    pub pad: AtomicU16,
    /// Pictures not shown yet, oldest first. The window takes one per refresh, so a picture is only
    /// lost when it falls behind by [`QUEUE`] (the Play screen's lesson: 59.29 Hz frames against a
    /// 60 Hz display wobble by about a millisecond, and "show the newest" drops them).
    pub pictures: Mutex<VecDeque<Arc<Picture>>>,
    /// The window shows the emulated picture beside the native view (`--view both`): only then
    /// is it copied out each frame.
    pub show_picture: AtomicBool,
    /// The native view's newest picture, until the window takes it.
    pub native: Mutex<Option<NativeFrame>>,
    /// The window's GPU, once it has one: the native view is composited there and shown from
    /// there, never read back.
    pub gpu: OnceLock<(wgpu::Device, wgpu::Queue)>,
    /// The native view lit (LT-112), else the game's original look (the L key, `--look`).
    pub lit: AtomicBool,
    /// The parts of the lighting on (the toolbar's Lights).
    pub lights: Mutex<Lights>,
    /// The native view's frames a second: 30, 60 or 120 (`--fps`, the toolbar).
    pub fps: AtomicU32,
    /// How much more than the game's 4:3 the native view shows (LT-114: the A and D keys,
    /// `--aspect`, `--distance`; the window's shape with `--aspect window`).
    pub reach: Mutex<lt_native::wide::Reach>,
    pub stats: Mutex<Stats>,
    pub stop: AtomicBool,
    /// Volume and mute (the M key, `--mute`).
    pub sound: Arc<Control>,
    /// The disc the core reads, once there is one, and a count that goes up whenever it or the
    /// sectors laid over it change: the native view then reads its assets again.
    pub disc: Mutex<Option<Arc<Overlay>>>,
    pub disc_changes: AtomicU64,
    /// `--host`: the protocol's handle for events the window sends (the player closed it), a
    /// request to show the window ([`lt_emu_protocol::Request::Focus`]), and the protocol having
    /// ended (Shutdown, or Living Tool closed the pipe).
    pub events: Mutex<Option<lt_emu_host::Out>>,
    pub focus: AtomicBool,
    pub ended: AtomicBool,
}

impl Shared {
    pub fn new(muted: bool) -> Arc<Self> {
        Arc::new(Shared {
            pad: AtomicU16::new(0),
            pictures: Mutex::new(VecDeque::new()),
            show_picture: AtomicBool::new(true),
            native: Mutex::new(None),
            gpu: OnceLock::new(),
            lit: AtomicBool::new(true),
            lights: Mutex::new(Lights::ALL),
            fps: AtomicU32::new(60),
            reach: Mutex::new(lt_native::wide::Reach::GAME),
            stats: Mutex::new(Stats::default()),
            stop: AtomicBool::new(false),
            sound: Control::new(1.0, muted),
            disc: Mutex::new(None),
            disc_changes: AtomicU64::new(0),
            events: Mutex::new(None),
            focus: AtomicBool::new(false),
            ended: AtomicBool::new(false),
        })
    }

    /// The core reads `disc` from now on.
    fn set_disc(&self, disc: Arc<Overlay>) {
        *self.disc.lock().unwrap() = Some(disc);
        self.disc_changes.fetch_add(1, Ordering::SeqCst);
    }
}

/// research 16's New Game tape: START pulses through the opening, then ✕ through the title, the
/// name and the bedroom, to the village (about frame 3,640).
pub fn new_game_tape(frame: u64) -> u16 {
    const START: u16 = 1 << 3;
    const CROSS: u16 = 1 << 14;
    if frame > 4400 || frame % 120 >= 8 {
        0
    } else if frame < 2300 {
        START
    } else {
        CROSS
    }
}

/// A drawn tick on its way to the native thread, with main RAM and the scratchpad as the frame
/// left them.
struct ToDraw {
    drawn: Drawn,
    ram: Vec<u8>,
    scratch: Vec<u8>,
}

const RAM: usize = 0x20_0000;
const SCRATCH: u32 = 0x1F80_0000;

/// One frame of the game with the native view's hooks: the pad held, the core run to the next
/// VBlank, the ticks it drew handed to the native thread, and the picture to the window when it
/// shows one.
pub struct Draws {
    player: Player,
    hooks: Hooks,
    /// The native view's hooks are the table's first `first`.
    first: usize,
    tx: SyncSender<ToDraw>,
    shared: Arc<Shared>,
    new_game: bool,
    /// The VRAM copies of ticks dropped since the last that got through: a frame freeze's (LT-140)
    /// and the battle transition's (LT-116).
    carried: (Vec<lt_native::takeover::ImageMove>, Vec<lt_native::takeover::ImageMove>),
}

impl Draws {
    fn new(shared: Arc<Shared>, tx: SyncSender<ToDraw>, new_game: bool) -> Result<Self, String> {
        // The native view's hooks first (their handler takes the table's first indices), then the
        // overlay's.
        let first = Player::hooks().len();
        let mut table = Player::hooks();
        table.extend(watched());
        Ok(Draws { player: Player::default(), hooks: Hooks::new(table)?, first, tx, shared, new_game, carried: Default::default() })
    }

    /// Sets the hooks on `cpu` (a new CPU, after a boot or a state, has none).
    fn install(&self, cpu: &mut Cpu) {
        self.hooks.install(cpu);
    }

    /// Runs frame `frame` (frames run before it) with `pad` held, the window's keys on top.
    fn frame(&mut self, cpu: &mut Cpu, bus: &mut Bus, frame: u64, pad: u16) -> Result<(), String> {
        let pad = pad | self.shared.pad.load(Ordering::Relaxed) | if self.new_game { new_game_tape(frame) } else { 0 };
        bus.set_port1_buttons(ButtonState::from_bits(pad));
        self.player.begin(frame);
        let (first, player) = (self.first, &mut self.player);
        self.hooks.run_frame(cpu, bus, &mut |c: &lt_emu_hooks::Call| if c.hook < first { player.call(c) } else { Action::Continue })?;
        for mut drawn in self.player.end() {
            // The copies of ticks dropped before it: a newer freeze stands for an older one; the
            // transition's moves go in order.
            let (frozen, moves) = std::mem::take(&mut self.carried);
            if drawn.table.frozen.is_empty() {
                drawn.table.frozen = frozen;
            }
            if !moves.is_empty() {
                drawn.table.moves = moves.into_iter().chain(std::mem::take(&mut drawn.table.moves)).collect();
            }
            match self.tx.try_send(ToDraw { drawn, ram: bus.ram()[..RAM].to_vec(), scratch: lt_emu_hooks::read(bus, SCRATCH, 0x400) }) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => {}
                Err(TrySendError::Full(t)) => {
                    self.shared.stats.lock().unwrap().native_dropped += 1;
                    self.carried = (t.drawn.table.frozen, t.drawn.table.moves);
                }
            }
        }
        if self.shared.show_picture.load(Ordering::Relaxed) {
            let (rgba, width, height) = bus.gpu.display_rgba8_with(Deinterlace::Weave);
            // Before the game sets a video mode the display is empty.
            if width > 0 && height > 0 {
                let mut q = self.shared.pictures.lock().unwrap();
                if q.len() >= QUEUE {
                    q.pop_front();
                }
                q.push_back(Arc::new(Picture { rgba, width, height, n: frame + 1 }));
            }
        }
        Ok(())
    }

    /// The overlay's hook counts.
    fn counts(&self) -> Vec<(String, u64)> {
        self.hooks.hooks().iter().zip(&self.hooks.counts.calls).skip(self.first).map(|(h, &n)| (h.name.clone(), n)).collect()
    }
}

/// The field context's chunk and mode, for the overlay.
fn field(bus: &Bus) -> (u16, u8) {
    let ctx = lt_emu_hooks::read(bus, SCRATCH, 0x100);
    (u16::from_le_bytes([ctx[0xE8], ctx[0xE9]]), ctx[0x43])
}

/// Runs the disc at `path` on its own, with the native view.
pub fn spawn(path: PathBuf, shared: Arc<Shared>, new_game: bool, scale: u32) -> std::thread::JoinHandle<()> {
    let tx = native_thread(shared.clone(), scale);
    std::thread::Builder::new()
        .name("lt-remaster-emu".into())
        .spawn(move || {
            lt_emu_host::thread::interactive();
            if let Err(e) = run(&path, &shared, new_game, tx) {
                shared.stats.lock().unwrap().halted = Some(e);
            }
        })
        .expect("emulator thread")
}

/// `--host`: serves the host program's protocol over stdin and `protocol` (the process's own
/// stdout, taken first thing in `main`) until Living Tool shuts it down or goes away; then
/// [`Shared::ended`] is set.
pub fn serve(protocol: std::fs::File, shared: Arc<Shared>, scale: u32) -> std::thread::JoinHandle<()> {
    let tx = native_thread(shared.clone(), scale);
    std::thread::Builder::new()
        .name("lt-remaster-emu".into())
        .spawn(move || {
            match Draws::new(shared.clone(), tx, false) {
                Ok(draws) => {
                    let stepper = HostStepper { draws, shared: shared.clone(), disc: None, window: (Instant::now(), 0, 0.0, 0.0, 0.0) };
                    let events = shared.clone();
                    lt_emu_host::serve(protocol, Box::new(stepper), move |out| *events.events.lock().unwrap() = Some(out));
                }
                Err(e) => shared.stats.lock().unwrap().halted = Some(e),
            }
            shared.ended.store(true, Ordering::SeqCst);
        })
        .expect("emulator thread")
}

/// The host program's runner runs frames with this: [`Draws::frame`], the overlay's numbers, and
/// the disc for the native view.
struct HostStepper {
    draws: Draws,
    shared: Arc<Shared>,
    disc: Option<Arc<Overlay>>,
    /// Since: frames, core ms, its longest, emulated seconds.
    window: (Instant, u64, f64, f64, f64),
}

impl lt_emu_host::Stepper for HostStepper {
    fn frame(&mut self, cpu: &mut Cpu, bus: &mut Bus, frame: u64, pad: u16) -> Result<(), String> {
        let (t0, c0) = (Instant::now(), bus.cycles());
        let r = self.draws.frame(cpu, bus, frame, pad);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let w = &mut self.window;
        w.1 += 1;
        w.2 += ms;
        w.3 = w.3.max(ms);
        w.4 += bus.cycles().saturating_sub(c0) as f64 / CPU_HZ;
        let span = w.0.elapsed().as_secs_f64();
        if span >= 1.0 {
            let mut st = self.shared.stats.lock().unwrap();
            st.frames = frame + 1;
            st.fps = w.1 as f64 / span;
            st.field_hz = w.1 as f64 / w.4.max(1e-9);
            st.emu_ms = w.2 / w.1 as f64;
            st.emu_ms_max = w.3;
            st.hooks = self.draws.counts();
            (st.chunk, st.mode) = field(bus);
            *w = (Instant::now(), 0, 0.0, 0.0, 0.0);
        }
        r
    }

    fn attach(&mut self, cpu: &mut Cpu, disc: &Arc<Overlay>) {
        self.draws.install(cpu);
        if !self.disc.as_ref().is_some_and(|d| Arc::ptr_eq(d, disc)) {
            self.disc = Some(disc.clone());
            self.shared.set_disc(disc.clone());
        }
    }

    fn patched(&mut self) {
        self.shared.disc_changes.fetch_add(1, Ordering::SeqCst);
    }

    fn focus(&mut self) -> Result<(), String> {
        self.shared.focus.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn program(&self) -> String {
        format!("lt-remaster {}", env!("CARGO_PKG_VERSION"))
    }
}

/// Starts the native thread; ticks go to it through the sender (one waiting at most: the native
/// view shows the newest it can).
fn native_thread(shared: Arc<Shared>, scale: u32) -> SyncSender<ToDraw> {
    let (tx, rx) = sync_channel::<ToDraw>(1);
    std::thread::Builder::new()
        .name("lt-remaster-native".into())
        .spawn(move || {
            lt_emu_host::thread::interactive();
            if let Err(e) = native(rx, &shared, scale) {
                shared.stats.lock().unwrap().native_error = Some(e);
            }
        })
        .expect("native thread");
    tx
}

/// FNV-1a, to tell a chunk's bytes apart in the report.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The native thread: each tick resolved to the disc's assets and composited. The assets are
/// read off the disc the core reads, again whenever it changes.
fn native(rx: Receiver<ToDraw>, shared: &Shared, scale: u32) -> Result<(), String> {
    use lt_assets::gpu::{Cache, Wgpu};
    // The window's device: wait for it.
    let (device, queue) = loop {
        if let Some(g) = shared.gpu.get() {
            break g.clone();
        }
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let tick_time = Duration::from_secs_f64(2.0 / 59.29);
    let mut pending: Option<ToDraw> = None;
    'disc: loop {
        // The disc to read (`--host`: once Living Tool opens one). Ticks meanwhile are dropped.
        let (seen, overlay) = loop {
            if shared.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let seen = shared.disc_changes.load(Ordering::SeqCst);
            if let Some(o) = shared.disc.lock().unwrap().clone() {
                break (seen, o);
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
                _ => continue,
            }
        };
        let t_load = Instant::now();
        let image = lt_core::DiscImage::from_source("the game's disc", Arc::new(crate::disc::Quiet(overlay))).map_err(|e| e.to_string())?;
        if let Err(e) = crate::disc::check_image(&image, "The game's disc") {
            shared.stats.lock().unwrap().native_error = Some(e);
            // Nothing to draw until the disc changes.
            while shared.disc_changes.load(Ordering::SeqCst) == seen {
                if shared.stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if let Err(RecvTimeoutError::Disconnected) = rx.recv_timeout(Duration::from_millis(50)) {
                    return Ok(());
                }
            }
            continue 'disc;
        }
        let exe = lt_core::PsxExe::from_disc(&image).map_err(|e| e.to_string())?;
        let table = lt_core::FileTable::parse(&exe, image.sectors()).map_err(|e| e.to_string())?;
        let files = lt_core::worldmap::WorldDisc { image: &image, table: &table, exe: &exe };
        let mut assets = lt_assets::Assets::new(&files, lt_assets::model::ModelIndex::build(&files));
        let gpu = Wgpu { device: device.clone(), queue: queue.clone() };
        let mut comp = lt_native::composite::Compositor::new(&gpu);
        let mut cache = Cache::new(gpu);
        {
            let mut st = shared.stats.lock().unwrap();
            st.assets_loads += 1;
            st.assets_ms = t_load.elapsed().as_secs_f64() * 1000.0;
            st.native_error = None;
            st.native_chunk = None;
        }
        // Frames a tick: the game ticks at 59.29 / 2 Hz; between ticks the native view blends the
        // last two (LT-113), drawing frame j of k at j / k of a tick after the tick came.
        let mut prev: Option<lt_native::play::Tick> = None;
        // The NPCs without a packet slot, posed tick to tick (LT-119).
        let mut crowd = lt_native::crowd::Crowd::new();
        let mut window = (Instant::now(), 0u64, 0u64, 0.0f64, 0.0f64); // start, ticks, frames, ms, max ms
        loop {
            if shared.disc_changes.load(Ordering::SeqCst) != seen {
                continue 'disc;
            }
            let t = match pending.take() {
                Some(t) => t,
                None => match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(t) => t,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                },
            };
            let came = Instant::now();
            let read = |a: u32, l: usize| -> Vec<u8> {
                if (a & 0x1FFF_FC00) == SCRATCH {
                    let o = (a & 0x3FF) as usize;
                    (o..o + l).map(|i| t.scratch.get(i).copied().unwrap_or(0)).collect()
                } else {
                    let o = (a & 0x1F_FFFF) as usize;
                    match t.ram.get(o..o + l) {
                        Some(b) => b.to_vec(),
                        None => (o..o + l).map(|i| t.ram[i % RAM]).collect(),
                    }
                }
            };
            let tick_n = t.drawn.record.tick;
            let reach = *shared.reach.lock().unwrap();
            let (tick, shown) = lt_native::play::resolve_with(t.drawn, &read, &mut assets, reach, &mut crowd);
            window.1 += 1;
            if shared.stats.lock().unwrap().native_chunk.is_none_or(|(c, _)| c != tick.record.chunk) {
                let hash = files.chunk(u32::from(tick.record.chunk)).map(|b| fnv(&b)).unwrap_or(0);
                shared.stats.lock().unwrap().native_chunk = Some((tick.record.chunk, hash));
            }
            {
                let mut st = shared.stats.lock().unwrap();
                st.native_characters = tick.record.characters.iter().map(|c| (c.index, c.position)).collect();
                st.native_crowd = tick.crowd.iter().map(|c| c.index).collect();
            }
            comp.lights = *shared.lights.lock().unwrap();
            // A movie shows one picture a tick.
            let k = (shared.fps.load(Ordering::Relaxed) / 30).max(1);
            let frames = if tick.table.passthrough { 1 } else { k };
            for j in 1..=frames {
                if j > 1 {
                    // A newer tick: blend toward it instead.
                    if let Ok(next) = rx.try_recv() {
                        pending = Some(next);
                        break;
                    }
                    let due = came + tick_time * (j - 1) / frames;
                    if let Some(wait) = due.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                }
                let t0 = Instant::now();
                let look = if shared.lit.load(Ordering::Relaxed) { lt_native::light::Look::Lit } else { lt_native::light::Look::Original };
                let (view, _) = lt_native::play::view_between(prev.as_ref(), &tick, j as f64 / frames as f64, &shown, &assets, &mut cache, &mut comp, scale, look, reach);
                let frame = match view {
                    lt_native::play::View::Rendered(r) => {
                        // A wider view (LT-114) adds the same columns each side of the display area, and
                        // the emulated picture's frame grows by them: the picture, margins and all, starts
                        // where it did.
                        let left = r.w.saturating_sub(tick.table.display[2] as u32 * scale) / 2;
                        NativeFrame::Gpu { texture: r.colour, w: r.w, h: r.h, canvas: [shown.w * scale + 2 * left, shown.h * scale], at: tick.table.screen.map(|v| v * scale as i32) }
                    }
                    lt_native::play::View::Picture(p) => {
                        let (rgba, width, height) = lt_native::play::on_screen(&p, &tick.table, &shown, scale);
                        NativeFrame::Pixels(Picture { rgba, width, height, n: tick_n })
                    }
                };
                *shared.native.lock().unwrap() = Some(frame);
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                window.2 += 1;
                window.3 += ms;
                window.4 = window.4.max(ms);
            }
            prev = Some(tick);
            if window.0.elapsed() >= Duration::from_secs(1) {
                let mut st = shared.stats.lock().unwrap();
                st.native_ticks += window.1;
                st.native_frames += window.2;
                st.native_fps = window.2 as f64 / window.0.elapsed().as_secs_f64();
                st.native_ms = window.3 / window.2.max(1) as f64;
                st.native_ms_max = window.4;
                window = (Instant::now(), 0, 0, 0.0, 0.0);
            }
        }
    }
}

fn run(path: &std::path::Path, shared: &Arc<Shared>, new_game: bool, tx: SyncSender<ToDraw>) -> Result<(), String> {
    let overlay = Arc::new(Overlay::new(Image::open(path)?, 0));
    shared.set_disc(overlay.clone());
    let disc = Disc::from_source(Box::new(Source(overlay)));
    let mut bus = Bus::new_without_bios();
    let mut cpu = Cpu::new();
    fast_boot_disc(&mut bus, &mut cpu, &disc).map_err(|e| format!("the disc didn't boot: {e:?}"))?;
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();
    bus.attach_memcard_port1(Vec::new());
    let mut draws = Draws::new(shared.clone(), tx, new_game)?;
    draws.install(&mut cpu);

    let mut sound = AudioOut::open(shared.sound.clone()).ok();
    let mut next = Instant::now();
    let mut frame = 0u64;
    let mut late = 0u64;
    let mut window = (Instant::now(), 0u64, 0.0f64, 0.0f64, 0.0f64); // start, frames, emu ms, emu max, emulated s
    while !shared.stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        if t0 > next + LATE {
            late += 1;
        }
        let c0 = bus.cycles();
        draws.frame(&mut cpu, &mut bus, frame, 0)?;
        bus.run_spu_to_current_cycle();
        let samples = bus.spu.drain_audio();
        frame += 1;
        let emu_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let emulated = Duration::from_secs_f64(bus.cycles().saturating_sub(c0) as f64 / CPU_HZ);
        if let Some(s) = sound.as_mut() {
            s.push(&samples);
        }

        window.1 += 1;
        window.2 += emu_ms;
        window.3 = window.3.max(emu_ms);
        window.4 += emulated.as_secs_f64();
        let span = t0.duration_since(window.0).as_secs_f64();
        if span >= 1.0 {
            let mut st = shared.stats.lock().unwrap();
            st.frames = frame;
            st.fps = window.1 as f64 / span;
            st.field_hz = window.1 as f64 / window.4.max(1e-9);
            st.emu_ms = window.2 / window.1 as f64;
            st.emu_ms_max = window.3;
            st.late = late;
            if let Some(s) = sound.as_ref() {
                let a = s.stats();
                st.audio_underruns = a.underruns;
                st.audio_queued_ms = a.queued_ms;
                st.audio_device = a.device.clone();
            }
            st.hooks = draws.counts();
            (st.chunk, st.mode) = field(&bus);
            window = (t0, 0, 0.0, 0.0, 0.0);
        }

        // Due when the emulated time this frame covered has passed; a long stall re-anchors.
        next += emulated.clamp(FRAME_MIN, FRAME_MAX);
        let now = Instant::now();
        if now > next {
            if now - next > Duration::from_millis(250) {
                next = now;
            }
        } else {
            let wait = next - now;
            if wait > Duration::from_millis(2) {
                std::thread::sleep(wait - Duration::from_millis(1));
            }
            while Instant::now() < next {
                std::hint::spin_loop();
            }
        }
    }
    if let Some(s) = sound.as_mut() {
        s.set_paused(true);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_new_game_tape() {
        assert_eq!(new_game_tape(0), 1 << 3);
        assert_eq!(new_game_tape(10), 0);
        assert_eq!(new_game_tape(2400), 1 << 14);
        assert_eq!(new_game_tape(4800), 0);
    }

    #[test]
    fn fnv_tells_bytes_apart() {
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_ne!(fnv(b"chunk 78"), fnv(b"chunk 79"));
    }
}
