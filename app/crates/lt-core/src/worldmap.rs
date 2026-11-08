//! Rendered views of the world archive: one chunk from above, the composed
//! 36×18 overworld and a sheet of interior thumbnails (port of `gc_world.py`
//! `export_world`, rasterised instead of exported).
//!
//! Overworld placement (`docs/screens/09-world-viewer.md` §4.1, `gc_world.py`
//! minimap): with `k` pixels per chunk (4096 units), chunk (gx, gy) covers
//! columns `gx·k/2 … +k` and rows `gy·k + k/2 … +k` of an `18k × 18k` image
//! that wraps toroidally; the band above row 0 is the bottom row's lower half.

use serde::Serialize;

use crate::error::Result;
use crate::fs::FileTable;
use crate::npc::NpcPack;
use crate::overlay::Staged;
use crate::placement;
use crate::raster::{draw_mesh, Canvas, Ortho, Style};
use crate::tim::Tim;
use crate::trigger::{TriggerGroup, TriggerRecord};
use crate::vram::Vram;
use crate::world::{self, CHUNK_COUNT, CHUNK_SECTORS, GRID_CHUNKS, GRID_W, TIM_OFFSET};
use crate::{DiscImage, LtError, PsxExe};

/// Read access to the files a world render needs.
pub struct WorldDisc<'a> {
    pub image: &'a DiscImage,
    pub table: &'a FileTable,
    pub exe: &'a PsxExe,
}

impl WorldDisc<'_> {
    /// `chunk/n`: 125 sectors at the world archive's LBA + n·125.
    pub fn chunk(&self, id: u32) -> Result<Vec<u8>> {
        let world = self.table.by_id("world").ok_or_else(|| LtError::FileNotFound("world".into()))?;
        let lba = world.extent.lba + id * CHUNK_SECTORS;
        if world::is_new_area(id) {
            // Zeros until made, unless a base image already holds it.
            if lba + CHUNK_SECTORS <= self.image.sectors() {
                return self.image.read_sectors(lba, CHUNK_SECTORS);
            }
            return Ok(vec![0; world::CHUNK_BYTES]);
        }
        if id >= CHUNK_COUNT {
            return Err(LtError::FileNotFound(format!("chunk/{id}")));
        }
        self.image.read_sectors(lba, CHUNK_SECTORS)
    }

    /// A whole file by canonical id (`sys/7`, `fe/0`, `ovr/2`, `npc/4`).
    pub fn file(&self, id: &str) -> Result<Vec<u8>> {
        let e = self.table.by_id(id).ok_or_else(|| LtError::FileNotFound(id.into()))?;
        self.image.read_sectors(e.extent.lba, e.extent.sectors)
    }
}

/// The files the world readers use: the retail [`WorldDisc`], or a
/// [`ProjectWorld`] that serves a project's staged copies first.
pub trait WorldFiles: Sync {
    fn exe(&self) -> &PsxExe;
    /// `chunk/n` (0–523).
    fn chunk(&self, id: u32) -> Result<Vec<u8>>;
    /// A whole file by canonical id.
    fn file(&self, id: &str) -> Result<Vec<u8>>;
}

impl WorldFiles for WorldDisc<'_> {
    fn exe(&self) -> &PsxExe {
        self.exe
    }
    fn chunk(&self, id: u32) -> Result<Vec<u8>> {
        WorldDisc::chunk(self, id)
    }
    fn file(&self, id: &str) -> Result<Vec<u8>> {
        WorldDisc::file(self, id)
    }
}

/// The disc as an open project sees it: staged chunks and files first. Pass
/// the project's EXE in `disc.exe` when the EXE is staged.
pub struct ProjectWorld<'a> {
    pub disc: WorldDisc<'a>,
    pub staged: &'a Staged,
}

impl WorldFiles for ProjectWorld<'_> {
    fn exe(&self) -> &PsxExe {
        self.disc.exe
    }
    fn chunk(&self, id: u32) -> Result<Vec<u8>> {
        match self.staged.get(&format!("chunk/{id}")) {
            Some(b) => Ok(b.as_ref().clone()),
            None => self.disc.chunk(id),
        }
    }
    fn file(&self, id: &str) -> Result<Vec<u8>> {
        match self.staged.get(id) {
            Some(b) => Ok(b.as_ref().clone()),
            None => self.disc.file(id),
        }
    }
}

/// The shared field texture sources: SYS08 atlas (`sys/7`) and the 57 region
/// sets (`fe/0..56`), plus `ChunkTextureSetIds`.
pub struct WorldTextures {
    atlas: Option<Vec<u8>>,
    sets: Vec<Option<Vec<u8>>>,
    pub texset_ids: Vec<u16>,
}

impl WorldTextures {
    pub fn load(d: &(impl WorldFiles + ?Sized)) -> Self {
        let atlas = d.file("sys/7").ok();
        let sets = (0..57).map(|k| d.file(&format!("fe/{k}")).ok()).collect();
        let texset_ids = world::texset_ids(d.exe()).unwrap_or_default();
        Self { atlas, sets, texset_ids }
    }

    /// Both shared sources were readable.
    pub fn complete(&self) -> bool {
        self.atlas.is_some() && self.sets.first().is_some_and(|s| s.is_some())
    }

    pub fn texset_for(&self, chunk: u32) -> u32 {
        world::texset_for(&self.texset_ids, chunk)
    }

    /// Atlas + region set `k` (steps 1–2 of the painting order).
    pub fn base(&self, set: u32) -> Vram {
        let mut v = Vram::new();
        if let Some(a) = &self.atlas {
            v.paint_tim(a);
        }
        if let Some(Some(s)) = self.sets.get(set as usize) {
            v.paint_tim(s);
        }
        v
    }

    /// The field VRAM for `id`: base, sibling pages (interiors) and its own TIM.
    pub fn vram_for(&self, d: &(impl WorldFiles + ?Sized), id: u32, chunk: &[u8]) -> Vram {
        let mut v = self.base(self.texset_for(id));
        if id >= GRID_CHUNKS {
            for sib in world::interior_group(id) {
                if sib != id && sib < CHUNK_COUNT {
                    if let Ok(c) = d.chunk(sib) {
                        v.paint_tim(&c[TIM_OFFSET..]);
                    }
                }
            }
        }
        v.paint_tim(&chunk[TIM_OFFSET..]);
        v
    }
}

/// An RGBA8 image.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    /// `[u32 width][u32 height][RGBA…]`, the IPC image payload.
    pub fn into_payload(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.rgba.len());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.rgba);
        out
    }
}

/// One chunk from above, `size`×`size` px for the ±0x800 extent (or fitted
/// to the mesh with `fit`). Transparent where nothing is drawn (textured style).
pub fn render_chunk(chunk: &[u8], vram: Option<&Vram>, size: usize, style: Style) -> Option<Image> {
    render_chunk_with(chunk, vram, size, style, false)
}

pub fn render_chunk_with(chunk: &[u8], vram: Option<&Vram>, size: usize, style: Style, fit: bool) -> Option<Image> {
    let mesh = world::mesh(chunk)?;
    let ortho = if fit { Ortho::fit(&mesh, size) } else { Ortho::chunk(size) };
    Some(render_mesh(&mesh, vram, size, style, ortho))
}

/// Renders a parsed mesh through any orthographic frame.
pub fn render_mesh(mesh: &world::Mesh, vram: Option<&Vram>, size: usize, style: Style, ortho: Ortho) -> Image {
    let clear = if style == Style::Groups { crate::raster::GROUPS_BG } else { [0, 0, 0, 0] };
    let mut cv = Canvas::new(size, size, clear);
    draw_mesh(&mut cv, mesh, vram, ortho, style);
    Image { width: size as u32, height: size as u32, rgba: cv.rgba }
}

/// Renders `ids` on worker threads, `f(id) -> tile`.
fn par_tiles<F>(ids: &[u32], f: F) -> Vec<(u32, Option<Image>)>
where
    F: Fn(u32) -> Option<Image> + Sync,
{
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut out: Vec<(u32, Option<Image>)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&id) = ids.get(i) else { break };
                        mine.push((id, f(id)));
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    out.sort_by_key(|(id, _)| *id);
    out
}

/// The 324 overworld chunks composed into one `18k × 18k` image, `k` px per
/// chunk. Pixels no polygon covers stay transparent (the UI shows ocean).
pub fn overview(d: &(impl WorldFiles + ?Sized), tex: &WorldTextures, k: usize) -> Image {
    let base = tex.base(0);
    let ids: Vec<u32> = (0..GRID_CHUNKS).collect();
    let tiles = par_tiles(&ids, |id| {
        let c = d.chunk(id).ok()?;
        let mut v = base.clone();
        v.paint_tim(&c[TIM_OFFSET..]);
        render_chunk(&c, Some(&v), k, Style::Textured)
    });
    let side = 18 * k;
    let mut rgba = vec![0u8; side * side * 4];
    for (id, tile) in tiles {
        let Some(t) = tile else { continue };
        let (gx, gy) = world::grid_cell(id).expect("grid chunk");
        let left = gx as usize * k / 2;
        let top = gy as usize * k + k / 2;
        for ty in 0..k {
            let y = (top + ty) % side;
            for tx in 0..k {
                let s = (ty * k + tx) * 4;
                if t.rgba[s + 3] == 0 {
                    continue;
                }
                let x = (left + tx) % side;
                let o = (y * side + x) * 4;
                rgba[o..o + 4].copy_from_slice(&t.rgba[s..s + 4]);
            }
        }
    }
    debug_assert_eq!(GRID_W as usize * k / 2, side);
    Image { width: side as u32, height: side as u32, rgba }
}

/// Interior chunks 324–523 as a sheet of `tile`-px thumbnails, `cols` per row,
/// in id order, each fitted to its rooms (interiors use a fraction of the extent).
pub fn interior_sheet(d: &(impl WorldFiles + ?Sized), tex: &WorldTextures, tile: usize, cols: usize) -> Image {
    let ids: Vec<u32> = (GRID_CHUNKS..CHUNK_COUNT).collect();
    let tiles = par_tiles(&ids, |id| {
        let c = d.chunk(id).ok()?;
        let v = tex.vram_for(d, id, &c);
        render_chunk_with(&c, Some(&v), tile, Style::Textured, true)
    });
    let rows = ids.len().div_ceil(cols);
    let (w, h) = (cols * tile, rows * tile);
    let mut rgba = vec![0u8; w * h * 4];
    for (n, (_, t)) in tiles.into_iter().enumerate() {
        let Some(t) = t else { continue };
        let (left, top) = ((n % cols) * tile, (n / cols) * tile);
        for ty in 0..tile {
            let s = ty * tile * 4;
            let o = ((top + ty) * w + left) * 4;
            rgba[o..o + tile * 4].copy_from_slice(&t.rgba[s..s + tile * 4]);
        }
    }
    Image { width: w as u32, height: h as u32, rgba }
}

/* ───────────────────────── Index (screen 09 inspector, hover card) ───────────────────────── */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimRow {
    pub bpp: u8,
    pub x: u16,
    pub y: u16,
    pub bytes: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeightRange {
    pub distinct: u32,
    pub min: i16,
    pub max: i16,
}

/// Flag-independent facts about one chunk.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkRow {
    pub id: u32,
    pub groups: u32,
    pub vertices: u32,
    pub polygons: u32,
    /// End of header + pool + meshes (cap 0x2A800).
    pub mesh_end: u32,
    pub tim: Option<TimRow>,
    pub heights: HeightRange,
    /// Distinct layer B attributes.
    pub attrs: Vec<u8>,
    /// `fe/k` region set.
    pub texset: u32,
    /// Distinct SYS08 atlas tpages (0x08, 0x09) the polygons sample.
    pub atlas_pages: u32,
    /// Any polygon is textured.
    pub textured: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackRow {
    pub id: u32,
    /// D-records (Character_Struct slots incl. Knight and Baby).
    pub actors: u32,
    /// Tier-1 door records over every group and rung.
    pub doors: u32,
    pub bytes: u32,
}

/// Per-pack data kept for flag-dependent queries.
pub struct PackData {
    pub row: PackRow,
    pub groups: Vec<TriggerGroup>,
}

pub struct WorldIndex {
    pub chunks: Vec<ChunkRow>,
    pub packs: Vec<PackData>,
    /// OVR02 chunk → pack pairs.
    pub pack_table: Vec<(u16, u16)>,
}

pub fn chunk_row(id: u32, c: &[u8], texset: u32) -> ChunkRow {
    let mesh = world::mesh(c);
    let heights = world::heightmap(c);
    let mut distinct: Vec<i16> = heights.clone();
    distinct.sort_unstable();
    distinct.dedup();
    let mut attrs = world::attributes(c);
    attrs.sort_unstable();
    attrs.dedup();
    let tim = Tim::parse(&c[TIM_OFFSET..]).map(|t| TimRow { bpp: t.info.bpp, x: t.info.image.x, y: t.info.image.y, bytes: t.info.len as u32 });
    let (groups, vertices, polygons, mesh_end, atlas_pages, textured) = match &mesh {
        Some(m) => {
            let mut pages: Vec<u16> =
                m.prims.iter().filter(|p| p.textured() && matches!(p.tpage & 0x1F, 8 | 9)).map(|p| p.tpage & 0x1F).collect();
            pages.sort_unstable();
            pages.dedup();
            let vertices = world::stats(id, c).map(|s| s.vertices).unwrap_or(m.pool.len() as u32);
            (m.groups.len() as u32, vertices, m.prims.len() as u32, m.mesh_end, pages.len() as u32, m.prims.iter().any(|p| p.textured()))
        }
        None => (0, 0, 0, 0, 0, false),
    };
    ChunkRow {
        id,
        groups,
        vertices,
        polygons,
        mesh_end,
        tim,
        heights: HeightRange {
            distinct: distinct.len() as u32,
            min: distinct.first().copied().unwrap_or(0),
            max: distinct.last().copied().unwrap_or(0),
        },
        attrs,
        texset,
        atlas_pages,
        textured,
    }
}

/// Parses all 524 chunks, the 50 NPC packs and the OVR02 pack table.
pub fn index(d: &(impl WorldFiles + ?Sized)) -> Result<WorldIndex> {
    let ids = world::texset_ids(d.exe()).unwrap_or_default();
    let all: Vec<u32> = (0..CHUNK_COUNT).collect();
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut chunks: Vec<ChunkRow> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&id) = all.get(i) else { break };
                        if let Ok(c) = d.chunk(id) {
                            mine.push(chunk_row(id, &c, world::texset_for(&ids, id)));
                        }
                    }
                    mine
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    chunks.sort_by_key(|c| c.id);
    Ok(WorldIndex { chunks, packs: packs(d), pack_table: pack_table(d) })
}

/// The 50 NPC packs' rows and trigger groups (the pack half of [`index`],
/// rebuilt alone when only packs change).
pub fn packs(d: &(impl WorldFiles + ?Sized)) -> Vec<PackData> {
    (0..50u32)
        .map(|n| {
            let b = d.file(&format!("npc/{n}")).unwrap_or_default();
            let (actors, groups) = match NpcPack::parse(&b) {
                Some(p) => (p.actors.len() as u32, placement::trigger_groups(&p, &b)),
                None => (0, Vec::new()),
            };
            PackData { row: PackRow { id: n, actors, doors: placement::door_total(&groups), bytes: b.len() as u32 }, groups }
        })
        .collect()
}

/// OVR02's chunk → pack pairs.
pub fn pack_table(d: &(impl WorldFiles + ?Sized)) -> Vec<(u16, u16)> {
    d.file("ovr/2").map(|o| world::pack_table_in(d.exe(), &o)).unwrap_or_default()
}

/// What serves a chunk in a flag state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkContents {
    /// `Field_LookupChunkNpcBinId`; `None` = inherited / door-requested.
    pub pack: Option<u32>,
    /// Active door records of the serving pack's group for this chunk
    /// (`None` when no pack serves it).
    pub doors: Option<u32>,
    /// The records that fire here in this flag state (first holding rung of
    /// each group for the chunk): `[kind, x, z, arg]`, kind 0 door (arg =
    /// destination chunk) · 1 talk (actor) · 2 set flag · 3 clear flag (flag) · 4 other.
    pub marks: Vec<[i16; 4]>,
}

impl WorldIndex {
    pub fn contents(&self, flag: &dyn Fn(u16) -> bool) -> Vec<ChunkContents> {
        (0..CHUNK_COUNT)
            .map(|id| {
                let pack = world::pack_for(&self.pack_table, id, flag);
                // A serving pack without a group for the chunk has no doors here (0).
                let data = pack.and_then(|p| self.packs.get(p as usize));
                let doors = data.map(|p| placement::doors_on(&p.groups, id, flag).unwrap_or(0));
                let marks = data
                    .map(|p| {
                        p.groups
                            .iter()
                            .filter(|g| g.chunk as i32 == id as i32)
                            .filter_map(|g| g.rungs.iter().find(|r| r.flag == 0 || (r.flag < 512 && flag(r.flag))))
                            .flat_map(|r| &r.records)
                            .map(|r| match *r {
                                TriggerRecord::Door { x, z, to, .. } => [0, x, z, to as i16],
                                TriggerRecord::Talk { x, z, actor, .. } => [1, x, z, actor as i16],
                                TriggerRecord::Set { x, z, flag, .. } => [2, x, z, flag as i16],
                                TriggerRecord::Clear { x, z, flag, .. } => [3, x, z, flag as i16],
                                TriggerRecord::Other { x, z, typ, .. } => [4, x, z, typ],
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                ChunkContents { pack, doors, marks }
            })
            .collect()
    }
}
