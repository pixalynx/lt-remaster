# Living Tool Remaster

Guardian's Crusade from the player's own disc, running the game's own code on the patched PSoXide
core, with a native renderer growing over it (`docs/research/16-native-remaster.md`, Laneway epic
LT-101). The working name keeps "Guardian's Crusade" out of the product's name.

The app (LT-107) checks the disc and runs the core in real time with sound and the pad. Its window
shows the emulated picture beside the native view.
- **The native view (LT-111)** draws the terrain and characters natively, at 2× by default, and
  layers the game's 2D (HUD, text, menus, effects) over them, sorted by the ordering table. Movies
  pass through.
- **Lighting (LT-112)** lights it: the area's sun or room light with real shadows from houses,
  props and characters, and the lamps the art paints. The original look is one key away (L).
- **60 and 120 Hz (LT-113):** it draws frames between the game's 30 ticks a second, blending the
  last two; warps, doors and cuts snap.
- **Widescreen and draw distance (LT-114):** 16:10, 16:9, 21:9 or the window's shape, with the
  neighbour chunks read off the disc, and up to 4× the game's draw distance. The HUD, dialogue and
  menus stay the game's 4:3 in the middle.
- **Characters without a slot (LT-119):** in a wider view, the NPCs, doors and chests the game has
  no packet slot for are posed from their records and drawn, at the edges and past its five.
- **Living Tool's Play screen (LT-117):** `lt-remaster --host` is the Play screen's Remastered
  switch. It speaks the emulator program's protocol, so the Play screen moves the running game into
  this window without a restart (its machine, disc, patches and memory card) and keeps syncing,
  warping and reading flags as it does with `lt-emu-host`. A toolbar sets the look, the lights, the
  aspect, the frame rate and the draw distance.

## Build and run

```sh
cd remaster
cargo build --release
target/release/lt-remaster "<Guardian's Crusade (USA).bin>" [--scale N] [--look lit|original] [--fps 30|60|120]
    [--aspect 4:3|16:10|16:9|21:9|window] [--distance 1-4] [--view both|native] [--size WxH] [--settings <file>]
```

The toolchain is pinned in `rust-toolchain.toml`. Linux needs `libasound2-dev libudev-dev
libxkbcommon-dev libwayland-dev`. CI: the Remaster workflow, run by hand.

- **The disc:** a raw image (2352-byte sectors) of Guardian's Crusade (USA), SLUS-00811, with the
  retail boot EXE (SHA-1 `207358f9…`). Another game, another release or a changed EXE is refused
  with a message saying so. Nothing from the game is in this program.
- **Keys:** the Play screen's defaults: arrows, X ✕, C ○, Z □, S △, Q/W L1/R1, 1/2 L2/R2, Enter
  START, Backspace SELECT. M mutes (`--mute` starts muted); L switches the native view between lit
  and the original look; A steps its aspect (4:3, 16:10, 16:9, 21:9, the window's) and D its draw
  distance (1 to 4); F2 hides the toolbar and F3 the overlay. A key bound to a PS1 button only
  plays. Controllers work in the standard layout.
- **Toolbar (LT-117):** Look (Lit, Original), Lights (Shadows, Lamps: off, the sun, sky and baked
  colours still light the scene), Aspect, Frame rate (30, 60, 120), Distance (1× to 4×), Original
  beside (the emulated picture next to the native view) and Stats (the overlay). It is egui on the
  window's own device, in Living Tool's colours, light or dark as the system is. `--settings
  <file>` keeps its choices (`key=value` lines) so the window opens as it was left; the options on
  the command line win over the file.
- **Layout:** the emulated picture (4:3) beside the native view (at its aspect), or with `--view
  native` the native view alone, filling the window below the toolbar.
- **Overlay:**
  - frames, game ticks, frames per second, and the emulated field rate (59.29 Hz);
  - the core's time per frame, frames that started late, frames shown, and frames the window
    never showed;
  - the sound queue and its underruns;
  - the native view: its scale and look, frames a second against `--fps`, ticks drawn, time a
    frame (resolve and composite, on the window's GPU), and ticks dropped because it was still
    busy; its aspect and draw distance;
  - the engine's draw functions as the code hooks count them (`lt-emu-hooks`);
  - the field context's chunk and mode, and the GPU.
- **Checks:** `--new-game` plays research 16's New Game tape, and `--quit-after <s> --report <file>
  --screenshot <png>` closes after that long and writes the run's numbers and the window's picture.
  To the village (85 s, Apple M1 Pro, 2×): 5,041 frames, none late; 2,230 native ticks at 7.1 ms
  each, none dropped.

## From Living Tool's Play screen (LT-117)

```sh
lt-remaster --host [--title <project>] [--keys <button=Code,…>] [--settings <file>] [options]
lt-remaster --version
```

With `--host` the program serves `lt-emu-protocol` over stdin and stdout instead of opening a disc
itself, through `lt-emu-host`'s own library: the same runner (requests between frames, real-time
pacing, sound, pictures for the Play screen's views, stats, card writes), with this program's frame
as its `Stepper` (code hooks, the scene capture and the native view). So:

- **The switch moves the game, it doesn't restart it.** Living Tool pauses `lt-emu-host`, takes
  its memory card and machine state, opens the same disc and patches here, loads the state and
  runs (`lt_emu::hand_over`). Both programs run the same core build, so the machine is the same
  byte for byte and the frame count goes on. Off, or the player closing this window
  (`Event::Closed`: the window hides and waits), moves it back the same way.
- **Sync, warps and flags work as on the Play screen.** They are the same requests. The native view
  reads its assets off the disc the core reads (the image with the project's sectors over it,
  `disc::Quiet`) and reads them again whenever the disc or its patches change, so an edited chunk
  or actor shows natively as soon as the game reads it. Light sets come with LT-134; until then the
  lamps are the ones the chunks paint, so a chunk edit that paints them moves them.
- **Sound and controllers are Living Tool's:** its volume and mute (`Request::Audio`) and its
  gamepads (sent with the pad). The window reads keys only: Living Tool's own bindings with
  `--keys`. `Request::Focus` (the Play screen's Show window) brings the window to the front.
- **Only the retail EXE.** The native view knows the retail EXE's code and tables; Living Tool
  doesn't offer Remastered for a project that changes the EXE or runs on a base, and a disc whose
  EXE changes under it (a Sync) stops the native view with a message.
- **Checks:** `--report` and `--screenshot` are written when the program ends; `--quit-after`
  closes the window as the player would (Living Tool takes the game back). In a debug build of
  Living Tool, `LT_DEV_REMASTER_ARGS` adds them to the arguments it starts the program with.
  `app/crates/lt-emu/tests/remaster.rs` (@LT-117/1–2) drives both programs: the handover, and a
  Sync of a deleted piece and a moved villager. Results: the game moved at frame 4,500 with RAM
  identical, ran on, and came back; after the Sync the native view read the disc again (59 ms) and
  drew the edited chunk 78 and the villager where the edit put it. In the app the switch moved the
  game at frame 3,632, a Pieces Delete synced 170 sectors into the window, and closing the window
  brought it back at frame 5,157 with the edit.

`app/paper/` holds Paper's screen 22 artboards for it, light and dark (see its README).

**Shipping (LT-141).** Living Tool ships `lt-remaster` as a second sidecar beside `lt-emu-host`
(`bundle.externalBin`): `app/scripts/remaster.mjs` builds it for a target and puts it in
`app/src-tauri/binaries/`, and `pnpm app` and every release build run it. The macOS bundler signs it
with the app (Developer ID, hardened runtime), and the build checks that. Its source is published at
<https://github.com/pixalynx/lt-remaster> by `app/scripts/remaster-publish.mjs` (the root README's
"Living Tool Remaster's source"; `remaster/publish/README.md` is that repository's README), and a
release only builds when it is there. `lt-remaster --version` prints its version, core build and
protocol, which match `lt-emu-host --version`'s core and protocol.

## What's here

| Path | What |
|---|---|
| `app/src/main.rs` | Arguments, the window, the overlay, the report; `--host`'s window (closing it hands the game back) |
| `app/src/disc.rs` | The disc check (`lt-core`'s release detection plus the EXE's SHA-1), and the disc the core reads as Living Tool's readers see it |
| `app/src/emu.rs` | The emulator thread: one frame with the native view's hooks (`Draws`), run on its own (paced by emulated time, sound out) or as `lt-emu-host`'s stepper with `--host`; the native thread, which reads the disc's assets (again after a Sync) and draws each tick |
| `app/src/render.rs` | wgpu: the picture, the native view (drawn from the texture the native thread composited on this device), the overlay, the toolbar's egui pass |
| `app/src/toolbar.rs` | The toolbar (egui, Living Tool's colours) and the view it sets, saved with `--settings` |
| `app/src/input.rs` | Keyboard (the defaults, or `--keys`) and controllers (gilrs) to PS1 buttons |
| `app/fonts/` | Inter for the toolbar (SIL OFL 1.1) |
| `app/paper/` | Paper's screen 22 artboards for the Remastered switch and window, light and dark |
| `testset/` | `lt-testset`: the test set, scene states and input recordings built from your disc (see its README) |
| `lockstep/` | `lt-lockstep`: two machines compared after every frame, the first difference named and traced (see its README) |
| `scene/` | `lt-scene`: one record per game tick of the camera, chunks, characters, torches and the character records, from the draw hooks (see its README) |
| `assets/` | `lt-assets`: each captured draw matched to the chunk or model file it came from, animated VRAM decoded each tick, and a GPU cache that uploads each asset once (see its README) |
| `native/` | `lt-native`: the captured scene drawn unlit through the game's camera (GTE-exact projection, wgpu); the gate that compares its coverage with the emulated frame (LT-110); the takeover, which reads each ordering table and composites native terrain and characters with the game's 2D (LT-111); lighting (LT-112); frames between ticks (LT-113); widescreen and draw distance (LT-114); characters without a slot (LT-119). See its README |

Licence: GPL-3.0-or-later (`LICENSE`); it links PSoXide (GPL-2.0-or-later) from the fork
`pixalynx/PSoXide-emulator`, as `emu-host/` does.
