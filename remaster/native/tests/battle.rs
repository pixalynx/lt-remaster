//! LT-116's criteria on the test set: battles drawn natively and lit. They need the disc, a test set
//! built from it (with the battle-1, battle-late and battle-start scenes) and a GPU:
//! `LT_DISC=<…bin> LT_TESTSET=<dir> cargo test --release -p lt-native --test battle -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::Arc;

use lt_assets::check::Disc;
use lt_assets::gpu::{Cache, Wgpu};
use lt_assets::model::ModelIndex;
use lt_assets::Assets;
use lt_native::check::{battle_scene, field_scene, guards, BattleTick};
use lt_native::composite::{Compositor, Picture};
use lt_native::light::{Area, Look, Rig};
use lt_native::play::{native_look, Probe, Session, Tick};
use lt_native::takeover::Shown;
use lt_scene::{ArenaLight, Drawer};
use lt_testset::{Image, Machine, Scene, SCENES};

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var(k).unwrap_or_else(|_| panic!("set {k}")))
}

/// The disc, its assets and a GPU, for `f`.
fn with<R>(f: impl FnOnce(&Arc<Image>, &mut Assets, &mut Cache<Wgpu>, &mut Compositor) -> R) -> R {
    let p = env("LT_DISC");
    let (image, disc) = (Image::open(&p).unwrap(), Disc::open(&p).unwrap());
    let files = disc.files();
    let mut assets = Assets::new(&files, ModelIndex::build(&files));
    let gpu = Wgpu::headless().expect("a GPU");
    let mut comp = Compositor::new(&gpu);
    let mut cache = Cache::new(gpu);
    f(&image, &mut assets, &mut cache, &mut comp)
}

fn scene(name: &str) -> (Scene, Vec<u8>) {
    Scene::load(&env("LT_TESTSET"), name).unwrap()
}

fn mean(v: impl Iterator<Item = f64>) -> f64 {
    let (s, n) = v.fold((0.0, 0u32), |(s, n), x| (s + x, n + 1));
    s / n.max(1) as f64
}

/// The battle's ticks every `every` frames (not while the transition swirls), each with the picture
/// the GPU made of it, played with `probe`'s changes.
fn battle_ticks(image: &Arc<Image>, name: &str, every: u64, probe: Probe, assets: &mut Assets) -> Vec<(Tick, Shown)> {
    let (s, state) = scene(name);
    let m = Machine::from_state(image, &s.card, &state, s.frame).unwrap();
    let mut session = Session::probing(m, probe).unwrap();
    let (mut out, mut last) = (Vec::new(), None);
    for &pad in &s.tape {
        for (tick, shown) in session.frame(pad, assets).unwrap() {
            if let Some(t) = last.take() {
                out.push((t, shown));
            }
            let r = &tick.record;
            if r.mode == lt_assets::BATTLE_MODE && r.camera.is_some() && tick.table.moves.is_empty() && r.frame % every < 2 {
                last = Some(tick);
            }
        }
    }
    out
}

/// `tick` with each battle part changed by `f`.
fn changed(tick: &Tick, f: impl Fn(&mut lt_assets::PartUse)) -> Tick {
    let mut frame = tick.frame.clone();
    for c in &mut frame.characters {
        for p in &mut c.parts {
            f(p);
        }
    }
    Tick { record: tick.record.clone(), frame, table: tick.table.clone(), crowd: tick.crowd.clone() }
}

fn rgb(p: &Picture, i: usize) -> [i32; 3] {
    [p.rgba[i * 4] as i32, p.rgba[i * 4 + 1] as i32, p.rgba[i * 4 + 2] as i32]
}

/// @LT-116/1: the level-1 battle (battle-1) and the late one (battle-late) are drawn natively and lit.
/// - Every actor, Toy and the arena are captured, resolved to their files (no part misses) and
///   drawn natively (their packets set aside): three actors in battle-1, four in battle-late (the
///   Baby among them).
/// - Every corner the GTE lights has its normal from the file (the arena's all; the Toys' and the
///   Knights' lit classes).
/// - Lit with those normals and the captured light, the original look keeps to the game's colours
///   (the GPU's dither apart); with the lighting taken off it doesn't.
/// - The lit look's rig is the game's battle light: its key light is the brighter of the world's
///   lights (`D_8014E440`), travelling the other way.
#[test]
#[ignore]
fn lt116_1_battles_drawn_natively_and_lit_with_real_normals() {
    with(|image, assets, cache, comp| {
        for (name, actors) in [("battle-1", 3), ("battle-late", 4)] {
            let (s, state) = scene(name);
            let ticks: Vec<BattleTick> = battle_scene(image, &s, &state, 60, Probe::default(), assets, cache, comp).unwrap();
            let with_actors: Vec<&BattleTick> = ticks.iter().filter(|b| b.actors > 0).collect();
            assert!(ticks.len() >= 8 && with_actors.len() >= 6, "{name}: {} battle ticks, {} with actors", ticks.len(), with_actors.len());
            for b in &ticks {
                assert!(b.arena && b.misses == 0 && b.aside > 300, "{name} f{}: arena {}, misses {}, set aside {}", b.frame, b.arena, b.misses, b.aside);
                assert!(b.lit_corners > 0 && b.with_normals == b.lit_corners, "{name} f{}: {} of {} lit corners have normals", b.frame, b.with_normals, b.lit_corners);
                assert!(b.abs <= 8.0, "{name} f{}: the original look {:.2} from the game", b.frame, b.abs);
            }
            assert_eq!(with_actors.iter().map(|b| b.actors).max(), Some(actors), "{name}: actors drawn");
            let (lit, unlit) = (mean(with_actors.iter().map(|b| b.abs)), mean(with_actors.iter().map(|b| b.abs_unlit)));
            println!("{name}: {} battle ticks; original look {lit:.2} from the game's colours, {unlit:.2} unlit; lit corners {}", ticks.len(), with_actors[0].lit_corners);
            assert!(lit < 0.75 * unlit, "{name}: lit {lit:.2} against unlit {unlit:.2}");
        }
        // The rig from the game's light.
        let ticks = battle_ticks(image, "battle-1", 300, Probe::default(), assets);
        let (tick, shown) = &ticks[1];
        let l = tick.record.battle_light.expect("the battle's light");
        let rig = Rig::for_record(&tick.record);
        let toward = lt_native::light::norm(l.llm[0].map(|v| v as f32));
        assert_eq!(rig.area, Area::Battle);
        assert!((0..3).all(|i| (rig.sun_dir[i] + toward[i]).abs() < 1e-5), "key {:?}, world light {:?}", rig.sun_dir, toward);
        let (o, lit) = (native_look(tick, shown, assets, cache, comp, 1, Look::Original), native_look(tick, shown, assets, cache, comp, 1, Look::Lit));
        let differ = (0..o.native.len()).filter(|&i| o.native[i] && rgb(&o, i) != rgb(&lit, i)).count();
        let native = o.native.iter().filter(|&&n| n).count();
        println!("lit look: {differ} of {native} native pixels lit differently from the original");
        assert!(differ * 2 > native);
    });
}

/// @LT-116/2: attack flashes and the arena light behave as in the original, then better.
/// - **Flashes:** battle-1 dims the arena and everyone in it for a spell (the back colour and the
///   colour matrix at 0.44 of the base). The original look follows it to the game's colours; the lit
///   look dims by about as much as the game (its albedo tinted by each actor's own light against the
///   base), and every flash of a single actor is captured with it.
/// - **The arena light** (forced on by a probe, as a Toy's ability sets it): the arena is drawn by
///   `FUN_8014c97c`, set aside whole, and the original look takes its colours as the game does
///   (the light's colour near it, the base colour past it). The game's actors keep their own light
///   (their registers hold no arena light), so the original look's actors are the same with it or
///   without. The lit look makes it a lamp falling off as the game's light does, raising only the
///   channels the light has over the base; better than the game, it lights the actors too.
///
///   (Forcing the light changes the machine's timing, `FUN_8014c97c` being another path than
///   `Model_DrawParts`: the game's picture is compared within the forced run, not with a plain one.)
#[test]
#[ignore]
fn lt116_2_flashes_and_the_arena_light() {
    with(|image, assets, cache, comp| {
        // Flashes.
        let (s, state) = scene("battle-1");
        let ticks = battle_scene(image, &s, &state, 30, Probe::default(), assets, cache, comp).unwrap();
        let dim: Vec<&BattleTick> = ticks.iter().filter(|b| b.arena_flash < 0.6).collect();
        let bright: Vec<&BattleTick> = ticks.iter().filter(|b| (b.arena_flash - 1.0).abs() < 0.01 && b.actors > 0).collect();
        let flashed = ticks.iter().filter(|b| (b.actor_flash - b.arena_flash).abs() > 0.05).count();
        assert!(dim.len() >= 3 && bright.len() >= 10 && flashed >= 2, "dim {}, bright {}, single actors flashing {flashed}", dim.len(), bright.len());
        for b in &dim {
            assert!(b.abs <= 8.0 && b.abs_unlit > 3.0 * b.abs, "f{}: {:.2} from the game, {:.2} unlit", b.frame, b.abs, b.abs_unlit);
        }
        let ratio = |f: &dyn Fn(&BattleTick) -> f64| mean(dim.iter().map(|b| f(b))) / mean(bright.iter().map(|b| f(b)));
        let (game, original, lit) = (ratio(&|b| b.bright_game), ratio(&|b| b.bright_original), ratio(&|b| b.bright_lit));
        println!("the dim: the game at {game:.2} of its brightness, the original look {original:.2}, the lit look {lit:.2}");
        assert!((original - game).abs() < 0.05 && (lit - game).abs() < 0.15 && lit < 0.6);

        // The arena light.
        let light = ArenaLight { offset: [0, 200, 0], strength: 400, rgb: [255, 70, 40] };
        let probe = Probe { arena_light: Some(light), ..Probe::default() };
        let (s, state) = scene("battle-1");
        let lit_ticks = battle_scene(image, &s, &state, 60, probe.clone(), assets, cache, comp).unwrap();
        assert!(lit_ticks.len() >= 8);
        for b in &lit_ticks {
            assert!(b.abs <= 8.0, "f{}: the lit arena {:.2} from the game", b.frame, b.abs);
        }
        let with_light = battle_ticks(image, "battle-1", 300, probe, assets);
        let (mut arena_red, mut arena_other, mut actor_red, mut actor_changed, mut n_arena, mut n_actor) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
        for (t, shown) in &with_light {
            let parts: Vec<_> = t.record.characters.iter().flat_map(|c| c.parts.iter().map(move |p| (c.by, p))).collect();
            assert!(parts.iter().any(|(by, p)| *by == Drawer::Arena && p.entry == 2 && p.arena_light == Some(light)), "f{}: the arena drawn with the light", t.record.frame);
            // No packet of the arena left to the 2D layer (`FUN_8014c97c`'s set aside).
            let lamp = native_look(t, shown, assets, cache, comp, 1, Look::Lit);
            let no_lamp = native_look(&changed(t, |p| p.arena_light = None), shown, assets, cache, comp, 1, Look::Lit);
            // The actors' pixels: where the original look changes with them taken out; and the
            // original look without the arena light.
            let full = native_look(t, shown, assets, cache, comp, 1, Look::Original);
            let mut arena_only = changed(t, |_| {});
            arena_only.frame.characters.retain(|c| c.index == lt_scene::ARENA_INDEX);
            let bare = native_look(&arena_only, shown, assets, cache, comp, 1, Look::Original);
            let unlit_arena = native_look(&changed(t, |p| p.arena_light = None), shown, assets, cache, comp, 1, Look::Original);
            for i in 0..lamp.native.len() {
                if !lamp.native[i] || lamp.layer[i] {
                    continue;
                }
                let (a, b) = (rgb(&lamp, i), rgb(&no_lamp, i));
                if rgb(&full, i) != rgb(&bare, i) {
                    n_actor += 1;
                    actor_red += (a[0] - b[0]) as i64;
                    actor_changed += i64::from(rgb(&full, i) != rgb(&unlit_arena, i));
                } else {
                    n_arena += 1;
                    arena_red += (a[0] - b[0]) as i64;
                    arena_other += ((a[1] - b[1]) + (a[2] - b[2])) as i64;
                }
            }
        }
        let per = |v: i64, n: i64| v as f64 / n.max(1) as f64;
        println!(
            "the arena light, lit look: arena red +{:.2}, green and blue {:+.2}; actors red +{:.2} over {n_arena} and {n_actor} pixels; the original look's actors changed by it at {actor_changed}",
            per(arena_red, n_arena),
            per(arena_other, 2 * n_arena),
            per(actor_red, n_actor),
        );
        assert!(per(arena_red, n_arena) > 4.0 && per(arena_other, 2 * n_arena).abs() < 1.0, "the lamp raises the light's channels over the base");
        assert!(n_actor > 1000 && per(actor_red, n_actor) > 4.0 && actor_changed == 0, "the lamp lights the actors; the game's light doesn't");
    });
}

/// @LT-116/3: effects sort correctly over native geometry; the battle transition shows the
/// remastered frame.
/// - In every battle tick sampled, the effects (2D packets in front of the arena) agree with the
///   game's where they show, the 2D layer matches, and the coverage is whole.
/// - battle-start walks into battle-1: the transition copies the field on screen into VRAM. The
///   native view's copy is its own picture of that frame, bit for bit (not the game's), the
///   transition's moves of columns and rows are made on it as the game makes them, the swirl's 2D
///   layer draws from it, and it's gone once the battle starts.
#[test]
#[ignore]
fn lt116_3_effects_sort_and_the_transition_is_remastered() {
    with(|image, assets, cache, comp| {
        for name in ["battle-1", "battle-late"] {
            let (s, state) = scene(name);
            let fs: Vec<_> = field_scene(image, &s, &state, 150, assets, cache, comp).unwrap().into_iter().filter(|f| f.mode == lt_assets::BATTLE_MODE && f.aside > 300).collect();
            assert!(fs.len() >= 5, "{name}: {} battle ticks measured", fs.len());
            for f in &fs {
                let pct = |a: u64, b: u64| 100.0 * a as f64 / b.max(1) as f64;
                println!("{name} f{}: covered {:.3}%, 2D layer {:.3}%, effects {:.3}% of {} pixels", f.frame, pct(f.covered_agree, f.pixels), pct(f.layer_equal, f.layer_pixels), pct(f.effect_agree, f.effect_pixels), f.effect_pixels);
                assert!(pct(f.covered_agree, f.pixels) > 99.9 && pct(f.layer_equal, f.layer_pixels) > 99.9 && pct(f.effect_agree, f.effect_pixels) > 98.0 && f.effect_pixels > 1000);
            }
        }

        // The transition.
        let (s, state) = scene("battle-start");
        let mut session = Session::new(image, &s, &state).unwrap();
        // Each draw area's last picture (the game copies a draw buffer: the frame last drawn there).
        let mut drawn: std::collections::HashMap<[u16; 2], Picture> = std::collections::HashMap::new();
        let (mut copied, mut moves, mut swirls, mut after) = (false, 0u32, 0u32, None);
        let mut copy: Option<([u16; 4], Vec<u8>, u32, u32)> = None;
        for &pad in &s.tape {
            for (tick, shown) in session.frame(pad, assets).unwrap() {
                // Each tick's own picture, kept by its draw area as the compositor keeps it.
                let before = comp.copied;
                let picture = native_look(&tick, &shown, assets, cache, comp, 2, Look::Lit);
                let first_copy = tick.table.moves.first().is_some_and(|m| m.from[2] == 320 && m.from[3] == 224);
                if first_copy {
                    // The copy is the native picture of the frame copied: the last drawn for that
                    // draw area.
                    let m = tick.table.moves[0];
                    let prev_picture = drawn.get(&[m.from[0], m.from[1]]).expect("a frame drawn there before the transition");
                    let (rect, c) = comp.frame_copy().expect("a frame copy");
                    assert_eq!(rect, [m.to[0], m.to[1], 320, 224]);
                    assert_eq!((c.w, c.h), (prev_picture.w, prev_picture.h));
                    assert!(c.rgba == prev_picture.rgba, "the copy is the native picture of the frame");
                    // Not the game's picture enlarged.
                    let differ = (0..(c.w * c.h) as usize).filter(|&i| {
                        let (x, y) = (i as u32 % c.w / 2, i as u32 / c.w / 2);
                        let o = ((y * shown.w + x) * 4) as usize;
                        (0..3).any(|k| c.rgba[i * 4 + k].abs_diff(shown.rgba[o + k]) > 24)
                    });
                    assert!(differ.count() > (c.w * c.h / 20) as usize, "the copy isn't the game's frame");
                    copied = true;
                    copy = Some((rect, c.rgba.clone(), c.w, c.h));
                } else if let Some((rect, rgba, w, h)) = copy.as_mut().filter(|_| !tick.table.moves.is_empty()) {
                    // The moves made on the copy as the game makes them on VRAM, at 2×.
                    for m in &tick.table.moves {
                        let [sx, sy, mw, mh] = m.from.map(|v| v as u32);
                        let (tx, ty) = (m.to[0] as u32, m.to[1] as u32);
                        let (cx, cy, cw, ch) = (rect[0] as u32, rect[1] as u32, rect[2] as u32, rect[3] as u32);
                        let inside = |x: u32, y: u32| x >= cx && y >= cy && x + mw <= cx + cw && y + mh <= cy + ch;
                        if !inside(sx, sy) || !inside(tx, ty) {
                            continue;
                        }
                        let src: Vec<u8> = (0..mh * 2).flat_map(|y| (0..mw * 2).flat_map(move |x| [((sy - cy) * 2 + y, (sx - cx) * 2 + x)])).flat_map(|(y, x)| rgba[((y * *w + x) * 4) as usize..((y * *w + x) * 4 + 4) as usize].to_vec()).collect();
                        for y in 0..mh * 2 {
                            for x in 0..mw * 2 {
                                let d = (((ty - cy) * 2 + y) * *w + (tx - cx) * 2 + x) as usize * 4;
                                let s = ((y * mw * 2 + x) * 4) as usize;
                                rgba[d..d + 4].copy_from_slice(&src[s..s + 4]);
                            }
                        }
                        moves += 1;
                    }
                    let _ = h;
                    let (_, c) = comp.frame_copy().expect("the copy kept through the transition");
                    assert!(c.rgba == *rgba, "f{}: the copy after {moves} moves", tick.record.frame);
                    swirls += u32::from(comp.copied > before);
                } else if copied && tick.table.moves.is_empty() && after.is_none() {
                    after = Some(comp.frame_copy().is_none());
                }
                drawn.insert([tick.table.display[0], tick.table.display[1]], picture);
            }
        }
        println!("battle-start: the frame copied, {moves} moves made on it, {swirls} swirl ticks drawn from it");
        assert!(copied && moves > 100 && swirls > 50 && after == Some(true));
    });
}

/// @LT-116/4: no field hook fires during battle. OVR02 (the field) and OVR04 (battle) share
/// 0x801DF000, and the field's hooks there are guarded by OVR02's words: in the three battle scenes
/// none of them is called in battle (OVR04's code runs at their addresses and the guard holds), no
/// `Model_DrawParts` call in battle is taken for a field character, an event actor or the first
/// flight's glow, and no battle hook fires outside battle. The capture and the takeover change
/// nothing in the machine (battle-start, through the transition and into battle, in lockstep).
#[test]
#[ignore]
fn lt116_4_no_field_hook_fires_in_battle() {
    let image = Image::open(&env("LT_DISC")).unwrap();
    assert!(SCENES.contains(&"battle-start"));
    let mut held = 0;
    for name in ["battle-1", "battle-late", "battle-start"] {
        let (s, state) = scene(name);
        let g = guards(&image, &s, &state).unwrap();
        let field: Vec<(&str, u64)> = g.field_calls_in_battle();
        held += g.hooks.iter().filter(|h| field.iter().any(|f| f.0 == h.0)).map(|h| h.3).sum::<u64>();
        println!("{name}: {} battle frames, {} Model_DrawParts calls in battle; field hooks in battle {field:?}", g.battle_frames, g.model_battle);
        assert!(g.battle_frames > 300 && g.model_battle > 300, "{name}: battle frames {}, draws {}", g.battle_frames, g.model_battle);
        assert!(field.len() >= 5 && field.iter().all(|f| f.1 == 0), "{name}: {field:?}");
        assert_eq!((g.field_in_battle, g.battle_outside), (0, 0), "{name}");
    }
    println!("field hooks held back by their guard words in battle: {held}");
    assert!(held > 0, "OVR04's code ran at a field hook's address and its guard held");
    let (s, state) = scene("battle-start");
    let (report, ticks) = lt_native::check::lockstep_player(&image, &s, &state).unwrap();
    println!("battle-start in lockstep with the native view's hooks: {} frames, {ticks} ticks captured", report.frames);
    assert!(report.divergence.is_none() && ticks > 300);
}

/// The battle ticks LT-149 measures: battle-1's and battle-late's, every 120 frames, with sky lines
/// (a 2D pixel at the 4:3 picture's edge above the battle's draw area, the status panel's top).
fn sky_ticks(image: &Arc<Image>, assets: &mut Assets) -> Vec<(String, Tick, Shown)> {
    let mut out = Vec::new();
    for name in ["battle-1", "battle-late"] {
        for (t, shown) in battle_ticks(image, name, 120, Probe::default(), assets) {
            out.push((name.to_string(), t, shown));
        }
    }
    out
}

/// A widened picture's margin pixels on the sky lines that nothing drew, and the margin pixels on
/// those lines: the lines where the 4:3 picture `game` has a 2D pixel at its edge columns, above the
/// battle's draw area's bottom (`bottom`, picture lines).
fn empty_sky(wide: &Picture, game: &Picture, left: u32, bottom: u32) -> (u64, u64) {
    let (mut empty, mut checked) = (0, 0);
    for y in 0..bottom.min(game.h) {
        let edge = |x: u32| {
            let i = (y * game.w + x) as usize;
            game.layer[i] && !game.native[i]
        };
        if !(edge(0) && edge(game.w - 1)) {
            continue;
        }
        for x in (0..left).chain(left + game.w..wide.w) {
            let i = (y * wide.w + x) as usize;
            checked += 1;
            empty += u64::from(!wide.native[i] && !wide.layer[i]);
        }
    }
    (empty, checked)
}

/// @LT-149/1: at 16:9 and 21:9, battle-1's and battle-late's margins hold the sky: on every line
/// where the 4:3 picture's edges are sky (above the status panel), no margin pixel is left empty.
/// Without the sky carried on (the table not taken for a battle's), they were.
#[test]
#[ignore]
fn lt149_1_the_battle_sky_reaches_the_margins() {
    use lt_native::play::native_at;
    use lt_native::wide::Reach;
    with(|image, assets, cache, comp| {
        let ticks = sky_ticks(image, assets);
        let (mut lines_ticks, mut empty, mut checked, mut empty_before) = (0, 0, 0, 0);
        for aspect in [16.0 / 9.0, 21.0 / 9.0] {
            let reach = Reach::new(aspect, 1.0);
            for (name, t, shown) in &ticks {
                let left = reach.margin(t.table.display[2] as u32);
                let bottom = t.table.clip3d.map_or(0, |c| (c[3] + 1).saturating_sub(t.table.display[1]) as u32);
                let game = native_at(t, shown, assets, cache, comp, 1, Look::Original, Reach::GAME);
                let wide = native_at(t, shown, assets, cache, comp, 1, Look::Original, reach);
                let mut plain = Tick { record: t.record.clone(), frame: t.frame.clone(), table: t.table.clone(), crowd: t.crowd.clone() };
                plain.table.battle = false;
                let before = native_at(&plain, shown, assets, cache, comp, 1, Look::Original, reach);
                let (e, c) = empty_sky(&wide, &game, left, bottom);
                let (eb, _) = empty_sky(&before, &game, left, bottom);
                assert_eq!(e, 0, "{name} f{} at {aspect:.3}: {e} of {c} margin pixels on the sky lines empty", t.record.frame);
                lines_ticks += u32::from(c > 0);
                (empty, checked, empty_before) = (empty + e, checked + c, empty_before + eb);
            }
        }
        println!("{lines_ticks} ticks with sky lines at 16:9 and 21:9: {checked} margin pixels on them, {empty} empty ({empty_before} without the sky carried on)");
        assert!(lines_ticks >= 8 && checked > 10_000 && empty == 0 && empty_before > checked / 4);
    });
}

/// @LT-149/2: the 4:3 middle of a widened picture and the status panel in it are unchanged, at 16:9
/// and 21:9 (the original look at 1×, the game's positions):
/// - the sky carried on changes no pixel of the middle (only the margins);
/// - every 2D pixel of the middle (the panel, the game's own sky, the effects) is the 4:3 picture's.
///
/// (The native 3D's own pixels can differ, a polygon's edge or the winner where two lie at one depth
/// landing a pixel apart in the widened target: LT-114's, counted here.)
#[test]
#[ignore]
fn lt149_2_the_middle_and_the_panel_are_unchanged() {
    use lt_native::play::native_at;
    use lt_native::wide::Reach;
    with(|image, assets, cache, comp| {
        let ticks = sky_ticks(image, assets);
        let (mut layer_px, mut compared, mut margins_changed, mut three_d) = (0u64, 0u64, 0u64, 0u64);
        for aspect in [16.0 / 9.0, 21.0 / 9.0] {
            let reach = Reach::new(aspect, 1.0);
            for (name, t, shown) in &ticks {
                let left = reach.margin(t.table.display[2] as u32);
                let game = native_at(t, shown, assets, cache, comp, 1, Look::Original, Reach::GAME);
                let wide = native_at(t, shown, assets, cache, comp, 1, Look::Original, reach);
                let mut plain = Tick { record: t.record.clone(), frame: t.frame.clone(), table: t.table.clone(), crowd: t.crowd.clone() };
                plain.table.battle = false;
                let before = native_at(&plain, shown, assets, cache, comp, 1, Look::Original, reach);
                let (mut by_copies, mut layer_differ) = (0, 0);
                for y in 0..game.h {
                    for x in 0..game.w {
                        let (g, w) = ((y * game.w + x) as usize, (y * wide.w + x + left) as usize);
                        compared += 1;
                        by_copies += u32::from(rgb(&wide, w) != rgb(&before, w));
                        if game.layer[g] && !game.native[g] && !wide.native[w] {
                            layer_px += 1;
                            layer_differ += u32::from(rgb(&game, g) != rgb(&wide, w));
                        } else {
                            three_d += u64::from(rgb(&game, g) != rgb(&wide, w));
                        }
                    }
                    margins_changed += (0..left).chain(left + game.w..wide.w).filter(|&x| rgb(&wide, (y * wide.w + x) as usize) != rgb(&before, (y * wide.w + x) as usize)).count() as u64;
                }
                assert_eq!((by_copies, layer_differ), (0, 0), "{name} f{} at {aspect:.3}: middle pixels changed by the sky carried on, 2D pixels unlike the 4:3 picture's", t.record.frame);
            }
        }
        println!("the 4:3 middle: {compared} pixels at 16:9 and 21:9, none changed by the sky carried on; {layer_px} 2D pixels (panel, sky, effects) all the 4:3 picture's; native 3D pixels whose winner differs: {three_d}; {margins_changed} margin pixels the sky fills");
        assert!(compared > 1_000_000 && layer_px > 500_000 && margins_changed > 10_000);
    });
}
