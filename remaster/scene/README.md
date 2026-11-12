# lt-scene

Living Tool Remaster's scene capture (LT-108, Stage A1; research 16 §4.2–4.3). Observe hooks on
the engine's draw functions turn each game tick into one `Record`: what a native renderer needs
to draw that frame. A tick is one pass of `main()`'s loop, 30 Hz.

| Part | From | What's kept |
|---|---|---|
| Tick | `main()`'s `jal ExecTaskTree` (0x80177E48) to `jal ProcessLoadRequestQueue` (0x80177E50) | tick, frame, play time, current chunk, mode |
| Camera | the first `WorldChunk_DrawObjects` of the tick | view matrix (ctx+0x98), GTE H, OFX/OFY, 320/512 |
| Terrain | `WorldChunk_DrawObjects` (OVR02 0x801E080C) | chunk id, slot, world offset of a neighbour, GTE model-view |
| Groups | `WorldChunk_DrawObjectMesh` (OVR02 0x801E090C), called from the above | mesh and object, id = chunk × 2³² + mesh offset |
| Characters | `Model_DrawParts` (0x8019DD70 / 0x8019DD68), called from `Npc_DrawIfVisible` (OVR02 0x801E9E90, `(obj, index, model, …)`) | index, record, model pointer, position; per part: world matrix (ctx+0x11C), GTE model-view, the part record, its live vertices (+0x2C, count at +0x38) |
| Torches | `Field_DrawTorchFlames` (OVR02 0x801EB4AC) | the type-5 field object records (`D_801FD638`, count `D_801FD758`): light positions |
| VRAM moves | `SetDrawMove` (main 0x801A8D48) | each VRAM-to-VRAM copy set up in the tick: the field's 13 texture and palette animations (the GPU runs them with the tick's ordering table) |
| Character records (LT-119) | the tick's end (`jal ProcessLoadRequestQueue`) | all 87 records (`CHARACTERS`, 0x80146600 × 0x38), drawn or not, and `D_801CE80C` (holds the NPCs' animation), `D_801D732C` (bit 2 draws them), `D_801FDAEC` (records the actor loop runs). The machine's RAM a frame later is already the next tick's |

- **Hooks:** observe only, and nothing is written to the machine. The OVR02 hooks are guarded by OVR02's first words.
- **Ids:** stable across ticks:
  - a group's id is its chunk and mesh offset, whichever slot holds the chunk;
  - characters are keyed by index;
  - a torch's id is its list's generation and its index. The generation steps when another area's list loads.
- **Battle (A8):** comes later.

```sh
cd remaster
cargo build --release -p lt-scene
target/release/lt-scene capture "<Guardian's Crusade (USA).bin>" <set> [scene] [out.jsonl]   # AC1, AC3
target/release/lt-scene match "<…bin>" <hooks.jsonl> 4300 120                                # AC2
target/release/lt-scene cost "<…bin>" <set> [scene] [runs]                                   # AC4
```

`match` compares against `emu-host`'s `draw_hooks.rs`, which logs the same frames by single-stepping from power-on with the same tape:

```sh
cd emu-host
cargo run --release --example draw_hooks -- "<…bin>" <dir> 4300 120 $(../remaster/target/release/lt-scene args)
```

Run the tagged tests (@LT-108/1–4) with:

```sh
LT_DISC=… LT_TESTSET=… LT_DRAW_HOOKS=<dir>/hooks.jsonl cargo test --release -p lt-scene -- --ignored
```

## Results (2026-11-07, test set at core `psoxide-b27fff9`)

- **Village, one tick:** 600 records for 1,200 frames, each complete. Per tick:
  - the camera: H 892, offset (257, 112), 512 wide; view row 0 is (4635, 0, 4635), as research 16 measured;
  - chunks 78, 77 and 79 in slots 2, 1 and 3, with neighbours at (∓0x800, 0, −0x1000);
  - 47.8 groups;
  - 4 characters with 441 posed vertices;
  - 5 torches. One of them is parked at x = 0x7FFF.
- **Against draw_hooks.rs:** 60 ticks (frames 4,300–4,420 from power-on), 0 differences. The comparison covers the view, the chunks and groups, each character part's record, world matrix and vertices, and the torch pass. A one-byte change to a vertex in the log is reported as that character's part and vertex.
- **Ids:** no id names two things in any field scene. About 99 % of group and character draws carry the last tick's id. The torch list reloads twice in `flight` and gets new ids each time.
- **Cost:** the capture adds 0.057 ms per tick on the village scene (best of 5, release, M1 Pro).
- **Other scenes:** every field tick is complete in `overworld`, `interior` (one chunk), `dungeon`, `late`, `flight`, `menu` and `cutscene`. Ticks with no field (a battle, a fade) are counted but not checked.

## The draw-path census (LT-118)

Which code draws 3D at all, and what A1's capture misses. The result is research 16 §4.5.

```sh
target/release/lt-scene census "<…bin>" <set> [scene…]           # the functions that project, then each scene's calls by caller
target/release/lt-scene probe "<…bin>" <set> first-flight [dir]  # OVR24's first-flight sky scene, which the test set doesn't reach
target/release/lt-scene refs "<…bin>" <function>…                # what calls a function, and where its address is stored
LT_DISC=… cargo test --release -p lt-scene --test census -- --ignored   # @LT-118/1 (/2 runs without the disc)
```

- **How it finds them (`census.rs`):** every 3D vertex goes through `RTPS` or `RTPT`, so `scan` lists the gc-sys
  functions of main and all 25 overlays (read off the disc) that hold one. There are 12.
- **Entry stubs:** `entry_stubs` adds tiny functions that branch into one of them. Main's `FUN_8019dd68` enters
  `Model_DrawParts` past its first instruction, so a hook there alone misses the arena and some NPC draws.
- **Counting:** `Census` hooks every entry, observe only. It counts each call by caller, named for the overlay
  actually loaded there: `Code::function_at` compares RAM with each overlay file, with no signature words.
- **Static callers:** `static_callers` finds every `jal`; `pointers_to` finds stored words and `lui`/`addiu` pairs.
- **Results:** all 15 scenes take about 5 minutes. Rows 1 and 2 (terrain; characters, doors and chest lids) are the
  field's only 3D, and A1 captures both. The rest are billboards, shadows and point tests (LT-144), two story oddities
  (LT-142 event actors, LT-143 OVR24's sky scene) and battles (LT-116). `func_8018C65C` and the two
  `Model_ProjectAndCull*` helpers are never called in retail.
