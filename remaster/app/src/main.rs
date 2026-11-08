//! Living Tool Remaster (epic LT-101), the app shell (LT-107).
//!
//! It opens the player's own Guardian's Crusade (USA) disc, checks it, and runs the game's own
//! code on the patched PSoXide core in real time, with sound and the pad. The window shows the
//! emulated picture beside the native view (LT-111: terrain and characters drawn natively, the 2D
//! layer emulated, at `--scale` ×). A toolbar (LT-117) sets the look, the lights, the aspect, the
//! frame rate and the draw distance; a debug overlay shows the core's timing, the native view's,
//! and what the engine's draw functions do.
//!
//! ```text
//! lt-remaster <disc.bin> [--mute] [--new-game] [options]
//! lt-remaster --host [--title <name>] [--keys <button=Code,…>] [options]
//! lt-remaster --version
//!   options: [--scale N] [--look lit|original] [--fps 30|60|120]
//!     [--aspect 4:3|16:10|16:9|21:9|window] [--distance 1-4] [--view both|native] [--size WxH]
//!     [--settings <file>] [--quit-after <seconds>] [--report <file>] [--screenshot <png>]
//! ```
//!
//! - `--mute` starts with the sound off (M turns it on and off).
//! - `--new-game` plays research 16's New Game tape on top of the pad, to the village.
//! - `--host` (LT-117) serves Living Tool's Play screen: the emulator program's protocol over
//!   stdin and stdout (`lt-emu-protocol`), so the Play screen opens its project's disc here, hands
//!   over the game it was running, and syncs and warps as it does with `lt-emu-host`. Sound and
//!   controllers are the Play screen's; `--keys` gives the window the player's keys
//!   (Living Tool's `inproc.keys`), `--title` names the project. Closing the window hands the game
//!   back to the Play screen; the program ends when the Play screen shuts it down.
//! - `--scale` is the native view's resolution, N × the PS1's (2 by default, 1 to 7).
//! - `--look` starts the native view lit (the default) or in the game's original look; L switches.
//! - `--fps` is the native view's frame rate: 30 draws each game tick, 60 (the default) and 120
//!   blend between ticks (LT-113). The game itself runs at its own 30.
//! - `--aspect` widens the native view past the game's 4:3 (LT-114): `window` follows the window's
//!   shape as it's resized. `--distance` draws that many times further than the game (1 to 4).
//!   The 2D layer (HUD, dialogue, menus) stays 4:3 in the middle.
//! - `--view native` shows the native view alone, filling the window (the default with `--host`);
//!   `--size` is the window's (logical pixels; 1280x520 on its own, 1280x760 with `--host`).
//! - `--settings` keeps the toolbar's choices in that file (read first, so the options above win).
//! - `--quit-after` closes after that long (with `--host`, as the player closing the window does:
//!   the Play screen takes the game back and shuts the program down); `--report` writes the run's
//!   numbers and `--screenshot` the window's picture (a PNG) when it ends, for checks.
//!
//! Keys: the Play screen's defaults (arrows, X ✕, C ○, Z □, S △, Q/W L1/R1, 1/2 L2/R2, Enter start,
//! Backspace select) or `--keys`; M mutes (on its own); L switches the look; A steps the aspect
//! (4:3, 16:10, 16:9, 21:9, the window's); D the distance (1 to 4); F2 hides the toolbar; F3 the
//! overlay. A key bound to a PS1 button only plays. Controllers work in the standard layout.

mod disc;
mod emu;
mod input;
mod render;
mod toolbar;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Theme, Window, WindowId};

use render::{Renderer, Which};
use toolbar::{View, ASPECTS};

struct Args {
    disc: PathBuf,
    host: bool,
    title: Option<String>,
    keys: Option<String>,
    mute: bool,
    new_game: bool,
    scale: u32,
    view: View,
    size: (f64, f64),
    settings: Option<PathBuf>,
    quit_after: Option<f64>,
    report: Option<PathBuf>,
    screenshot: Option<PathBuf>,
}

const USAGE: &str = "usage: lt-remaster <disc.bin> [--mute] [--new-game] [options], or lt-remaster --host [--title <name>] [--keys <button=Code,...>] [options]; options: [--scale N] [--look lit|original] [--fps 30|60|120] [--aspect 4:3|16:10|16:9|21:9|window] [--distance 1-4] [--view both|native] [--size WxH] [--settings <file>] [--quit-after <seconds>] [--report <file>] [--screenshot <png>]";

fn args() -> Result<Args, String> {
    let all: Vec<String> = std::env::args().skip(1).collect();
    let host = all.iter().any(|a| a == "--host");
    // The settings file first: the options given win over it.
    let settings: Option<PathBuf> = all.iter().position(|a| a == "--settings").map(|i| all.get(i + 1).cloned().ok_or("--settings takes a file")).transpose()?.map(PathBuf::from);
    let standard = View { lit: true, lights: Default::default(), aspect: if host { 4 } else { 0 }, fps: 60, distance: 1.0, native_only: host, stats: !host, toolbar: true };
    let view = match settings.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => standard.with_text(&text),
        None => standard,
    };
    let mut a = Args {
        disc: PathBuf::new(),
        host,
        title: None,
        keys: None,
        mute: false,
        new_game: false,
        scale: 2,
        view,
        size: if host { (1280.0, 760.0) } else { (1280.0, 520.0) },
        settings,
        quit_after: None,
        report: None,
        screenshot: None,
    };
    let mut it = all.into_iter();
    while let Some(s) = it.next() {
        match s.as_str() {
            "--host" => {}
            "--settings" => {
                it.next();
            }
            "--title" => a.title = Some(it.next().ok_or("--title takes a name")?),
            "--keys" => a.keys = Some(it.next().ok_or("--keys takes button=Code pairs")?),
            "--mute" => a.mute = true,
            "--new-game" => a.new_game = true,
            "--scale" => a.scale = it.next().and_then(|v| v.parse().ok()).filter(|n| (1..=7).contains(n)).ok_or("--scale takes 1 to 7")?,
            "--fps" => a.view.fps = it.next().and_then(|v| v.parse().ok()).filter(|n| toolbar::FRAME_RATES.contains(n)).ok_or("--fps takes 30, 60 or 120")?,
            "--look" => {
                a.view.lit = match it.next().as_deref() {
                    Some("lit") => true,
                    Some("original") => false,
                    _ => return Err("--look takes lit or original".into()),
                }
            }
            "--aspect" => {
                let v = it.next().unwrap_or_default();
                a.view.aspect = ASPECTS.iter().position(|(n, _)| n.eq_ignore_ascii_case(&v)).ok_or("--aspect takes 4:3, 16:10, 16:9, 21:9 or window")?;
            }
            "--distance" => a.view.distance = it.next().and_then(|v| v.parse().ok()).filter(|d: &f64| (1.0..=lt_native::wide::MAX_DISTANCE).contains(d)).ok_or("--distance takes 1 to 4")?,
            "--view" => {
                a.view.native_only = match it.next().as_deref() {
                    Some("both") => false,
                    Some("native") => true,
                    _ => return Err("--view takes both or native".into()),
                }
            }
            "--size" => {
                let v = it.next().unwrap_or_default();
                let (w, h) = v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).filter(|&(w, h): &(f64, f64)| w >= 200.0 && h >= 150.0).ok_or("--size takes WxH")?;
                a.size = (w, h);
            }
            "--quit-after" => a.quit_after = Some(it.next().and_then(|v| v.parse().ok()).ok_or("--quit-after takes seconds")?),
            "--report" => a.report = Some(it.next().ok_or("--report takes a file")?.into()),
            "--screenshot" => a.screenshot = Some(it.next().ok_or("--screenshot takes a file")?.into()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => a.disc = s.into(),
        }
    }
    if !a.host && a.disc.as_os_str().is_empty() {
        return Err(USAGE.into());
    }
    Ok(a)
}

struct App {
    args: Args,
    title: String,
    shared: Arc<emu::Shared>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    input: input::Input,
    /// The toolbar: egui, its window input, and the system's appearance.
    ui: egui::Context,
    ui_input: Option<egui_winit::State>,
    dark: bool,
    started: Instant,
    /// The picture last shown, and frames the window never showed.
    shown: u64,
    presented: u64,
    dropped: u64,
    /// Native pictures shown.
    native_shown: u64,
    /// The window is covered (macOS stops drawing it), since when, and for how long in all: frames
    /// it misses then aren't counted as dropped.
    occluded: Option<Instant>,
    occluded_for: Duration,
    /// When pictures were dropped (seconds since start, how many), the longest gap between two
    /// redraws, and the longest queue the window found.
    drops_at: Vec<(f64, u64)>,
    last_redraw: Option<Instant>,
    longest_gap: Duration,
    deepest: usize,
    overlay_at: Instant,
    /// `--host`: the window was closed (until Living Tool shows it again), and `--quit-after` did it.
    closed: bool,
    quit: bool,
}

impl App {
    /// The view's choices handed to the native thread and the window's layout: the look, the
    /// lights, the frame rate, and the reach from the aspect and distance chosen (the window's
    /// shape for `window`).
    fn apply_view(&mut self) {
        let v = &self.args.view;
        self.shared.lit.store(v.lit, Ordering::Relaxed);
        *self.shared.lights.lock().unwrap() = v.lights;
        self.shared.fps.store(v.fps, Ordering::Relaxed);
        self.shared.show_picture.store(!v.native_only, Ordering::Relaxed);
        if v.native_only {
            self.shared.pictures.lock().unwrap().clear();
        }
        let fixed = ASPECTS[v.aspect].1;
        if let Some(r) = self.renderer.as_mut() {
            r.native_only = v.native_only;
        }
        let room = self.renderer.as_ref().map_or(4.0 / 3.0, |r| r.room() as f64);
        let reach = lt_native::wide::Reach::new(fixed.unwrap_or(room), v.distance);
        *self.shared.reach.lock().unwrap() = reach;
        if let Some(r) = self.renderer.as_mut() {
            r.native_aspect = reach.aspect as f32;
        }
    }

    /// Applies a changed view and keeps it in `--settings`.
    fn set_view(&mut self, v: View) {
        if v == self.args.view {
            return;
        }
        self.args.view = v;
        self.apply_view();
        if let Some(p) = &self.args.settings {
            if let Err(e) = std::fs::write(p, self.args.view.to_text()) {
                eprintln!("lt-remaster: {}: {e}", p.display());
            }
        }
    }

    fn overlay_lines(&self, st: &emu::Stats) -> Vec<String> {
        let hooks: Vec<String> = st.hooks.iter().map(|(n, c)| format!("{n} {c}")).collect();
        let v = &self.args.view;
        let mut lines = vec![
            format!("Living Tool Remaster - {}{}", self.title, if self.args.host { "  (Living Tool's Play screen)" } else { "" }),
            format!("frame {}  tick {}  {:.2} fps  field {:.2} Hz", st.frames, st.frames / 2, st.fps, st.field_hz),
            format!("core {:.2} ms (max {:.2})  late {}  shown {}  dropped {}", st.emu_ms, st.emu_ms_max, st.late, self.presented, self.dropped),
            if self.args.host {
                "sound and controllers: the Play screen's".to_string()
            } else {
                format!(
                    "sound {}{:.0} ms queued  underruns {}  {}",
                    if self.shared.sound.muted() { "MUTED  " } else { "" },
                    st.audio_queued_ms,
                    st.audio_underruns,
                    st.audio_device
                )
            },
            format!(
                "native {}x {}{}  {:.1} fps of {}  {} ticks  {:.2} ms a frame (max {:.2})  dropped {}{}",
                self.args.scale,
                if v.lit { "lit" } else { "original" },
                if v.lit && v.lights != lt_native::light::Lights::ALL { format!(" (shadows {}, lamps {})", on(v.lights.shadows), on(v.lights.lamps)) } else { String::new() },
                st.native_fps,
                v.fps,
                st.native_ticks,
                st.native_ms,
                st.native_ms_max,
                st.native_dropped,
                st.native_error.as_deref().map(|e| format!("  ERROR: {e}")).unwrap_or_default()
            ),
            {
                let r = *self.shared.reach.lock().unwrap();
                format!("view {} ({:.3}:1)  distance {}{}", ASPECTS[v.aspect].0, r.aspect, r.distance, if v.native_only { "  native only" } else { "" })
            },
            format!(
                "assets read {} times (last {:.0} ms)  chunk {}",
                st.assets_loads,
                st.assets_ms,
                st.native_chunk.map(|(c, h)| format!("{c} {h:016x}")).unwrap_or_else(|| "-".into())
            ),
            format!("hooks: {}", hooks.join("  ")),
            format!("chunk {}  mode {}  pad {:04x}  controllers {}", st.chunk, st.mode, self.shared.pad.load(Ordering::Relaxed), self.input.controllers()),
        ];
        if let Some(r) = self.renderer.as_ref() {
            lines.push(format!("gpu {}", r.adapter));
        }
        if let Some(e) = &st.halted {
            lines.push(format!("HALTED: {e}"));
        }
        lines
    }

    fn report(&self) {
        let Some(path) = &self.args.report else { return };
        let st = self.shared.stats.lock().unwrap().clone();
        let secs = self.started.elapsed().as_secs_f64();
        let mut s = String::new();
        for l in self.overlay_lines(&st) {
            s.push_str(&l);
            s.push('\n');
        }
        let covered = self.occluded_for + self.occluded.map_or(Duration::ZERO, |t| t.elapsed());
        s.push_str(&format!(
            "seconds {secs:.1}\nframes {}\nexpected_frames {:.0}\nlate {}\npresented {}\ndropped {}\nnative_ticks {}\nnative_frames {}\nnative_ms {:.2}\nnative_dropped {}\nnative_shown {}\ncovered_seconds {:.1}\nlongest_redraw_gap_ms {:.1}\ndeepest_queue {}\ndrops_at {}\naudio_underruns {}\nchunk {}\nmode {}\nhalted {}\n",
            st.frames,
            secs * 59.29,
            st.late,
            self.presented,
            self.dropped,
            st.native_ticks,
            st.native_frames,
            st.native_ms,
            st.native_dropped,
            self.native_shown,
            covered.as_secs_f64(),
            self.longest_gap.as_secs_f64() * 1000.0,
            self.deepest,
            self.drops_at.iter().map(|(t, n)| format!("{t:.1}s:{n}")).collect::<Vec<_>>().join(" "),
            st.audio_underruns,
            st.chunk,
            st.mode,
            st.halted.as_deref().unwrap_or("no")
        ));
        // LT-117's checks: the mode, the disc's assets as the native view read them, and what it drew.
        s.push_str(&format!(
            "host {}\nnative_error {}\nassets_loads {}\nassets_ms {:.0}\nnative_chunk {}\nnative_characters {}\nnative_crowd {}\nview {}\n",
            if self.args.host { "yes" } else { "no" },
            st.native_error.as_deref().unwrap_or("none"),
            st.assets_loads,
            st.assets_ms,
            st.native_chunk.map(|(c, h)| format!("{c} {h:016x}")).unwrap_or_else(|| "none".into()),
            st.native_characters.iter().map(|(i, p)| format!("{i}:{},{},{}", p[0], p[1], p[2])).collect::<Vec<_>>().join(" "),
            st.native_crowd.iter().map(u16::to_string).collect::<Vec<_>>().join(" "),
            self.args.view.to_text().trim_end().replace('\n', " ")
        ));
        let _ = std::fs::write(path, s);
    }

    /// The report and the screenshot, then the end.
    fn finish(&mut self, el: &ActiveEventLoop) {
        self.report();
        if let (Some(path), Some(r)) = (self.args.screenshot.clone(), self.renderer.as_mut()) {
            let (rgba, w, h) = r.capture();
            if let Err(e) = write_png(&path, &rgba, w, h) {
                eprintln!("lt-remaster: {}: {e}", path.display());
            }
        }
        el.exit();
    }

    /// `--host`: the player closed the window. It hides, and Living Tool takes the game back, then
    /// shuts this program down.
    fn close_hosted(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(w) = self.window.as_ref() {
            w.set_visible(false);
        }
        self.input.release_keys();
        self.shared.pad.store(0, Ordering::Relaxed);
        if let Some(out) = self.shared.events.lock().unwrap().as_ref() {
            out.event(lt_emu_protocol::Event::Closed);
        }
    }

    /// The toolbar for this frame: egui run on the window's input, drawn by the renderer.
    fn toolbar(&mut self) {
        let (Some(window), Some(state)) = (self.window.clone(), self.ui_input.as_mut()) else { return };
        let raw = state.take_egui_input(&window);
        let ctx = self.ui.clone();
        let mut view = self.args.view.clone();
        let dark = self.dark;
        let mut height = 0.0;
        let out = ctx.run(raw, |ctx| height = toolbar::show(ctx, dark, &mut view));
        state.handle_platform_output(&window, out.platform_output);
        let jobs = ctx.tessellate(out.shapes, out.pixels_per_point);
        if let Some(r) = self.renderer.as_mut() {
            r.set_ui(out.textures_delta, jobs, out.pixels_per_point);
            let top = height * out.pixels_per_point;
            if (r.top - top).abs() > 0.5 {
                r.top = top;
                // The room below changed (the window's aspect follows it).
                self.apply_view();
            }
        }
        self.set_view(view);
    }
}

fn on(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let name = match &self.args.title {
            Some(t) => format!("Living Tool Remaster · {t}"),
            None => "Living Tool Remaster".into(),
        };
        let attrs = Window::default_attributes().with_title(name).with_inner_size(LogicalSize::new(self.args.size.0, self.args.size.1));
        let window = Arc::new(el.create_window(attrs).expect("window"));
        match Renderer::new(window.clone()) {
            Ok(r) => {
                // The native thread composites on this device from now on.
                let _ = self.shared.gpu.set(r.gpu());
                self.renderer = Some(r);
                self.apply_view();
            }
            Err(e) => {
                eprintln!("lt-remaster: {e}");
                el.exit();
                return;
            }
        }
        self.dark = window.theme() != Some(Theme::Light);
        toolbar::fonts(&self.ui);
        self.ui_input = Some(egui_winit::State::new(self.ui.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None));
        window.request_redraw();
        if let (Ok(pos), size) = (window.outer_position(), window.outer_size()) {
            let s = window.scale_factor();
            eprintln!("window {:.0},{:.0},{:.0},{:.0} (points)", pos.x as f64 / s, pos.y as f64 / s, size.width as f64 / s, size.height as f64 / s);
        }
        self.window = Some(window);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let (Some(state), Some(w)) = (self.ui_input.as_mut(), self.window.as_ref()) {
            let _ = state.on_window_event(w, &event);
        }
        match event {
            WindowEvent::CloseRequested if self.args.host => self.close_hosted(),
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::ThemeChanged(theme) => self.dark = theme != Theme::Light,
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                self.apply_view();
            }
            WindowEvent::Focused(false) => self.input.release_keys(),
            WindowEvent::Occluded(covered) => {
                if covered {
                    self.occluded.get_or_insert_with(Instant::now);
                } else if let Some(t) = self.occluded.take() {
                    self.occluded_for += t.elapsed();
                    // Start counting again from the next picture shown.
                    self.shown = 0;
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(code), state, repeat: false, .. }, .. } => {
                let pressed = state == ElementState::Pressed;
                // A key the pad uses only plays.
                if pressed && self.input.bits(code) == 0 {
                    let mut v = self.args.view.clone();
                    match code {
                        KeyCode::F2 => v.toolbar = !v.toolbar,
                        KeyCode::F3 => v.stats = !v.stats,
                        KeyCode::KeyL => v.lit = !v.lit,
                        KeyCode::KeyA => v.aspect = (v.aspect + 1) % ASPECTS.len(),
                        KeyCode::KeyD => v.distance = if v.distance >= lt_native::wide::MAX_DISTANCE { 1.0 } else { v.distance.floor() + 1.0 },
                        KeyCode::KeyM if !self.args.host => {
                            let s = &self.shared.sound;
                            s.set_muted(!s.muted());
                        }
                        _ => {}
                    }
                    self.set_view(v);
                }
                self.input.key(code, pressed);
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                if let Some(t) = self.last_redraw.replace(now) {
                    self.longest_gap = self.longest_gap.max(now - t);
                }
                // The oldest picture not shown yet: one per refresh, so none is skipped.
                let picture = {
                    let mut q = self.shared.pictures.lock().unwrap();
                    self.deepest = self.deepest.max(q.len());
                    q.pop_front()
                };
                if let (Some(p), Some(r)) = (picture, self.renderer.as_mut()) {
                    if p.n != self.shown {
                        if self.shown != 0 && self.occluded.is_none() && p.n > self.shown + 1 {
                            self.dropped += p.n - self.shown - 1;
                            self.drops_at.push((self.started.elapsed().as_secs_f64(), p.n - self.shown - 1));
                        }
                        self.shown = p.n;
                        self.presented += 1;
                        r.upload(Which::Picture, &p.rgba, p.width, p.height);
                    }
                }
                let native = self.shared.native.lock().unwrap().take();
                if let (Some(f), Some(r)) = (native, self.renderer.as_mut()) {
                    self.native_shown += 1;
                    match f {
                        emu::NativeFrame::Gpu { texture, w, h, canvas, at } => r.show_native(&texture, w, h, canvas, at),
                        emu::NativeFrame::Pixels(p) => r.upload(Which::Native, &p.rgba, p.width, p.height),
                    }
                }
                if self.overlay_at.elapsed() >= Duration::from_millis(500) {
                    self.overlay_at = Instant::now();
                    let st = self.shared.stats.lock().unwrap().clone();
                    // Why the native view stopped shows even without the stats (a Sync that changed the EXE).
                    let lines = match (&st.native_error, self.args.view.stats) {
                        (_, true) => self.overlay_lines(&st),
                        (Some(e), false) => vec![format!("The native view stopped: {e}")],
                        (None, false) => vec![],
                    };
                    let (rgba, w, h) = if lines.is_empty() { (vec![0; 4], 1, 1) } else { render::text(&lines) };
                    if let Some(r) = self.renderer.as_mut() {
                        r.upload(Which::Overlay, &rgba, w, h);
                    }
                }
                self.toolbar();
                if let Some(r) = self.renderer.as_mut() {
                    r.draw();
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.shared.pad.store(self.input.pad(), Ordering::Relaxed);
        if self.shared.focus.swap(false, Ordering::SeqCst) {
            self.closed = false;
            if let Some(w) = self.window.as_ref() {
                w.set_visible(true);
                w.focus_window();
            }
        }
        let due = self.args.quit_after.is_some_and(|s| self.started.elapsed().as_secs_f64() >= s);
        if self.shared.ended.load(Ordering::SeqCst) || (due && !self.args.host) {
            self.finish(el);
        } else if due && !self.quit {
            self.quit = true;
            self.close_hosted();
        }
    }
}

fn write_png(path: &std::path::Path, rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut e = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    let mut wr = e.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(rgba).map_err(|e| e.to_string())
}

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("lt-remaster {} ({}, protocol {})", env!("CARGO_PKG_VERSION"), lt_emu_host::CORE, lt_emu_protocol::PROTOCOL);
        return;
    }
    // `--host`: the protocol takes the process's stdout before any thread exists (the core's TTY
    // output goes to stderr from then on).
    let protocol = std::env::args().any(|a| a == "--host").then(lt_emu_host::protocol_out);
    let args = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lt-remaster: {e}");
            std::process::exit(2);
        }
    };
    let keys = match args.keys.as_deref().map(input::parse_keys).transpose() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("lt-remaster: {e}");
            std::process::exit(2);
        }
    };
    let shared = emu::Shared::new(args.mute);
    let (emu, title) = match protocol {
        Some(p) => (emu::serve(p, shared.clone(), args.scale), args.title.clone().unwrap_or_else(|| "Guardian's Crusade".into())),
        None => match disc::check(&args.disc) {
            Ok(c) => (emu::spawn(args.disc.clone(), shared.clone(), args.new_game, args.scale), c.title.to_string()),
            Err(e) => {
                eprintln!("lt-remaster: {e}");
                std::process::exit(2);
            }
        },
    };
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let input = input::Input::new(!args.host, keys);
    let mut app = App {
        args,
        title,
        shared: shared.clone(),
        window: None,
        renderer: None,
        input,
        ui: egui::Context::default(),
        ui_input: None,
        dark: true,
        started: Instant::now(),
        shown: 0,
        presented: 0,
        dropped: 0,
        native_shown: 0,
        occluded: None,
        occluded_for: Duration::ZERO,
        drops_at: Vec::new(),
        last_redraw: None,
        longest_gap: Duration::ZERO,
        deepest: 0,
        overlay_at: Instant::now() - Duration::from_secs(1),
        closed: false,
        quit: false,
    };
    app.apply_view();
    let result = event_loop.run_app(&mut app);
    shared.stop.store(true, Ordering::Relaxed);
    // `--host`: the protocol thread ends with the pipe; don't wait on Living Tool to close it.
    if !app.args.host {
        let _ = emu.join();
    }
    if let Err(e) = result {
        eprintln!("lt-remaster: {e}");
        std::process::exit(1);
    }
}
