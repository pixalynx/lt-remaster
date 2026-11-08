//! Morph-target pose blocks ("Group B"). Pose 0 is the base mesh and is never
//! stored; each stored pose is a list of records `{u32 more, u32 first_vertex,
//! u32 count, s16[3] × count}` packed without alignment. The game applies only
//! the first record of a pose (`FindPoseRecord`).

use serde::Serialize;

use crate::container::{le32, le_i16};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoseStats {
    /// 1-based pose number.
    pub pose: u32,
    pub first_vertex: u32,
    pub count: u32,
    /// Records stored for this pose (the engine uses only the first).
    pub records: u32,
    /// Largest absolute delta on any axis, in model units.
    pub max_delta: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoseBlock {
    pub poses: Vec<PoseStats>,
    /// Bytes the block occupies.
    pub len: usize,
}

pub fn parse(b: &[u8]) -> Option<PoseBlock> {
    let n = le32(b.get(0..4)?, 0);
    if n > 256 {
        return None;
    }
    let mut p = 4usize;
    let mut poses = Vec::with_capacity(n as usize);
    for i in 0..n {
        let mut first: Option<(u32, u32, u16)> = None;
        let mut records = 0;
        loop {
            let hdr = b.get(p..p + 12)?;
            let (more, first_vertex, count) = (le32(hdr, 0), le32(hdr, 4), le32(hdr, 8));
            let data = b.get(p + 12..p + 12 + count as usize * 6)?;
            let max_delta = (0..count as usize * 3).map(|k| le_i16(data, k * 2).unsigned_abs()).max().unwrap_or(0);
            if first.is_none() {
                first = Some((first_vertex, count, max_delta));
            }
            records += 1;
            p += 12 + count as usize * 6;
            if more == 0 {
                break;
            }
        }
        let (first_vertex, count, max_delta) = first?;
        poses.push(PoseStats { pose: i + 1, first_vertex, count, records, max_delta });
    }
    Some(PoseBlock { poses, len: p })
}
