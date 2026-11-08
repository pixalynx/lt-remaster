//! Unbaking retail chunks (`edit::unbake`, research 12) against the user's own
//! retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test unbake`
//! (a debug build takes minutes over all 524 chunks).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use lt_core::edit::bake;
use lt_core::edit::chunk::Chunk;
use lt_core::edit::unbake::{self, Move, PieceMove, Split, Taken};
use lt_core::edit::{pack, placement, remove};
use lt_core::npc::NpcPack;
use lt_core::world::{self, CHUNK_COUNT};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, exe, table })
}

impl Disc {
    fn world(&self) -> WorldDisc<'_> {
        WorldDisc { image: &self.image, table: &self.table, exe: &self.exe }
    }
}

/// Runs `f` on every retail chunk, on every core.
fn each_chunk(d: &Disc, f: impl Fn(u32, &[u8]) + Sync) {
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let id = next.fetch_add(1, Ordering::Relaxed) as u32;
                if id >= CHUNK_COUNT {
                    break;
                }
                let raw = d.world().chunk(id).unwrap();
                f(id, &raw);
            });
        }
    });
}

/// The split is the same every time, and so are the prints a project keys its moves by:
/// unique within the chunk, each naming its own piece.
#[test]
fn every_chunk_splits_the_same_way_twice() {
    let Some(d) = disc() else { return };
    let pieces = AtomicUsize::new(0);
    each_chunk(&d, |id, raw| {
        let c = Chunk::parse(raw).unwrap();
        let (a, b) = (unbake::split(&c), unbake::split(&c));
        assert_eq!(a.pieces.len(), b.pieces.len(), "chunk/{id}");
        for (p, q) in a.pieces.iter().zip(&b.pieces) {
            assert_eq!((&p.prims, p.min, p.max, p.fixed), (&q.prims, q.min, q.max, q.fixed), "chunk/{id}");
        }
        assert_eq!(a.ground, b.ground, "chunk/{id}");
        let prints = unbake::fingerprints(&c, &a);
        assert_eq!(prints, unbake::fingerprints(&c, &b), "chunk/{id}");
        let mut hashes: Vec<&str> = prints.iter().map(|p| p.hash.as_str()).collect();
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), prints.len(), "chunk/{id}: two pieces share a print");
        let identity: Vec<Option<usize>> = (0..prints.len()).map(Some).collect();
        assert_eq!(unbake::match_prints(&prints, &prints), identity, "chunk/{id}");
        pieces.fetch_add(a.pieces.len(), Ordering::Relaxed);
    });
    // Research 12 §2: 8,955 pieces on the disc (9,522 before roof parts rode on their roofs).
    assert_eq!(pieces.into_inner(), 8955);
}

/// Every movable piece can be deleted, and moved two cells, and the chunk
/// still serialises: only chunks too close to the mesh budget refuse (the
/// fill a piece leaves adds polygons, so a few refuse even a delete).
#[test]
fn every_piece_deletes_and_moves_two_cells() {
    let Some(d) = disc() else { return };
    let (movable, deleted, moved) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let refused: Mutex<BTreeMap<(&str, u32), usize>> = Mutex::default();
    each_chunk(&d, |id, raw| {
        let c = Chunk::parse(raw).unwrap();
        let split = unbake::split(&c);
        for (n, p) in split.pieces.iter().enumerate().filter(|(_, p)| !p.fixed) {
            movable.fetch_add(1, Ordering::Relaxed);
            let delete = unbake::rebake(&c, &split, &[Move { piece: n, dc: 0, dr: 0, turns: 0, delete: true, ..Default::default() }]).and_then(|(out, _)| {
                // On the overworld every cell it covered has ground again (research 12 §5.2; interiors don't fill yet).
                if world::grid_cell(id).is_some() {
                    let floor = |s: &[Vec<bake::Surface>], k: usize| s[k].iter().any(|f| f.flat >= bake::FLOOR_FLAT);
                    let (before, after) = (bake::surfaces(&c), bake::surfaces(&out));
                    let lost = unbake::cells_under(&c, &p.prims).into_iter().filter(|&k| floor(&before, k) && !floor(&after, k)).count();
                    assert_eq!(lost, 0, "chunk/{id} piece {n}: a delete left cells with no floor");
                }
                out.serialize()
            });
            // Two cells whichever way keeps it inside the chunk.
            let tried = [(2, 0), (-2, 0), (0, 2), (0, -2)].into_iter().map(|(dc, dr)| unbake::rebake(&c, &split, &[Move { piece: n, dc, dr, turns: 0, delete: false, ..Default::default() }]));
            let rebaked = tried.into_iter().find(|r| !matches!(r, Err(e) if e.code == "UNBAKE_OUTSIDE"));
            let mv = rebaked.unwrap_or_else(|| panic!("chunk/{id} piece {n} ({:?}..{:?}) has no room to move", p.min, p.max)).and_then(|(out, _)| out.serialize());
            for (what, result, count) in [("delete", delete, &deleted), ("move", mv, &moved)] {
                match result {
                    Ok(_) => {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) if e.code == "CHUNK_MESH_BUDGET" => *refused.lock().unwrap().entry((what, id)).or_default() += 1,
                    Err(e) => panic!("chunk/{id} piece {n} {what}: {e}"),
                }
            }
        }
    });
    let refused = refused.into_inner().unwrap();
    let over = |what: &str| refused.iter().filter(|((w, _), _)| *w == what).map(|(_, n)| n).sum::<usize>();
    let (movable, deleted, moved) = (movable.into_inner(), deleted.into_inner(), moved.into_inner());
    eprintln!("movable {movable}: deleted {deleted}, moved {moved}; over the mesh budget: {} deletes, {} moves {refused:?}", over("delete"), over("move"));
    // Research 12 §5.2: 7,631 movable pieces; 47 moves don't fit their chunk's mesh budget.
    assert_eq!(movable, 7631);
    assert_eq!(deleted + over("delete"), movable);
    assert_eq!(moved + over("move"), movable);
    assert!(over("delete") <= DELETES_OVER && over("move") <= MOVES_OVER, "more rebakes over the mesh budget than measured: {refused:?}");
}

/// Rebakes over the mesh budget on the retail USA disc (more is a regression: the fill grew).
const DELETES_OVER: usize = 0;
const MOVES_OVER: usize = 47;

/// Research 12 §5.1 offline: Orgo's blue house, stored by its print, moves 19
/// cells west and takes its door actor (npc/6 record 14) along; undone, the
/// chunk and the pack are the bytes they were.
#[test]
fn orgos_blue_house_moves_with_its_door_and_comes_back_exactly() {
    let Some(d) = disc() else { return };
    let w = d.world();
    let raw = w.chunk(78).unwrap();
    let c = Chunk::parse(&raw).unwrap();
    let split = unbake::split(&c);
    let prints = unbake::fingerprints(&c, &split);
    let (x, z) = (-128, -1088);
    let house = (0..split.pieces.len())
        .filter(|&n| {
            let p = &split.pieces[n];
            x >= p.min[0] as i32 && x <= p.max[0] as i32 && z >= p.min[2] as i32 && z <= p.max[2] as i32
        })
        .min_by_key(|&n| {
            let p = &split.pieces[n];
            (p.max[0] as i32 - p.min[0] as i32) * (p.max[2] as i32 - p.min[2] as i32)
        })
        .unwrap();
    // The print is what projects store: it must not drift between releases.
    assert_eq!(prints[house].hash, BLUE_HOUSE, "the blue house's print changed; saved moves would no longer find it");
    let stored = PieceMove { piece: prints[house].clone(), dc: -19, dr: 0, turns: 0, delete: false, copy: false, hold: false };
    let (moves, named) = unbake::resolve(&prints, &[stored]);
    assert_eq!(named, vec![Some(house)]);
    let (out, rep) = unbake::rebake(&c, &split, &moves).unwrap();
    assert!(out.serialize().is_ok());
    assert_eq!(rep.moved_polygons, split.pieces[house].prims.len());

    let packs = world::pack_table_in(&d.exe, &w.file("ovr/2").unwrap());
    let pack = world::pack_for(&packs, 78, &|_| false).unwrap();
    assert_eq!(pack, 6);
    let b = w.file("npc/6").unwrap();
    let (writes, carried) = unbake::carry_pack(pack, &b, 78, &split, &moves, &rep.lifts, &[]);
    let door = unbake::spots(&b, 78).into_iter().find(|s| s.slot == 14).unwrap();
    assert_eq!((door.kind, door.place.x, door.place.z), (2, -96, -1024));
    let to = carried.iter().find(|k| k.at == door.at).expect("the door actor goes with the house").to;
    assert_eq!((to.x, to.z), (-96 - 19 * 32, -1024));
    assert!(carried.iter().all(|k| writes.iter().any(|w| w.at == k.at)));
    let moved = placement::apply(&b, &b, &writes).unwrap();
    assert_ne!(moved, b);

    // Undo: no moves. The chunk is its base again, and the carried words go back.
    let (none, _) = unbake::resolve(&prints, &[]);
    assert!(none.is_empty());
    assert_eq!(c.serialize().unwrap(), raw);
    let (back, kept) = unbake::carry_pack(pack, &moved, 78, &split, &[], &[], &carried);
    assert!(kept.is_empty());
    assert_eq!(placement::apply(&b, &moved, &back).unwrap(), b);

    // Moved elsewhere by hand since: the pieces let go of it and leave it there.
    let mut by_hand = carried.clone();
    by_hand.iter_mut().filter(|k| k.at == door.at).for_each(|k| k.to.x += 1);
    let (back, _) = unbake::carry_pack(pack, &moved, 78, &split, &[], &[], &by_hand);
    assert!(back.iter().all(|w| w.at != door.at));
}

/// The smallest piece whose bounds hold chunk-local (x, z).
fn piece_at(split: &Split, x: i32, z: i32) -> usize {
    (0..split.pieces.len())
        .filter(|&n| {
            let p = &split.pieces[n];
            x >= p.min[0] as i32 && x <= p.max[0] as i32 && z >= p.min[2] as i32 && z <= p.max[2] as i32
        })
        .min_by_key(|&n| {
            let p = &split.pieces[n];
            (p.max[0] as i32 - p.min[0] as i32) * (p.max[2] as i32 - p.min[2] as i32)
        })
        .unwrap_or_else(|| panic!("no piece holds ({x}, {z})"))
}

fn delete(piece: usize) -> Move {
    Move { piece, dc: 0, dr: 0, turns: 0, delete: true, ..Default::default() }
}

/// Every pack serving `chunk` in some story state (the flag cases of `world::pack_for`).
fn serving(d: &Disc, chunk: u32) -> Vec<u32> {
    let table = world::pack_table_in(&d.exe, &d.world().file("ovr/2").unwrap());
    let flags = [0xE6u16, 0xBA, 0xF7];
    let mut out: Vec<u32> = (0..8u8).filter_map(|bits| world::pack_for(&table, chunk, &|f| flags.iter().enumerate().any(|(k, &v)| v == f && bits >> k & 1 == 1))).collect();
    out.sort();
    out.dedup();
    out
}

/// Record `slot`'s words (script, b, placement, d) in `b`.
fn record(b: &[u8], slot: u32) -> [u32; 4] {
    let a = &NpcPack::parse(b).unwrap().actors[slot as usize];
    [a.script, a.b, a.placement, a.d]
}

/// Research 12 §3.3: deleting Orgo's house 6 (chunk 78) takes its door
/// (npc/6 record 18, drawn as the door; ✕ led inside) out of the game with the
/// blank slate's removal block; putting the house back gives the pack's bytes
/// back. Put back in the game by hand meanwhile, the door stays.
#[test]
fn a_deleted_house_takes_its_door_out_and_puts_it_back_exactly() {
    let Some(d) = disc() else { return };
    let w = d.world();
    let c = Chunk::parse(&w.chunk(78).unwrap()).unwrap();
    let split = unbake::split(&c);
    let house = piece_at(&split, -192, 64);
    assert_eq!((split.pieces[house].min[0], split.pieces[house].max[0], split.pieces[house].min[2], split.pieces[house].max[2]), (-512, 128, -256, 384));
    let moves = [delete(house)];
    let (_, rep) = unbake::rebake(&c, &split, &moves).unwrap();
    let retail = w.file("npc/6").unwrap();
    let out = unbake::bake_pack(6, &retail, &retail, 78, &split, &moves, &rep.lifts, &[], &[]).unwrap();
    assert!(out.carried.is_empty());
    assert_eq!(out.taken, vec![Taken { pack: 6, slot: 18, let_go: false }], "the door, and nothing else: Algo (record 2) is a character");
    let gone = out.bytes.expect("the pack changes");
    assert_eq!(remove::recover(&retail, &gone).unwrap(), [18].into_iter().collect());
    assert!(pack::reads_as_edits(&retail, &gone));
    // The door's NPC structure: absent (its placement is the block), its ladder words untouched.
    let np = NpcPack::parse(&gone).unwrap();
    assert!(remove::placement_removed(np.d_block(&gone), np.actors[18].placement, NpcPack::parse(&retail).unwrap().d_span));
    for slot in (0..np.actors.len() as u32).filter(|&s| s != 18) {
        assert_eq!(record(&gone, slot), record(&retail, slot), "record {slot}");
    }

    // A second save with the delete standing changes nothing.
    let again = unbake::bake_pack(6, &retail, &gone, 78, &split, &moves, &rep.lifts, &[], &out.taken).unwrap();
    assert!(again.bytes.is_none());
    assert_eq!(again.taken, out.taken);

    // Put back: the retail bytes, nothing kept.
    let back = unbake::bake_pack(6, &retail, &gone, 78, &split, &[], &[], &[], &out.taken).unwrap();
    assert_eq!(back.bytes.as_deref(), Some(&retail[..]));
    assert!(back.taken.is_empty());

    // Put back in the game by hand (Select): the delete lets go of it and doesn't take it again.
    let by_hand = remove::apply(&retail, &gone, &Default::default()).unwrap();
    assert_eq!(by_hand, retail);
    let held = unbake::bake_pack(6, &retail, &by_hand, 78, &split, &moves, &rep.lifts, &[], &out.taken).unwrap();
    assert!(held.bytes.is_none(), "the door stays in the game");
    assert_eq!(held.taken, vec![Taken { pack: 6, slot: 18, let_go: true }]);
    let home = unbake::bake_pack(6, &retail, &by_hand, 78, &split, &[], &[], &[], &held.taken).unwrap();
    assert!(home.bytes.is_none() && home.taken.is_empty());
}

/// A house with smoke (chunk 78 piece at x −896..−256: door 13, smoke 53) and
/// one with examine spots (x 0..512: records 29–32): deleted, the door is
/// absent, the smoke an examine spot running END (the field draws type 5
/// only), the examine spots run END; put back, the bytes are retail. The blue
/// house moved, then deleted, then moved again: the door goes out from where
/// it stood and comes back carried.
#[test]
fn smoke_and_examine_spots_go_too_and_a_move_survives_a_delete() {
    let Some(d) = disc() else { return };
    let w = d.world();
    let c = Chunk::parse(&w.chunk(78).unwrap()).unwrap();
    let split = unbake::split(&c);
    let retail = w.file("npc/6").unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    let (smoky, windows) = (piece_at(&split, -576, -384), piece_at(&split, 192, -576));
    let moves = [delete(smoky), delete(windows)];
    let (_, rep) = unbake::rebake(&c, &split, &moves).unwrap();
    let out = unbake::bake_pack(6, &retail, &retail, 78, &split, &moves, &rep.lifts, &[], &[]).unwrap();
    let slots: Vec<u32> = out.taken.iter().map(|t| t.slot).collect();
    assert_eq!(slots, vec![13, 29, 30, 31, 32, 53]);
    let gone = out.bytes.unwrap();
    let g = NpcPack::parse(&gone).unwrap();
    let gd = g.d_block(&gone);
    assert!(remove::placement_removed(gd, g.actors[13].placement, r.d_span));
    for slot in [29, 30, 31, 32, 53] {
        assert!(remove::is_end(gd, g.actors[slot].script, r.d_span), "record {slot} runs END");
        assert_eq!(g.actors[slot].placement, r.actors[slot].placement, "record {slot} stays where it was");
    }
    assert_eq!((r.actors[53].d >> 8) & 0xFF, 5);
    assert_eq!((g.actors[53].d >> 8) & 0xFF, 4, "the smoke is an examine spot now: not drawn");
    assert_eq!((g.actors[29].d >> 8) & 0xFF, 4);
    assert_eq!(remove::recover_hidden(&retail, &gone).unwrap(), [53].into_iter().collect());
    assert!(pack::reads_as_edits(&retail, &gone));
    let back = unbake::bake_pack(6, &retail, &gone, 78, &split, &[], &[], &[], &out.taken).unwrap();
    assert_eq!(back.bytes.as_deref(), Some(&retail[..]));

    // The blue house: moved 19 cells west (door 14 carried), deleted, moved again, put back.
    let blue = piece_at(&split, -128, -1088);
    let step = |delete| [Move { piece: blue, dc: -19, dr: 0, turns: 0, delete, ..Default::default() }];
    let lifts = |m: &[Move]| unbake::rebake(&c, &split, m).unwrap().1.lifts;
    let (mv, del) = (step(false), step(true));
    let moved = unbake::bake_pack(6, &retail, &retail, 78, &split, &mv, &lifts(&mv), &[], &[]).unwrap();
    assert_eq!(moved.carried.len(), 2, "door 14's two placing branches");
    assert!(moved.taken.is_empty());
    let moved_bytes = moved.bytes.unwrap();
    let deleted = unbake::bake_pack(6, &retail, &moved_bytes, 78, &split, &del, &lifts(&del), &moved.carried, &moved.taken).unwrap();
    assert!(deleted.carried.is_empty());
    assert_eq!(deleted.taken.iter().map(|t| t.slot).collect::<Vec<_>>(), vec![14]);
    let deleted_bytes = deleted.bytes.unwrap();
    // Out with its ladder back at the words it had before the move.
    assert_eq!(remove::apply(&retail, &deleted_bytes, &Default::default()).unwrap(), retail);
    let again = unbake::bake_pack(6, &retail, &deleted_bytes, 78, &split, &mv, &lifts(&mv), &deleted.carried, &deleted.taken).unwrap();
    assert_eq!(again.bytes.as_deref(), Some(&moved_bytes[..]), "moved again, the door is carried as before");
    assert_eq!(again.carried, moved.carried);
    assert!(again.taken.is_empty());
    let home = unbake::bake_pack(6, &retail, &deleted_bytes, 78, &split, &[], &[], &deleted.carried, &deleted.taken).unwrap();
    assert_eq!(home.bytes.as_deref(), Some(&retail[..]));
}

/// Every movable piece of every overworld chunk deleted at once: each pack
/// serving the chunk takes their records out and still lays out, and putting
/// them all back gives each pack's retail bytes.
#[test]
fn every_overworld_delete_takes_records_out_and_back() {
    let Some(d) = disc() else { return };
    let (taken, packs) = (AtomicUsize::new(0), AtomicUsize::new(0));
    each_chunk(&d, |id, raw| {
        if world::grid_cell(id).is_none() {
            return;
        }
        let c = Chunk::parse(raw).unwrap();
        let split = unbake::split(&c);
        let moves: Vec<Move> = (0..split.pieces.len()).filter(|&n| !split.pieces[n].fixed).map(delete).collect();
        for pack_id in serving(&d, id) {
            let retail = d.world().file(&format!("npc/{pack_id}")).unwrap();
            let out = unbake::bake_pack(pack_id, &retail, &retail, id, &split, &moves, &[], &[], &[]).unwrap_or_else(|e| panic!("chunk/{id} npc/{pack_id}: {e}"));
            packs.fetch_add(1, Ordering::Relaxed);
            let Some(gone) = out.bytes else { continue };
            taken.fetch_add(out.taken.len(), Ordering::Relaxed);
            let back = unbake::bake_pack(pack_id, &retail, &gone, id, &split, &[], &[], &[], &out.taken).unwrap();
            assert_eq!(back.bytes.as_deref(), Some(&retail[..]), "chunk/{id} npc/{pack_id}: put back isn't retail");
        }
    });
    eprintln!("{} records taken out over {} chunk packs", taken.into_inner(), packs.into_inner());
}

/// Fingerprint of Orgo's blue house (chunk 78) on the retail USA disc.
const BLUE_HOUSE: &str = "3051f1e95cb65be0";
