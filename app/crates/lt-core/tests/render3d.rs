//! Render-mesh export checks (screens 06, 08, 09, 10) against the user's own
//! retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test render3d`.
//! `LT_TEST_OUT=<dir>` also writes some payloads for eyeballing.

// Vertex × axis loops read clearer indexed.
#![allow(clippy::needless_range_loop)]

use std::time::Instant;

use lt_core::container;
use lt_core::mesh::{self, flag};
use lt_core::world;
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
    fn file(&self, id: &str) -> Vec<u8> {
        let e = self.table.by_id(id).unwrap_or_else(|| panic!("no {id}"));
        self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
    }

    fn chunk(&self, id: u32) -> Vec<u8> {
        let w = self.table.by_id("world").unwrap();
        self.image.read_sectors(w.extent.lba + id * world::CHUNK_SECTORS, world::CHUNK_SECTORS).unwrap()
    }

    /// The field VRAM actors on `chunk` sample (`render3d.rs` `actor_vram`): SYS08, the
    /// chunk's `fe/k` set, sibling interior pages, the chunk, then SYS14.
    fn actor_vram(&self, sys2: &[u8], sys7: &[u8], ids: &[u16], chunk: u32) -> mesh::OwnedVram {
        let set = self.file(&format!("fe/{}", world::texset_for(ids, chunk)));
        let siblings: Vec<Vec<u8>> = if chunk >= world::GRID_CHUNKS {
            world::interior_group(chunk).into_iter().filter(|&s| s != chunk && world::valid_chunk(s)).map(|s| self.chunk(s)).collect()
        } else {
            Vec::new()
        };
        let mut vram = mesh::field_vram(Some(sys7), Some(&set), &siblings, &self.chunk(chunk));
        vram.paint_tim(sys2);
        vram
    }

    fn battle(&self) -> mesh::OwnedVram {
        let (s2, s4, s19) = (self.file("sys/2"), self.file("sys/4"), self.file("sys/19"));
        mesh::battle_vram(Some(&s2), Some(&s4), Some(&s19))
    }
}

/// `LT_TEST_OUT=<dir>` also writes the payloads (`<name>.bin`) for the WebGL harness.
fn dump(name: &str, bytes: &[u8]) {
    let Some(dir) = std::env::var_os("LT_TEST_OUT") else { return };
    std::fs::write(std::path::PathBuf::from(dir).join(format!("{name}.bin")), bytes).unwrap();
}

fn section(b: &[u8], n: usize, i: usize) -> &[u8] {
    let s = &container::sections(b, n).unwrap()[i];
    &b[s.offset as usize..(s.offset + s.len) as usize]
}

/// A toy's main (7) or effect (4) model with the battle VRAM and its own TIM.
fn toy(d: &Disc, base: &mesh::OwnedVram, n: u32, sec: usize) -> (mesh::RenderMesh, mesh::Atlas) {
    let b = d.file(&format!("toy/{n}"));
    let mut m = mesh::tmd_mesh(section(&b, 8, sec)).unwrap();
    let mut v = base.clone();
    v.paint_tim(section(&b, 8, 6));
    let a = mesh::build_atlas(&mut m, &v);
    (m, a)
}

#[test]
fn draken_main_model() {
    let Some(d) = disc() else { return };
    let base = d.battle();
    let (m, a) = toy(&d, &base, 22, 7);
    assert_eq!((m.polys.len(), m.verts.len(), m.normals.len()), (304, 219, 259));
    assert_eq!(m.skipped, 0);
    // Build passes: 3400 ×8 (slot 2), 3C00 ×4 (3), 3004 ×218 (14), 3804 ×74 (15).
    let slots: Vec<u8> = m.polys.iter().map(|p| p.slot).collect();
    assert!(slots.windows(2).all(|w| w[0] <= w[1]), "polygons are in class-pass order");
    let count = |k: u16| m.polys.iter().filter(|p| p.class == k).count();
    assert_eq!((count(0x3400), count(0x3C00), count(0x3004), count(0x3804)), (8, 4, 218, 74));
    assert!(m.polys.iter().all(|p| p.flags & flag::LIT != 0));
    // Textured polygons sample the toy page (tpage 0x00A) through rows (112, 480..482).
    let tex: Vec<_> = m.polys.iter().filter(|p| p.textured()).collect();
    assert_eq!(tex.len(), 12);
    assert!(tex.iter().all(|p| p.tpage & 0x1F == 0x0A && (p.clut & 0x3F) == 7 && (480..483).contains(&(p.clut >> 6))));
    assert!(tex.iter().all(|p| p.flags & flag::UNOWNED == 0), "the own TIM covers every main-model texel");
    assert!(!a.tiles.is_empty() && a.tiles.len() <= 3);
    assert_eq!(a.rgba.len() as u32, a.w * a.h * 4);
    // Every atlas UV lands inside its tile.
    for p in &tex {
        let t = a.tiles[p.tile as usize];
        for c in 0..p.n as usize {
            let [u, v] = p.uv[c];
            assert!(u >= t.x && u < t.x + t.w && v >= t.y && v < t.y + t.h);
        }
    }
    let bytes = mesh::encode(&m, &a);
    dump("toy22", &bytes);
    assert_eq!(&bytes[0..4], b"LT3D");
    let expect = 64 + (219 + 259) * 8 + 16 + a.tiles.len() * 16 + 304 * 64 + a.rgba.len();
    assert_eq!(bytes.len(), expect);
}

#[test]
fn draken_effect_model_samples_unowned_vram() {
    let Some(d) = disc() else { return };
    let base = d.battle();
    let (m, a) = toy(&d, &base, 22, 4);
    dump("toy22fx", &mesh::encode(&m, &a));
    assert_eq!((m.polys.len(), m.verts.len()), (32, 32));
    // 3F01 ×32: GT4 prelit textured, semi-transparent, ABR 1 (B+F).
    assert!(m.polys.iter().all(|p| p.class == 0x3F01 && p.flags & flag::SEMI != 0 && p.abr == 1));
    // t02A c7809: u 128–159 → VRAM (672..679, 0..31), which no file paints.
    assert!(m.polys.iter().all(|p| p.tpage == 0x2A && p.clut == 0x7809));
    assert!(m.polys.iter().any(|p| p.flags & flag::UNOWNED != 0));
}

#[test]
fn every_toy_exports() {
    let Some(d) = disc() else { return };
    let base = d.battle();
    let t = Instant::now();
    let mut largest = 0usize;
    for n in 0..72 {
        let b = d.file(&format!("toy/{n}"));
        let stats = lt_core::toy::parse(&b).unwrap();
        let (m, a) = toy(&d, &base, n, 7);
        let s = stats.model.unwrap();
        assert_eq!(m.polys.len() as u32 + m.skipped, s.primitives, "toy/{n} polygons");
        assert_eq!(m.verts.len() as u32, s.vertices, "toy/{n} vertices");
        let bytes = mesh::encode(&m, &a);
        if [4, 5, 21, 60].contains(&n) {
            dump(&format!("toy{n}"), &bytes);
        }
        largest = largest.max(bytes.len());
        let (fx, _) = toy(&d, &base, n, 4);
        assert_eq!(fx.polys.len() as u32 + fx.skipped, stats.effect_model.map(|s| s.primitives).unwrap_or(0), "toy/{n} fx");
    }
    eprintln!("72 toys × 2 models in {:?}, largest payload {} B", t.elapsed(), largest);
}

#[test]
fn heal_bat_morph_targets() {
    let Some(d) = disc() else { return };
    let b = d.file("toy/40");
    let targets = mesh::morph_targets(section(&b, 8, 0)).unwrap();
    assert_eq!(targets.len(), 8);
    assert!(targets.iter().all(|t| t.first == 0 && t.deltas.len() == 145), "all 8 poses are dense");
    let max: Vec<u16> = targets.iter().map(|t| t.max_delta()).collect();
    assert_eq!(max, [24, 34, 41, 46, 60, 55, 34, 27]);
    let m = mesh::tmd_mesh(section(&b, 8, 7)).unwrap();
    assert_eq!(m.verts.len(), 145);
    let bytes = mesh::encode_poses(&targets, m.verts.len() as u32);
    dump("toy40poses", &bytes);
    let base = d.battle();
    let (tm, ta) = toy(&d, &base, 40, 7);
    dump("toy40", &mesh::encode(&tm, &ta));
    assert_eq!(&bytes[0..4], b"LTPS");
    assert_eq!(bytes.len(), 16 + 8 * (8 + 145 * 6 + 2));

    // Integer blend: full weight lands exactly on base + delta; half weight truncates (>>12).
    let full = mesh::blend(&m.verts, &targets, 0, 0, 3, 256);
    for (k, v) in full.iter().enumerate() {
        for c in 0..3 {
            assert_eq!(v[c], m.verts[k][c].wrapping_add(targets[2].deltas[k][c]));
        }
    }
    let half = mesh::blend(&m.verts, &targets, 1, 128, 2, 128);
    for k in 0..145 {
        for c in 0..3 {
            let a = (targets[0].deltas[k][c] as i32 * (128 << 4)) >> 12;
            let b2 = (targets[1].deltas[k][c] as i32 * (128 << 4)) >> 12;
            assert_eq!(half[k][c] as i32, m.verts[k][c] as i32 + a + b2);
        }
    }
}

#[test]
fn chunk39_mesh_and_atlas() {
    let Some(d) = disc() else { return };
    let c = d.chunk(39);
    let t = Instant::now();
    let parsed = world::mesh(&c).unwrap();
    let mut m = mesh::chunk_mesh(&parsed);
    assert_eq!(m.polys.len(), 3880);
    assert_eq!(m.verts.len(), 3635);
    assert_eq!(m.groups.len(), 53);
    assert_eq!(m.groups.iter().map(|g| g.count).sum::<u32>(), 3880);
    let ids = world::texset_ids(&d.exe).unwrap();
    let set = d.file(&format!("fe/{}", world::texset_for(&ids, 39)));
    let v = mesh::field_vram(Some(&d.file("sys/7")), Some(&set), &[], &c);
    let a = mesh::build_atlas(&mut m, &v);
    let bytes = mesh::encode(&m, &a);
    dump("chunk39", &bytes);
    eprintln!(
        "chunk/39: {} tiles, atlas {}×{}, {} B payload, {:?}; unowned polys {}",
        a.tiles.len(),
        a.w,
        a.h,
        bytes.len(),
        t.elapsed(),
        m.polys.iter().filter(|p| p.flags & flag::UNOWNED != 0).count()
    );
    assert!(bytes.len() < 4 << 20, "payload stays compact");
    assert!(m.polys.iter().filter(|p| p.textured()).all(|p| p.tile != 0xFFFF));
}

#[test]
fn other_containers_match_the_prd() {
    let Some(d) = disc() else { return };
    let base = d.battle();
    let model = |id: &str, kind: mesh::Container| {
        let b = d.file(id);
        let parts = mesh::model_parts(kind, &b).unwrap_or_else(|| panic!("{id} parts"));
        let mut m = mesh::tmd_mesh(parts.main).unwrap_or_else(|| panic!("{id} TMD"));
        let mut v = base.clone();
        if let Some(t) = parts.tim {
            v.paint_tim(t);
        }
        let a = mesh::build_atlas(&mut m, &v);
        (m, a)
    };
    // PRD 06 §3 table.
    let (k, _) = model("knight/0", mesh::Container::Knight);
    assert_eq!((k.polys.len(), k.verts.len(), k.normals.len()), (658, 534, 746));
    let (pb, _) = model("pb/0", mesh::Container::Pb);
    assert_eq!((pb.polys.len(), pb.verts.len(), pb.normals.len()), (522, 338, 447));
    let (fk, _) = model("sys/6", mesh::Container::Sys6Knight);
    assert_eq!((fk.polys.len(), fk.verts.len(), fk.normals.len()), (234, 285, 291));
    // Every enemy, Baby form and arena exports.
    for n in 2..112 {
        let (m, _) = model(&format!("enemy/{n}"), mesh::Container::Enemy);
        assert!(!m.polys.is_empty(), "enemy/{n}");
    }
    for n in 0..19 {
        model(&format!("pb/{n}"), mesh::Container::Pb);
    }
    for n in 0..48 {
        let (m, a) = model(&format!("bf/{n}"), mesh::Container::Bf);
        assert!(m.polys.iter().filter(|p| p.textured()).all(|p| p.tile != 0xFFFF), "bf/{n}");
        assert!(a.w > 0);
    }
    for id in ["enemy/0", "enemy/1"] {
        let (m, _) = model(id, mesh::Container::Knight);
        assert!(!m.polys.is_empty(), "{id} (KNIGHT02/03)");
    }
}

/// Every actor model the field draws (`ActorLook::drawn`) in all 50 packs, as
/// `render3d_actor` builds it: the Group A TMD in the first pose of its set,
/// a lid lifted on two-part objects, textured through its home chunk's field
/// VRAM plus SYS14. Every character is fully textured; the few that sample a
/// page no file of that context paints (retail: 9 door panels, 6 or 19
/// polygons, on chunks whose neighbour holds the page) show the hatch.
#[test]
fn every_drawn_actor_model_renders() {
    let Some(d) = disc() else { return };
    let (sys2, sys7) = (d.file("sys/2"), d.file("sys/7"));
    let ids = world::texset_ids(&d.exe).unwrap_or_default();
    let (mut models, mut posed, mut lids, mut unowned) = (0, 0, 0, Vec::new());
    for p in 0..50 {
        let b = d.file(&format!("npc/{p}"));
        let pack = lt_core::npc::NpcPack::parse(&b).unwrap_or_else(|| panic!("npc/{p} did not parse"));
        for a in pack.actors.iter().filter(|a| a.slot >= 2) {
            let look = lt_core::npc::ActorLook::of(a);
            if !look.drawn() {
                continue;
            }
            let at = format!("npc/{p} actor {}", a.slot);
            let index = look.model.unwrap() as usize;
            let tmd = pack.model(&b, index).unwrap_or_else(|| panic!("{at}: no model {index}"));
            let mut m = mesh::tmd_mesh(tmd).unwrap_or_else(|| panic!("{at}: model {index} is not a TMD"));
            assert!(!m.polys.is_empty(), "{at}: model {index} has no polygons");
            if let Some(set) = look.poses.map(usize::from) {
                let targets = pack.pose_block(&b, set).and_then(mesh::morph_targets).unwrap_or_else(|| panic!("{at}: pose block {set}"));
                let pose = pack.first_pose(&b, set).unwrap_or_else(|| panic!("{at}: no animation 0 in set {set}"));
                assert!(pose as usize <= targets.len(), "{at}: pose {pose} of {}", targets.len());
                m.verts = mesh::blend(&m.verts, &targets, pose, 256, 0, 0);
                posed += 1;
            }
            if look.kind & 0xF == 3 {
                assert!(m.groups.len() >= 2, "{at}: a two-part object with {} TMD object(s)", m.groups.len());
                mesh::lift_object(&mut m, 1, -0x28);
                lids += 1;
            }
            let home = a.b & 0xFFFF;
            if world::valid_chunk(home) && home < world::CHUNK_COUNT {
                let vram = d.actor_vram(&sys2, &sys7, &ids, home);
                mesh::build_atlas(&mut m, &vram);
                let bad = m.polys.iter().filter(|q| q.flags & flag::UNOWNED != 0).count();
                if bad > 0 {
                    assert!(m.polys.len() <= 32 && look.poses.is_none(), "{at} (chunk {home}): {bad} of {} polygons unowned", m.polys.len());
                    unowned.push(format!("{at} (chunk {home}): {bad} of {}", m.polys.len()));
                }
            }
            models += 1;
        }
    }
    println!("{models} drawn actor models, {posed} posed, {lids} with a lid; unowned texels: {unowned:#?}");
    assert!(models > 1000, "only {models} drawn actors");
}

/// Every Group A model of every retail pack as `render3d_model` draws `npc/n#i`
/// in the model browser (screen 06): `mesh::npc_model` (the first pose of the set
/// its first animated actor gives it, else the base pose) textured through the
/// field VRAM of its first actor's home chunk (else the pack's) plus SYS14; only
/// 10 door panels (6 polygons) sample a page of another chunk. 530 models in the 50 table entries (`npc/17`
/// repeats `npc/0`'s 4); 27 no actor record from slot 2 on uses (the party
/// records' byte 2 is 0 but never read). `npc/7#12` is the purple-robed figure
/// PSXPrev lists in ext NPC08.BIN at 0x138B8: 245 polygons, 102 of them quads
/// (347 triangles), unused, so in its base pose.
#[test]
fn every_npc_group_a_model_renders() {
    let Some(d) = disc() else { return };
    let (sys2, sys7) = (d.file("sys/2"), d.file("sys/7"));
    let ids = world::texset_ids(&d.exe).unwrap_or_default();
    let served = world::pack_table(&d.file("ovr/2"));
    let (mut models, mut posed, mut unused, mut unowned) = (0, 0, Vec::new(), Vec::new());
    for p in 0..50u32 {
        let b = d.file(&format!("npc/{p}"));
        let pack = lt_core::npc::NpcPack::parse(&b).unwrap_or_else(|| panic!("npc/{p} did not parse"));
        let fallback = served.iter().find(|&&(c, q)| u32::from(q) == p && world::valid_chunk(c.into())).map(|&(c, _)| u32::from(c));
        let uses = pack.model_uses(&b);
        assert_eq!(uses.len(), pack.group_a(&b), "npc/{p}");
        for u in &uses {
            let at = format!("npc/{p}#{}", u.index);
            let chunk = u.chunk.or(fallback);
            let vram = match chunk {
                Some(c) => d.actor_vram(&sys2, &sys7, &ids, c),
                None => mesh::field_model_vram(Some(&sys2), Some(&sys7)),
            };
            assert_eq!(lt_core::npc::model_id(&at), Some((p, u.index as usize)));
            let mut m = mesh::npc_model(&pack, &b, u).unwrap_or_else(|| panic!("{at} is not a TMD"));
            assert!(!m.polys.is_empty(), "{at} has no polygons");
            if let Some(set) = u.set {
                let targets = pack.pose_block(&b, set as usize).and_then(mesh::morph_targets).unwrap_or_else(|| panic!("{at}: pose block {set}"));
                assert!(u.pose.is_some_and(|k| k as usize <= targets.len()), "{at}: pose {:?} of {}", u.pose, targets.len());
                posed += 1;
            }
            let a = mesh::build_atlas(&mut m, &vram);
            assert!(m.polys.iter().filter(|q| q.textured()).all(|q| q.tile != 0xFFFF), "{at}: a textured polygon has no atlas tile");
            assert!(a.w > 0 || m.polys.iter().all(|q| !q.textured()), "{at}: empty atlas");
            let bad = m.polys.iter().filter(|q| q.flags & flag::UNOWNED != 0).count();
            if bad > 0 {
                // Door panels sample a neighbour chunk's page; characters are fully textured.
                assert!(m.polys.len() <= 32, "{at} (chunk {chunk:?}): {bad} of {} polygons unowned", m.polys.len());
                unowned.push(format!("{at} (chunk {chunk:?}): {bad} of {}", m.polys.len()));
            }
            if u.actors.is_empty() {
                unused.push(at.clone());
            }
            if at == "npc/7#12" {
                assert_eq!(u.offset, 0x138B8);
                let quads = m.polys.iter().filter(|q| q.flags & flag::QUAD != 0).count();
                assert_eq!((m.polys.len(), quads, m.polys.len() + quads, m.skipped), (245, 102, 347, 0));
                assert_eq!((u.set, chunk), (None, Some(324)), "unused, drawn on the Knight's house VRAM");
                assert_eq!(bad, 0, "npc/7#12 is fully textured by chunk 324");
                dump("npc_7_12", &mesh::encode(&m, &a));
            }
            models += 1;
        }
    }
    println!("{models} Group A models, {posed} posed; unowned texels: {unowned:#?}");
    assert_eq!(models, 530);
    let expect = [
        "npc/3#12", "npc/3#20", "npc/6#7", "npc/7#12", "npc/8#0", "npc/12#0", "npc/12#1", "npc/12#5", "npc/12#8", "npc/12#9", "npc/12#10",
        "npc/12#12", "npc/12#14", "npc/12#16", "npc/13#4", "npc/15#0", "npc/21#6", "npc/21#9", "npc/22#5", "npc/22#8", "npc/25#6",
        "npc/25#9", "npc/27#7", "npc/28#4", "npc/31#5", "npc/34#6", "npc/49#2",
    ];
    assert_eq!(unused, expect);
}
