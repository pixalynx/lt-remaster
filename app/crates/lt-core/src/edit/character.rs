//! Characters brought into an NPC pack from another (screen 10, research 06
//! phase 1 "NPC pack from scratch"): a Group A model, its Group B pose block
//! and its P-chain animation set, so an added actor can look like anyone in
//! the game, not only the pack's own cast.
//!
//! A pack keeps its models after the D-block, reached through the `+8`
//! pointer table: Group A (`count_a` TMDs, `g_NpcModelPtrTable`, 48 entries
//! at 0x801FD688 up to the packet-slot flags at 0x801FD748) then Group B
//! (`count_b` pose blocks, `…Table2`, 16 entries at 0x801FD648). An actor's
//! record `b` byte 2 picks a Group A model and byte 3 both a pose block and
//! the P-chain set of the same index. The P-chain is one run of P-relative
//! words up to a 0: the first `count_b` words point at each set's list, the
//! lists point at `u8 count; (pose, speed) × count` sequences after the run.
//!
//! Adding characters ([`apply`]):
//! - the table grows by one word per model at the end of Group A and one per
//!   pose block at the end of Group B, so P and D move down (D-relative
//!   operands don't care; every model pointer moves with them);
//! - the new sets' first words follow the old ones, their lists follow the
//!   old lists, their sequences follow the old data before D, and every
//!   P-relative word is moved by what was inserted before its target;
//! - the sections go after the pack's last used byte, each behind a tagged
//!   header the engine never reads: `[label][pad]"LTch" len base set
//!   label_len looks count_b` before a model (count_b: the header's own,
//!   which can differ from the table: npc/15 declares 8 and points at 6),
//!   `"LTpo" len` before a pose block.
//!
//! [`split`] is its exact inverse: the pack without them, byte for byte as
//! the text layout made it, and the characters. Every pack writer works on
//! that base ([`super::text::layout`], [`super::pack::Edits::recover`]) and
//! the characters go back on last, so they survive every other edit.

use std::borrow::Cow;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::container::{le16, le32};
use crate::npc::NpcPack;

use super::{EditError, Result};

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

/// Group A entries the engine's table holds (0x801FD688..0x801FD748).
pub const MAX_MODELS: usize = 48;
/// Group B entries (0x801FD648..0x801FD688).
pub const MAX_POSE_BLOCKS: usize = 16;
/// An NPC model packet slot (`Npc_AllocPacketSlot`: 0x800F5000 + i·0x5A00);
/// bigger models need the big-model kind bit and two slots.
pub const PACKET_SLOT: u32 = 0x5A00;
/// Kind bit 0x20 (record `d` byte 1): spans consecutive packet slots.
pub const BIG_MODEL: u8 = 0x20;

const MAGIC_MODEL: &[u8; 4] = b"LTch";
const MAGIC_POSES: &[u8; 4] = b"LTpo";
const MODEL_HEADER: usize = 20;
const POSE_HEADER: usize = 8;
/// Zero bytes kept after the pack's last used byte before the sections.
const GAP: usize = 64;

/// A character the project brought into a pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Character {
    /// Where it came from, for people (`npc/3 · actor 12`).
    pub label: String,
    /// The Group A TMD.
    #[serde(skip)]
    pub model: Vec<u8>,
    /// The Group B pose block, `None` for a still model.
    #[serde(skip)]
    pub poses: Option<Vec<u8>>,
    /// The P-chain set: one `(pose, speed)` sequence per animation state.
    pub anims: Vec<Vec<(u8, u8)>>,
    /// Record `d` bytes 0–1 of the actor it came from: shadow size, kind.
    pub looks: u16,
}

impl Character {
    /// Actor `slot` of pack `src` as a character.
    pub fn from_actor(src: &[u8], slot: usize, label: impl Into<String>) -> Result<Self> {
        let p = NpcPack::parse(src).ok_or_else(|| bad("NPC_BAD_HEADER", "The source pack has no header."))?;
        let a = p.actors.get(slot).ok_or_else(|| bad("ACTOR_UNKNOWN", format!("The source pack has no actor {slot}.")))?;
        let (m, s) = (((a.b >> 16) & 0xFF) as usize, ((a.b >> 24) & 0xFF) as usize);
        if m == 0xFF {
            return Err(bad("CHARACTER_NO_MODEL", format!("Actor {slot} has no model to bring.")));
        }
        let section = p.model(src, m).ok_or_else(|| bad("CHARACTER_NO_MODEL", format!("The source pack has no model {m}.")))?;
        let model = super::tmdcodec::Tmd::parse(section)?.write_trimmed();
        let (poses, anims) = if s == 0xFF {
            (None, Vec::new())
        } else {
            let block = p.pose_block(src, s).ok_or_else(|| bad("CHARACTER_NO_POSES", format!("The source pack has no pose block {s}.")))?;
            let len = crate::pose::parse(block).ok_or_else(|| bad("POSES_BAD", "The source pose block doesn't parse."))?.len;
            let anims = p.anim_set(src, s);
            if anims.is_empty() {
                return Err(bad("CHARACTER_NO_ANIMS", format!("Pose set {s} has no animations in the source pack.")));
            }
            (Some(block[..len].to_vec()), anims)
        };
        Ok(Self { label: label.into(), model, poses, anims, looks: (a.d & 0xFFFF) as u16 })
    }

    /// Bytes the engine builds for its model (one packet slot unless big).
    pub fn built(&self) -> u32 {
        super::tmdcodec::Tmd::parse(&self.model).map(|t| t.sizes().built).unwrap_or(u32::MAX)
    }

    fn check(&self, i: usize) -> Result<()> {
        let built = self.built();
        let big = (self.looks >> 8) as u8 & BIG_MODEL != 0;
        if built > PACKET_SLOT * if big { 2 } else { 1 } {
            return Err(bad("CHARACTER_BUDGET", format!("Character {i}'s model builds {built} B; a packet slot takes {PACKET_SLOT} B{}.", if big { " (two for a big model)" } else { "" })));
        }
        if self.poses.is_some() && self.anims.is_empty() {
            return Err(bad("CHARACTER_NO_ANIMS", format!("Character {i} has poses but no animations.")));
        }
        for (k, seq) in self.anims.iter().enumerate() {
            if seq.is_empty() || seq.len() > 255 {
                return Err(bad("CHARACTER_ANIMS", format!("Character {i}'s animation {k} needs 1–255 steps.")));
            }
        }
        Ok(())
    }
}

/* ───────────────────────── Portability ───────────────────────── */

/// The VRAM a textured model samples, in halfwords: per primitive its
/// texels (page plus UV bounds) and its CLUT row.
pub fn texture_rects(model: &[u8]) -> Vec<crate::tim::VramRect> {
    use crate::tim::VramRect;
    let Ok(t) = super::tmdcodec::Tmd::parse(model) else { return Vec::new() };
    let mut out = Vec::new();
    for p in t.objects.iter().flat_map(|o| &o.prims).filter(|p| p.mode & 0x04 != 0 && p.words.len() >= 3) {
        let n = if p.mode & 0x08 != 0 { 4 } else { 3 };
        let (cba, tsb) = ((p.words[0] >> 16) as u16, (p.words[1] >> 16) as u16);
        let uvs: Vec<(u16, u16)> = (0..n).filter_map(|k| p.words.get(k).map(|w| ((w & 0xFF) as u16, ((w >> 8) & 0xFF) as u16))).collect();
        let (u0, u1) = (uvs.iter().map(|x| x.0).min().unwrap_or(0), uvs.iter().map(|x| x.0).max().unwrap_or(0));
        let (v0, v1) = (uvs.iter().map(|x| x.1).min().unwrap_or(0), uvs.iter().map(|x| x.1).max().unwrap_or(0));
        let depth = (tsb >> 7) & 3;
        let per = match depth {
            0 => 4,
            1 => 2,
            _ => 1,
        };
        let (px, py) = ((tsb & 0xF) * 64, ((tsb >> 4) & 1) * 256);
        out.push(VramRect { x: px + u0 / per, y: py + v0, w: (u1 / per - u0 / per) + 1, h: v1 - v0 + 1 });
        if depth < 2 {
            out.push(VramRect { x: (cba & 0x3F) * 16, y: (cba >> 6) & 0x1FF, w: if depth == 0 { 16 } else { 256 }, h: 1 });
        }
    }
    out
}

/// Whether every texel and CLUT row the model samples lies in `owned`
/// (the VRAM every field area paints the same: SYS14 and SYS08). An
/// untextured model is portable; one textured from its own area's pages
/// draws whatever another area loads there.
pub fn portable(model: &[u8], owned: &[crate::tim::VramRect]) -> bool {
    let inside = |r: &crate::tim::VramRect| owned.iter().any(|o| r.x >= o.x && r.y >= o.y && r.x + r.w <= o.x + o.w && r.y + r.h <= o.y + o.h);
    texture_rects(model).iter().all(inside)
}

/// The VRAM rectangles a set of TIM files paints (images and CLUTs).
pub fn owned_by(tims: &[&[u8]]) -> Vec<crate::tim::VramRect> {
    tims.iter()
        .filter_map(|b| crate::tim::Tim::parse(b))
        .flat_map(|t| std::iter::once(t.info.image).chain(t.info.clut))
        .collect()
}

/* ───────────────────────── The P block ───────────────────────── */

/// The P block: counts, the P-chain run, the data after it (up to D).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PBlock {
    ca: u16,
    cb: u16,
    words: Vec<u32>,
    data: Vec<u8>,
}

impl PBlock {
    fn read(b: &[u8], pack: &NpcPack) -> Result<Self> {
        let (p, d) = (pack.p as usize, pack.d as usize);
        let mut words = Vec::new();
        let mut at = p + 4;
        loop {
            if at + 4 > d {
                return Err(bad("P_CHAIN_BAD", "The P-chain runs into the D-block."));
            }
            let w = le32(b, at);
            at += 4;
            if w == 0 {
                break;
            }
            words.push(w);
        }
        Ok(Self { ca: le16(b, p), cb: le16(b, p + 2), words, data: b[at..d].to_vec() })
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.words.len() * 4 + self.data.len());
        out.extend_from_slice(&self.ca.to_le_bytes());
        out.extend_from_slice(&self.cb.to_le_bytes());
        for w in &self.words {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    /// P-relative offset of the data after the run.
    fn data_at(&self) -> u32 {
        8 + 4 * self.words.len() as u32
    }

    /// Adds sets at word `rb` (the pose-block count: a set and its pose
    /// block share an index; some packs carry unused set words past it,
    /// which move up), their lists after the old lists, their sequences
    /// after the old data. The header's `count_b` covers the new blocks.
    fn add_sets(&self, sets: &[&[Vec<(u8, u8)>]], models: u16, rb: u32) -> Self {
        let (n, cb) = (self.words.len() as u32, rb);
        let nb = sets.len() as u32;
        let nl: u32 = sets.iter().map(|s| s.len() as u32).sum();
        let old_data = self.data_at();
        let remap = |t: u32| -> u32 {
            if t < 4 {
                t
            } else if t < old_data {
                let j = (t - 4) / 4;
                t + if j >= n { 4 * (nb + nl) } else if j >= cb { 4 * nb } else { 0 }
            } else {
                t + 4 * (nb + nl)
            }
        };
        let mut data = self.data.clone();
        let new_data = old_data + 4 * (nb + nl);
        let mut set_words = Vec::new();
        let mut list_words = Vec::new();
        for set in sets {
            set_words.push(4 + 4 * (n + nb + list_words.len() as u32));
            for seq in set.iter() {
                list_words.push(new_data + data.len() as u32);
                data.push(seq.len() as u8);
                for &(p, s) in seq {
                    data.extend_from_slice(&[p, s]);
                }
            }
        }
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
        let mut words: Vec<u32> = self.words[..cb as usize].iter().map(|&t| remap(t)).collect();
        words.extend(set_words);
        words.extend(self.words[cb as usize..].iter().map(|&t| remap(t)));
        words.extend(list_words);
        Self { ca: self.ca + models, cb: self.cb.max((rb + nb) as u16), words, data }
    }

    /// The inverse of [`Self::add_sets`]: drops sets `rb..rb + nb`, their
    /// lists (the run's tail from the first of them) and their sequences,
    /// and puts back the header's `count_b`.
    fn drop_sets(&self, ra: u16, rb: u16, nb: u16, header_cb: u16) -> Result<Self> {
        let n = self.words.len() as u32;
        let cb = rb + nb;
        let nb = nb as u32;
        if nb == 0 {
            return Ok(Self { ca: ra, cb: header_cb, ..self.clone() });
        }
        let bad_chain = || bad("P_CHAIN_BAD", "The added animation sets don't read back.");
        let firsts: Vec<u32> = self.words.get(rb as usize..cb as usize).ok_or_else(bad_chain)?.to_vec();
        let first_list = firsts.iter().map(|&t| (t.wrapping_sub(4)) / 4).min().ok_or_else(bad_chain)?;
        if first_list > n {
            return Err(bad_chain());
        }
        let nl = n - first_list;
        let data_at = self.data_at();
        let first_seq = self.words[first_list as usize..].iter().copied().min().unwrap_or(data_at + self.data.len() as u32);
        if first_seq < data_at {
            return Err(bad_chain());
        }
        let unmap = |t: u32| -> u32 {
            if t < 4 {
                t
            } else if t < data_at {
                let j = (t - 4) / 4;
                t - if j >= first_list { 4 * (nb + nl) } else if j >= rb as u32 + nb { 4 * nb } else { 0 }
            } else {
                t - 4 * (nb + nl)
            }
        };
        let mut words: Vec<u32> = self.words[..rb as usize].iter().map(|&t| unmap(t)).collect();
        words.extend(self.words[cb as usize..first_list as usize].iter().map(|&t| unmap(t)));
        let keep = (first_seq - data_at) as usize;
        Ok(Self { ca: ra, cb: header_cb, words, data: self.data[..keep].to_vec() })
    }
}

/* ───────────────────────── Apply / split ───────────────────────── */

fn header(b: &[u8], what: &str) -> Result<NpcPack> {
    NpcPack::parse(b).ok_or_else(|| bad("NPC_BAD_HEADER", format!("The {what} pack has no header.")))
}

/// Retail Group A and Group B counts.
pub fn retail_counts(retail: &[u8]) -> Result<(usize, usize)> {
    let r = header(retail, "retail")?;
    let a = r.group_a(retail);
    Ok((a, r.models.len() - a))
}

/// Per character: the record bytes an actor that looks like it takes —
/// model (record `b` byte 2), pose set (byte 3, 0xFF for a still model)
/// and looks (`d` bytes 0–1).
pub fn looks(retail: &[u8], chars: &[Character]) -> Result<Vec<(u8, u8, u16)>> {
    let (ra, rb) = retail_counts(retail)?;
    let mut set = rb;
    Ok(chars
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let s = if c.poses.is_some() {
                set += 1;
                (set - 1) as u8
            } else {
                0xFF
            };
            ((ra + k).min(0xFE) as u8, s, c.looks)
        })
        .collect())
}

/// Retail sections the project dropped to make room: Group A models and
/// Group B pose blocks only removed actors used ([`freeable`]). Their
/// pointers aim at an empty stub; [`split`] puts the retail bytes back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Freed {
    pub models: BTreeSet<u8>,
    pub poses: BTreeSet<u8>,
}

impl Freed {
    pub fn is_empty(&self) -> bool {
        self.models.is_empty() && self.poses.is_empty()
    }
}

const MAGIC_FREED: &[u8; 4] = b"LTfr";
const FREED_HEADER: usize = 24;
/// An empty TMD: one object with no vertices, normals or primitives.
const STUB_TMD: [u8; 40] = {
    let mut b = [0u8; 40];
    b[0] = 0x41;
    b[8] = 1;
    // vert_top, normal_top, prim_top = 28 (just past the table, relative to +12)
    b[12] = 28;
    b[20] = 28;
    b[28] = 28;
    b
};
const STUB_POSES: [u8; 4] = [0; 4];

/// Retail sections (file offset, length) in file order.
fn retail_sections(r: &NpcPack, retail: &[u8]) -> Vec<(u32, u32)> {
    let mut offs: Vec<u32> = r.models.iter().copied().filter(|&m| m > r.d && (m as usize) < retail.len()).collect();
    offs.sort_unstable();
    offs.dedup();
    offs.iter().enumerate().map(|(k, &o)| (o, offs.get(k + 1).copied().unwrap_or(retail.len() as u32) - o)).collect()
}

/// The models and pose blocks only taken-out actors use: what the project
/// can drop to make room. Copies of retail actors keep their source's;
/// a section two indices share goes only when both can.
pub fn freeable(retail: &[u8], removed: &BTreeSet<u32>, added: &[super::actor::AddedActor]) -> Result<Freed> {
    let r = header(retail, "retail")?;
    let (ra, rb) = retail_counts(retail)?;
    let (mut used_a, mut used_b) = (BTreeSet::new(), BTreeSet::new());
    let (mut out_a, mut out_b) = (BTreeSet::new(), BTreeSet::new());
    let pick = |b: u32| (((b >> 16) & 0xFF) as usize, ((b >> 24) & 0xFF) as usize);
    for a in &r.actors {
        let (m, s) = pick(a.b);
        let (ua, ub) = if removed.contains(&a.slot) { (&mut out_a, &mut out_b) } else { (&mut used_a, &mut used_b) };
        if m < ra {
            ua.insert(m);
        }
        if s < rb {
            ub.insert(s);
        }
    }
    for a in added.iter().filter(|a| a.character.is_none() && !a.hidden && !a.spot) {
        if let Some(src) = r.actors.get(a.from as usize) {
            let (m, s) = pick(src.b);
            used_a.insert(m);
            used_b.insert(s);
        }
    }
    let mut models: BTreeSet<u8> = out_a.difference(&used_a).map(|&m| m as u8).collect();
    let mut poses: BTreeSet<u8> = out_b.difference(&used_b).map(|&s| s as u8).collect();
    // A shared section stays unless every index on it goes.
    let ptr = |i: usize| r.models[i];
    let keep_a: Vec<u8> = models.iter().copied().filter(|&m| (0..r.models.len()).any(|j| ptr(j) == ptr(m as usize) && !(j < ra && models.contains(&(j as u8))) && !(j >= ra && poses.contains(&((j - ra) as u8))))).collect();
    let keep_b: Vec<u8> = poses.iter().copied().filter(|&s| (0..r.models.len()).any(|j| ptr(j) == ptr(ra + s as usize) && !(j < ra && models.contains(&(j as u8))) && !(j >= ra && poses.contains(&((j - ra) as u8))))).collect();
    for m in keep_a {
        models.remove(&m);
    }
    for s in keep_b {
        poses.remove(&s);
    }
    Ok(Freed { models, poses })
}

/// Bytes the freed sections take in retail.
pub fn freed_bytes(retail: &[u8], freed: &Freed) -> Result<u32> {
    let r = header(retail, "retail")?;
    let (ra, _) = retail_counts(retail)?;
    let secs = retail_sections(&r, retail);
    let offs: BTreeSet<u32> = freed.models.iter().map(|&m| r.models[m as usize]).chain(freed.poses.iter().map(|&s| r.models[ra + s as usize])).collect();
    Ok(secs.iter().filter(|(o, _)| offs.contains(o)).map(|(_, l)| l).sum())
}

/// `base` (a pack as the text layout makes it) with `chars` added and the
/// `freed` retail sections dropped.
pub fn apply(retail: &[u8], base: &[u8], chars: &[Character], freed: &Freed) -> Result<Vec<u8>> {
    if chars.is_empty() && freed.is_empty() {
        return Ok(base.to_vec());
    }
    let (ra, rb) = retail_counts(retail)?;
    let r = header(retail, "retail")?;
    let c = header(base, "project's")?;
    if c.group_a(base) != ra || c.models.len() != ra + rb {
        return Err(bad("CHARACTER_BASE", "The pack's model table isn't the retail one."));
    }
    for (i, ch) in chars.iter().enumerate() {
        ch.check(i)?;
    }
    if freed.models.iter().any(|&m| m as usize >= ra) || freed.poses.iter().any(|&s| s as usize >= rb) {
        return Err(bad("FREED_RANGE", "A freed model or pose block isn't one of the pack's."));
    }
    let na = chars.len();
    let posed: Vec<&Character> = chars.iter().filter(|c| c.poses.is_some()).collect();
    let nb = posed.len();
    if ra + na > MAX_MODELS {
        return Err(bad("CHARACTER_MODELS", format!("The pack has {ra} models; the game's table takes {MAX_MODELS}, so {} more fit.", MAX_MODELS - ra)));
    }
    if rb + nb > MAX_POSE_BLOCKS {
        return Err(bad("CHARACTER_POSE_BLOCKS", format!("The pack has {rb} pose sets; the game's table takes {MAX_POSE_BLOCKS}, so {} more animated characters fit.", MAX_POSE_BLOCKS - rb)));
    }
    // 1. Drop the freed sections (each section is where retail had it, moved by the text layout's area).
    let r_first = r.models.iter().copied().filter(|&m| m > r.d).min().unwrap_or(retail.len() as u32);
    let b_first = c.models.iter().copied().filter(|&m| m > c.d).min().unwrap_or(base.len() as u32);
    let delta = b_first.wrapping_sub(r_first);
    let mut mid = base.to_vec();
    let mut ptrs = c.models.clone();
    let is_freed = |i: usize| if i < ra { freed.models.contains(&(i as u8)) } else { freed.poses.contains(&((i - ra) as u8)) };
    let secs = retail_sections(&r, retail);
    let mut cuts: Vec<(u32, u32)> = secs
        .iter()
        .filter(|(o, _)| (0..ra + rb).any(|i| is_freed(i) && r.models[i] == *o))
        .map(|&(o, l)| (o.wrapping_add(delta), l))
        .collect();
    cuts.sort_unstable_by_key(|c| std::cmp::Reverse(c.0));
    for &(at, len) in &cuts {
        let (a, e) = (at as usize, ((at + len) as usize).min(mid.len()));
        if a > mid.len() {
            continue;
        }
        mid.drain(a..e);
        for (i, p) in ptrs.iter_mut().enumerate() {
            if !is_freed(i) && *p > at {
                *p -= (e - a) as u32;
            }
        }
    }
    // 2. The table, the P block and the appended sections.
    let pb = PBlock::read(&mid, &c)?;
    let sets: Vec<&[Vec<(u8, u8)>]> = posed.iter().map(|c| c.anims.as_slice()).collect();
    let header_cb = pb.cb;
    let npb = pb.add_sets(&sets, na as u16, rb as u32).bytes();
    let p2 = 8 + 4 * (ra + na + rb + nb);
    let d2 = p2 + npb.len();
    let shift = d2 as i64 - c.d as i64;
    let moved = |v: u32| (v as i64 + shift) as u32;
    let mut out = Vec::with_capacity(base.len() + 4096);
    out.extend_from_slice(&(p2 as u32).to_le_bytes());
    out.extend_from_slice(&(d2 as u32).to_le_bytes());
    let table_at = out.len();
    let slot_of = |i: usize| if i < ra { table_at + 4 * i } else { table_at + 4 * (na + i) };
    for &m in &ptrs[..ra] {
        out.extend_from_slice(&moved(m).to_le_bytes());
    }
    out.resize(out.len() + 4 * na, 0);
    for &m in &ptrs[ra..] {
        out.extend_from_slice(&moved(m).to_le_bytes());
    }
    out.resize(out.len() + 4 * nb, 0);
    out.extend_from_slice(&npb);
    out.extend_from_slice(&mid[c.d as usize..]);
    let used = out.len() - out.iter().rev().take_while(|&&x| x == 0).count();
    out.truncate(used);
    out.resize((used + GAP).next_multiple_of(4), 0);
    if !freed.is_empty() {
        out.extend_from_slice(MAGIC_FREED);
        out.extend_from_slice(&(base.len() as u32).to_le_bytes());
        out.extend_from_slice(&delta.to_le_bytes());
        let mask_a: u64 = freed.models.iter().fold(0, |m, &i| m | 1 << i);
        let mask_b: u16 = freed.poses.iter().fold(0, |m, &i| m | 1 << i);
        out.extend_from_slice(&mask_a.to_le_bytes());
        out.extend_from_slice(&mask_b.to_le_bytes());
        out.extend_from_slice(&header_cb.to_le_bytes());
        let tmd_at = out.len() as u32;
        out.extend_from_slice(&STUB_TMD);
        let poses_at = out.len() as u32;
        out.extend_from_slice(&STUB_POSES);
        for i in (0..ra + rb).filter(|&i| is_freed(i)) {
            let at = if i < ra { tmd_at } else { poses_at };
            out[slot_of(i)..slot_of(i) + 4].copy_from_slice(&at.to_le_bytes());
        }
    }
    let mut set_index = 0u16;
    for (i, ch) in chars.iter().enumerate() {
        let label = ch.label.as_bytes();
        out.extend_from_slice(label);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let set = if ch.poses.is_some() { set_index } else { 0xFFFF };
        out.extend_from_slice(MAGIC_MODEL);
        out.extend_from_slice(&(ch.model.len() as u32).to_le_bytes());
        out.extend_from_slice(&(base.len() as u32).to_le_bytes());
        out.extend_from_slice(&set.to_le_bytes());
        out.extend_from_slice(&(label.len() as u16).to_le_bytes());
        out.extend_from_slice(&ch.looks.to_le_bytes());
        out.extend_from_slice(&header_cb.to_le_bytes());
        let at = out.len() as u32;
        out[table_at + 4 * (ra + i)..table_at + 4 * (ra + i) + 4].copy_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&ch.model);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        if let Some(poses) = &ch.poses {
            out.extend_from_slice(MAGIC_POSES);
            out.extend_from_slice(&(poses.len() as u32).to_le_bytes());
            let at = out.len() as u32;
            let slot = table_at + 4 * (ra + na + rb + set_index as usize);
            out[slot..slot + 4].copy_from_slice(&at.to_le_bytes());
            out.extend_from_slice(poses);
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            set_index += 1;
        }
    }
    out.resize((out.len() + GAP).div_ceil(2048).max(retail.len().div_ceil(2048)) * 2048, 0);
    if out.len() as u32 > super::text::CAP_BYTES {
        let room = super::text::CAP_BYTES as i64 - (used + GAP) as i64;
        return Err(bad(
            "NPC_CAP",
            format!("With the characters the pack would be {} sectors; the loader takes {} ({} B were free). Free the taken-out actors' models, or bring a smaller character.", out.len() / 2048, crate::npc::CAP_SECTORS, room.max(0)),
        ));
    }
    // It must split back into exactly what went in.
    let (back, got, got_freed) = split(retail, &out)?;
    if back.as_ref() != base {
        let at = back.iter().zip(base).position(|(x, y)| x != y).unwrap_or(back.len().min(base.len()));
        return Err(bad("CHARACTER_VERIFY", format!("The pack with the characters doesn't split back into the same pack (first difference at {at:#x}).")));
    }
    if got != chars || got_freed != *freed {
        return Err(bad("CHARACTER_VERIFY", "The characters or freed models don't read back from the pack."));
    }
    Ok(out)
}

/// `current` without its added characters and with the freed retail
/// sections back (the pack the text layout made), the characters, and
/// what was freed.
pub fn split<'a>(retail: &[u8], current: &'a [u8]) -> Result<(Cow<'a, [u8]>, Vec<Character>, Freed)> {
    let (ra, rb) = retail_counts(retail)?;
    let c = header(current, "project's")?;
    let ca = c.group_a(current);
    let cb = c.models.len() - ca;
    // The freed sections' stubs sit behind an "LTfr" header.
    let magic_at = |at: Option<usize>| at.filter(|&h| current.get(h..h + 4) == Some(MAGIC_FREED.as_slice()));
    let freed_at = c.models.iter().find_map(|&p| magic_at((p as usize).checked_sub(FREED_HEADER)).or_else(|| magic_at((p as usize).checked_sub(FREED_HEADER + STUB_TMD.len()))));
    if ca == ra && cb == rb && freed_at.is_none() {
        return Ok((Cow::Borrowed(current), Vec::new(), Freed::default()));
    }
    if ca < ra || cb < rb {
        return Err(bad("CHARACTER_BASE", "The pack has fewer models than retail."));
    }
    let lost = || bad("CHARACTER_LOST", "An added character's or freed model's header is missing; revert the pack.");
    let rd = |at: usize, n: usize| current.get(at..at + n).ok_or_else(lost);
    let mut chars = Vec::new();
    let mut cut = freed_at.unwrap_or(usize::MAX);
    let mut base_len = freed_at.map(|h| le32(current, h + 4) as usize);
    let mut header_cb: Option<u16> = freed_at.map(|h| le16(current, h + 22));
    let mut posed: Vec<(u16, usize)> = Vec::new();
    for i in 0..ca - ra {
        let at = c.models[ra + i] as usize;
        let h = at.checked_sub(MODEL_HEADER).ok_or_else(lost)?;
        if rd(h, 4)? != MAGIC_MODEL {
            return Err(lost());
        }
        let len = le32(current, h + 4) as usize;
        base_len = Some(le32(current, h + 8) as usize);
        let set = le16(current, h + 12);
        let label_len = le16(current, h + 14) as usize;
        let looks = le16(current, h + 16);
        header_cb.get_or_insert(le16(current, h + 18));
        let label_at = h.checked_sub(label_len.next_multiple_of(4)).ok_or_else(lost)?;
        let label = String::from_utf8_lossy(rd(label_at, label_len)?).into_owned();
        cut = cut.min(label_at);
        let model = rd(at, len)?.to_vec();
        if set != 0xFFFF {
            posed.push((set, chars.len()));
        }
        chars.push(Character { label, model, poses: None, anims: Vec::new(), looks });
    }
    for (set, k) in &posed {
        let slot = ca + rb + *set as usize;
        let at = *c.models.get(slot).ok_or_else(lost)? as usize;
        let h = at.checked_sub(POSE_HEADER).ok_or_else(lost)?;
        if rd(h, 4)? != MAGIC_POSES {
            return Err(lost());
        }
        let len = le32(current, h + 4) as usize;
        cut = cut.min(h);
        chars[*k].poses = Some(rd(at, len)?.to_vec());
        chars[*k].anims = c.anim_set(current, rb + *set as usize);
    }
    if posed.len() != cb - rb {
        return Err(lost());
    }
    // The table without the characters' words, the P block without their sets.
    let pb = PBlock::read(current, &c)?.drop_sets(ra as u16, rb as u16, (cb - rb) as u16, header_cb.ok_or_else(lost)?)?.bytes();
    let p0 = 8 + 4 * (ra + rb);
    let d0 = p0 + pb.len();
    let shift = c.d as i64 - d0 as i64;
    let mut ptrs: Vec<u32> = c.models[..ra].iter().chain(&c.models[ca..ca + rb]).map(|&v| (v as i64 - shift) as u32).collect();
    if cut < c.d as usize {
        return Err(lost());
    }
    let mut mid = Vec::with_capacity(current.len());
    mid.extend_from_slice(&(p0 as u32).to_le_bytes());
    mid.extend_from_slice(&(d0 as u32).to_le_bytes());
    mid.resize(p0, 0);
    mid.extend_from_slice(&pb);
    mid.extend_from_slice(&current[c.d as usize..cut]);
    // The freed sections back, from retail, where the text layout had them.
    let mut freed = Freed::default();
    if let Some(h) = freed_at {
        let delta = le32(current, h + 8);
        let mask_a = u64::from_le_bytes(current[h + 12..h + 20].try_into().expect("8 bytes"));
        let mask_b = le16(current, h + 20);
        freed.models = (0..ra.min(64)).filter(|&i| mask_a >> i & 1 != 0).map(|i| i as u8).collect();
        freed.poses = (0..rb.min(16)).filter(|&i| mask_b >> i & 1 != 0).map(|i| i as u8).collect();
        let r = header(retail, "retail")?;
        let is_freed = |i: usize| if i < ra { freed.models.contains(&(i as u8)) } else { freed.poses.contains(&((i - ra) as u8)) };
        let mut secs: Vec<(u32, u32)> = retail_sections(&r, retail).into_iter().filter(|(o, _)| (0..ra + rb).any(|i| is_freed(i) && r.models[i] == *o)).collect();
        secs.sort_unstable();
        for (o, len) in secs {
            let at = o.wrapping_add(delta);
            if at as usize > mid.len() {
                mid.resize(at as usize, 0);
            }
            mid.splice(at as usize..at as usize, retail[o as usize..(o + len) as usize].iter().copied());
            for (i, p) in ptrs.iter_mut().enumerate() {
                if is_freed(i) {
                    if r.models[i] == o {
                        *p = at;
                    }
                } else if *p >= at {
                    *p += len;
                }
            }
        }
    }
    for (i, p) in ptrs.iter().enumerate() {
        mid[8 + 4 * i..12 + 4 * i].copy_from_slice(&p.to_le_bytes());
    }
    mid.resize(base_len.ok_or_else(lost)?, 0);
    Ok((Cow::Owned(mid), chars, freed))
}

/// `current` without its added characters (borrowed when it has none).
pub fn base<'a>(retail: &[u8], current: &'a [u8]) -> Result<Cow<'a, [u8]>> {
    Ok(split(retail, current)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_go_in_and_come_out() {
        // Two sets, lists of 2 and 1 sequences, data after the run.
        let mut pb = PBlock { ca: 3, cb: 2, words: vec![], data: vec![] };
        // run: [set0 → list@w2, set1 → list@w4, l0a, l0b, l1a]
        let data_at = 8 + 4 * 5;
        pb.words = vec![4 + 4 * 2, 4 + 4 * 4, data_at, data_at + 3, data_at + 6];
        pb.data = vec![1, 1, 13, 1, 2, 9, 1, 3, 9, 0, 0, 0];
        let add: Vec<Vec<(u8, u8)>> = vec![vec![(1, 20), (2, 20)], vec![(3, 30)]];
        let grown = pb.add_sets(&[add.as_slice()], 1, 2);
        assert_eq!((grown.ca, grown.cb), (4, 3));
        assert_eq!(grown.drop_sets(3, 2, 1, 2).unwrap(), pb);
        // The old sets still decode the same through the grown block.
        let bytes = grown.bytes();
        let word = |i: usize| u32::from_le_bytes(bytes[4 + 4 * i..8 + 4 * i].try_into().unwrap());
        // set 0's list starts at word (word(0)-4)/4, its first sequence's count is 1, pose 1.
        let list0 = (word(0) - 4) / 4;
        let seq0 = word(list0 as usize) as usize;
        assert_eq!(&bytes[seq0..seq0 + 3], &[1, 1, 13]);
        // the new set (index 2) has 2 sequences.
        let list2 = (word(2) - 4) / 4;
        let s = word(list2 as usize) as usize;
        assert_eq!(&bytes[s..s + 5], &[2, 1, 20, 2, 20]);
    }
}
