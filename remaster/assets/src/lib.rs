//! Living Tool Remaster's assets (LT-109, Stage A2; research 16 §6.4).
//!
//! [`Assets::resolve`] turns a captured tick (`lt_scene::Record`) into a [`Frame`] the renderer
//! draws from assets it has uploaded once:
//! - **Chunk groups** match by their mesh offset in the chunk slot to the groups Living Tool
//!   decodes from the chunk (disc or project, through `lt_core::worldmap::WorldFiles`), with
//!   their lists in RAM checked against the file ([`chunk`]).
//! - **Characters** match their model TMD to a file by its primitives, and each drawn part to its
//!   TMD object; a model no file holds is read back from RAM ([`model`]). Either way the live,
//!   posed vertices of the part are what's drawn.
//! - **Textures:** a chunk's atlas comes from the files (`field_vram`, `build_atlas`); a model's
//!   from the running game's VRAM. Tiles that sample VRAM the game animates (the water and the
//!   field's 13 `SetDrawMove` copies) are decoded again every tick from the emulated VRAM
//!   ([`vram`]) and sent as [`TextureWrite`]s.
//! - **Keys** are content: a chunk is its id and file hash, a model its signature, a texture its
//!   pixels' hash. [`gpu::Cache`] uploads each key once.
//!
//! Anything that doesn't match is listed in [`Frame::misses`], never guessed.

use std::collections::{BTreeSet, HashMap};

use lt_core::mesh::{build_atlas, OwnedVram};
use lt_core::worldmap::WorldFiles;
use lt_scene::{Matrix, Record};

pub mod check;
pub mod chunk;
pub mod gpu;
pub mod mesh;
pub mod model;
pub mod tmd;
pub mod vram;

use chunk::{ChunkAsset, FieldTextures};
use mesh::{MeshData, TextureData};
use model::{
    Fingerprint, ModelAsset, ModelIndex, ModelSource, CHARACTER_SLOT, PART_BYTES, PART_RECORDS,
};
use tmd::{fnv_on, Ram};
use vram::Animated;

/// A mesh asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MeshKey {
    /// A chunk: its id and the hash of the bytes it was decoded from.
    Chunk(u16, u64),
    /// A model: its signature.
    Model(u64),
}

/// A texture asset: the hash of its size and pixels as first decoded.
pub type TextureKey = u64;

pub fn texture_key(t: &TextureData) -> TextureKey {
    let h = fnv_on(tmd::fnv(b"atlas"), &t.w.to_le_bytes());
    fnv_on(fnv_on(h, &t.h.to_le_bytes()), &t.rgba)
}

/// A drawn chunk: its asset, the groups drawn this tick and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkUse {
    pub chunk: u16,
    pub mesh: MeshKey,
    pub texture: TextureKey,
    pub slot: u8,
    pub offset: [i32; 3],
    pub model_view: Matrix,
    /// Group indices: ranges of the mesh.
    pub groups: Vec<u32>,
}

/// A drawn character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharacterUse {
    pub index: u16,
    pub mesh: MeshKey,
    pub texture: TextureKey,
    pub source: ModelSource,
    pub parts: Vec<PartUse>,
}

/// One drawn part: a TMD object posed this tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartUse {
    /// The object: a range of the model's mesh.
    pub object: u16,
    pub world: Matrix,
    pub model_view: Matrix,
    /// This tick's vertices, which the object's corners index.
    pub vertices: Vec<[i16; 3]>,
}

/// New texels for an animated tile of a texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureWrite {
    pub texture: TextureKey,
    pub tile: usize,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// A draw that matched nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Miss {
    Group {
        chunk: u16,
        offset: u32,
        why: String,
    },
    Character {
        index: u16,
        why: String,
    },
    Part {
        index: u16,
        record: u32,
        why: String,
    },
}

impl std::fmt::Display for Miss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Miss::Group { chunk, offset, why } => {
                write!(f, "chunk {chunk} group at {offset:#x}: {why}")
            }
            Miss::Character { index, why } => write!(f, "character {index}: {why}"),
            Miss::Part { index, record, why } => {
                write!(f, "character {index} part {record:#010x}: {why}")
            }
        }
    }
}

/// One tick, in assets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub tick: u64,
    pub chunks: Vec<ChunkUse>,
    pub characters: Vec<CharacterUse>,
    pub writes: Vec<TextureWrite>,
    pub misses: Vec<Miss>,
}

impl Frame {
    /// Every texture the frame samples.
    pub fn textures(&self) -> BTreeSet<TextureKey> {
        self.chunks
            .iter()
            .map(|c| c.texture)
            .chain(self.characters.iter().map(|c| c.texture))
            .collect()
    }

    /// Every mesh the frame draws.
    pub fn meshes(&self) -> BTreeSet<MeshKey> {
        self.chunks
            .iter()
            .map(|c| c.mesh)
            .chain(self.characters.iter().map(|c| c.mesh))
            .collect()
    }
}

/// A texture and which of its tiles the game animates.
pub struct Texture {
    pub data: TextureData,
    animated: Vec<usize>,
    /// The [`Animated::version`] `animated` was found for.
    version: u32,
}

/// The assets met so far, decoded once each.
pub struct Assets<'a> {
    files: &'a dyn WorldFiles,
    field: FieldTextures,
    pub index: ModelIndex,
    chunks: HashMap<u16, Option<ChunkAsset>>,
    models: HashMap<u64, Option<ModelAsset>>,
    textures: HashMap<TextureKey, Texture>,
    /// A model's texture in an area (the current chunk): decoded from VRAM there once.
    model_textures: HashMap<(u64, u16), TextureKey>,
    pub animated: Animated,
    /// Parts matched by fingerprint where objects of several models had their shape (the first,
    /// a field model, was taken).
    pub shared_shapes: u64,
}

impl<'a> Assets<'a> {
    /// Assets read through `files` (the disc, or a project's `ProjectWorld`), models found in
    /// `index`.
    pub fn new(files: &'a dyn WorldFiles, index: ModelIndex) -> Self {
        Assets {
            files,
            field: FieldTextures::load(files),
            index,
            chunks: HashMap::new(),
            models: HashMap::new(),
            textures: HashMap::new(),
            model_textures: HashMap::new(),
            animated: Animated::default(),
            shared_shapes: 0,
        }
    }

    pub fn mesh(&self, key: MeshKey) -> Option<&MeshData> {
        match key {
            MeshKey::Chunk(id, _) => self.chunks.get(&id)?.as_ref().map(|c| &c.mesh),
            MeshKey::Model(sig) => self.models.get(&sig)?.as_ref().map(|m| &m.mesh),
        }
    }

    /// What a mesh is, for logs: `chunk/78`, `npc/6#3`, `RAM 0x80117fc0`.
    pub fn label(&self, key: MeshKey) -> String {
        match key {
            MeshKey::Chunk(id, _) => format!("chunk/{id}"),
            MeshKey::Model(sig) => self
                .models
                .get(&sig)
                .and_then(|m| m.as_ref())
                .map_or_else(|| format!("model {sig:016x}"), |m| m.source.to_string()),
        }
    }

    pub fn model(&self, sig: u64) -> Option<&ModelAsset> {
        self.models.get(&sig)?.as_ref()
    }

    /// Every model met.
    pub fn models(&self) -> impl Iterator<Item = &ModelAsset> {
        self.models.values().flatten()
    }

    pub fn chunk(&self, id: u16) -> Option<&ChunkAsset> {
        self.chunks.get(&id)?.as_ref()
    }

    pub fn texture(&self, key: TextureKey) -> Option<&TextureData> {
        self.textures.get(&key).map(|t| &t.data)
    }

    /// The animated tiles of a texture (as last found).
    pub fn animated_tiles(&self, key: TextureKey) -> &[usize] {
        self.textures.get(&key).map_or(&[], |t| &t.animated)
    }

    fn add_texture(&mut self, data: TextureData) -> TextureKey {
        let key = texture_key(&data);
        self.textures.entry(key).or_insert(Texture {
            data,
            animated: Vec::new(),
            version: 0,
        });
        key
    }

    /// `chunk/id` decoded (once) for drawing though the game hasn't drawn it: a neighbour a wider
    /// view shows (LT-114). Its mesh and atlas keys, or `None` when it can't be read.
    pub fn load_chunk(&mut self, id: u16) -> Option<(MeshKey, TextureKey)> {
        self.chunk_asset(id)
    }

    /// The model whose TMD is at `tmd` in RAM, for a character the game didn't draw (LT-119): its
    /// mesh key (the index's file when one holds it, else the RAM copy), its source, and its
    /// texture in area `chunk`, decoded from `vram` (VRAM as the tick's table starts) the first
    /// time, as [`Assets::resolve_at`] does for a drawn one.
    pub fn character_model(&mut self, ram: &dyn Ram, tmd: u32, chunk: u16, vram: &[u16]) -> Option<(MeshKey, TextureKey, ModelSource)> {
        let file = tmd::from_ram(ram, tmd)?;
        let sig = tmd::signature(&file)?;
        let source = self.model_asset(sig, Some((tmd, &file)))?.source.clone();
        let texture = match self.model_textures.get(&(sig, chunk)) {
            Some(&k) => k,
            None => {
                let v = OwnedVram::whole(vram.to_vec());
                let mut m = self.models[&sig].as_ref().expect("built").render.clone();
                let tex: TextureData = build_atlas(&mut m, &v).into();
                let k = self.add_texture(tex);
                self.model_textures.insert((sig, chunk), k);
                k
            }
        };
        Some((MeshKey::Model(sig), texture, source))
    }

    /// The chunk asset, decoded on first use.
    fn chunk_asset(&mut self, id: u16) -> Option<(MeshKey, TextureKey)> {
        if !self.chunks.contains_key(&id) {
            let built = chunk::build(self.files, &mut self.field, id);
            let built = built.map(|(c, tex)| {
                let key = self.add_texture(tex);
                (c, key)
            });
            self.chunks.insert(
                id,
                built.map(|(mut c, key)| {
                    c.texture = key;
                    c
                }),
            );
        }
        self.chunks
            .get(&id)?
            .as_ref()
            .map(|c| (MeshKey::Chunk(c.id, c.file), c.texture))
    }

    /// The model with signature `sig`, built on first use from the index, or else from `ram_tmd`
    /// (its address in RAM and file form).
    fn model_asset(&mut self, sig: u64, ram_tmd: Option<(u32, &[u8])>) -> Option<&ModelAsset> {
        if !self.models.contains_key(&sig) {
            let built = match (self.index.get(sig), ram_tmd) {
                (Some((source, file)), _) => model::build(sig, source.clone(), file),
                (None, Some((at, tmd))) => model::build(sig, ModelSource::Ram { at }, tmd),
                (None, None) => None,
            };
            self.models.insert(sig, built);
        }
        self.models[&sig].as_ref()
    }

    /// The model and object a part with shape `fp` is: the previous part's model, the model
    /// pointer's TMD (`ptr`: its address, signature and file form), or any object on the disc
    /// with that shape. `at_slot` is the object the part record's place in its slot says.
    fn pick(
        &mut self,
        chosen: Option<u64>,
        ptr: Option<&(u32, u64, Vec<u8>)>,
        at_slot: Option<u16>,
        fp: &Fingerprint,
    ) -> Result<(u64, u16), String> {
        let fits = |m: &ModelAsset| -> Option<u16> {
            match at_slot.filter(|&o| m.shapes.get(o as usize) == Some(fp)) {
                Some(o) => Some(o),
                None => m.shapes.iter().position(|s| s == fp).map(|o| o as u16),
            }
        };
        if let Some(sig) = chosen {
            if let Some(o) = self.model(sig).and_then(fits) {
                return Ok((sig, o));
            }
        }
        if let Some((at, sig, tmd)) = ptr {
            if let Some(o) = self.model_asset(*sig, Some((*at, tmd))).and_then(fits) {
                return Ok((*sig, o));
            }
        }
        let shaped = self.index.shaped(fp);
        if let Some(&(sig, o)) = shaped.first() {
            if shaped.iter().any(|&(s, _)| s != sig) {
                self.shared_shapes += 1;
            }
            if self.model_asset(sig, None).is_some() {
                return Ok((sig, o));
            }
        }
        Err(format!(
            "no model has an object of {} vertices with these polygon counts {:?}",
            fp.verts, fp.classes
        ))
    }

    /// The tick's frame: `ram` reads the machine as it is at the tick's end, `vram` is its video
    /// memory then (1024 × 512 halfwords).
    pub fn resolve(&mut self, r: &Record, ram: &dyn Ram, vram: &[u16]) -> Frame {
        let view = vram::view(vram, &r.moves);
        self.resolve_at(r, ram, &view)
    }

    /// [`Assets::resolve`] with `view` taken as the VRAM the frame samples, as it is. `main()` links
    /// the tick's `SetDrawMove`s into bucket 0, which the GPU draws last, so the frame's polygons
    /// sample VRAM before the moves: VRAM as `Dbg_DrawOTag` starts the table (LT-111).
    pub fn resolve_at(&mut self, r: &Record, ram: &dyn Ram, view: &[u16]) -> Frame {
        self.animated.learn(&r.moves);
        let view = view.to_vec();
        let mut f = Frame {
            tick: r.tick,
            ..Frame::default()
        };

        for d in &r.chunks {
            let Some((mesh, texture)) = self.chunk_asset(d.chunk) else {
                let why = format!("chunk/{} can't be read or decoded", d.chunk);
                f.misses.extend(d.groups.iter().map(|g| Miss::Group {
                    chunk: d.chunk,
                    offset: g.id as u32,
                    why: why.clone(),
                }));
                continue;
            };
            let asset = self.chunks[&d.chunk].as_ref().expect("built");
            let mut groups = Vec::with_capacity(d.groups.len());
            for g in &d.groups {
                let ofs = g.id as u32;
                let len = asset.list_bytes(ofs);
                let found = if len == 0 {
                    Err(format!("chunk/{} has no group at {ofs:#x}", d.chunk))
                } else {
                    asset.group(ofs, &ram.read(g.mesh, len))
                };
                match found {
                    Ok(i) => groups.push(i),
                    Err(why) => f.misses.push(Miss::Group {
                        chunk: d.chunk,
                        offset: ofs,
                        why,
                    }),
                }
            }
            f.chunks.push(ChunkUse {
                chunk: d.chunk,
                mesh,
                texture,
                slot: d.slot,
                offset: d.offset,
                model_view: d.model_view,
                groups,
            });
        }

        let mut whole: Option<OwnedVram> = None;
        for ch in &r.characters {
            // The model pointer's TMD: an NPC's model (the party's is a leftover).
            let ptr =
                tmd::from_ram(ram, ch.model).and_then(|t| Some((ch.model, tmd::signature(&t)?, t)));
            let slot = ram
                .read(ch.record + CHARACTER_SLOT, 1)
                .first()
                .copied()
                .unwrap_or(0xFF) as u32;
            let first = PART_RECORDS + slot * PART_BYTES;
            let mut chosen: Option<u64> = None;
            let mut parts = Vec::with_capacity(ch.parts.len());
            for p in &ch.parts {
                let fp = Fingerprint::of_part(&ram.read(p.record, PART_BYTES as usize));
                if fp.verts as usize != p.vertices.len() {
                    f.misses.push(Miss::Part {
                        index: ch.index,
                        record: p.record,
                        why: format!(
                            "the record says {} vertices, {} were read",
                            fp.verts,
                            p.vertices.len()
                        ),
                    });
                    continue;
                }
                let at_slot = p
                    .record
                    .checked_sub(first)
                    .filter(|o| o % PART_BYTES == 0)
                    .map(|o| (o / PART_BYTES) as u16);
                match self.pick(chosen, ptr.as_ref(), at_slot, &fp) {
                    Ok((sig, object)) if chosen.is_none_or(|c| c == sig) => {
                        chosen = Some(sig);
                        parts.push(PartUse {
                            object,
                            world: p.world,
                            model_view: p.model_view,
                            vertices: p.vertices.clone(),
                        });
                    }
                    Ok((sig, _)) => f.misses.push(Miss::Part {
                        index: ch.index,
                        record: p.record,
                        why: format!("its model {sig:016x} isn't the character's first part's"),
                    }),
                    Err(why) => f.misses.push(Miss::Part {
                        index: ch.index,
                        record: p.record,
                        why,
                    }),
                }
            }
            let Some(sig) = chosen else {
                if ch.parts.is_empty() {
                    f.misses.push(Miss::Character {
                        index: ch.index,
                        why: "no part drawn".into(),
                    });
                }
                continue;
            };
            let texture = match self.model_textures.get(&(sig, r.chunk)) {
                Some(&k) => k,
                None => {
                    let v = whole.get_or_insert_with(|| OwnedVram::whole(view.clone()));
                    let mut m = self.models[&sig].as_ref().expect("picked").render.clone();
                    let tex: TextureData = build_atlas(&mut m, v).into();
                    let k = self.add_texture(tex);
                    self.model_textures.insert((sig, r.chunk), k);
                    k
                }
            };
            let source = self.models[&sig].as_ref().expect("picked").source.clone();
            f.characters.push(CharacterUse {
                index: ch.index,
                mesh: MeshKey::Model(sig),
                texture,
                source,
                parts,
            });
        }

        // Animated tiles of every texture drawn, from this tick's VRAM.
        for key in f.textures() {
            let Some(t) = self.textures.get_mut(&key) else {
                continue;
            };
            if t.version != self.animated.version {
                t.animated = t
                    .data
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|(_, tile)| self.animated.touches(tile))
                    .map(|(i, _)| i)
                    .collect();
                t.version = self.animated.version;
            }
            if t.animated.is_empty() {
                continue;
            }
            let v = whole.get_or_insert_with(|| OwnedVram::whole(view.clone()));
            for &i in &t.animated {
                let tile = t.data.tiles[i];
                let rgba = vram::decode(&tile, v);
                for row in 0..tile.h as usize {
                    let o = ((tile.y as usize + row) * t.data.w as usize + tile.x as usize) * 4;
                    let w = tile.w as usize * 4;
                    t.data.rgba[o..o + w].copy_from_slice(&rgba[row * w..row * w + w]);
                }
                f.writes.push(TextureWrite {
                    texture: key,
                    tile: i,
                    x: tile.x as u32,
                    y: tile.y as u32,
                    w: tile.w as u32,
                    h: tile.h as u32,
                    rgba,
                });
            }
        }
        f
    }
}
