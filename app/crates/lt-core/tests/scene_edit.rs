//! New cutscenes (`edit::scene`, screen 20) on the retail village pack: the
//! director, its wake-up record and its program lay out, read back, play in
//! the scene player, move, and go away again. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test scene_edit`.

use lt_core::edit::scene::{self, NewScene};
use lt_core::edit::script::{Arg, Node};
use lt_core::npc::NpcPack;
use lt_core::scene::{math::Tables, Ending, SceneInput, SceneKind, Start};
use lt_core::trigger::TriggerRecord;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

const CUE: u16 = 0x1E3;

fn op(op: u8, args: Vec<Arg>) -> Node {
    Node::Op { op, args, at: None }
}
fn text(s: &str) -> Arg {
    Arg::Text { markup: s.into() }
}
fn num(v: i64) -> Arg {
    Arg::Num { value: v }
}

/// SPEAKER, SAY, NO_SPEAKER, CLEAR cue, END.
fn program(cue: u16) -> Vec<Node> {
    let mut t = vec![op(0x1F, vec![text("Elder")]), op(0x01, vec![text("Welcome to{nl}the new scene!")]), op(0x20, vec![])];
    if cue != 0 {
        t.push(op(0x05, vec![num(cue as i64)]));
    }
    t.push(op(0x00, vec![]));
    t
}

#[test]
fn a_new_scene_lays_out_plays_moves_and_goes_away() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let chunk = w.chunk(78).unwrap();
    let heights = lt_core::world::heightmap(&chunk);
    let h_at = |x: i16, z: i16| heights[(((z as i32 + 0x800) >> 5) * 128 + ((x as i32 + 0x800) >> 5)) as usize];
    // Near the dream's spot, on walkable ground.
    let (x, z) = (-1600i16, -856i16);
    let h = h_at(x, z);
    assert!(h < 0 && h != -1, "({x}, {z}) isn't walkable");

    let (bytes, slot) = scene::create(&retail, &retail, &NewScene { chunk: 78, x, h, z, cue: CUE }, &program(CUE), &[]).unwrap();
    let pack = NpcPack::parse(&bytes).unwrap();
    let r = NpcPack::parse(&retail).unwrap();
    assert_eq!(slot as usize, r.actors.len());
    let dir = &pack.actors[slot as usize];
    assert_eq!((dir.b >> 16, dir.d & 0xFFFF, dir.b & 0xFFFF), (0xFFFF, 0, 78), "a hidden director homed on 78");

    // The trigger names the director's character slot, in a cue rung at the top of chunk 78's table-2 ladder.
    let list = scene::list(&retail, &bytes).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].slot, list[0].x, list[0].z, list[0].cue), (slot, x, z, CUE));
    let g = pack.triggers(&bytes).into_iter().find(|g| g.pass == 1 && g.chunk == 78).unwrap();
    assert_eq!(g.rungs[0].flag, CUE);
    assert!(matches!(g.rungs[0].records[0], TriggerRecord::Talk { actor, .. } if actor == list[0].actor));
    assert_eq!(scene::record_of(&pack.actors, list[0].actor), Some(slot as usize));

    // The catalogue finds it as a walk-in scene on its cue, and it plays to END with its line.
    let entry = lt_core::scene::catalog(&pack, &bytes).into_iter().find(|e| e.slot == slot).expect("in the catalogue");
    assert_eq!((entry.kind, entry.chunk, entry.flag, entry.x, entry.z), (SceneKind::Trigger, 78, CUE, x, z));
    let tables = Tables::from_exe(&exe).unwrap();
    let item = |n: u16| format!("item{n}").into_bytes();
    let input = SceneInput {
        tables: &tables,
        pack: &pack,
        pack_bytes: &bytes,
        chunk: 78,
        chunk_bytes: &chunk,
        slot,
        start: Start::Trigger { x, z },
        flags: vec![0xFC, 0xFE, 0xFF, CUE],
        answers: vec![],
        baby: true,
        yaw: None,
        hold_base: 20,
        hold_per_glyph: 1.0,
        names: ["Knight".into(), "Baby".into(), "Nehani".into(), "K-Knight".into(), "Bonik".into()],
        item_name: &item,
        toy_name: &item,
        max_frames: 3000,
    };
    let pb = lt_core::scene::play(&input);
    assert!(matches!(pb.meta.ending, Ending::End), "{:?} {:?}", pb.meta.ending, pb.meta.notes);
    assert_eq!(pb.meta.speakers, vec!["Elder".to_string()]);
    assert_eq!(pb.meta.pages.len(), 1);
    assert!(!pb.meta.flags_after.contains(&CUE), "the scene clears its cue");
    // The director is never drawn.
    let stride = lt_core::scene::FRAME_HEAD + lt_core::scene::ACTOR_BYTES * pb.meta.actors.len();
    let me = pb.meta.actors.iter().position(|a| a.record == slot).unwrap();
    assert!((0..pb.meta.frames as usize).all(|f| pb.frames[f * stride + lt_core::scene::FRAME_HEAD + me * lt_core::scene::ACTOR_BYTES + 11] & 2 == 0));

    // Move it and drop the cue: the record now joins every rung.
    let (x2, z2) = (x + 96, z);
    let moved = scene::retrigger(&retail, &bytes, slot, x2, h_at(x2, z2), z2, 0).unwrap();
    let list = scene::list(&retail, &moved).unwrap();
    assert_eq!((list[0].x, list[0].z, list[0].cue), (x2, z2, 0));
    let p2 = NpcPack::parse(&moved).unwrap();
    let g = p2.triggers(&moved).into_iter().find(|g| g.pass == 1 && g.chunk == 78).unwrap();
    assert!(g.rungs.iter().all(|r| r.records.iter().any(|rec| matches!(rec, TriggerRecord::Talk { actor, .. } if *actor == list[0].actor))));
    let entry = lt_core::scene::catalog(&p2, &moved).into_iter().find(|e| e.slot == slot).unwrap();
    assert_eq!(entry.flag, 0, "a record in every rung holds in every state");

    // The program reads back for editing; deleting the scene gives retail back.
    let src = lt_core::edit::script::source(&retail, &moved, slot, "npc/6").unwrap();
    assert!(src.dsl.contains("Welcome to"), "{}", src.dsl);
    let gone = scene::delete(&retail, &moved, slot).unwrap();
    assert_eq!(gone, retail);
}

/// A new scene walks the Knight to a point nothing on the disc names: the path
/// is laid out with the program, plays in the scene player (the Knight ends on
/// it, facing its direction), reads back as a path for the editor and the DSL,
/// and saves again unchanged.
#[test]
fn a_new_scene_walks_the_knight_to_a_new_point() {
    use lt_core::edit::script::Arg;
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let w = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = w.file("npc/6").unwrap();
    let chunk = w.chunk(78).unwrap();
    let heights = lt_core::world::heightmap(&chunk);
    let h_at = |x: i16, z: i16| heights[(((z as i32 + 0x800) >> 5) * 128 + ((x as i32 + 0x800) >> 5)) as usize];
    let (x, z) = (-1600i16, -856i16);
    let (wx, wz, dir) = (-1540i16, -760i16, 0x400u16);
    let walk = Node::Op { op: 0x1C, args: vec![Arg::Path { x: wx, h: h_at(wx, wz), z: wz, dir, words: vec![] }], at: None };
    let tree = vec![walk.clone(), op(0x01, vec![text("Here.")]), op(0x00, vec![])];
    let (bytes, slot) = scene::create(&retail, &retail, &NewScene { chunk: 78, x, h: h_at(x, z), z, cue: 0 }, &tree, &[]).unwrap();

    // The tree reads the walk back as a path; the DSL prints it as to(…).
    let src = lt_core::edit::script::source(&retail, &bytes, slot, "npc/6").unwrap();
    assert!(src.dsl.contains(&format!("walk to({wx} {} {wz} {dir})", h_at(wx, wz))), "{}", src.dsl);
    assert!(matches!(&src.tree[0], Node::Op { op: 0x1C, args, .. } if matches!(args[0], Arg::Path { x, z, .. } if (x, z) == (wx, wz))));

    // Saving the read-back tree again changes nothing.
    let again = lt_core::edit::script::check_tree(&retail, &bytes, slot, Some(&src.tree), &[]).unwrap();
    assert_eq!(again.bytes.as_deref(), Some(&bytes[..]), "{:?}", again.diagnostics);

    // It plays: the Knight walks there, turns, and the line follows.
    let pack = NpcPack::parse(&bytes).unwrap();
    let tables = Tables::from_exe(&exe).unwrap();
    let item = |n: u16| format!("item{n}").into_bytes();
    let pb = lt_core::scene::play(&SceneInput {
        tables: &tables,
        pack: &pack,
        pack_bytes: &bytes,
        chunk: 78,
        chunk_bytes: &chunk,
        slot,
        start: Start::Trigger { x, z },
        flags: vec![0xFC, 0xFE, 0xFF],
        answers: vec![],
        baby: true,
        yaw: None,
        hold_base: 20,
        hold_per_glyph: 1.0,
        names: ["Knight".into(), "Baby".into(), "Nehani".into(), "K-Knight".into(), "Bonik".into()],
        item_name: &item,
        toy_name: &item,
        max_frames: 3000,
    });
    assert!(matches!(pb.meta.ending, Ending::End), "{:?} {:?}", pb.meta.ending, pb.meta.notes);
    let stride = lt_core::scene::FRAME_HEAD + lt_core::scene::ACTOR_BYTES * pb.meta.actors.len();
    let last = pb.meta.frames as usize - 1;
    let rd = |o: usize| i16::from_le_bytes([pb.frames[last * stride + lt_core::scene::FRAME_HEAD + o], pb.frames[last * stride + lt_core::scene::FRAME_HEAD + o + 1]]);
    assert_eq!((rd(0), rd(4), rd(6) as u16 & 0xFFF), (wx, wz, dir), "the Knight ends on the path, facing its direction");
    assert_eq!(pb.meta.pages.len(), 1);
}

/// Every pack takes a new scene. Ten copy one of retail's own scene directors, whose looks are
/// already a director's, and the copy must still read back as a scene (LT-42, npc/2).
#[test]
fn every_pack_takes_a_scene() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let mut no_source = Vec::new();
    for n in (0..50u32).filter(|&n| n != 17) {
        let e = table.by_id(&format!("npc/{n}")).unwrap();
        let retail = image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
        let p = NpcPack::parse(&retail).unwrap();
        let Some(g) = p.triggers(&retail).into_iter().find(|g| g.pass == 1) else { continue };
        let chunk = g.chunk as u16;
        let s = NewScene { chunk, x: 0x80, h: -0x20, z: 0x80, cue: 0xFF };
        let (out, slot) = match scene::create(&retail, &retail, &s, &lt_core::edit::blank::starter_program_at(chunk, 0x80, 0x80, 0), &[]) {
            Ok(made) => made,
            // npc/8 has no actor with a talk script to copy.
            Err(e) if e.code == "SCENE_NO_SOURCE" => {
                no_source.push(n);
                continue;
            }
            Err(e) => panic!("npc/{n}: {e}"),
        };
        let listed = scene::list(&retail, &out).unwrap();
        assert!(listed.iter().any(|l| l.slot == slot && l.chunk == chunk && l.cue == 0xFF), "npc/{n}: the scene lists");
    }
    assert_eq!(no_source, [8]);
}
