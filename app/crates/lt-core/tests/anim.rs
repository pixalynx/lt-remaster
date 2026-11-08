//! Poses and battle sequences (`edit::anim`) on every character model the
//! disc holds. Pose blocks read and write back byte for byte; every pose
//! exports as an OBJ frame and imports back to the same file; every battle
//! sequence of toys, enemies and knights rewrites in place unchanged and
//! survives a move to free space with the other blocks untouched.
//! Opt in with `LT_TEST_DISC=… cargo test --release -p lt-core --test anim`.

use lt_core::edit::anim::{self, Pose};
use lt_core::edit::meshin::parse_obj;
use lt_core::edit::model::{self, Container};
use lt_core::mesh::model_parts;
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    table: FileTable,
}

impl Disc {
    fn open() -> Option<Self> {
        let image = DiscImage::open(std::env::var_os("LT_TEST_DISC")?).unwrap();
        let exe = PsxExe::from_disc(&image).unwrap();
        let table = FileTable::parse(&exe, image.sectors()).unwrap();
        Some(Self { image, table })
    }
    fn read(&self, id: &str) -> Option<Vec<u8>> {
        let e = self.table.by_id(id)?;
        self.image.read_sectors(e.extent.lba, e.extent.sectors).ok()
    }
}

fn ids() -> Vec<String> {
    let mut v: Vec<String> = (0..73).map(|i| format!("toy/{i}")).collect();
    v.extend((0..112).map(|i| format!("enemy/{i}")));
    v.extend((0..19).map(|i| format!("pb/{i}")));
    v.extend(["knight/0", "sys/6#knight", "sys/6#baby"].map(String::from));
    v
}

#[test]
fn every_pose_block_round_trips_through_obj_frames() {
    let Some(d) = Disc::open() else { return };
    let (mut files, mut poses) = (0, 0);
    for id in ids() {
        let (file, kind) = model::source(&id).unwrap();
        let Some(b) = d.read(&file) else { continue };
        let Some(parts) = model_parts(kind, &b) else { continue };
        let Some(block) = parts.poses else { continue };
        let read = anim::read_poses(block).unwrap_or_else(|| panic!("{id}: pose block"));
        let written = anim::write_poses(&read);
        assert_eq!(&written[..], &block[..written.len()], "{id}: pose block bytes");
        if read.is_empty() {
            continue;
        }
        // Export every pose as an OBJ frame, import the frames back.
        let objs = anim::pose_objs(kind, &b, "m").unwrap();
        let frames: Vec<_> = objs.iter().map(|o| parse_obj(o, None).unwrap()).collect();
        let (tmd, _) = model::model_of(kind, &b).unwrap();
        let (back, rep) = anim::poses_from_frames(&tmd.objects[0], &frames, 1.0, &read).unwrap();
        assert_eq!(back, read, "{id}: poses");
        assert_eq!(rep.clamped, 0);
        let (out, r) = anim::replace_poses(kind, &b, &back).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(out, b, "{id}: an unchanged import rewrites the file");
        assert_eq!(r.poses, read.len() as u32);
        // Without the reference, the records are the smallest ranges with the same effect.
        let (tight, _) = anim::poses_from_frames(&tmd.objects[0], &frames, 1.0, &[]).unwrap();
        let n = tmd.objects[0].verts.len();
        for (a, z) in tight.iter().zip(&read) {
            assert_eq!(a.dense(n), z.dense(n), "{id}");
            assert!(!a.deltas.is_empty());
        }
        files += 1;
        poses += read.len();
    }
    eprintln!("{files} files, {poses} poses");
    assert!(files > 190, "{files}");
}

#[test]
fn every_battle_sequence_rewrites_in_place_and_moves_to_free_space() {
    let Some(d) = Disc::open() else { return };
    let (mut files, mut seqs, mut moved) = (0, 0, 0);
    for id in ids() {
        if anim::read_only(&id).is_some() {
            continue;
        }
        let (file, kind) = model::source(&id).unwrap();
        if !matches!(kind, Container::Toy | Container::Enemy | Container::Knight) {
            continue;
        }
        let Some(b) = d.read(&file) else { continue };
        let t = anim::sequences(kind, &b).unwrap_or_else(|e| panic!("{id}: {e}"));
        let npose = anim::poses_of(kind, &b).unwrap().len();
        assert!(t.used <= t.cap, "{id}: retail uses {:#x} of {:#x}", t.used, t.cap);
        assert!(t.highest_pose() as usize <= npose, "{id}: a sequence stands on pose {} of {npose}", t.highest_pose());
        for s in t.slots.iter().filter(|s| !s.pairs.is_empty()) {
            anim::check_pairs(&s.pairs, npose).unwrap_or_else(|e| panic!("{id} seq {}: {e} {:?}", s.index, s.pairs));
            // The same pairs: nothing changes.
            let (same, rep) = anim::set_sequence(kind, &b, s.index, &s.pairs).unwrap_or_else(|e| panic!("{id} seq {}: {e}", s.index));
            assert_eq!(same, b, "{id} seq {}", s.index);
            assert!(rep.in_place || s.shared);
            seqs += 1;
        }
        // One longer sequence moves to the free space; every other block stays.
        if let Some(s) = t.slots.iter().find(|s| !s.pairs.is_empty() && s.pairs.last().unwrap().0 != 0xFF) {
            let mut longer = s.pairs.clone();
            let last = longer.pop().unwrap();
            longer.push((last.0, 0x40));
            longer.push((0, 0x40));
            longer.push((last.0, 0));
            match anim::set_sequence(kind, &b, s.index, &longer) {
                Ok((out, rep)) => {
                    assert!(!rep.in_place);
                    let t2 = anim::sequences(kind, &out).unwrap();
                    assert_eq!(t2.slots[s.index].pairs, longer, "{id}");
                    for (x, y) in t.slots.iter().zip(&t2.slots) {
                        if x.index != s.index {
                            assert_eq!(x.pairs, y.pairs, "{id} seq {}", x.index);
                        }
                    }
                    assert_eq!(t.scripts, t2.scripts, "{id}: script offsets");
                    // Script bytes unchanged.
                    let sec = |f: &[u8]| {
                        let (n, i) = match kind { Container::Toy => (8, 1), Container::Enemy => (7, 6), _ => (4, 3) };
                        let s = lt_core::container::sections(f, n).unwrap()[i].clone();
                        f[s.offset as usize..(s.offset + s.len) as usize].to_vec()
                    };
                    let (a, z) = (sec(&b), sec(&out));
                    let base = match kind { Container::Toy => 0xA8, Container::Enemy => 0x140, _ => 0x250 };
                    for &o in &t.scripts {
                        let at = base + o as usize;
                        let end = (at + 20).min(a.len()).min(z.len());
                        assert_eq!(a[at..end], z[at..end], "{id}: script at {o:#x}");
                    }
                    moved += 1;
                }
                Err(e) => assert_eq!(e.code, "SEQ_BUDGET", "{id}: {e}"),
            }
        }
        files += 1;
    }
    eprintln!("{files} files, {seqs} sequences, {moved} moved");
    assert!(files > 180 && moved > 100, "{files} {moved}");
}

#[test]
fn neutral_poses_are_never_empty() {
    let block = anim::write_poses(&vec![Pose::neutral(); 3]);
    assert_eq!(block.len(), 4 + 3 * 18);
    assert!(lt_core::pose::parse(&block).unwrap().poses.iter().all(|p| p.count == 1));
}
