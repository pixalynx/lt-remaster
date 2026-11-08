//! Actor previews for the Place actor bar (LT-78) on retail packs: what the thumbnail of each
//! actor a copy can be made of, and of each character brought in, shows. The meshes are built
//! the way `render3d_actor` and `render3d_character` build them with `preview: true`.
//! Opt in with `LT_TEST_DISC=… cargo test --release -p lt-core --test actor_previews`.

use lt_core::edit::character::{self, Character};
use lt_core::mesh::{self, RenderMesh, FOLDED};
use lt_core::npc::{ActorLook, NpcPack};
use lt_core::{DiscImage, FileTable, PsxExe};

fn packs() -> Option<Vec<(usize, Vec<u8>)>> {
    let image = DiscImage::open(std::env::var_os("LT_TEST_DISC")?).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some((0..50).filter_map(|n| table.by_id(&format!("npc/{n}")).map(|e| (n, image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()))).collect())
}

/// Model `model` of `b` in pose set `set` (0xFF = still) as a preview draws it.
fn preview(p: &NpcPack, b: &[u8], model: u8, set: u8) -> Option<RenderMesh> {
    let mut m = mesh::tmd_mesh(p.model(b, model as usize)?)?;
    if set != 0xFF {
        if let (Some(t), Some(pose)) = (p.pose_block(b, set as usize).and_then(mesh::morph_targets), p.first_pose(b, set as usize)) {
            mesh::preview_pose(&mut m, &t, pose);
        }
    }
    Some(m)
}

/// The same model in the first pose of its set, as the 3D view draws it.
fn first_pose(p: &NpcPack, b: &[u8], model: u8, set: u8) -> Option<Vec<[i16; 3]>> {
    let m = mesh::tmd_mesh(p.model(b, model as usize)?)?;
    let t = p.pose_block(b, set as usize).and_then(mesh::morph_targets);
    Some(match (t, p.first_pose(b, set as usize)) {
        (Some(t), Some(pose)) if set != 0xFF => mesh::blend(&m.verts, &t, pose, 256, 0, 0),
        _ => m.verts,
    })
}

/// The slots the Copy menu lists (`Roster.copyable`).
fn copyable(p: &NpcPack) -> impl Iterator<Item = &lt_core::npc::ActorRecord> {
    p.actors.iter().filter(|a| a.slot >= 2 && a.script != 0 && !lt_core::edit::actor::is_object(a.d))
}

/// @LT-78/1: every actor the Copy menu lists, in every pack, previews as a model with a shape
/// (drawn, or hidden until a scene reveals it), or has no model at all and gets the placeholder.
/// npc/6 lists 21, two of them without a model.
#[test]
fn every_copyable_actor_previews_or_is_a_placeholder() {
    let Some(all) = packs() else { return };
    let (mut shown, mut none, mut hidden) = (0, 0, 0);
    for (n, b) in &all {
        let p = NpcPack::parse(b).unwrap();
        for a in copyable(&p) {
            let look = ActorLook::of(a);
            if !look.previewed() {
                assert!(look.model.is_none(), "npc/{n} actor {}: a model and no preview ({look:?})", a.slot);
                none += 1;
                continue;
            }
            let m = preview(&p, b, look.model.unwrap(), look.poses.unwrap_or(0xFF)).unwrap_or_else(|| panic!("npc/{n} actor {}: no mesh", a.slot));
            assert!(!m.polys.is_empty(), "npc/{n} actor {}", a.slot);
            assert!(m.extent() >= FOLDED, "npc/{n} actor {}: extent {}", a.slot, m.extent());
            shown += 1;
            hidden += !look.drawn() as u32;
        }
        if *n == 6 {
            let rows: Vec<_> = copyable(&p).collect();
            assert_eq!(rows.len(), 21);
            assert_eq!(rows.iter().filter(|a| ActorLook::of(a).model.is_none()).count(), 2);
        }
    }
    assert!(shown > 500 && none > 0 && hidden > 0, "{shown} shown, {none} placeholders, {hidden} hidden");
}

/// @LT-78/1: npc/6 actor 10, a ghost a scene grows out of a point, is the one actor whose first
/// pose folds it away; its preview is the pose where it is biggest. Every other actor previews in
/// the first pose of its idle set, exactly as the 3D view draws it.
#[test]
fn a_folded_ghost_previews_at_its_biggest() {
    let Some(all) = packs() else { return };
    let mut folded = Vec::new();
    for (n, b) in &all {
        let p = NpcPack::parse(b).unwrap();
        for a in p.actors.iter().filter(|a| a.slot >= 2) {
            let look = ActorLook::of(a);
            let (Some(model), Some(set)) = (look.model, look.poses) else { continue };
            if !look.previewed() {
                continue;
            }
            let Some(m) = preview(&p, b, model, set) else { continue };
            let first = first_pose(&p, b, model, set).unwrap();
            if m.verts != first {
                folded.push((*n, a.slot, m.extent()));
                let mut shut = m.clone();
                shut.verts = first;
                assert!(shut.extent() < FOLDED, "npc/{n} actor {}: re-posed with a shape", a.slot);
            }
        }
    }
    assert_eq!(folded.len(), 1, "{folded:?}");
    let (n, slot, extent) = folded[0];
    assert_eq!((n, slot), (6, 10));
    assert!(extent > 60, "the ghost is figure-sized: {extent}");
}

/// The first actor of `b` with a model and a pose set.
fn posed_actor(b: &[u8]) -> Option<usize> {
    let p = NpcPack::parse(b)?;
    p.actors.iter().position(|a| a.slot >= 2 && (a.b >> 16) & 0xFF != 0xFF && (a.b >> 24) & 0xFF != 0xFF && !p.anim_set(b, ((a.b >> 24) & 0xFF) as usize).is_empty())
}

/// @LT-78/3: characters brought into a pack (a posed actor of npc/7 and a still model of npc/23,
/// into the first pack with room for both) preview as the model, pose set and looks an actor that
/// looks like them takes, the same picture as their source actor.
#[test]
fn brought_in_characters_preview() {
    let Some(all) = packs() else { return };
    let pack = |n: usize| &all.iter().find(|(k, _)| *k == n).unwrap().1;
    let (src7, src23) = (pack(7), pack(23));
    let (s7, s23) = (posed_actor(src7).unwrap(), posed_actor(src23).unwrap());
    let a = Character::from_actor(src7, s7, "npc/7").unwrap();
    let mut still = Character::from_actor(src23, s23, "npc/23").unwrap();
    still.poses = None;
    still.anims.clear();
    let chars = vec![a, still];
    let (into, out) = all
        .iter()
        .filter(|(n, _)| *n != 7 && *n != 23)
        .find_map(|(_, b)| character::apply(b, b, &chars, &Default::default()).ok().map(|out| (b, out)))
        .expect("a pack with room");
    let o = NpcPack::parse(&out).unwrap();
    let looks = character::looks(into, &chars).unwrap();
    assert_eq!(looks.len(), 2);
    for (k, &(model, set, _)) in looks.iter().enumerate() {
        let m = preview(&o, &out, model, set).unwrap_or_else(|| panic!("character {k}: no mesh"));
        assert!(m.extent() >= FOLDED, "character {k}: extent {}", m.extent());
        // The same model and pose as the actor it came from.
        let (src, slot) = if k == 0 { (src7, s7) } else { (src23, s23) };
        let sp = NpcPack::parse(src).unwrap();
        let look = ActorLook::of(&sp.actors[slot]);
        let from = preview(&sp, src, look.model.unwrap(), if k == 0 { look.poses.unwrap() } else { 0xFF }).unwrap();
        assert_eq!(m.verts, from.verts, "character {k}");
    }
}
