//! Poses and battle sequences written back (screen 08, research 06 phase 1).
//!
//! **Poses** are vertex morphs: one record per pose, `{u32 more, u32 first,
//! u32 count, s16[3] × count}`, packed without alignment (research 02 §3.1).
//! The engine applies only the first record of a pose, so the writer emits
//! exactly one, covering the smallest vertex range that moves. A record
//! never has count 0: `ApplyWeightedVertexDeltas` is a do-while, so an empty
//! record runs 2³² times; a pose that moves nothing is one zero delta on
//! vertex 0.
//!
//! New poses come from **OBJ frames** (Blender: File › Export › Wavefront
//! with *Animation* ticked writes one OBJ per frame, in mesh vertex order, so
//! shape keys and armatures both bake down to vertex positions) or from a
//! glTF's **morph targets** (shape keys), which the model importer reads
//! with the mesh ([`super::model::replace`]).
//!
//! **Battle sequences** are `(pose, speed)` pairs ending in speed 0 (stand
//! on the pose) or a `(0xFF, n)` loop marker, reached through a table of
//! u16 offsets from a data base, followed by the action scripts (6-byte
//! header, 14-byte records up to an all-zero record):
//!
//! | Container | Section | Offsets | Data | Copied |
//! |---|---|---|---|---|
//! | toy | 1 (ability data) | +0x90 [6] seqs, [6] scripts | +0xA8 | whole section (toy slot 0xE000) |
//! | enemy | 6 (profile) | +0x11C [12] seqs, +0x134 [6] scripts | +0x140 | 0x398 |
//! | battle Knight | 3 (profile) | +0x200 [16] seqs, +0x230 [16] scripts | +0x250 | 0x570 |
//!
//! A changed sequence that fits its old block (and no other slot shares it)
//! is written in place; otherwise it moves to the free space after the last
//! block, inside what the battle copies. Baby forms (PB01 has a 24-slot
//! player table whose script table position isn't pinned down), the three
//! prototype enemies (fields shifted one byte) and the field sets in the EXE
//! are read-only here.

use serde::Serialize;

use crate::container::{le16, le32, le_i16};

use super::meshin::InMesh;
use super::model::{self, Container};
use super::tmdcodec::{Object, Tmd};
use super::{EditError, Result};

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

/* ───────────────────────── Poses ───────────────────────── */

/// One pose: deltas (model units, Y down) for vertices `first..first + len`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pose {
    pub first: u32,
    pub deltas: Vec<[i16; 3]>,
}

impl Pose {
    /// Moves nothing (one zero delta: the engine's loop runs at least once).
    pub fn neutral() -> Self {
        Self { first: 0, deltas: vec![[0; 3]] }
    }

    /// The smallest single record that carries `dense` (one delta per vertex).
    pub fn from_dense(dense: &[[i16; 3]]) -> Self {
        let moved = |d: &[i16; 3]| *d != [0; 3];
        match (dense.iter().position(moved), dense.iter().rposition(moved)) {
            (Some(a), Some(z)) => Self { first: a as u32, deltas: dense[a..=z].to_vec() },
            _ => Self::neutral(),
        }
    }

    /// One delta per vertex of an `n`-vertex model.
    pub fn dense(&self, n: usize) -> Vec<[i16; 3]> {
        let mut out = vec![[0; 3]; n];
        for (k, d) in self.deltas.iter().enumerate() {
            if let Some(slot) = out.get_mut(self.first as usize + k) {
                *slot = *d;
            }
        }
        out
    }

    pub fn end(&self) -> usize {
        self.first as usize + self.deltas.len()
    }
}

/// The first record of each pose in a pose block (what the engine applies).
pub fn read_poses(b: &[u8]) -> Option<Vec<Pose>> {
    let n = le32(b.get(0..4)?, 0) as usize;
    if n > 256 {
        return None;
    }
    let mut p = 4usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let mut first: Option<Pose> = None;
        loop {
            let hdr = b.get(p..p + 12)?;
            let (more, start, count) = (le32(hdr, 0), le32(hdr, 4), le32(hdr, 8) as usize);
            let data = b.get(p + 12..p + 12 + count * 6)?;
            if first.is_none() {
                let deltas = (0..count).map(|k| [le_i16(data, k * 6), le_i16(data, k * 6 + 2), le_i16(data, k * 6 + 4)]).collect();
                first = Some(Pose { first: start, deltas });
            }
            p += 12 + count * 6;
            if more == 0 {
                break;
            }
        }
        out.push(first?);
    }
    Some(out)
}

/// A pose block: the count, then one record per pose.
pub fn write_poses(poses: &[Pose]) -> Vec<u8> {
    let mut out = (poses.len() as u32).to_le_bytes().to_vec();
    for p in poses {
        let p = if p.deltas.is_empty() { Pose::neutral() } else { p.clone() };
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&p.first.to_le_bytes());
        out.extend_from_slice(&(p.deltas.len() as u32).to_le_bytes());
        for d in &p.deltas {
            for c in d {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
    }
    out
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FramesReport {
    pub poses: u32,
    /// Largest delta on any axis, model units.
    pub max_delta: u16,
    /// Deltas past ±32767 that were saturated.
    pub clamped: u32,
    /// Poses that move nothing.
    pub still: u32,
}

fn q16(v: f32) -> i16 {
    v.round().clamp(-32768.0, 32767.0) as i16
}

/// Poses from posed copies of `base`: each frame is an OBJ with the same
/// vertices in the same order (Y up, as exported), frame i → pose i + 1.
/// A frame that reproduces `reference[i]` keeps that pose's record as it
/// was (its vertex range, even one past the model as `toy/23` has).
pub fn poses_from_frames(base: &Object, frames: &[InMesh], scale: f32, reference: &[Pose]) -> Result<(Vec<Pose>, FramesReport)> {
    let n = base.verts.len();
    let mut report = FramesReport { poses: frames.len() as u32, ..Default::default() };
    let mut out = Vec::with_capacity(frames.len());
    for (i, f) in frames.iter().enumerate() {
        if !f.indexed {
            return Err(bad("POSE_FRAME_FORMAT", format!("Frame {} isn't an OBJ: frames must keep the model's vertex order.", i + 1)));
        }
        if f.positions.len() != n {
            return Err(bad("POSE_FRAME_VERTICES", format!("Frame {} has {} vertices; the model has {n}. Export frames from the same mesh.", i + 1, f.positions.len())));
        }
        let dense: Vec<[i16; 3]> = f
            .positions
            .iter()
            .zip(&base.verts)
            .map(|(p, b)| {
                let target = [p[0] * scale, -p[1] * scale, p[2] * scale];
                let mut d = [0i16; 3];
                for c in 0..3 {
                    let v = target[c].round() - b[c] as f32;
                    if v.abs() > 32767.0 {
                        report.clamped += 1;
                    }
                    d[c] = q16(v);
                    report.max_delta = report.max_delta.max(d[c].unsigned_abs());
                }
                d
            })
            .collect();
        let pose = match reference.get(i) {
            Some(r) if r.dense(n) == dense => r.clone(),
            _ => Pose::from_dense(&dense),
        };
        if dense.iter().all(|d| *d == [0; 3]) {
            report.still += 1;
        }
        out.push(pose);
    }
    Ok((out, report))
}

/// Poses from a mesh's morph targets (glTF shape keys), as deltas per
/// encoded vertex: `vid` maps each mesh position to its welded vertex (the
/// first position to reach a vertex gives its delta).
pub fn poses_from_morphs(mesh: &InMesh, vid: &[usize], nverts: usize, scale: f32) -> (Vec<Pose>, FramesReport) {
    let mut report = FramesReport { poses: mesh.morphs.len() as u32, ..Default::default() };
    let poses = mesh
        .morphs
        .iter()
        .map(|m| {
            let mut dense = vec![[0i16; 3]; nverts];
            let mut set = vec![false; nverts];
            for (i, d) in m.iter().enumerate() {
                let Some(&v) = vid.get(i) else { continue };
                if v >= nverts || set[v] {
                    continue;
                }
                set[v] = true;
                let raw = [d[0] * scale, -d[1] * scale, d[2] * scale];
                for c in 0..3 {
                    if raw[c].abs() > 32767.0 {
                        report.clamped += 1;
                    }
                    dense[v][c] = q16(raw[c]);
                    report.max_delta = report.max_delta.max(dense[v][c].unsigned_abs());
                }
            }
            let p = Pose::from_dense(&dense);
            if p == Pose::neutral() {
                report.still += 1;
            }
            p
        })
        .collect();
    (poses, report)
}

/// Poses from a glTF's shape keys on a model that wasn't imported from it
/// (an exported OBJ given shape keys in Blender, then saved as glTF): each
/// model vertex takes the morph of the mesh position at its rest position.
/// Returns the poses and how many model vertices found no position.
pub fn poses_from_morphs_by_position(base: &Object, mesh: &InMesh, scale: f32) -> Result<(Vec<Pose>, FramesReport, u32)> {
    if mesh.morphs.is_empty() {
        return Err(bad("POSE_NO_SHAPE_KEYS", "The glTF has no shape keys (morph targets)."));
    }
    let key = |p: [f32; 3]| [q16(p[0] * scale), q16(-p[1] * scale), q16(p[2] * scale)];
    let mut at: std::collections::HashMap<[i16; 3], usize> = std::collections::HashMap::new();
    for (i, p) in mesh.positions.iter().enumerate() {
        at.entry(key(*p)).or_insert(i);
    }
    let mut missing = 0;
    let vid: Vec<usize> = base
        .verts
        .iter()
        .map(|v| match at.get(&[v[0], v[1], v[2]]) {
            Some(&i) => i,
            None => {
                missing += 1;
                usize::MAX
            }
        })
        .collect();
    let mut report = FramesReport { poses: mesh.morphs.len() as u32, ..Default::default() };
    let poses = mesh
        .morphs
        .iter()
        .map(|m| {
            let dense: Vec<[i16; 3]> = vid
                .iter()
                .map(|&i| {
                    let Some(d) = m.get(i) else { return [0; 3] };
                    let raw = [d[0] * scale, -d[1] * scale, d[2] * scale];
                    let mut out = [0i16; 3];
                    for c in 0..3 {
                        if raw[c].abs() > 32767.0 {
                            report.clamped += 1;
                        }
                        out[c] = q16(raw[c]);
                        report.max_delta = report.max_delta.max(out[c].unsigned_abs());
                    }
                    out
                })
                .collect();
            if dense.iter().all(|d| *d == [0; 3]) {
                report.still += 1;
            }
            Pose::from_dense(&dense)
        })
        .collect();
    Ok((poses, report, missing))
}

/// Each pose of `file`'s model as an OBJ (the model at full weight on that
/// pose), sharing the base export's MTL: `name` is the MTL's stem.
pub fn pose_objs(kind: Container, file: &[u8], name: &str) -> Result<Vec<String>> {
    let (tmd, bind) = model::model_of(kind, file)?;
    let poses = poses_of(kind, file)?;
    let base = tmd.objects.first().ok_or_else(|| bad("MODEL_EMPTY", "The model has no object."))?;
    Ok(poses
        .iter()
        .map(|p| {
            let mut o = base.clone();
            for (v, d) in o.verts.iter_mut().zip(p.dense(base.verts.len())) {
                for c in 0..3 {
                    v[c] = v[c].wrapping_add(d[c]);
                }
            }
            model::export_obj(&Tmd::new(vec![o]), bind, name).0
        })
        .collect())
}

/// The pose block of `file` (`kind`), first records.
pub fn poses_of(kind: Container, file: &[u8]) -> Result<Vec<Pose>> {
    let parts = crate::mesh::model_parts(kind, file).ok_or_else(|| bad("CONTAINER_BAD", "The file's section table doesn't parse."))?;
    let b = parts.poses.ok_or_else(|| bad("NO_POSES", "This file has no pose block."))?;
    read_poses(b).ok_or_else(|| bad("POSES_BAD", "The pose block doesn't parse."))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PosesReplaced {
    pub poses: u32,
    pub poses_before: u32,
    /// Highest pose a battle sequence of this file stands on (0 = none read).
    pub highest_used: u32,
    pub block_bytes: u32,
    pub file_bytes: u32,
    pub budgets: Vec<model::Budget>,
}

/// `file` with its pose block replaced by `poses` (vertex order locked to
/// the model). Fewer poses than a battle sequence stands on is refused.
pub fn replace_poses(kind: Container, file: &[u8], poses: &[Pose]) -> Result<(Vec<u8>, PosesReplaced)> {
    let (tmd, _) = model::model_of(kind, file)?;
    let n = tmd.objects.first().map_or(0, |o| o.verts.len());
    let old = poses_of(kind, file)?;
    // Unchanged retail poses may already reach past the model (toy/23); new ones may not.
    if let Some((i, p)) = poses.iter().enumerate().find(|(i, p)| p.end() > n && old.get(*i) != Some(p)) {
        return Err(bad("POSE_RANGE", format!("Pose {} reaches vertex {}; the model has {n}.", i + 1, p.end())));
    }
    if poses.len() > 255 {
        return Err(bad("POSE_COUNT", format!("{} poses; sequences address 255 at most.", poses.len())));
    }
    let before = old.len() as u32;
    let highest_used = sequences(kind, file).map(|t| t.highest_pose()).unwrap_or(0);
    if (poses.len() as u32) < highest_used {
        return Err(bad("POSE_COUNT", format!("A battle sequence stands on pose {highest_used}, but only {} poses would be left.", poses.len())));
    }
    let block = write_poses(poses);
    let out = model::replace_section(kind, file, model::Role::Poses, block.clone())?;
    let budgets = model::budgets_of(kind, &out, file.len())?;
    if let Some(b) = budgets.iter().find(|b| b.used > b.cap) {
        return Err(bad("POSE_BUDGET", format!("{}: {} B used, the game stops at {} B.", b.what, b.used, b.cap)));
    }
    let rep = PosesReplaced { poses: poses.len() as u32, poses_before: before, highest_used, block_bytes: block.len() as u32, file_bytes: out.len() as u32, budgets };
    Ok((out, rep))
}

/* ───────────────────────── Battle sequences ───────────────────────── */

/// Where a container keeps its battle sequences.
#[derive(Debug, Clone, Copy)]
struct Layout {
    sections: usize,
    section: usize,
    seq_table: usize,
    seqs: usize,
    script_table: usize,
    scripts: usize,
    base: usize,
    /// Bytes the battle copies from the section start (`None`: the whole section).
    cap: Option<usize>,
}

fn layout(kind: Container) -> Option<Layout> {
    Some(match kind {
        Container::Toy => Layout { sections: 8, section: 1, seq_table: 0x90, seqs: 6, script_table: 0x9C, scripts: 6, base: 0xA8, cap: None },
        Container::Enemy => Layout { sections: 7, section: 6, seq_table: 0x11C, seqs: 12, script_table: 0x134, scripts: 6, base: 0x140, cap: Some(0x398) },
        Container::Knight => Layout { sections: 4, section: 3, seq_table: 0x200, seqs: 16, script_table: 0x230, scripts: 16, base: 0x250, cap: Some(0x570) },
        _ => return None,
    })
}

/// One battle sequence slot.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    pub index: usize,
    /// Data offset (from the base).
    pub offset: u16,
    /// `(pose, speed)` pairs, the `(0xFF, n)` loop marker included. Empty
    /// when the slot points at another kind of block (unused slots point
    /// at the next block, often script 0).
    pub pairs: Vec<(u8, u8)>,
    /// Another slot shares this block.
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sequences {
    pub slots: Vec<Slot>,
    /// Script offsets (from the base).
    pub scripts: Vec<u16>,
    /// End of the last block, from the section start.
    pub used: usize,
    /// What the battle copies (or the section length for toys).
    pub cap: usize,
}

impl Sequences {
    pub fn highest_pose(&self) -> u32 {
        self.slots.iter().flat_map(|s| s.pairs.iter()).filter(|p| p.0 != 0xFF).map(|p| p.0 as u32).max().unwrap_or(0)
    }
}

/// The pairs at `at` (section offset): up to speed 0 or a loop marker.
fn read_pairs(sec: &[u8], at: usize) -> Option<Vec<(u8, u8)>> {
    let mut out = Vec::new();
    let mut p = at;
    loop {
        let pair = sec.get(p..p + 2)?;
        out.push((pair[0], pair[1]));
        p += 2;
        if pair[0] == 0xFF || pair[1] == 0 {
            return Some(out);
        }
        if out.len() > 128 {
            return None;
        }
    }
}

/// End of the action script at `at`: 6-byte header, 14-byte records up to an all-zero one.
fn script_end(sec: &[u8], at: usize) -> Option<usize> {
    let mut p = at + 6;
    for _ in 0..256 {
        let rec = sec.get(p..p + 14)?;
        p += 14;
        if rec.iter().all(|&b| b == 0) {
            return Some(p);
        }
    }
    None
}

/// Whether the block at `at` reads as a sequence: every pose within the
/// pose count (or the loop marker pointing back inside it).
fn looks_like_sequence(pairs: &[(u8, u8)], poses: usize) -> bool {
    let n = pairs.len();
    pairs.iter().enumerate().all(|(i, &(p, s))| if p == 0xFF { i == n - 1 && (s as usize) < i.max(1) } else { p as usize <= poses })
}

fn section(kind: Container, file: &[u8]) -> Result<(Layout, crate::container::Section)> {
    let l = layout(kind).ok_or_else(|| bad("SEQ_LAYOUT", format!("{kind:?} sequences aren't editable here.")))?;
    let s = crate::container::sections(file, l.sections).ok_or_else(|| bad("CONTAINER_BAD", "The file's section table doesn't parse."))?;
    Ok((l, s[l.section].clone()))
}

/// The battle sequences of `file`.
pub fn sequences(kind: Container, file: &[u8]) -> Result<Sequences> {
    let (l, s) = section(kind, file)?;
    let sec = &file[s.offset as usize..(s.offset + s.len) as usize];
    if sec.len() < l.base {
        return Err(bad("SEQ_SHORT", "The section is shorter than its sequence table."));
    }
    let poses = poses_of(kind, file).map(|p| p.len()).unwrap_or(0);
    let scripts: Vec<u16> = (0..l.scripts).map(|i| le16(sec, l.script_table + i * 2)).collect();
    let offsets: Vec<u16> = (0..l.seqs).map(|i| le16(sec, l.seq_table + i * 2)).collect();
    let mut used = l.base;
    for &o in &scripts {
        // A script cut off by the section's end (toys' unused slots 7–11
        // point 2 bytes before it) is taken to use the rest of the section.
        used = used.max(script_end(sec, l.base + o as usize).unwrap_or(sec.len()));
    }
    let slots: Vec<Slot> = offsets
        .iter()
        .enumerate()
        .map(|(i, &o)| {
            let pairs = if scripts.contains(&o) {
                Vec::new()
            } else {
                read_pairs(sec, l.base + o as usize).filter(|p| looks_like_sequence(p, poses)).unwrap_or_default()
            };
            if !pairs.is_empty() {
                used = used.max(l.base + o as usize + pairs.len() * 2);
            }
            Slot { index: i, offset: o, shared: offsets.iter().filter(|&&x| x == o).count() > 1, pairs }
        })
        .collect();
    Ok(Sequences { slots, scripts, used, cap: l.cap.unwrap_or(sec.len()) })
}

/// Why a file's sequences can't be read or written here, by model id.
pub fn read_only(id: &str) -> Option<&'static str> {
    let (kind, n) = id.split_once('/')?;
    let n: u16 = n.parse().ok()?;
    match kind {
        "enemy" if crate::gamedata::enemy::PROTOTYPES.contains(&n) => Some("Prototype enemy: its profile fields sit one byte further."),
        "pb" => Some("Baby forms: the player table's script offsets aren't pinned down yet."),
        _ if id.starts_with("sys/6#") => Some("Field sets live in the EXE."),
        _ => None,
    }
}

/// Checks a pair list: entries end in speed 0 or a loop marker that points back.
pub fn check_pairs(pairs: &[(u8, u8)], poses: usize) -> Result<()> {
    let Some(&(lp, ls)) = pairs.last() else { return Err(bad("SEQ_EMPTY", "A sequence needs at least one entry.")) };
    if lp == 0xFF {
        if pairs.len() < 2 || ls as usize >= pairs.len() - 1 {
            return Err(bad("SEQ_LOOP", format!("The loop goes back to entry {ls}; there are {} entries before it.", pairs.len() - 1)));
        }
    } else if ls != 0 {
        return Err(bad("SEQ_END", "The last entry needs speed 0 (stand on the pose) or a loop back."));
    }
    for (i, &(p, s)) in pairs.iter().enumerate() {
        if p == 0xFF && i != pairs.len() - 1 {
            return Err(bad("SEQ_LOOP", format!("Entry {i} is a loop marker before the end.")));
        }
        if p != 0xFF && p as usize > poses {
            return Err(bad("SEQ_POSE", format!("Entry {i} stands on pose {p}; the model has {poses}.")));
        }
        if p != 0xFF && s == 0 && i != pairs.len() - 1 {
            return Err(bad("SEQ_END", format!("Entry {i} has speed 0, which ends the sequence early.")));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceWritten {
    pub slot: usize,
    /// Written over the old block (else moved to free space).
    pub in_place: bool,
    pub offset: u16,
    pub used: u32,
    pub cap: u32,
    pub file_bytes: u32,
}

/// `file` with battle sequence `slot` set to `pairs`.
pub fn set_sequence(kind: Container, file: &[u8], slot: usize, pairs: &[(u8, u8)]) -> Result<(Vec<u8>, SequenceWritten)> {
    let (l, s) = section(kind, file)?;
    let seqs = sequences(kind, file)?;
    let cur = seqs.slots.get(slot).ok_or_else(|| bad("SEQ_SLOT", format!("No sequence slot {slot}; this layout has {}.", l.seqs)))?;
    let poses = poses_of(kind, file)?.len();
    check_pairs(pairs, poses)?;
    if cur.pairs == pairs {
        let rep = SequenceWritten { slot, in_place: true, offset: cur.offset, used: seqs.used as u32, cap: seqs.cap as u32, file_bytes: file.len() as u32 };
        return Ok((file.to_vec(), rep));
    }
    // A slot that shares its block with another moves, so the other keeps its pairs.
    let mut sec = file[s.offset as usize..(s.offset + s.len) as usize].to_vec();
    let bytes: Vec<u8> = pairs.iter().flat_map(|&(p, sp)| [p, sp]).collect();
    let old_len = cur.pairs.len() * 2;
    let (offset, in_place) = if !cur.shared && !cur.pairs.is_empty() && bytes.len() <= old_len {
        let at = l.base + cur.offset as usize;
        sec[at..at + old_len].fill(0);
        sec[at..at + bytes.len()].copy_from_slice(&bytes);
        (cur.offset, true)
    } else {
        // Past every block, and never on an offset another slot points at
        // (toys' unused script slots point at the section's end).
        let mut at = seqs.used.next_multiple_of(2);
        let taken = |a: usize| a >= l.base && (seqs.scripts.iter().chain(seqs.slots.iter().map(|s| &s.offset)).any(|&o| l.base + o as usize == a));
        while taken(at) {
            at += 2;
        }
        let end = at + bytes.len();
        if let Some(cap) = l.cap {
            if end > cap {
                return Err(bad("SEQ_BUDGET", format!("The sequences would run to {end:#x}; the battle copies {cap:#x} bytes. Shorten a sequence.")));
            }
        }
        if end > sec.len() {
            sec.resize(end, 0);
        }
        sec[at..end].copy_from_slice(&bytes);
        let o = at - l.base;
        if o > 0xFFFF {
            return Err(bad("SEQ_BUDGET", "The sequence data would pass 64 KB."));
        }
        (o as u16, false)
    };
    sec[l.seq_table + slot * 2..l.seq_table + slot * 2 + 2].copy_from_slice(&offset.to_le_bytes());
    let out = model::replace_section_index(kind, file, l.section, sec)?;
    let budgets = model::budgets_of(kind, &out, file.len())?;
    if let Some(b) = budgets.iter().find(|b| b.used > b.cap) {
        return Err(bad("SEQ_BUDGET", format!("{}: {} B used, the game stops at {} B.", b.what, b.used, b.cap)));
    }
    let after = sequences(kind, &out)?;
    let rep = SequenceWritten { slot, in_place, offset, used: after.used as u32, cap: after.cap as u32, file_bytes: out.len() as u32 };
    Ok((out, rep))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poses_round_trip_and_stay_non_empty() {
        let dense = vec![[0, 0, 0], [0, 2, 0], [0, 0, 0], [1, 0, -1], [0, 0, 0]];
        let p = Pose::from_dense(&dense);
        assert_eq!((p.first, p.deltas.len()), (1, 3));
        assert_eq!(p.dense(5), dense);
        assert_eq!(Pose::from_dense(&[[0; 3]; 4]), Pose::neutral());
        let block = write_poses(&[p.clone(), Pose::neutral(), Pose { first: 2, deltas: vec![] }]);
        let back = read_poses(&block).unwrap();
        assert_eq!(back, vec![p, Pose::neutral(), Pose::neutral()]);
        assert!(back.iter().all(|p| !p.deltas.is_empty()));
    }

    #[test]
    fn pair_lists_are_checked() {
        assert!(check_pairs(&[(1, 64), (2, 64), (0xFF, 0)], 2).is_ok());
        assert!(check_pairs(&[(1, 64), (2, 0)], 2).is_ok());
        assert!(check_pairs(&[(1, 64), (2, 64)], 2).is_err());
        assert!(check_pairs(&[(3, 0)], 2).is_err());
        assert!(check_pairs(&[(1, 64), (0xFF, 1)], 2).is_err());
        assert!(check_pairs(&[(1, 0), (2, 0)], 2).is_err());
    }
}
