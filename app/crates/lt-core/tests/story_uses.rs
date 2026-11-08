//! The story graph's reverse index (`story::uses`, screen 21's "Also used in
//! the story" and the MCP `story_uses` tool). The disc test opts in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test story_uses`.

use lt_core::npc::NpcPack;
use lt_core::story::{self, Graph, Node, PackIn, Page, PageKind, Start, Step, UseKey};
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn page(id: &str, pack: u16, place: &str, flow: Vec<Step>) -> Page {
    Page {
        id: id.into(),
        pack,
        slot: Some(2),
        kind: PageKind::Talk,
        chunk: 78,
        place: place.into(),
        speaker: None,
        line: None,
        start: Start::Talk { ladder: Vec::new() },
        chapter: None,
        layer: 0,
        added: false,
        removed: false,
        script: 0,
        stmts: 0,
        nodes: 0,
        stages: 0,
        branches: 0,
        flow,
        sets: Vec::new(),
        clears: Vec::new(),
        tests: Vec::new(),
    }
}

fn step(at: u32, node: Node) -> Step {
    Step { at, node }
}

fn flag(f: u16, place: Option<&str>) -> UseKey {
    UseKey::Flag { flag: f, place: place.map(String::from) }
}

/// Keys print as the frontend's `keysOf`; scratch flags match per place, or
/// anywhere without one; lanes are searched.
#[test]
fn keys_and_uses() {
    let say = Node::Say { speaker: Some("Mayor".into()), lines: vec!["Hi".into(), "Bye".into()], texts: vec![0x10C4, 0x10D0] };
    let branch = Node::Branch {
        kind: "flag",
        value: 0x97,
        question: None,
        yes: None,
        no: None,
        then: vec![step(0x20, Node::Flag { flag: 0x05, set: true }), step(0x24, Node::Give { item: 1003 })],
        els: vec![step(0x30, Node::Chest { flag: 0x120, item: 12 })],
    };
    let a = page("6.11", 6, "Orgo", vec![step(0x10, say), step(0x18, branch), step(0x40, Node::Stage { ops: vec!["sound".into()], sfx: vec![40], bgm: vec![7] })]);
    let b = page("12.3", 12, "Isten", vec![step(0x08, Node::Flag { flag: 0x05, set: false }), step(0x0C, Node::Warp { chunk: 78, x: 0, z: 0, facing: 0 })]);

    let keys: Vec<String> = story::keys_of(&a.flow[0].node, &a).iter().map(ToString::to_string).collect();
    assert_eq!(keys, ["text:6:4292", "text:6:4304"]);
    assert_eq!(story::keys_of(&a.flow[1].node, &a), [flag(0x97, None)]);
    let Node::Branch { then, els, .. } = &a.flow[1].node else { unreachable!() };
    assert_eq!(story::keys_of(&then[0].node, &a)[0].to_string(), "flag:5@Orgo");
    assert_eq!(story::keys_of(&els[0].node, &a).iter().map(ToString::to_string).collect::<Vec<_>>(), ["item:12", "flag:288"]);
    assert_eq!(story::keys_of(&a.flow[2].node, &a).iter().map(ToString::to_string).collect::<Vec<_>>(), ["sfx:40", "bgm:7"]);

    let g = Graph { pages: vec![a, b], links: Vec::new(), unset: Vec::new(), exits: Vec::new() };
    assert_eq!(story::uses(&g, &UseKey::Text { pack: 6, offset: 0x10D0 }), [(0, 0x10)]);
    assert_eq!(story::uses(&g, &UseKey::Text { pack: 7, offset: 0x10D0 }), []);
    assert_eq!(story::uses(&g, &flag(0x97, None)), [(0, 0x18)]);
    // A story flag ignores a place.
    assert_eq!(story::uses(&g, &flag(0x97, Some("Isten"))), [(0, 0x18)]);
    assert_eq!(story::uses(&g, &flag(0x05, Some("Orgo"))), [(0, 0x20)]);
    assert_eq!(story::uses(&g, &flag(0x05, None)), [(0, 0x20), (1, 0x08)]);
    assert_eq!(story::uses(&g, &flag(0x120, None)), [(0, 0x30)]);
    assert_eq!(story::uses(&g, &UseKey::Item(1003)), [(0, 0x24)]);
    assert_eq!(story::uses(&g, &UseKey::Chunk(78)), [(1, 0x0C)]);
    assert_eq!(story::uses(&g, &UseKey::Bgm(7)), [(0, 0x40)]);
    assert_eq!(story::uses(&g, &UseKey::Sfx(7)), []);

    assert!(matches!(story::step_at(&g.pages[0].flow, 0x24).map(|s| &s.node), Some(Node::Give { item: 1003 })));
    assert!(matches!(story::step_at(&g.pages[0].flow, 0x18).map(|s| &s.node), Some(Node::Branch { .. })));
    assert!(story::step_at(&g.pages[0].flow, 0x99).is_none());
}

/// `FileTable_NpcBin` entry 17 aliases entry 0.
const PACK_ALIAS: u16 = 17;

fn retail_graph() -> Option<Graph> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let bytes: Vec<(u16, Vec<u8>)> = (0..50).filter(|&p| p != PACK_ALIAS).filter_map(|p| Some((p, wd.file(&format!("npc/{p}")).ok()?))).collect();
    let parsed: Vec<NpcPack> = bytes.iter().map(|(_, b)| NpcPack::parse(b).unwrap()).collect();
    let ins: Vec<PackIn> = bytes.iter().zip(&parsed).map(|((index, b), p)| PackIn { index: *index, bytes: b, retail_actors: p.actors.len() }).collect();
    Some(story::build(&ins))
}

/// The stranger's dream (page 6.11) opens with line npc/6@10C4 at D+0x1B12
/// and starts on stranger_dream_night (0x097); every hit is a node that
/// touches its key.
#[test]
fn retail_uses() {
    let Some(g) = retail_graph() else { return };
    let dream = g.pages.iter().position(|p| p.id == "6.11").expect("page 6.11") as u32;

    let line = story::uses(&g, &UseKey::Text { pack: 6, offset: 0x10C4 });
    assert!(line.contains(&(dream, 6930)), "{line:?}");
    assert!(line.iter().all(|&(p, _)| g.pages[p as usize].pack == 6));

    let state = story::uses(&g, &flag(0x097, None));
    assert!(state.iter().any(|&(p, _)| p == dream), "{state:?}");
    for &(p, at) in &state {
        let pg = &g.pages[p as usize];
        let s = story::step_at(&pg.flow, at).unwrap_or_else(|| panic!("{} @{at} is a node", pg.id));
        assert!(story::keys_of(&s.node, pg).contains(&flag(0x097, None)), "{} @{at}", pg.id);
    }

    // A scratch flag in one place is a subset of it everywhere.
    let orgo = story::uses(&g, &flag(0x004, Some("Orgo")));
    let all = story::uses(&g, &flag(0x004, None));
    assert!(!orgo.is_empty() && orgo.len() < all.len(), "{} of {}", orgo.len(), all.len());
    assert!(orgo.iter().all(|&(p, _)| g.pages[p as usize].place == "Orgo"));
}
