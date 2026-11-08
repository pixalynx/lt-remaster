//! Story graph checks (screen 21, research 08) against the user's own retail image.
//! Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test --release -p lt-core --test story`.

use std::collections::{BTreeMap, BTreeSet};

use lt_core::npc::NpcPack;
use lt_core::story::{self, LinkKind, Node, PackIn, PageKind, Step};
use lt_core::tier2;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

/// `FileTable_NpcBin` entry 17 aliases entry 0.
const PACK_ALIAS: u16 = 17;

fn packs() -> Option<Vec<(u16, Vec<u8>)>> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    Some((0..50).filter(|&p| p != PACK_ALIAS).filter_map(|p| Some((p, wd.file(&format!("npc/{p}")).ok()?))).collect())
}

fn ats(flow: &[Step], out: &mut Vec<u32>) {
    for s in flow {
        match &s.node {
            Node::Label { .. } => {}
            Node::Branch { then, els, .. } => {
                out.push(s.at);
                ats(then, out);
                ats(els, out);
            }
            _ => out.push(s.at),
        }
    }
}

fn jumps(flow: &[Step], out: &mut Vec<String>) {
    for s in flow {
        match &s.node {
            Node::Jump { label, .. } => out.push(label.clone()),
            Node::Branch { then, els, .. } => {
                jumps(then, out);
                jumps(els, out);
            }
            _ => {}
        }
    }
}

/// The retail graph has the pages, links and loose ends research 08 measured;
/// every node sits on an instruction of its own program; no jump leaves its page.
#[test]
fn the_retail_story_graph() {
    let Some(bytes) = packs() else { return };
    let parsed: Vec<NpcPack> = bytes.iter().map(|(_, b)| NpcPack::parse(b).unwrap()).collect();
    let ins: Vec<PackIn> = bytes.iter().zip(&parsed).map(|((index, b), p)| PackIn { index: *index, bytes: b, retail_actors: p.actors.len() }).collect();
    let g = story::build(&ins);

    let mut kinds: BTreeMap<PageKind, usize> = BTreeMap::new();
    for p in &g.pages {
        *kinds.entry(p.kind).or_default() += 1;
    }
    assert_eq!(g.pages.len(), 3535);
    assert_eq!(
        kinds,
        BTreeMap::from([(PageKind::Scene, 115), (PageKind::Talk, 411), (PageKind::Examine, 159), (PageKind::Silent, 288), (PageKind::Spot, 2421), (PageKind::Step, 141)])
    );
    assert!(g.pages.iter().all(|p| !p.added), "retail adds nothing");
    assert!(g.pages.iter().all(|p| !p.removed), "retail removes nothing");
    let programs = g.pages.iter().filter(|p| p.slot.is_some());
    assert!(programs.clone().all(|p| p.script > 0), "a program's page knows where it starts");
    for p in programs {
        let pack = &parsed[bytes.iter().position(|(i, _)| *i == p.pack).unwrap()];
        assert_eq!(p.script, pack.actors[p.slot.unwrap() as usize].script, "page {} starts where its record says", p.id);
    }
    let ids: BTreeSet<&str> = g.pages.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids.len(), g.pages.len(), "page ids are unique");

    let mut lk: BTreeMap<LinkKind, usize> = BTreeMap::new();
    for l in &g.links {
        *lk.entry(l.kind).or_default() += 1;
    }
    assert_eq!(g.links.len(), 2921);
    assert_eq!(
        lk,
        BTreeMap::from([(LinkKind::Unlocks, 547), (LinkKind::Removes, 339), (LinkKind::Changes, 1704), (LinkKind::Cues, 163), (LinkKind::Clears, 168)])
    );
    // Story flags read with no page setting them: five never set, three set in C (§3.4).
    let story_band: Vec<u16> = g.unset.iter().copied().filter(|f| *f >= 0x40).collect();
    assert_eq!(story_band, vec![0x054, 0x056, 0x05F, 0x061, 0x093, 0x0BA, 0x0BF, 0x0C2]);

    let (nodes, stages, branches) = g.pages.iter().fold((0, 0, 0), |a, p| (a.0 + p.nodes, a.1 + p.stages, a.2 + p.branches));
    assert_eq!((nodes, stages, branches), (13350, 1649, 1902));
    assert_eq!(g.pages.iter().map(|p| p.stmts).sum::<u32>(), 19247);
    assert_eq!(g.pages.iter().map(|p| p.layer).max(), Some(10));
    // Doors that leave their place (Orgo has none: its village is a world-map chunk, left on foot).
    assert_eq!(g.exits.len(), 53);
    assert!(g.exits.iter().all(|e| e.to_place.as_deref() != Some(e.place.as_str())));
    assert!(!g.exits.iter().any(|e| e.place == "Orgo"));

    let mut with_jumps = 0;
    for (p, (_, b)) in g.pages.iter().filter(|p| p.slot.is_some()).map(|p| (p, &bytes[ins.iter().position(|i| i.index == p.pack).unwrap()])) {
        let pack = NpcPack::parse(b).unwrap();
        let d = pack.d_block(b);
        let code = tier2::disassemble(d, pack.actors[p.slot.unwrap() as usize].script).unwrap();
        let starts: BTreeSet<u32> = code.iter().map(|i| i.at).collect();
        let mut at = Vec::new();
        ats(&p.flow, &mut at);
        assert!(at.iter().all(|a| starts.contains(a)), "{}: every node sits on an instruction", p.id);
        let mut j = Vec::new();
        jumps(&p.flow, &mut j);
        assert!(j.iter().all(|l| l.starts_with('L')), "{}: jumps stay in the page", p.id);
        with_jumps += usize::from(!j.is_empty());
    }
    assert_eq!(with_jumps, 23);
}
