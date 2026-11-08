# lt-lockstep

Living Tool Remaster's lockstep checker (LT-105), the safety net for every native replacement.

Two machines start from the same test-set scene (`lt-testset`) and play the same input tape:
- side A runs the original code;
- side B runs with hooks, and later with native replacements.

After every frame (two per game tick) their main RAM and scratchpad are compared, less any masks.

On the first difference the report gives:
- the frame and game tick;
- the address and its `gc-sys` name (`CHARACTERS[0].talk`, `CTX+0x43`);
- what changed it. The checker plays that frame again on both sides together, an instruction or a hook call at a time:
  - when the byte changes, it names the store's function, or the hook that wrote it;
  - when control flow splits first (a replace hook skipped a function), it runs each side on alone and names the last code that changed the byte on each;
- the last hook called and the functions run before the cause, by decomp name.

A mask is a `Mask { name, addr, len, why }`; a mask without a reason a reviewer can judge doesn't
belong. The report lists every mask given and how many bytes differed in it.
`RENDER_MASKS` holds the field's packet bands, the ordering tables and the actor packets, for
Stage A's renderer (LT-111).

```sh
cd remaster
cargo build --release -p lt-lockstep
target/release/lt-lockstep check "<Guardian's Crusade (USA).bin>" <set>    # observe hooks, all 11 scenes
target/release/lt-lockstep plant "<Guardian's Crusade (USA).bin>" <set>    # a planted byte, found and traced
target/release/lt-lockstep masks "<Guardian's Crusade (USA).bin>" <set>    # a masked terrain replacement
LT_DISC="<…bin>" LT_TESTSET=<set> cargo test --release -p lt-lockstep -- --ignored   # @LT-105/1–4
```

## As a library

```rust
let report = lt_lockstep::run(&image, &scene, &state, &lt_lockstep::original, &my_side, &masks)?;
print!("{}", report.text());
```

A side is a function that builds its `Hooks` and handler, called again for each re-run, so a
handler with state starts fresh. It gets the frame being run. Handlers change the game's memory
only through `Call::write` (`lt-emu-hooks`). `gc::observer`, `gc::planter` and
`gc::terrain_skipper` are examples.

## Results (2026-11-07, M1 Pro, test set built at core `psoxide-b27fff9`)

- **check:** all 11 scenes, 17,100 frames (8,550 ticks), identical after every frame with 13
  observe hooks. A ends on the test set's digest in each, in 139 s. OVR04's guarded camera hook
  fires in the three scenes that reach a battle (`overworld` meets one on the way).
- **plant:** one bit of `CHARACTERS[0].talk`, flipped by a hook at village frame 300, is reported
  after frame 300 (tick 150) as `CHARACTERS[0].talk`, written by the hook. With the Knight's `x`
  planted and masked, the game's own code carries it on: `Model_DrawParts+0x564` stores a
  different screen vertex into the Knight's packets the same frame.
- **masks:** a bare return from `WorldChunk_DrawObjectMesh` (no terrain packets) differs in all
  three render masks. The first difference outside them is real game state:
  - `CHARACTERS[2]+0x1d`, NPC 2's animation frame byte, is stored by `NPC_UpdateActors+0x394` on B only;
  - its animation step is gated on `D_801CE80C`, which the terrain draw path affects.

  Stage A has to keep that (LT-111).
