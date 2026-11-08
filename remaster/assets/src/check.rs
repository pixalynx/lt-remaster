//! LT-109's checks: the test set's scenes played with the capture on and every tick resolved.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use lt_core::mesh::OwnedVram;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};
use lt_emu_hooks::Hooks;
use lt_scene::{hooks, Capture, Record};
use lt_testset::{Image, Machine, Scene};

use crate::gpu::{Cache, Gpu, Upload};
use crate::model::ModelSource;
use crate::{vram, Assets, Frame, MeshKey, Miss};

/// The disc's files as Living Tool reads them.
pub struct Disc {
    pub image: DiscImage,
    pub exe: PsxExe,
    pub table: FileTable,
}

impl Disc {
    pub fn open(path: &std::path::Path) -> Result<Disc, String> {
        let image = DiscImage::open(path).map_err(|e| e.to_string())?;
        let exe = PsxExe::from_disc(&image).map_err(|e| e.to_string())?;
        let table = FileTable::parse(&exe, image.sectors()).map_err(|e| e.to_string())?;
        Ok(Disc { image, exe, table })
    }

    pub fn files(&self) -> WorldDisc<'_> {
        WorldDisc {
            image: &self.image,
            table: &self.table,
            exe: &self.exe,
        }
    }
}

const CTX: u32 = 0x1F80_0000;

/// A machine playing with the capture's hooks.
pub struct Player {
    pub m: Machine,
    h: Hooks,
    cap: Capture,
    /// Time spent in [`Assets::resolve`].
    pub resolving: std::time::Duration,
}

impl Player {
    pub fn new(image: &Arc<Image>, scene: &Scene, state: &[u8]) -> Result<Player, String> {
        let mut m = Machine::from_state(image, &scene.card, state, scene.frame)?;
        let h = hooks();
        h.install(&mut m.cpu);
        Ok(Player {
            m,
            h,
            cap: Capture::new(),
            resolving: std::time::Duration::ZERO,
        })
    }

    /// One frame; the ticks that ended in it.
    pub fn frame(&mut self, pad: u16) -> Result<Vec<Record>, String> {
        lt_scene::check::frame(&mut self.m, &mut self.h, &mut self.cap, pad)?;
        Ok(self.cap.take())
    }

    pub fn vram(&self) -> &[u16] {
        self.m.bus.gpu.vram.words()
    }

    /// One frame with every tick that ended in it resolved: `each(machine, record, frame)`.
    pub fn play(
        &mut self,
        pad: u16,
        assets: &mut Assets,
        each: &mut dyn FnMut(&Machine, &Record, &Frame),
    ) -> Result<(), String> {
        for r in self.frame(pad)? {
            let m = &self.m;
            let t = std::time::Instant::now();
            let f = assets.resolve(&r, &|a: u32, l: usize| m.read(a, l), m.bus.gpu.vram.words());
            self.resolving += t.elapsed();
            each(m, &r, &f);
        }
        Ok(())
    }

    /// One frame with every tick that ended in it resolved.
    pub fn frames(
        &mut self,
        pad: u16,
        assets: &mut Assets,
    ) -> Result<Vec<(Record, Frame)>, String> {
        let mut out = Vec::new();
        self.play(pad, assets, &mut |_, r, f| out.push((r.clone(), f.clone())))?;
        Ok(out)
    }

    /// `lt_warp` (ctx+0x50 = chunk | 0x8000 once the field is idle), played until the field is
    /// idle on `chunk`, then `settle` frames more. Every tick on the way is resolved and handed to
    /// `each(assets, record, frame)`.
    pub fn warp(
        &mut self,
        chunk: u16,
        settle: u64,
        assets: &mut Assets,
        each: &mut dyn FnMut(&Assets, &Record, &Frame),
    ) -> Result<(), String> {
        let mut step = |p: &mut Player, assets: &mut Assets| -> Result<(), String> {
            for (r, f) in p.frames(0, assets)? {
                each(assets, &r, &f);
            }
            Ok(())
        };
        let mut n = 0;
        while !self.m.field().idle() {
            step(self, assets)?;
            n += 1;
            if n > 600 {
                return Err("the field never went idle".into());
            }
        }
        self.m.write(
            CTX + 0x50,
            &[(chunk & 0xFF) as u8, (chunk >> 8) as u8 | 0x80],
        );
        for _ in 0..10 {
            step(self, assets)?;
        }
        n = 0;
        while !(self.m.field().on_field() && self.m.field().cur == chunk) {
            step(self, assets)?;
            n += 1;
            if n > 900 {
                return Err(format!("never idle on chunk {chunk}"));
            }
        }
        for _ in 0..settle {
            step(self, assets)?;
        }
        Ok(())
    }
}

/// What a scene's ticks resolved to.
#[derive(Debug, Default)]
pub struct Tally {
    pub ticks: u64,
    pub groups: u64,
    pub groups_matched: u64,
    pub characters: u64,
    pub parts: u64,
    pub parts_matched: u64,
    /// Characters drawn per model source.
    pub sources: BTreeMap<String, u64>,
    /// Characters whose model no file holds.
    pub from_ram: u64,
    /// Group and part draws listed as missed, and the first few.
    pub group_misses: u64,
    pub part_misses: u64,
    pub misses: Vec<String>,
    pub chunks: BTreeSet<u16>,
    /// Milliseconds [`Assets::resolve`] took a tick.
    pub resolve_ms: f64,
}

impl Tally {
    pub fn add(&mut self, r: &Record, f: &Frame) {
        self.ticks += 1;
        self.groups += r.chunks.iter().map(|c| c.groups.len() as u64).sum::<u64>();
        self.groups_matched += f.chunks.iter().map(|c| c.groups.len() as u64).sum::<u64>();
        self.characters += r.characters.len() as u64;
        self.parts += r
            .characters
            .iter()
            .map(|c| c.parts.len() as u64)
            .sum::<u64>();
        self.parts_matched += f
            .characters
            .iter()
            .map(|c| c.parts.len() as u64)
            .sum::<u64>();
        for c in &f.characters {
            *self.sources.entry(c.source.to_string()).or_default() += 1;
            if matches!(c.source, ModelSource::Ram { .. }) {
                self.from_ram += 1;
            }
        }
        self.chunks.extend(f.chunks.iter().map(|c| c.chunk));
        for m in &f.misses {
            match m {
                Miss::Group { .. } => self.group_misses += 1,
                _ => self.part_misses += 1,
            }
            if self.misses.len() < 20 {
                self.misses.push(format!("tick {}: {m}", f.tick));
            }
        }
    }

    pub fn missed(&self) -> u64 {
        (self.groups - self.groups_matched) + (self.parts - self.parts_matched)
    }
}

/// Plays a scene's tape, resolving every tick into `assets`.
pub fn tally(
    image: &Arc<Image>,
    scene: &Scene,
    state: &[u8],
    assets: &mut Assets,
) -> Result<Tally, String> {
    let mut p = Player::new(image, scene, state)?;
    let mut t = Tally::default();
    for &pad in &scene.tape {
        p.play(pad, assets, &mut |_, r, f| t.add(r, f))?;
    }
    t.resolve_ms = p.resolving.as_secs_f64() * 1000.0 / t.ticks.max(1) as f64;
    Ok(t)
}

/// Every character model met in a scene, with its mesh: (signature, source, corners as uploaded).
pub fn models(assets: &Assets, sigs: &BTreeSet<u64>) -> Vec<(u64, String, Vec<u8>)> {
    sigs.iter()
        .filter_map(|&s| {
            let m = assets.model(s)?;
            Some((
                s,
                m.source.to_string(),
                crate::mesh::Corner::bytes(&m.mesh.corners),
            ))
        })
        .collect()
}

/// The animated textures over a scene (AC3).
#[derive(Debug, Default)]
pub struct Animation {
    pub ticks: u64,
    /// Ticks that wrote animated tiles, and the tiles written.
    pub ticks_written: u64,
    pub writes: u64,
    /// Writes that equal the tile decoded from the VRAM the GPU had made by the next tick.
    pub writes_checked: u64,
    pub writes_equal: u64,
    /// Distinct pixel contents per (texture, tile): animated tiles change.
    pub distinct: BTreeMap<(u64, usize), usize>,
    /// Move destinations: halfwords the view predicted, and those equal to the VRAM a tick later.
    pub moved: u64,
    pub moved_equal: u64,
    pub rects: Vec<vram::Rect>,
}

/// Plays a scene and checks each tick's animated tiles against the VRAM the GPU makes.
pub fn animation(
    image: &Arc<Image>,
    scene: &Scene,
    state: &[u8],
    assets: &mut Assets,
) -> Result<Animation, String> {
    let mut p = Player::new(image, scene, state)?;
    let mut a = Animation::default();
    let mut last: Option<(Record, Frame, Vec<u16>)> = None;
    let mut seen: HashMap<(u64, usize), BTreeSet<u64>> = HashMap::new();
    for &pad in &scene.tape {
        let mut done = Vec::new();
        p.play(pad, assets, &mut |m, r, f| {
            done.push((r.clone(), f.clone(), m.bus.gpu.vram.words().to_vec()))
        })?;
        for (r, f, now) in done {
            a.ticks += 1;
            if !f.writes.is_empty() {
                a.ticks_written += 1;
            }
            a.writes += f.writes.len() as u64;
            for w in &f.writes {
                seen.entry((w.texture, w.tile))
                    .or_default()
                    .insert(crate::tmd::fnv(&w.rgba));
            }
            if let Some((lr, lf, lv)) = &last {
                // The VRAM now has the last tick's moves done by the GPU.
                let predicted = vram::view(lv, &lr.moves);
                for mv in &lr.moves {
                    for y in 0..mv.from[3] as usize {
                        for x in 0..mv.from[2] as usize {
                            let i = (mv.to[1] as usize + y) * 1024 + mv.to[0] as usize + x;
                            a.moved += 1;
                            a.moved_equal += (predicted[i] == now[i]) as u64;
                        }
                    }
                }
                let whole = OwnedVram::whole(now.clone());
                for w in &lf.writes {
                    let tile = assets.texture(w.texture).map(|t| t.tiles[w.tile]);
                    if let Some(tile) = tile {
                        a.writes_checked += 1;
                        a.writes_equal += (vram::decode(&tile, &whole) == w.rgba) as u64;
                    }
                }
            }
            last = Some((r, f, now));
        }
    }
    a.distinct = seen.into_iter().map(|(k, v)| (k, v.len())).collect();
    a.rects = assets.animated.rects().to_vec();
    Ok(a)
}

/// The cache over a scene, a warp to `to`, and a warp back (AC4).
#[derive(Debug, Default)]
pub struct Warps {
    /// Uploads per phase: the scene, after the warp to `to`, after the warp back.
    pub phases: [Vec<Upload>; 3],
    /// Chunks drawn per phase.
    pub drawn: [BTreeSet<u16>; 3],
    pub ticks: [u64; 3],
    pub writes: u64,
}

fn note<G: Gpu>(w: &mut Warps, phase: usize, assets: &Assets, cache: &mut Cache<G>, f: &Frame) {
    w.drawn[phase].extend(f.chunks.iter().map(|c| c.chunk));
    w.ticks[phase] += 1;
    cache.prepare(assets, f);
}

/// Plays `scene` with the cache, then warps to `to` and back, `settle` frames after each.
pub fn warps<G: Gpu>(
    image: &Arc<Image>,
    scene: &Scene,
    state: &[u8],
    to: u16,
    settle: u64,
    assets: &mut Assets,
    cache: &mut Cache<G>,
) -> Result<Warps, String> {
    let mut p = Player::new(image, scene, state)?;
    let mut w = Warps::default();
    for &pad in &scene.tape {
        for (_, f) in p.frames(pad, assets)? {
            note(&mut w, 0, assets, cache, &f);
        }
    }
    let at0 = cache.stats.uploads.len();
    p.warp(to, settle, assets, &mut |a, _, f| {
        note(&mut w, 1, a, cache, f)
    })?;
    let at1 = cache.stats.uploads.len();
    p.warp(scene.chunk, settle, assets, &mut |a, _, f| {
        note(&mut w, 2, a, cache, f)
    })?;
    let ups = &cache.stats.uploads;
    w.phases = [
        ups[..at0].to_vec(),
        ups[at0..at1].to_vec(),
        ups[at1..].to_vec(),
    ];
    w.writes = cache.stats.writes;
    Ok(w)
}

/// Mesh keys uploaded in some uploads.
pub fn meshes(ups: &[Upload]) -> Vec<(MeshKey, String)> {
    ups.iter()
        .filter_map(|u| match u {
            Upload::Mesh { key, label, .. } => Some((*key, label.clone())),
            _ => None,
        })
        .collect()
}
