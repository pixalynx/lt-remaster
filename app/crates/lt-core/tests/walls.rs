//! Interior walls (`world::walk`, research 07) against the retail image. Opt in
//! with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test walls`.

use std::collections::BTreeMap;

use lt_core::fs::Kind;
use lt_core::npc::NpcPack;
use lt_core::placement::{self, Body};
use lt_core::trigger::TriggerRecord;
use lt_core::world::walk::{self, Blocked, Cell, Walk};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, exe, table })
}

impl Disc {
    fn w(&self) -> WorldDisc<'_> {
        WorldDisc { image: &self.image, table: &self.table, exe: &self.exe }
    }

    /// Every distinct pack (npc/17 aliases npc/0).
    fn packs(&self) -> Vec<(String, Vec<u8>)> {
        self.table
            .of_kind(Kind::Npc)
            .filter(|e| e.aliases.is_empty() || e.global < e.aliases[0])
            .map(|e| (e.id.clone(), self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()))
            .collect()
    }
}

/// @LT-58/1: the Knight's house (chunk 326) keeps its walls in layer A, as cells
/// raised 192 above the floor. The spots LT-49's slice found: a door box at
/// (544, 144) he stops 48 short of (at z = 96), and the partition he walked into
/// at x = 320.
#[test]
fn the_knights_house_walls_are_raised_walk_cells() {
    let Some(d) = disc() else { return };
    let house = d.w().chunk(326).unwrap();
    let w = Walk::of_chunk(326, &house, &[]);
    let floor = w.stand(476, 76).expect("the slice's arrival in the house");
    assert_eq!(floor, -33);
    // The north wall over the door (cell 81, 68) and the partition (cell 72, 64).
    assert_eq!(w.cell(81, 68), Cell::Wall);
    assert_eq!(w.height(81, 68), -225);
    assert_eq!(w.cell(81, 67), Cell::Floor);
    assert_eq!(w.cell(72, 64), Cell::Wall);
    assert!(matches!(w.stand(544, 144), Err(Blocked::Wall { col: 81, row: 68, h: -225, rise: Some(192) })));
    // He stops at z = 96 below the door, and at x = 320 right of the partition.
    assert!(w.can_stand(544, 96) && !w.can_stand(544, 97));
    assert!(w.can_stand(320, 16) && !w.can_stand(319, 16));
    assert_eq!(w.reach_box(544, 144), Err(Blocked::OutOfReach { nearest: Some((544, 96)), gap: Some(48), fit: Some((544, 128)) }));
    // A box 16 nearer the room fires.
    let inside = w.arrival_region(476, 76).unwrap();
    assert_eq!(w.reach_box(544, 128), Ok(vec![inside]));
    // The first innkeeper spot is floor, behind the partition, and reachable round it.
    assert_eq!(w.stand(96, 16), Ok(-33));
    assert!(w.reach_box(96, 16).unwrap().contains(&inside));
    // Retail starts a new game in this house, on the floor.
    let (x, h, z) = lt_core::edit::blank::BOOT_SPOT;
    assert_eq!(w.stand(x, z), Ok(h));
}

/// @LT-58/2: what retail does, so the checks refuse what retail never needs. With
/// the arrivals into each chunk as entries, every retail arrival lands on floor
/// but the switch castle's (npc/16), which land on void its switches change;
/// 97% of door boxes are in reach on foot (the rest fire from scripted walks,
/// the water bug or that castle); and one actor spot is a wall top.
#[test]
fn retail_arrivals_and_door_boxes_are_in_reach() {
    let Some(d) = disc() else { return };
    let wd = d.w();
    let packs = d.packs();
    let groups: Vec<_> = packs.iter().flat_map(|(_, b)| placement::trigger_groups(&NpcPack::parse(b).unwrap(), b)).collect();
    let mut walks: BTreeMap<u32, Walk> = BTreeMap::new();
    for id in 0..524 {
        walks.insert(id, Walk::of_chunk(id, &wd.chunk(id).unwrap(), &walk::arrivals_into(id, &groups)));
    }
    let walk = |id: u32| &walks[&id];
    let (mut arrivals, mut boxes, mut actors) = (0, 0, 0);
    let mut bad_arrivals = Vec::new();
    let mut bad_boxes: BTreeMap<i16, usize> = BTreeMap::new();
    let mut walled_actors = Vec::new();
    for (id, b) in &packs {
        let p = NpcPack::parse(b).unwrap();
        for g in placement::trigger_groups(&p, b) {
            for r in g.rungs.iter().flat_map(|r| &r.records) {
                let TriggerRecord::Door { x, z, to, tx, tz, .. } = *r else { continue };
                if (to as u32) < 524 {
                    arrivals += 1;
                    if let Err(e) = walk(to as u32).stand(tx, tz) {
                        println!("{id} {}→{to} ({tx}, {tz}): {e:?}", g.chunk);
                        bad_arrivals.push((id.clone(), to, e));
                    }
                }
                if (0..524).contains(&g.chunk) {
                    boxes += 1;
                    if let Err(e) = walk(g.chunk as u32).reach_box(x, z) {
                        println!("{id} chunk {} ({x}, {z}): {e:?}", g.chunk);
                        *bad_boxes.entry(g.chunk).or_default() += 1;
                    }
                }
            }
        }
        for a in placement::roster(&p, b) {
            if a.party || a.object || a.home == 0 || a.home as u32 >= 524 {
                continue;
            }
            for br in &a.branches {
                let Body::Place { x, z, .. } = br.body else { continue };
                if !(-0x800..0x800).contains(&x) || !(-0x800..0x800).contains(&z) {
                    continue;
                }
                actors += 1;
                if let Err(Blocked::Wall { .. }) = walk(a.home as u32).stand(x, z) {
                    walled_actors.push((id.clone(), a.slot, a.home));
                }
            }
        }
    }
    let interior_walls: usize = (324..524).map(|id| walk(id).walls()).sum();
    println!("{arrivals} arrivals, {} off the floor; {boxes} door boxes, {} out of reach {bad_boxes:?}; {actors} actor spots, {walled_actors:?} on walls; {interior_walls} interior wall cells", bad_arrivals.len(), bad_boxes.values().sum::<usize>());
    assert_eq!((arrivals, boxes), (1930, 1930));
    assert_eq!(bad_arrivals.len(), 24);
    assert!(bad_arrivals.iter().all(|(id, to, e)| id == "npc/16" && (479..=491).contains(to) && matches!(e, Blocked::Void { h: 0, .. })), "{bad_arrivals:?}");
    let castle: usize = bad_boxes.iter().filter(|(c, _)| (479..=491).contains(*c)).map(|(_, n)| n).sum();
    assert_eq!(castle, 32);
    assert_eq!(bad_boxes.keys().filter(|c| !(479..=491).contains(*c)).copied().collect::<Vec<_>>(), [29, 81, 174, 364, 428, 454, 460]);
    assert_eq!(bad_boxes.values().sum::<usize>(), 55);
    assert_eq!(walled_actors, [("npc/7".to_string(), 12, 326)]);
    assert!(actors > 700);
}
