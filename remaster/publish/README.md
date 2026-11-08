# lt-remaster

Living Tool Remaster: Guardian's Crusade from the player's own disc, running the game's own code on
the [PSoXide](https://github.com/EBonura/PSoXide-emulator) core, drawn by a native renderer. It is
lit, widescreen, and runs at 60 or 120 frames a second. [Living Tool](https://livingtool.app), a mod
tool for the game, ships it as `lt-remaster` beside the app. Its Play screen's **Remastered** switch
moves the running game into this program's window.

This repository is the complete source of the `lt-remaster` that ships with Living Tool. It is
published from Living Tool's own repository: each commit here holds exactly the folders
`lt-remaster` builds from, at the paths they have there, as a Living Tool build had them. A Living
Tool release only builds when the program it ships is here. Tags `vX.Y.Z` mark `lt-remaster`'s
versions.

Nothing from the game is in here: the program asks for the player's own disc (Guardian's Crusade,
USA, with its retail boot EXE) and reads everything from it.

## What's here

| Path | What |
|---|---|
| `remaster/` | The program (`app/`, `lt-remaster`) and its crates: the scene capture, the asset resolver, the native renderer and lighting, the lockstep checker and the test set. See `remaster/README.md` |
| `emu-host/` | Living Tool's emulator program and its library (`lt-emu-host`, `lt-emu-protocol`, `lt-emu-hooks`), whose protocol server and machine `lt-remaster` uses. Also published on its own at <https://github.com/pixalynx/lt-emu-host> |
| `app/crates/lt-core/` | Living Tool's reader and writer for the game's disc formats, which the native renderer reads assets with |
| `app/crates/gc-sys/` | The game's memory by name: struct layouts, globals and functions, generated from the decompilation |
| `app/Cargo.toml`, `app/src/data/` | What those two crates read from outside their folders: the workspace's version and licence, and two data files |

## Build

```sh
cd remaster
cargo build --release -p lt-remaster        # target/release/lt-remaster
target/release/lt-remaster "<Guardian's Crusade (USA).bin>"
```

The toolchain is pinned in `remaster/rust-toolchain.toml`. Linux needs `libasound2-dev libudev-dev
libxkbcommon-dev libwayland-dev`.

## Licence

GPL-3.0-or-later (`LICENSE`). The core it links, PSoXide, is GPL-2.0-or-later, from the fork
[`pixalynx/PSoXide-emulator`](https://github.com/pixalynx/PSoXide-emulator). The toolbar font,
Inter, is under the SIL Open Font License 1.1 (`remaster/app/fonts/OFL.txt`).
