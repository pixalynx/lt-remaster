# gc-sys

Guardian's Crusade's memory by name, generated from the decomp (LT-104, research 16): struct
layouts (`Ctx`, `Character`, `Part`, `ChunkSlots`), globals with their retail addresses (moved for a
HackerGC base by `Addrs::with_ranges`), and every function of main and the 25 overlays, with a
lookup that knows which overlay is loaded in a shared slot.

## Regenerating

`layouts.toml` names the fields; `src/generated.rs` is written from it and the decomp:

```sh
python3 app/crates/gc-sys/gen/gen.py --decomp ../psx-dev/gc_decomp          # writes src/generated.rs
python3 app/crates/gc-sys/gen/gen.py --decomp ../psx-dev/gc_decomp --check  # exits 1 if it's stale
```

It needs Python 3.11+ and a built decomp (its link maps). Each field cites the decomp struct field
it comes from, which `gen.py` measures with the decomp's own matched compiler (GCC 2.7.2 CDK), or a
decomp symbol, or a document; a field with no decomp type is checked live instead.

## Checks

- `cargo test -p gc-sys`: symbol order and lookup, shared overlay slots, fields and globals.
- `LT_TEST_BASE=<base package> cargo test -p gc-sys --test base`: a shifted HackerGC base maps every
  global and every main function exactly as `lt_core::layout::Layout` does.
- `cargo run --release -p lt-emu --example gc_sys_live -- <disc.bin>`: boots the retail disc in
  lt-emu-host to the village and reads every struct live (13 checks).
