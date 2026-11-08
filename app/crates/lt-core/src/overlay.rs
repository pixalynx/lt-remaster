//! A project's staged files laid over the retail disc. Readers that show
//! editable data take an optional [`Staged`] map so a saved edit appears
//! everywhere; the disc itself is never written (`docs/screens/03-projects-and-saving.md` §5.10).

use std::collections::HashMap;
use std::sync::Arc;

use crate::disc::{DiscImage, USER_DATA};
use crate::error::{LtError, Result};
use crate::fs::FileTable;
use crate::world::{is_new_area, CHUNK_COUNT, CHUNK_SECTORS};

/// Staged bytes by canonical id (`npc/7`, `enemy/17`, `chunk/39`, `exe`).
/// Every member of an alias group has its own key (`npc/0` and `npc/17`).
pub type Staged = HashMap<String, Arc<Vec<u8>>>;

/// Retail bytes of a table file or a world chunk (`chunk/n`), sector-padded.
pub fn retail(image: &DiscImage, table: &FileTable, id: &str) -> Result<Vec<u8>> {
    if let Some(n) = id.strip_prefix("chunk/") {
        let n: u32 = n.parse().map_err(|_| LtError::FileNotFound(id.into()))?;
        let world = table.by_id("world").ok_or_else(|| LtError::FileNotFound("world".into()))?;
        let lba = world.extent.lba + n * CHUNK_SECTORS;
        if is_new_area(n) {
            // A new area reads as zeros until the project makes it, unless the image
            // (a base) already holds it past the retail end.
            if lba + CHUNK_SECTORS <= image.sectors() {
                return image.read_sectors(lba, CHUNK_SECTORS);
            }
            return Ok(vec![0; CHUNK_SECTORS as usize * USER_DATA]);
        }
        if n >= CHUNK_COUNT {
            return Err(LtError::FileNotFound(id.into()));
        }
        return image.read_sectors(lba, CHUNK_SECTORS);
    }
    let e = table.by_id(id).ok_or_else(|| LtError::FileNotFound(id.into()))?;
    image.read_sectors(e.extent.lba, e.extent.sectors)
}

/// `id` as a project sees it: the staged bytes if there are any, else the disc's.
pub fn file(image: &DiscImage, table: &FileTable, staged: Option<&Staged>, id: &str) -> Result<Vec<u8>> {
    match staged.and_then(|s| s.get(id)) {
        Some(b) => Ok(b.as_ref().clone()),
        None => retail(image, table, id),
    }
}

/// Size in bytes of `id` as a project sees it (whole sectors on the disc).
pub fn file_len(table: &FileTable, staged: Option<&Staged>, id: &str) -> Result<u32> {
    if let Some(b) = staged.and_then(|s| s.get(id)) {
        return Ok(b.len() as u32);
    }
    let e = table.by_id(id).ok_or_else(|| LtError::FileNotFound(id.into()))?;
    Ok(e.extent.sectors * USER_DATA as u32)
}

/// `len` bytes at `offset` of `id` as a project sees it (staged files are
/// sliced; disc files read only the sectors needed). Clipped to the end.
pub fn file_part(image: &DiscImage, table: &FileTable, staged: Option<&Staged>, id: &str, offset: u32, len: usize) -> Result<Vec<u8>> {
    if let Some(b) = staged.and_then(|s| s.get(id)) {
        let start = (offset as usize).min(b.len());
        return Ok(b[start..(start + len).min(b.len())].to_vec());
    }
    let e = table.by_id(id).ok_or_else(|| LtError::FileNotFound(id.into()))?;
    let size = e.extent.sectors as usize * USER_DATA;
    let start = (offset as usize).min(size);
    image.read_file_bytes(e.extent.lba, start as u64, len.min(size - start))
}

/// The ids whose staged bytes differ between two overlays (added, removed or
/// changed). Cheap: unchanged entries are usually the same allocation.
pub fn changed(old: &Staged, new: &Staged) -> Vec<String> {
    let mut out: Vec<String> = old
        .iter()
        .filter(|(id, b)| new.get(*id).is_none_or(|n| !Arc::ptr_eq(n, b) && n.as_slice() != b.as_slice()))
        .map(|(id, _)| id.clone())
        .collect();
    out.extend(new.keys().filter(|id| !old.contains_key(*id)).cloned());
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_ids() {
        let a: Staged = [("npc/7".to_string(), Arc::new(vec![1u8, 2])), ("enemy/17".to_string(), Arc::new(vec![3u8]))].into();
        let mut b = a.clone();
        assert!(changed(&a, &b).is_empty());
        b.insert("npc/7".into(), Arc::new(vec![1, 2])); // same bytes, new allocation
        assert!(changed(&a, &b).is_empty());
        b.insert("npc/7".into(), Arc::new(vec![1, 9]));
        b.remove("enemy/17");
        b.insert("chunk/39".into(), Arc::new(vec![0]));
        assert_eq!(changed(&a, &b), vec!["chunk/39", "enemy/17", "npc/7"]);
    }
}
