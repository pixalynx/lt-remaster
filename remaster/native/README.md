# lt-native

Living Tool Remaster's native renderer (research 16 §6). A tick that `lt-scene` captures is resolved to assets by `lt-assets`, then drawn unlit, with its baked colours, through the game's own camera.
- **The Stage A gate (LT-110)** compares that with the frame the emulator made of the same tick.
- **The native view (LT-111)** takes the terrain and characters over and layers the game's 2D over them, sorted by the ordering table.
- **Dynamic lighting (LT-112)** lights that 3D with the area's sun or room light, real shadows and the lamps the art paints; the original look is one switch away.
- **Frames between ticks (LT-113)** draw it at 60 or 120 Hz while the game runs at its own 30.
- **Widescreen and draw distance (LT-114)** show it at 16:10, 16:9, 21:9 or the window's shape, and further than the game draws; the 2D layer stays 4:3 in the middle.
- **Characters without a slot (LT-119)**: the NPCs, doors and chests the game has no packet slot for (past the 4:3 edge, or past its five) are posed from their records and drawn too.

| Part | What it does |
|---|---|
| `gte` | RTPS in the GTE's integer arithmetic (UNR division, flags), and each draw path's polygon tests. **Terrain** (`WorldChunk_DrawObjectMesh`): flags & 0x7F818000, `nclip`, the farthest SZ in 1..0x3000, on screen. **Models** (`Model_DrawParts`): flags & 0xFFC40000, `nclip`, the farthest SZ < 0x3000. A `Float` mode projects in floating point instead. |
| `draw` | A tick's chunk groups and character parts as screen triangles, with each part's posed vertices. The camera is the view, H 892 and offset 257 (or 160), 112; the 512 mode's 1.6 x-stretch is in the view matrix already. |
| `raster` | wgpu: a colour image and a coverage mask. GTE vertices sit at integer + 0.5, so pixel centres are the points the PS1 samples. Texture coordinates are affine, and transparent texels (0x0000) are cut out. |
| `gate` | The comparison, the report image, and `explain`, which gives each mismatched pixel a cause. |
| `takeover` | Reads each ordering table as `Dbg_DrawOTag` starts it: every packet with its bucket, the terrain and character packets marked as set aside, the draw environment, the clear, VRAM, and the picture on screen. |
| `layer2d` | The packets not set aside, decoded by PSoXide's hardware renderer (`psx_gpu_render::Translator`), each vertex with its bucket. |
| `composite` | wgpu: native 3D (keeping each pixel's bucket; lit: a shadow map first, then a depth pass and the lit colour), then the 2D layer through PSoXide's shader (`shaders/layer2d.wgsl`), sorted against it, then a resolve pass. `render` leaves the picture on the GPU; `draw` reads it back. |
| `light` | LT-112: world space from the camera, smoothed normals, lamps found in the paint, the area rigs, the shadow-map fit, the lit pass's uniforms (`shaders/lit.wgsl`, `shaders/shadow.wgsl`). |
| `play` | The capture and takeover hooks on one machine (`Player`, used by the app too); each table paired with its tick and resolved; the native view of a tick, and framing it like the display. |
| `interp` | LT-113: the pose between two ticks (camera, chunk and part transforms, posed vertices), and when to snap instead. |
| `wide` | LT-114: the reach (aspect and distance), the game's object test widened, the grid neighbours read off the disc, and the count of characters in view nothing draws. |
| `crowd` | LT-119: the NPCs, doors and chests without a packet slot, posed from their records with a shadow of the game's animation. |
| `check` | LT-111's measurements (`lt-native check`), the lockstep runs with the takeover's and all the native view's hooks, LT-112's (`lt-native light`), the timing bench (`lt-native bench`), LT-113's (`lt-native motion`), LT-114's (`lt-native wide`) and LT-119's (`lt-native characters`). |

## The native view (LT-111)

The game draws every frame exactly as before; the native view never changes what it draws.
- **Observe only.** PSoXide times the GPU's linked-list DMA, its FIFO and `DrawSync` cycle by cycle. Taking packets out of the list (unlinking them, or zeroing their length) changes CPU and interrupt timing. Lockstep then diverged within a few ticks, first in the kernel's interrupt frame (0x8000E2AC). So the takeover only reads: at `Dbg_DrawOTag` it walks the table from its last entry (bucket 0xBFF, the farthest) to bucket 0, as the GPU will.
- **What's set aside** (drawn natively):
  - terrain: the packets in the field band (0x800C8000 + buffer × 0x8000) from the cursor the tick's first `WorldChunk_DrawObjects` gets;
  - characters: the part records `Model_DrawParts` fills when `Npc_DrawIfVisible` calls it (`part+0x28`, each of the 20 classes with its record size).
- **Everything else is the 2D layer:** the HUD, text, menus, torch flames, the sparkle trail, blob shadows and fades. Every packet still goes through the decoder in order, set aside or not, so the texture page, blend mode and draw area are the GPU's.

**Sorting.** A packet sits in bucket SZ/4 of its key vertex: chunk polygons key on the farthest vertex, models on the nearest. The GPU draws from the farthest bucket to the nearest. The native 3D writes each pixel's polygon bucket + 1 to an R32Float target. A 2D fragment shows only where the polygon under it is in a farther bucket. Within one bucket the GPU draws the later-linked packet first, so an effect linked after the terrain is behind it. Testing against per-pixel depth instead (bucket b as depth 4b) let a polygon's near pixels through a full-screen fade in a nearer bucket; buckets are what the painting gives.

**Colour.** The 2D pipelines hand the blender `8 × code`, and only a value's top five bits are colour, as in PSoXide, whose VRAM holds the code.
- A pixel only the 2D layer drew is read back as its code, replicated.
- Where 2D blended over native 3D, the 3D keeps its low bits, and a channel below 8 is black, so a fade-in from black is black. Before, the sparkles glowed through the dungeon's fade-in.

**The background.**
- A table that clears (isbg) starts from its colour.
- One that doesn't (a menu over the frozen field) starts from the draw buffer's VRAM, enlarged.
- The `SetDrawMove`s and the HUD are linked into bucket 0, so a tick's polygons sample VRAM as the table starts (`Assets::resolve_at`).

**Movies.** The movie player runs the display in 24-bit colour from a buffer of its own. When the display isn't on the last table's draw origin, the native view is the displayed picture, enlarged.

**On screen.** PSoXide's picture is the display area shifted by the screen position (GP1 06h/07h: 8 lines, a few pixels). `play::on_screen` places the native view the same way, so the app's two panes line up line for line.

### Checks

```sh
target/release/lt-native check "<Guardian's Crusade (USA).bin>" <set> [scene…] [--every N]
target/release/lt-native lockstep "<Guardian's Crusade (USA).bin>" <set> [scene…]
target/release/lt-native composite "<Guardian's Crusade (USA).bin>" <set> <scene> [--scale N] [--every F] [--out DIR]
LT_DISC=… LT_TESTSET=… cargo test --release -p lt-native --test takeover -- --ignored --test-threads=1   # @LT-111/1–4
```

For a field tick, `check` runs three times from one state, each clearing to magenta:
- **A:** as it is;
- **B:** without the tick's terrain and characters;
- **E:** with the tick's in-world 2D packets hidden (bucket > 0).

The native view is drawn to match: whole, 2D layer alone, and without those packets (1×, GTE-exact). It measures:
- **Coverage (holes):** A's drawn pixels against the native view's.
- **The 2D layer (doubled geometry):** B against the layer alone, within one 5-bit step. Terrain or a character in the layer would show where B is magenta.
- **Effects (sorting):** where A and E differ against where the native view does.

**At 2×,** the menu, the bedroom dialogue (cutscene) and the boot movie are compared with the emulated picture of the same tick. The check uses pixels only the 2D layer drew, and leaves out those within one game pixel of native 3D. Under a semi-transparent panel, a polygon edge that the GPU and wgpu place a pixel apart shows as polygon in one and clear in the other. Those edge pixels made the cutscene 96.6 % before; the panels themselves match.

### Results (2026-11-08, test set at core `psoxide-b27fff9`, a tick every 120 frames)

| Scene | Field ticks | Coverage | 2D layer = game without geometry | Effects agree |
|---|---|---|---|---|
| village | 10 | 99.989 % | 100 % of 40,717 px | 99.936 % |
| overworld | 4 | 99.999 % | 100 % of 17,788 px | 99.987 % |
| interior | 10 | 99.990 % | 100 % of 44,846 px | 99.682 % |
| dungeon | 9 | 99.989 % | 100 % of 250,183 px | 99.217 % |
| late | 10 | 99.998 % | 100 % of 45,993 px | 99.979 % |
| flight | 10 | 99.998 % | 100 % of 212,858 px | 99.984 % |
| cutscene | 12 | 99.916 % | 100 % of 163,154 px | 99.711 % |
| menu | 7 | 99.996 % | 100 % of 74,282 px | 99.943 % |

At 2×:
- menu: 100 % of 269,544 2D-only pixels within a step;
- cutscene: 99.988 % of 25,857;
- boot: 100 % of 3,506, and 4/4 movie ticks passed through pixel for pixel.

**Lockstep** with the takeover's hooks against the original code: every field scene's whole tape (1,200 frames, the cutscene 1,500) with no game state different outside the render masks. Over the 8 scenes the takeover read 4,910 tables and set aside 1,023,008 terrain and 797,275 character packets.

**In the app** (`--new-game` to the village, 85 s, Apple M1 Pro, 2×): 5,041 frames, none late, the core 4.7 ms a frame; 2,230 native ticks at 7.1 ms each (max 10.9), none dropped.

## Dynamic lighting (LT-112)

The lit look (`--look lit` in `lt-native composite`, the default in `lt-remaster`, L switches) lights the native terrain and characters. The 2D layer stays the game's, except that the characters' blob shadows (the subtractive quads at 0x80147DA0) are left out: real ones replace them.

**World space.** The capture has every draw's model-view matrix and the camera's view matrix (world to view; in the 512 mode it carries the 1.6 x-stretch). Inverting the view puts every corner in world space (the game's: Y down), where angles and distances are true. Positions on screen don't change: the lit pass draws the same triangles as the original look.

**Normals.** The field's chunks and characters are all prelit: no normals on the disc. They're computed as `remaster_preview` does: area-weighted face normals, smoothed across corners that share a position and face within 50°. Facing is from the GTE's winding (`(c − a) × (b − a)` faces the viewer). A chunk's are computed once per mesh; a character's every tick, from its pose.

**The light** (`shaders/lit.wgsl`, in linear light, through the ACES curve):
- **Albedo:** texture × baked colour / 128, as the original look. Where a real shadow falls, part of the baked darkening is lifted (`delit`), so a painted shadow under a real one isn't dark twice.
- **Ambient:** a hemisphere, sky above and ground below.
- **The main light:** a sun, or a room's light from above, with a shadow map.
- **Lamps:** up to 16 point lights.

**Rigs, by area** (`light::area`): outdoors (the grid's 324 chunks, Trisken, flight), rooms (the other chunks from 324 up), dungeons (the Cave of Fear 42, the North Tower 433–442, the Holy Armor dungeon 453–459, the switch castle 468–492, God's Tower 506–522, from research 03; 453, 455–457 and 506–522 are also the game's own dungeon list for wandering monsters, `D_801FC924`). The field camera looks along +Z, so the lights come from the far side: shadows fall toward the camera, where they're seen. With them from the camera's side the first try had every shadow hidden behind its caster.

**Shadows.** One 2048² map: GC's field camera sees a small patch from a fixed height, so one map fitted to it (the receivers on screen across the light; along it, 1,536 units toward the light past the nearest) keeps under a world unit a texel. Casters are every opaque polygon of each drawn chunk (all its groups, so a house just off screen still casts; kept on the GPU per mesh, drawn with the chunk's map to world space) and the characters posed this tick. Cut-out texels cast nothing. Four filtered comparisons soften the edge.

**Lamps from the paint.** The game has no light objects:
- `Field_DrawTorchFlames`, which the capture read as torches, draws grey chimney smoke (all 55 retail type-5 records are smoke): the first lit village showed its "torches" exactly on the chimney tops.
- No field fire is an animated texture: in the test scenes only water animates.

What marks a lamp, a fireplace or a brazier is its paint: the artists lit the surfaces round it with warm vertex colours brighter than neutral. Corners with red ≥ 200, green ≥ 130 and red ≥ blue + 60 (textured, modulated) are clustered within 160 units; a cluster of four or more painted positions is a lamp, 64 units above its centre, coloured by its paint, reaching 1.5 × its radius (at least 220). 45 retail chunks have such paint; the most are inn and house interiors (the bar of chunk 348 has five lamps). Nothing is added by hand.

**Speed** (60 fps at 1440p): the frame is composited on the GPU and stays there; `lt-remaster` composites on its window's own device and draws the texture as it is.
- A depth pass first: the lit shader then runs once a pixel. Apple's GPUs skip hidden pixels only for shaders that don't discard, and the cut-outs discard.
- The chunks' casters stay on the GPU per mesh.
- Vertex buffers are kept between ticks.
- A resolve pass does the colour codes the CPU used to.

Reading a 3584 × 1568 frame back cost 15–24 ms, and unpacking it as much again.

### Checks

```sh
target/release/lt-native light "<Guardian's Crusade (USA).bin>" <set> [scene…] [--every N]
target/release/lt-native bench "<Guardian's Crusade (USA).bin>" <set> [scene…] [--height 1440] [--every N]
target/release/lt-native composite "<Guardian's Crusade (USA).bin>" <set> <scene> --look lit [--scale N]
LT_DISC=… LT_TESTSET=… cargo test --release -p lt-native --test light -- --ignored --test-threads=1   # @LT-112/1–4
```

For a field tick, `light` draws the lit look (1×, GTE-exact) four ways:
- as it is;
- with no casters: where it's darker, real shadows fall;
- with no character casters: the characters' shadows;
- with no lamps: where the lamps light.

It also compares the original look with LT-110's unlit image of the same tick, pixel for pixel. `bench` times the lit view of every tick at the first scale with at least that many lines (7×, 1,568 lines, for 1440p), from the capture resolved to the GPU done, as the app runs it.

`LT_LIGHT_DEBUG=1|2|3|4` draws the main light's visibility, the normals, the shadow map's depth against the receiver's, or the lamps' light instead of the colour; `LT_LIGHT_TRACE=1` prints each tick's rig, fit and lamps.

### Results

2026-11-08, test set at core `psoxide-b27fff9`, Apple M1 Pro. `light` a tick every 120 frames, at 1×; `bench` every tick, at 7×. The machine was busy with other work throughout (load average 30–43), which shows in the 95th percentiles.

| Scene | Rig | Real shadows (of native pixels) | Characters' shadows | Lamps (pixels they light) | Original look = unlit image | Time a tick at 7×: mean (95th pct.) |
|---|---|---|---|---|---|---|
| village | outdoor | 13.24 % | 0.772 % | — | 100 % of 1,088,497 px | 8.95 ms (14.54) |
| overworld | outdoor | 7.32 % | 0.784 % | — | 100 % of 435,455 px | 9.07 ms (14.42) |
| interior | room | 15.74 % | 2.400 % | — | 100 % of 740,123 px | 7.82 ms (12.69) |
| dungeon (Trisken) | outdoor | 30.64 % | 0.474 % | — | 100 % of 836,073 px | 12.43 ms (22.03) |
| cave | dungeon | 13.44 % | 0.199 % | — | 100 % of 928,103 px | 7.82 ms (12.02) |
| tower | dungeon | 10.85 % | 0.065 % | — | 100 % of 1,088,640 px | 8.39 ms (13.12) |
| lamps | room | 8.01 % | 0.340 % | 5 (20.96 %) | 100 % of 597,730 px | 9.31 ms (15.23) |

- **60 fps at 1440p:** every scene's mean is well inside a 16.7 ms frame, with the picture 3584 or 2240 × 1568 (more than 1440p's 3.7 million pixels in the 512-wide modes).
- **In the app** at 7×, lit, New Game to the village (load average about 45): 9.2 ms a tick; the core held 59.29 Hz with 3 frames late, and the window dropped 145 of 5,041 pictures.
- **The lit look changes nothing else:** LT-111's `check` gives the same numbers as before, and LT-110's gate passes.

## Frames between ticks (LT-113)

The game ticks at 30 a second (`VSync(2)`), and about 110 timers and `TaskNode.tick` count ticks, so its logic stays at 30 (research 16 §4.1). The native view keeps the last two ticks and draws frames in between, one tick behind: `interp::between(A, B, t)`. `lt-remaster --fps 60` (the default) draws 2 frames a tick and `--fps 120` draws 4, each at its fraction of a tick after the tick came; `--fps 30` draws each tick once. The emulator never waits for it.

**In world space**, as SoH does, so a turning character doesn't flatten like paper:
- **The camera:** rotation slerped, with the 512 mode's 1.6 x-stretch kept apart as row scales; the eye lerped.
- **Chunks** stay where tick B has them, seen through the camera in between. A group either tick drew is drawn, so nothing pops in at the screen's edge.
- **Character parts:** each part's world rotation slerped (its scale kept apart as column scales), its position lerped, and its posed vertices lerped one by one. A character's vertex count is the same from tick to tick, so the morph animation blends too.

**Two ticks, two frames.** The game's world is the current chunk's: crossing into a neighbour moves every position by whole chunks. A chunk both ticks drew gives the shift, so walking across chunk seams blends like the rest.

**Snaps.** In these cases the frames show tick B as it is:
- no chunk in common (a warp, a door into another area);
- another mode, or a tick without the field;
- the camera's eye moving more than 768 units or turning more than 45° in one tick (a cut; walking moves it about 6);
- one character moving more than 256 units, for that character only.

The 2D layer stays at 30: every frame takes tick B's.

Draws between ticks are in floating point: `Gte::exact` carries the blended transform, and the GTE's integer tests run on it rounded. Vertices are `f32` throughout, so whole-unit vertices give the GTE-exact image as before.

### Checks

```sh
target/release/lt-native motion "<Guardian's Crusade (USA).bin>" <set> [scene…] [--every K]   # K frames a tick: 2 (60 Hz) or 4 (120)
LT_DISC=… LT_TESTSET=… cargo test --release -p lt-native --test interp -- --ignored --test-threads=1   # @LT-113/1–3
```

`motion` blends every pair of ticks K ways (the poses; nothing is drawn) and counts:
- **Moving frames shown twice:** a fixed point on the ground under the camera, on screen, frame by frame.
- **Evenness:** a tick's largest step over its smallest, for the ground and for the camera's turn.
- **Snaps** by reason, and **area changes** (the new current chunk not drawn the tick before) against them.
- **Character parts** whose vertices blend.

The tests also run lockstep with every hook the native view uses (capture and takeover, `check::lockstep_player`).

### Results

2026-11-08, test set at core `psoxide-b27fff9`, every pair of ticks.

| Scene | Pairs blended | Snaps | Area changes snapped | Moving frames shown twice (60 / 120 Hz) | Most uneven tick (60 / 120) | Parts blended |
|---|---|---|---|---|---|---|
| village | 598 of 598 | — | — | 0 of 1,166 / 0 of 2,332 | 1.002 / 1.003 | 2,363 of 2,376 |
| overworld | 261 of 262 | 1 mode | — | 0 of 466 / 0 of 932 | 1.002 / 1.003 | 556 of 558 |
| interior | 598 of 598 | — | — | 0 of 1,108 / 0 of 2,216 | 1.001 / 1.002 | 1,794 of 1,794 |
| dungeon (Trisken) | 518 of 520 | 1 no field, 1 mode | 1 of 1 (a doorway) | 0 of 636 / 0 of 1,272 | 1.003 / 1.004 | 1,946 of 1,952 |
| cave | 598 of 598 | — | — | 0 of 1,032 / 0 of 2,064 | 1.003 / 1.004 | 598 of 598 |
| tower | 598 of 598 | — | — | 0 of 1,196 / 0 of 2,392 | 1.006 / 1.009 | 598 of 598 |
| late | 598 of 598 | — | — | 0 of 712 / 0 of 1,424 | 1.001 / 1.002 | 1,196 of 1,196 |
| flight | 550 of 550 | — | — | 0 of 1,100 / 0 of 2,200 | 1.015 / 1.022 | 550 of 550 |
| cutscene | 748 of 748 | — | — | 0 of 108 / 0 of 216 | 1.001 / 1.001 | 864 of 864 |
| door | 490 of 491 | 1 no field | 1 of 1 (house to village) | — | — | 1,148 of 1,148 |

- **Camera turns** (flight: 282 ticks turning half a degree or more) step evenly: 1.000 at both rates.
- **No snap while walking**, including across chunk seams on the overworld. A warp (the village's tick, then an interior's) snaps for want of a shared chunk; a camera put 868 units away snaps as a cut.
- **The parts not blended** (19 of 7,384 in the walking scenes) changed vertex count between ticks; they're drawn at tick B.
- **Lockstep** with all the native view's hooks: the village and flight, whole tapes, no difference outside the render masks.
- **In the app** (New Game to the village, 2×, lit, M1 Pro):
  - `--fps 60`: 59.5 native frames a second at 6.4 ms each;
  - `--fps 120`: 117.9 at 7.1 ms;
  - neither dropped a tick, and the core was never late.

## Widescreen and draw distance (LT-114)

`Reach { aspect, distance }` says how much more than the game the view shows: `Reach::GAME` is 4:3 at distance 1, and draws exactly as before. A wider aspect adds whole game pixels to each side of the screen (`Reach::margin`: 85 a side at 16:9 in the 512 mode, 192 at 21:9). The game draws for its 4:3 screen in three places, and each is widened:
- **Objects.** `WorldChunk_DrawObjects` turns each map object's culling centre (in the chunk's header) through the chunk's model-view matrix and draws the object only if `|x| ≤ 0x800` and `-0x200 ≤ z ≤ 0x1000` in view space. `wide::object_in` is that test; at the game's reach it picks exactly the objects the game drew, every tick of every scene. Wider, the x limit grows by the extra screen at the object's depth (`left · z / H`), so an object just past the new edge keeps the slack the game gives one past its own. A longer distance scales the z limit; beyond the game's, an object must also be within the screen's height (the field camera looks down, so most of a longer distance is above the top edge).
- **Chunks.** The game holds four chunk slots: the current chunk and the neighbours ahead of the camera. On the overworld grid a wider view also needs the chunks beside those, and a longer one the rows beyond. `wide::load` reads them off the disc as each tick is resolved (once each), and `wide::frame` places them as `Field_ChunkStreamTask` places a neighbour: the view moved to the chunk's grid offset (`overworld::origin`; the captured offsets agree). Interiors, dungeons and caves are one chunk each.
- **Polygons.** The terrain path's off-screen test takes the wider screen, and its depth test 0x3000 × the distance (`gte::Bounds`).

**The 2D layer stays 4:3, in the middle.** The compositor's target grows by the margins; the 2D layer's view, scissors and dither move with the display area, so HUD, dialogue and menus come out pixel for pixel as at 4:3. Guardian's Crusade's field has no HUD at the screen's edges to anchor (its menus and dialogue are panels in the middle), so nothing moves outward. In-world effects drawn as 2D (torch flames, sparkles, blob shadows) are clipped at the 4:3 edges, as the game clips them.

**Menus over the field** show it as a 2D picture: while the field menu is open the game draws no field, only a dimmed copy of the last frame. That copy is 4:3, so the menu shows between black margins.

**Characters past the 4:3 edge** have no packet slot: `Npc_DrawIfVisible` gives one only to a character whose position projects within `-64..=576` on the 512-wide screen (`-60..=380` at 320) while a slot is free. Since LT-119 the crowd draws them (below). `wide::npc_gap` counts what's still left out: the placed characters in view that neither the game nor the crowd draws.

**In the app:** `lt-remaster --aspect 4:3|16:10|16:9|21:9|window --distance 1-4 --view both|native`. A steps the aspect and D the distance. `window` follows the window's shape as it's resized: the room beside the 4:3 picture, or the whole window with `--view native`. A movie stays 4:3 in the middle of the pane.

### Checks

```sh
target/release/lt-native wide "<Guardian's Crusade (USA).bin>" <set> [scene…] [--aspect 16:9,21:9] [--distance D] [--every N] [--out DIR]
target/release/lt-native wide … --distance 3 --tilt 30   # a camera turned up toward the horizon
target/release/lt-native wide … --aspect 16:10,16:9,21:9 --hud 2   # the 2D layer against 4:3
LT_DISC=… LT_TESTSET=… cargo test --release -p lt-native --test wide -- --ignored --test-threads=1   # @LT-114/1–4
```

For each field tick measured, `wide` draws the native 3D (1×, GTE positions) three ways at the wider aspect:
- **naive:** only what the game drew (its chunks, its objects): the empty corners a wider screen alone gives;
- **the view:** `wide::frame`;
- **everything:** every object of every chunk within two rows (`wide::everything`), culled by nothing but the draw paths: what there is to see.

It counts margin pixels with no 3D (naive and the view), and pixels everything covers that the view doesn't (missing), in the margins and in the middle. Every tick it also checks the object test against the objects the game drew, and counts the character gap.

`--tilt` turns the camera up toward the horizon (as a free camera will, LT-125) and counts the pixels the distance adds. `--hud` draws the same ticks at 4:3 and wider (2×) and compares the 2D layer's pixels.

### Results

2026-11-08, test set at core `psoxide-b27fff9`, a tick every 30 frames (the object test every tick).

| Scene | Ticks | Objects as the game's | Margins empty, the game's chunks only (16:9 / 21:9) | Margins empty, the view | of those on the grid | Chunks at 21:9 (game) | Characters in the margins without a slot (drawn since LT-119) |
|---|---|---|---|---|---|---|---|
| village | 40 | 1,797 of 1,797 | 0.01 % / 0.00 % | 0.01 % / 0.00 % | 0.005 % / 0.004 % | 240 (120) | 14 / 36 |
| overworld | 18 | 694 of 694 | 0.00 % / 0.00 % | 0.00 % / 0.00 % | 0.001 % / 0.001 % | 110 (47) | 8 / 17 |
| flight | 37 | 2,204 of 2,204 | 0.18 % / **2.32 %** | 0.00 % / **0.00 %** | 0.002 % / 0.002 % | 365 (148) | 4 / 18 |
| menu | 31 | 1,356 of 1,356 | 0.00 % / 0.03 % | 0.00 % / 0.03 % | 0.004 % / 0.025 % | 215 (93) | 5 / 14 |
| door (house, then village) | 33 | 1,267 of 1,267 | 2.97 % / 3.97 % | 2.97 % / 3.97 % | 0.003 % / 0.003 % | 183 (84) | 3 / 44 |
| dungeon (Trisken, then out) | 35 | 1,204 of 1,204 | 20.02 % / 23.22 % | 14.62 % / 17.61 % | 0.007 % / 0.007 % | 139 (81) | 22 / 40 |
| interior | 40 | 599 of 599 | 80.70 % / 89.35 % | the same | — | 40 (40) | 0 / 0 |
| late | 40 | 599 of 599 | 66.02 % / 76.52 % | the same | — | 40 (40) | 0 / 0 |
| cutscene | 50 | 749 of 749 | 54.37 % / 70.20 % | the same | — | 50 (50) | 2 / 42 |
| cave | 40 | 599 of 599 | 21.52 % / 14.72 % | the same | — | 40 (40) | 0 / 29 |
| tower | 40 | 599 of 599 | 0.00 % / 0.00 % | the same | — | 40 (40) | 0 / 0 |
| lamps | 40 | 599 of 599 | 50.84 % / 56.58 % | the same | — | 40 (40) | 0 / 0 |

- **The object test is the game's:** every chunk draw of every tick, 12,266 in all, picks exactly the objects the game drew.
- **Nothing that's there is left out:** pixels every object of every chunk near would cover and the view doesn't: 0 in the margins and in the middle, every scene, both aspects.
- **On the overworld grid** (ground everywhere) the margins are drawn to the edges: at most 0.025 % of their pixels empty.
- **The empty corners** a wider screen alone gives show in flight (2.32 % of the margins at 21:9, the bottom corners) and in the dungeon scene's walk out onto the grid. The neighbour chunks fill them. Elsewhere the game's own four chunks already reach the margins.
- **A room's margins are its void:** an interior, a cave or a tower is one chunk, and the view draws all of it in view; the empty margins are where the room ends (the door scene starts in the house).
- **Characters in the margins** without a slot (summed over the ticks measured): up to 44 at 21:9. Since LT-119 the crowd draws every one; the records are read right (0 misread).

**The 2D layer** (`--hud 2`, a tick every 60 frames, against 4:3):

| Scene | 2D pixels at 4:3 | Same in the middle (16:10 / 16:9 / 21:9) | In the margins |
|---|---|---|---|
| cutscene (naming, dialogue) | 1,559,002 | 100.000 % / 99.998 % / 100.000 % | 0 |
| menu | 2,070,889 | 100.000 % / 100.000 % / 99.999 % | 0 |
| village (effects) | 317,433 | 99.997 % / 99.997 % / 99.995 % | 0 |

The panels and text match pixel for pixel; the few pixels that differ (at most 0.005 %) weren't traced.

**Draw distance:**
- **With the game's camera** (16:9, distance 3, a tick every 60 frames) it draws about what distance 1 does: village 120 chunks (the game 60) and 1,255 objects (954); overworld 54 (24) and 564 (327); flight 209 (76) and 3,260 (1,262). The field camera looks down, so what's past the game's distance is above the top edge.
- **With a camera that looks further** (turned up 30° toward the horizon, `--tilt 30`, a tick every 120 frames), distance 3 fills what the game's distance leaves empty: village 93 chunks against 43, 48.99 % of the pixels; overworld 46 against 28, 71.60 %; flight 65 against 49, 0.91 % (the plane's camera, turned up, still looks mostly at the ground).
- The object limit (0x1000 × the distance) is what holds it: no polygon went past the polygons' own 0x3000.

**In the app** (New Game to the village, 2×, lit, 60 Hz, M1 Pro), no tick dropped and the core never late:
- `--aspect 21:9 --view native`: 59.5 frames a second at 6.8 ms each;
- `--aspect 16:9` beside the emulated picture: 59.0 at 7.3 ms;
- `--aspect window --view native` in a 1500 × 560 window (2.68:1): 59.5 at 6.3 ms.

## Characters without a slot (LT-119)

The game draws at most five NPCs (and four doors or chests) at once, and only those whose position projects within `-64..=576` (the 512 mode; `-60..=380` at 320): each takes a packet slot, whose part record holds the posed vertices `Model_DrawParts` draws. Past the 4:3 edge, or past the five, a character has no posed mesh. Three ways were weighed:
1. HackerGC's slot lift (12 slots, 8 drawn): a base only, not retail.
2. Pose them in the renderer from their records.
3. Widen the game's slot test with a hook: changes game state, breaking the observer rule.

**Option 2 is the one**, because the records hold everything the game poses from, and the native view only reads them. LT-114's NPC criterion now takes option 2: in a wider view, every character in view is drawn, the game's or the crowd's.

**What stays live without a slot** (`lt-native characters`; the capture now keeps the character records as each tick leaves them, `Record::actors`, since RAM a frame later is the next tick's):
- **Live:** the position (the walk scripts run), the facing (+0xC), the animation (+0x14).
- **Racing:** the frame (+0x30) and target pose (+0x1D) step every tick. The sequence steps whenever the weight is 0, and nothing moves the weight.
- **Frozen:** the current pose (+0x1C) and weight (+0x1F): the blend (`UpdateNpcPoseBlendState`) runs only in `Npc_DrawIfVisible`.
- **Not refreshed:** the height (+2) comes from the terrain only while a walker has its slot.

**How the crowd draws them** (`crowd::Crowd`, kept tick to tick by the session and the app):
- **Place:** a turn about Y of `0x800 − dir` (the game's sine table truncates), moved to the position. For a slotted character that is exactly the game's world matrix, and with the view, its model-view.
- **Pose:** the model's TMD (`D_801FD688[+0x16]`) and its pose block (+0x18), blended by `lt_core::mesh::blend` (`BlendTwoPoseAnimation`). From a slotted NPC's record, these are exactly its drawn vertices.
- **The shadow animation:** for each NPC without a slot, the crowd runs both of the game's steps itself (the sequence step from the record's table at +0x20, then the blend), starting from the record when it lost its slot. It predicts a slotted NPC's next state exactly on all but one of 1,980 ticks (that one an animation restarted by its script).
- **Doors** (kind 2) are the model's base pose. **Chests** (kind 3) are a body and a lid. The lid is lifted `lid × 2 + 0x28` and turned `lid << 5` about Y, where `lid` is +0x30. The turn is written over the body's facing, not added to it (`FUN_801a0ec0`; the game's own quirk, kept).
- **Which characters:** the game's own test (placed, not in an unloaded area, a model, not kind 1 or state 2, an interior's own, the first `D_801FDAEC` records, NPC drawing on), in view of the wider screen. The overworld's wanderers (from 0x50) are a separate loop with their own layout, left to the game.
- **The game's reach** draws none of them, so the 4:3 picture stays the game's.

### Checks

```sh
target/release/lt-native characters "<Guardian's Crusade (USA).bin>" <set> [scene…]   # live fields, rebuilt characters, the shadow step, chests
target/release/lt-native wide … --aspect 21:9 --out DIR   # <scene>-crowd.png: the tick with the most, lit at 2×: 4:3, wide without the crowd, wide with it
LT_DISC=… LT_TESTSET=… cargo test --release -p lt-native --test crowd -- --ignored --test-threads=1   # @LT-119/1–3
```

`characters` compares every character the game drew with what its record rebuilds. To check chests, which no scene draws, it moves the cave's chest in front of the Knight in a measuring run, at three lids and two facings, and compares the game's two parts.

### Results

2026-11-08, test set at core `psoxide-b27fff9`, every tick.

**What changes from one tick to the next**, for an NPC without a slot (with one, the weight changes every tick):

| Scene | NPC ticks without a slot | x / z | facing | frame, target pose | current pose, weight |
|---|---|---|---|---|---|
| village | 4,907 | 0.8 % / 1.5 % | 0.2 % | 85.3 % | 0 |
| door (the walk out of the house) | 3,801 | 2.7 % / 3.2 % | 0.2 % | 82.1 % | 0 |
| dungeon (Trisken) | 6,941 | 4.8 % / 4.7 % | 0.9 % | 96.4 % | 0 |
| interior | 2,392 | 0 / 0 | 0 | 75.0 % | 0 |

**Rebuilt from the records**: every character the game drew (NPCs and doors), its world matrix, model-view and vertices: village 1,780 of 1,780, door 705 of 705, dungeon 1,440 of 1,440, interior 1,198 of 1,198. A moved chest at lids 0, 0x10, 0x20 and a turned one: all four, both parts. The shadow step: 1,979 of 1,980 ticks.

**At the edge** (`lt-native wide`, a tick every 30 frames): the crowd draws, in the margins, 14 / 36 characters in the village at 16:9 / 21:9 (and 6 more at 16:9 inside the 4:3 view, past the game's five), 3 / 44 on the walk out of the house, 22 / 40 in Trisken, 2 / 42 in the bedroom scene, 5 / 14 behind the menu, and the cave's chest at 21:9 (29 ticks). Characters in view that nothing draws: 0 in every scene at both aspects. Trisken at 21:9 has a villager walking at the right edge, posed mid-stride, that the game alone leaves out.

**In the app** (New Game to the village, 21:9, 2×, lit, 60 Hz, M1 Pro under heavy load, load averages 8–20): 60 native frames a second, 10–15 ms each. Back-to-back runs with the crowd switched off gave the same within that noise (15.0 against 15.1 ms, 10.0 against 12.0 ms).

## The gate

**Emulated coverage.** Two runs start from one saved state:
- **A:** the game as it is.
- **B:** the same input, with the tick's terrain (`WorldChunk_DrawObjects`) and characters (`Model_DrawParts` from `Npc_DrawIfVisible`) skipped.
- **Clear colour:** both runs clear to magenta. A hook on `Dbg_PutDrawEnv` (0x801A4D18) sets the environment's background colour, so geometry that draws black, or the same green as flight's ground, still differs from the background.

**The tick's picture.** The GPU draws a tick during the next one, and the display flips after that. The tick's picture is the one where A and B differ and B shows the most magenta. It is read from VRAM at the display start: `display_rgba8` would slide the picture 8 lines down and a few pixels right, by the screen position the game sets.

**What counts.** A pixel is covered where A ≠ B. These pixels are excluded:
- where B isn't magenta: the HUD, effects, fades;
- a 4-pixel margin.

Semi-transparent overlays (the compass, torch flames) still differ where geometry lies under them.

```sh
cd remaster
cargo build --release -p lt-native
target/release/lt-native gate "<Guardian's Crusade (USA).bin>" <set> [scene…] [--every N] [--out DIR] [--float] [--why] [--all]
LT_DISC=… LT_TESTSET=… cargo test --release -p lt-native --test disc -- --ignored --test-threads=1   # @LT-110/1–3
```

**`--out`** writes `<scene>.png` for each scene's worst tick: emulated, native, mismatch, and the emulated frame without the tick's geometry. In the mismatch panel:

| Colour | Meaning |
|---|---|
| Grey | Both covered |
| Black | Neither |
| Green | Emulated only |
| Magenta | Native only |
| Blue | Excluded |

The images from the run below are in `docs/research/16-native-remaster/gate/`.

**`--why`** gives each mismatched pixel a cause. A cause is one of:
- a polygon the native tests dropped (and why);
- a draw path the capture doesn't see;
- an edge tie;
- a cut-out (a texel transparent on one side only).

## Results (2026-11-07, test set at core `psoxide-b27fff9`, a tick every 60 frames)

| Scene | Ticks | GTE-exact agreement (worst tick) | Float |
|---|---|---|---|
| village | 20 | 99.992 % (99.981) | 100.000 % |
| overworld | 9 | 99.999 % (99.993) | 100.000 % |
| interior | 20 | 99.997 % (99.988) | 99.625 % |
| dungeon | 14 | 99.988 % (99.963) | 99.887 % |
| late | 20 | 99.997 % (99.990) | 99.591 % |
| flight | 20 | 99.998 % (99.984) | 99.997 % |
| cutscene | 25 | 99.985 % (99.966) | 99.712 % |
| menu | 16 | 99.994 % (99.989) | 100.000 % |

- **The silhouettes line up.** The gate passes, and so does Stage A's hook layer.
- **What's left in GTE-exact mode:** 755 pixels over about 12.6 million compared, and none native-only. Every one is an edge tie: a pixel exactly on an edge or at a shared vertex of a drawn polygon, where PSoXide's rasterizer and wgpu's top-left rule break ties differently (LT-138).
- **Nothing else is left:** no polygon the native tests drop that the game draws, and no draw path the capture misses. A rare texel the game's VRAM has transparent where the disc atlas doesn't shows up as a cut-out: one pixel in flight when sampled every 120 frames.
- **Float projection costs up to 0.4 %,** where silhouettes meet the background. A modern renderer at the original resolution would show that much; the GTE-exact mode removes almost all of it.

### What the gate found on the way

- **The display offset.** `display_rgba8` slides the picture by the game's screen position (8 lines, about 3 pixels). Fully covered scenes don't show it; interiors did, as 6–10-pixel bands. The gate now reads VRAM.
- **Black geometry.** Wall tops and character outlines draw black, the same as the game's clear colour, so they looked uncovered. Both runs now clear to magenta.
- **Flight.** An earlier picture already differs a little between the runs, while the ground is still drawn in B. The tick's picture is now the one where B shows the background.
