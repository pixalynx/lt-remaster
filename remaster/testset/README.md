# lt-testset

Living Tool Remaster's test set (LT-106): fixed starting points in Guardian's Crusade with input
recordings from them. Every remaster check runs on it: lockstep, render matching and timing.

A scene is:
- a machine state (PSoXide's own serde layout, as `emu-host` saves states);
- the memory card at that moment (states leave the card out);
- a tape of pads, one per frame and at least 1,200 frames (600 game ticks);
- the digest of the whole machine after the tape plays.

The digest covers main RAM, the scratchpad, the picture, the CPU's registers and the clock. States
hold disc data, so the set lives outside git and is built from your own disc and card.

```sh
cd remaster
cargo run --release -p lt-testset -- build "<Guardian's Crusade (USA).bin>" <card.mcd> <set dir>
cargo run --release -p lt-testset -- check "<Guardian's Crusade (USA).bin>" <set dir>
```

- `build` makes every scene from power-on, in about four minutes on an M1 Pro (15 scenes).
- `check` replays each scene twice on fresh machines and fails unless both replays end with the
  digest the build stored.
- The card is the late-game one the compat tour uses (`memcard04-karmine-fly.mcd`, level 6–43
  saves); its first save is loaded.
- States only load into the core build that saved them (`core` in `manifest.txt`): after moving the
  PSoXide rev, build the set again.

## The scenes

The recipes follow `app/crates/lt-emu/examples/compat_tour.rs`:
- **New Game:** research 16's New Game tape.
- **Warps:** `lt_warp` (ctx+0x50 = chunk | 0x8000 while idle), with the Knight then stood on the
  chunk's walk map (nearest (0, 0), or a given spot).
- **Random battles:** found by walking squares.
- **Flight:** the World mod's root-mode switch.

| Scene | Starts at | Tape |
|---|---|---|
| `boot` | power-on | 2,400 frames of no input: the boot movies (MDEC, XA) |
| `cutscene` | the Knight's bedroom (chunk 326) on New Game | 1,500 frames of ✕ through the opening scene, out into the village |
| `village` | the village (chunk 78), idle | walking four ways, ✕ now and then |
| `menu` | the village | the field menu: open, down twice, into a page, back out |
| `overworld` | chunk 80, beside the village | walking right, up, left across chunk seams (neighbour streaming) |
| `battle-1` | a level-1 random battle on chunk 80 | ✕ every 40 frames, 2,400 frames |
| `interior` | an interior (chunk 330) | walking |
| `dungeon` | Trisken (chunk 372): named a dungeon when built, it's a town past the grid; the name stays so earlier results read the same | walking |
| `late` | the card's first save, loaded | walking |
| `flight` | flying over the world map from the late save | flying four ways |
| `battle-late` | a random battle on chunk 114 with the late party | ✕ every 40 frames, 2,400 frames |
| `cave` | the Holy Armor dungeon's ice cave (chunk 454) | walking |
| `tower` | the North Tower (chunk 433) | walking |
| `lamps` | an inn's bar (chunk 348), the Knight stood by its table lamps | standing |
| `door` | the opening, 300 frames before the Knight leaves the house into the village (chunk 326 to 78) | the New Game tape: ✕ on |

## Which checks use which scenes (research 16, epic LT-101)

| Check | Scenes |
|---|---|
| LT-105 lockstep checker | all of them |
| LT-108 / LT-110 scene capture and the unlit match (A1, A3) | `village`, `overworld`, `interior`, `dungeon` |
| LT-111 the native view (A4) | the field scenes, `menu`, `cutscene`, `boot` |
| LT-112 dynamic lighting (A5) | `village`, `overworld`, `interior`, `dungeon`, `cave`, `tower`, `lamps` |
| LT-113 60–120 fps interpolation (A6) | `village`, `overworld`, `flight` (smooth), `door` (snaps), the field scenes (morphs) |
| LT-114 widescreen and draw distance (A7) | `village`, `overworld` |
| LT-116 battles (A8), LT-129 battle logic (C5) | `battle-1`, `battle-late` |
| LT-118 draw paths spike | `flight`, `menu`, `cutscene`, `interior` (doors) |
| LT-120 / LT-121 sequencer hook and synth (B1, B2) | `cutscene`, `village`, `battle-1` |
| LT-124 native FMV (B5) | `boot` |
| LT-125 input and camera (C1) | `village`, `overworld` |
| LT-126 UI and text (C2) | `menu`, `cutscene` |
| LT-127 field systems (C3) | `village`, `overworld`, `interior`, `dungeon` |
| LT-130 saves and specials (C6) | `late` |
