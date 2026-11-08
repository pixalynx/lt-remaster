# lt-assets

Living Tool Remaster's assets (LT-109, Stage A2; research 16 §6.4). `Assets::resolve` turns each
tick `lt-scene` captures into a `Frame` of assets the renderer uploads once:

| Draw | Matched to | How |
|---|---|---|
| Chunk group | a group Living Tool decodes from `chunk/n` (`lt_core::mesh::chunk_mesh`), from the disc or a project (`WorldFiles`) | The captured mesh offset is the group's `mesh_ofs`, and the group's eight lists in RAM must equal the file's |
| Character part | a TMD object in a file | Its part record's shape: the vertex count (+0x38) and the 20 polygon counts per class pass (+0x00) |
| Chunk atlas | `field_vram` + `build_atlas` | From the files |
| Model atlas | `build_atlas` over the emulated VRAM | Decoded the first time a model is seen in an area (NPC models sample the field's VRAM) |
| Animated tiles | the emulated VRAM, every tick | The tick's VRAM with its own `SetDrawMove`s applied |

How a character part is matched:
- **Through the model pointer.** An NPC's `model` argument is its TMD in RAM, relocated. The TMD's signature (counts and primitive bytes, not vertex positions) names it in the index.
- **By shape.** The party's parts were built from `sys/5`, `sys/6` or `sys/13` when the field loaded. Their `model` argument is a leftover NPC model, and RAM no longer holds their TMD, so the index finds the object with the part's shape instead.
- **The index.** It covers every TMD in the files that hold models: 999 on the USA disc, 661 of them distinct.
- **From RAM.** A model no file holds is read back from RAM and drawn the same way.

Every frame lists what didn't match (`Frame::misses`); nothing is guessed. Keys are content: a chunk is its id and file hash, a model its signature, a texture its pixels' hash. `gpu::Cache` uploads a key the first time a frame draws it:
- `gpu::Wgpu` puts it in vertex buffers of 16-byte corners and RGBA8 textures.
- `gpu::Counting` only counts.

What the VRAM looks like: the field copies 13 rects a tick: 8 texture strips (the water at (544, 256) among them) and 5 palette rows at (160, 480–484). The GPU runs a tick's copies during the next tick, so the VRAM a tick's frame samples is its end-of-tick VRAM with its own copies applied (`vram::view`).

```sh
cd remaster
cargo build --release -p lt-assets
target/release/lt-assets match "<Guardian's Crusade (USA).bin>" <set> [scene…]   # AC1, AC2
target/release/lt-assets vram "<…bin>" <set> [scene]                              # AC3
target/release/lt-assets cache "<…bin>" <set> [to] [--wgpu]                       # AC4: village, warp to 330 and back
LT_DISC=… LT_TESTSET=… cargo test --release -p lt-assets -- --ignored --test-threads=1   # @LT-109/1–4
```

## Results (2026-11-07, test set at core `psoxide-b27fff9`)

**Groups.** Every group drawn in the eight field scenes matches, 138,294 in all:

| Scene | Groups |
|---|---|
| village | 28,664 |
| overworld | 9,946 |
| interior | 5,257 |
| dungeon | 19,368 |
| late | 6,840 |
| flight | 36,637 |
| cutscene | 1,500 |
| menu | 30,082 |

**Characters.** Every part drawn in those scenes matches a file's model, and none come from RAM:
- the Knight is `sys/6#0`, and in the late save `sys/5`;
- the flight form is `sys/13`;
- the overworld's travellers are `sys/9`;
- NPCs and doors are actor models of the NPC packs (`npc/6#3`, `npc/6#10`, …).
- No part's shape is shared between models.

**From RAM.** With the NPC packs left out of the index, the village's NPCs are read back from RAM and give the same corners as the disc's models.

**VRAM.**
- In the village, all 600 ticks write their 3 animated tiles.
- 1,797 of 1,797 checked writes equal the tile decoded from the VRAM the GPU had made a tick later. The 2,348,080 moved halfwords are all predicted.
- The water tiles go through 32 distinct frames.

**Cache.** 17 uploads in all, none twice:

| Phase | Uploaded |
|---|---|
| Village | 6 meshes (chunks 77, 78 and 79, the Knight, an NPC, a door) and 6 textures |
| Warp to 330 | chunk 330, its two NPC models and 2 textures |
| Warp back to 78 | nothing |

The same run on a wgpu device (Metal) reads every texture back equal to the assets' copy.

**Cost.** Resolving takes about 0.1–1.2 ms a tick. That was measured at a load average near 100, and the higher figures include the first ticks' decoding.

**Not covered.** A party model that no file holds can't be drawn: RAM keeps only its parts. It would have to be read from the packets `Model_DrawParts` draws, where each polygon's record starts with pointers to its live vertices (LT-137).
