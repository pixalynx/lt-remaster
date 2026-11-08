//! Characters brought into NPC packs (`edit::character`) on every pack the
//! disc holds: a model, pose block and animation set from another pack go in,
//! every retail model, pose block, animation set and D-block byte stays, the
//! new ones read back at the new indices, and the pack splits back into the
//! exact retail bytes.
//! Opt in with `LT_TEST_DISC=… cargo test --release -p lt-core --test character`.

use lt_core::edit::character::{self, Character};
use lt_core::npc::NpcPack;
use lt_core::{DiscImage, FileTable, PsxExe};

fn packs() -> Option<Vec<(usize, Vec<u8>)>> {
    let image = DiscImage::open(std::env::var_os("LT_TEST_DISC")?).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some((0..50).filter_map(|n| table.by_id(&format!("npc/{n}")).map(|e| (n, image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()))).collect())
}

/// The first actor of `b` with a model and a pose set.
fn posed_actor(b: &[u8]) -> Option<usize> {
    let p = NpcPack::parse(b)?;
    p.actors.iter().position(|a| a.slot >= 2 && (a.b >> 16) & 0xFF != 0xFF && (a.b >> 24) & 0xFF != 0xFF && !p.anim_set(b, ((a.b >> 24) & 0xFF) as usize).is_empty())
}

#[test]
fn a_character_from_another_pack_goes_into_every_pack() {
    let Some(all) = packs() else { return };
    // Sources: a posed actor from npc/7 and from npc/23, and a still model.
    let src7 = &all.iter().find(|(n, _)| *n == 7).unwrap().1;
    let src23 = &all.iter().find(|(n, _)| *n == 23).unwrap().1;
    let a = Character::from_actor(src7, posed_actor(src7).unwrap(), "npc/7").unwrap();
    let b = Character::from_actor(src23, posed_actor(src23).unwrap(), "npc/23").unwrap();
    let mut still = b.clone();
    still.poses = None;
    still.anims.clear();
    still.label = "still".into();
    let (mut done, mut full) = (0, 0);
    for (n, retail) in &all {
        let r = NpcPack::parse(retail).unwrap();
        let (ra, rb) = character::retail_counts(retail).unwrap();
        let mut chars: Vec<Character> = if rb + 2 <= character::MAX_POSE_BLOCKS { vec![a.clone(), still.clone(), b.clone()] } else { vec![still.clone()] };
        let out = match character::apply(retail, retail, &chars, &Default::default()) {
            Ok(o) => o,
            // Near the loader cap: one still model, else the pack is full.
            Err(e) if e.code == "NPC_CAP" => {
                chars = vec![still.clone()];
                match character::apply(retail, retail, &chars, &Default::default()) {
                    Ok(o) => o,
                    Err(e) if e.code == "NPC_CAP" => {
                        full += 1;
                        continue;
                    }
                    Err(e) => panic!("npc/{n}: {e}"),
                }
            }
            Err(e) => panic!("npc/{n}: {e}"),
        };
        let (base, got, _) = character::split(retail, &out).unwrap();
        assert_eq!(base.as_ref(), retail.as_slice(), "npc/{n}: splits back");
        assert_eq!(got, chars, "npc/{n}");
        let o = NpcPack::parse(&out).unwrap();
        assert_eq!(o.group_a(&out), ra + chars.len());
        // Retail models, pose blocks, sets and the D-block are unchanged.
        for i in 0..ra {
            assert_eq!(o.model(&out, i).map(|m| &m[..m.len().min(64)]), r.model(retail, i).map(|m| &m[..m.len().min(64)]), "npc/{n} model {i}");
        }
        for s in 0..rb {
            assert_eq!(o.anim_set(&out, s), r.anim_set(retail, s), "npc/{n} set {s}");
            let (x, y) = (o.pose_block(&out, s).unwrap(), r.pose_block(retail, s).unwrap());
            let len = lt_core::pose::parse(y).map(|p| p.len).unwrap_or(x.len().min(y.len()).min(256));
            assert_eq!(&x[..len], &y[..len], "npc/{n} pose block {s}");
        }
        assert_eq!(&out[o.d as usize..(o.d + r.d_span) as usize], &retail[r.d as usize..(r.d + r.d_span) as usize], "npc/{n}: D-span");
        // The new ones read back.
        let mut set = rb;
        for (k, ch) in chars.iter().enumerate() {
            let m = o.model(&out, ra + k).unwrap();
            assert_eq!(&m[..ch.model.len()], ch.model.as_slice(), "npc/{n} char {k} model");
            if let Some(p) = &ch.poses {
                assert_eq!(&o.pose_block(&out, set).unwrap()[..p.len()], p.as_slice());
                assert_eq!(o.anim_set(&out, set), ch.anims, "npc/{n} char {k} anims");
                set += 1;
            }
        }
        done += 1;
    }
    eprintln!("{done} packs took characters, {full} are too close to the loader cap");
    assert!(done + full == 50 && done >= 45, "{done} {full}");
}

/// npc/7 (the Knight's house, 4 KB below the loader cap) takes the
/// smallest character on the disc, npc/29 actor 12, and an added actor that
/// looks like it; a re-layout and the blank slate on top keep both, and the
/// actor's record points at the character's model and set.
#[test]
fn an_added_actor_looks_like_a_brought_in_character_through_every_writer() {
    use lt_core::edit::{actor::AddedActor, blank, pack::Edits, text};
    let Some(all) = packs() else { return };
    let retail = &all.iter().find(|(n, _)| *n == 7).unwrap().1;
    let src = &all.iter().find(|(n, _)| *n == 29).unwrap().1;
    let ch = Character::from_actor(src, 12, "npc/29 · actor 12").unwrap();
    assert!(ch.poses.is_some() && !ch.anims.is_empty());
    let r = NpcPack::parse(retail).unwrap();
    let (ra, rb) = character::retail_counts(retail).unwrap();
    let from = r.actors.iter().position(|a| a.slot >= 2 && !lt_core::edit::actor::is_object(a.d)).unwrap() as u32;
    let mut set = Edits::recover(retail, retail).unwrap();
    set.characters = vec![ch.clone()];
    set.actors = vec![AddedActor { from, home: 326, x: 560, h: -33, z: 100, dir: 1024, hidden: false, own: None, character: Some(0), spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false }];
    let (out, _, _) = text::layout(retail, retail, &set).unwrap();
    let back = Edits::recover(retail, &out).unwrap();
    assert_eq!(back.characters, set.characters);
    assert_eq!(back.actors[0].character, Some(0));
    let o = NpcPack::parse(&out).unwrap();
    let rec = &o.actors[r.actors.len()];
    assert_eq!(((rec.b >> 16) & 0xFF) as usize, ra);
    assert_eq!(((rec.b >> 24) & 0xFF) as usize, rb);
    assert_eq!((rec.d & 0xFFFF) as u16, ch.looks);
    assert_eq!(o.anim_set(&out, rb), ch.anims);
    // The layout is stable, and a text re-layout keeps them.
    assert_eq!(text::layout(retail, &out, &back).unwrap().0, out);
    let (t, _) = text::apply(retail, &out, &back.text).unwrap();
    assert_eq!(t, out);
    // The blank slate keeps them.
    let (blanked, _) = blank::blank(retail, &out, &Default::default()).unwrap();
    let after = Edits::recover(retail, &blanked).unwrap();
    assert_eq!(after.characters, set.characters);
    assert_eq!(after.actors.len(), 1);
    assert_eq!(after.actors[0].character, Some(0));
    // Too big for the room left: a clear refusal.
    let big = Character::from_actor(&all.iter().find(|(n, _)| *n == 23).unwrap().1, posed_actor(&all.iter().find(|(n, _)| *n == 23).unwrap().1).unwrap(), "npc/23").unwrap();
    set.characters.push(big);
    assert_eq!(text::layout(retail, retail, &set).unwrap_err().code, "NPC_CAP");
}

/// Every blanked pack frees the models only taken-out actors used, splits
/// back to the blanked bytes, and restores to retail byte for byte; the
/// Knight's house then has room for an untextured villager from npc/23.
#[test]
fn a_blank_pack_frees_its_taken_out_models_and_puts_them_back() {
    use lt_core::edit::{actor::AddedActor, blank, pack::Edits, text};
    let Some(all) = packs() else { return };
    let (mut packs_freed, mut total) = (0, 0u64);
    for (n, retail) in &all {
        let (blanked, _) = blank::blank(retail, retail, &Default::default()).unwrap();
        let mut set = Edits::recover(retail, &blanked).unwrap();
        set.freed = character::freeable(retail, &set.removed, &set.actors).unwrap();
        if set.freed.is_empty() {
            continue;
        }
        let saved = character::freed_bytes(retail, &set.freed).unwrap();
        let (out, _, _) = text::layout(retail, &blanked, &set).unwrap_or_else(|e| panic!("npc/{n}: {e}"));
        let used = |b: &[u8]| b.len() - b.iter().rev().take_while(|&&x| x == 0).count();
        // The freed lengths include retail's zero tail after the last section.
        let tail = retail.len() - used(retail);
        assert!(used(&out) + saved as usize <= used(&blanked) + tail + 256, "npc/{n}: {} + {saved} vs {} (+{tail})", used(&out), used(&blanked));
        let (base, _, freed) = character::split(retail, &out).unwrap();
        assert_eq!(base.as_ref(), blanked.as_slice(), "npc/{n}: splits back");
        assert_eq!(freed, set.freed);
        assert_eq!(Edits::recover(retail, &out).unwrap().freed, set.freed);
        // Putting the story back brings every model back.
        let restored = blank::restore(retail, &out, None).unwrap();
        assert_eq!(restored, *retail, "npc/{n}: restores to retail");
        packs_freed += 1;
        total += saved as u64;
    }
    eprintln!("{packs_freed} packs free {total} B in all");
    assert!(packs_freed > 40);
    // The Knight's house, freed, takes a villager from npc/23.
    let retail = &all.iter().find(|(n, _)| *n == 7).unwrap().1;
    let src = &all.iter().find(|(n, _)| *n == 23).unwrap().1;
    let villager = (2..NpcPack::parse(src).unwrap().actors.len())
        .filter_map(|s| Character::from_actor(src, s, format!("npc/23 · {s}")).ok())
        .find(|c| lt_core::edit::tmdcodec::Tmd::parse(&c.model).unwrap().objects.iter().all(|o| o.prims.iter().all(|p| p.mode & 4 == 0)) && c.poses.is_some())
        .expect("an untextured villager");
    let (blanked, _) = blank::blank(retail, retail, &Default::default()).unwrap();
    let mut set = Edits::recover(retail, &blanked).unwrap();
    set.freed = character::freeable(retail, &set.removed, &set.actors).unwrap();
    let r = NpcPack::parse(retail).unwrap();
    let from = r.actors.iter().position(|a| a.slot >= 2 && !lt_core::edit::actor::is_object(a.d)).unwrap() as u32;
    set.characters.push(villager);
    set.actors.push(AddedActor { from, home: 326, x: 640, h: -33, z: 20, dir: 3072, hidden: false, own: None, character: Some(0), spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false });
    let (out, rep, _) = text::layout(retail, &blanked, &set).unwrap();
    assert!(rep.sectors <= 98);
    let back = Edits::recover(retail, &out).unwrap();
    assert_eq!(back.characters.len(), 1);
    assert_eq!(back.freed, set.freed);
}

/// A model textured from its own area's pages isn't portable (npc/29 actor
/// 12 draws transparent in the Knight's house, checked in Redux); an
/// untextured villager is, and so are models textured from SYS14/SYS08.
#[test]
fn portability_follows_the_vram_every_area_shares() {
    let Some(all) = packs() else { return };
    let image = DiscImage::open(std::env::var_os("LT_TEST_DISC").unwrap()).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let rd = |id: &str| table.by_id(id).map(|e| image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()).unwrap();
    let (s2, s7) = (rd("sys/2"), rd("sys/7"));
    let owned = character::owned_by(&[&s2, &s7]);
    let get = |n: usize| &all.iter().find(|(k, _)| *k == n).unwrap().1;
    assert!(!character::portable(&Character::from_actor(get(29), 12, "").unwrap().model, &owned));
    assert!(character::portable(&Character::from_actor(get(7), 2, "").unwrap().model, &owned));
    let (mut yes, mut no) = (0, 0);
    for (_, b) in &all {
        let p = NpcPack::parse(b).unwrap();
        for m in 0..p.group_a(b) {
            if let Some(sec) = p.model(b, m) {
                if character::portable(sec, &owned) { yes += 1 } else { no += 1 }
            }
        }
    }
    eprintln!("{yes} portable models, {no} textured from their area's pages");
    assert!(yes > 0 && no > 0);
}
