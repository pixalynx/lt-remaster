//! Model export and import (`edit::model`) on every character model the
//! disc holds: toys, enemies, knights, Baby forms and the field Knight and
//! Baby. Each exports to OBJ, imports back into its own file and draws the
//! same (classes, corners, UVs, bindings, colours, normals), keeps its poses
//! and stays inside the loader's budgets. NPC Group A models export with
//! their pages from the field VRAM.
//! Opt in with `LT_TEST_DISC=… cargo test --release -p lt-core --test model`.

use std::collections::{BTreeMap, BTreeSet};

use lt_core::edit::meshin::parse_obj;
use lt_core::edit::model::{self, differences, ImportOptions};
use lt_core::edit::tmdcodec::Tmd;
use lt_core::mesh::{self, model_parts};
use lt_core::npc::NpcPack;
use lt_core::world;
use lt_core::{DiscImage, FileTable, PsxExe};

fn ids() -> Vec<String> {
    let mut v: Vec<String> = (0..73).map(|i| format!("toy/{i}")).collect();
    v.extend((0..112).map(|i| format!("enemy/{i}")));
    v.extend((0..19).map(|i| format!("pb/{i}")));
    v.extend(["knight/0", "sys/6#knight", "sys/6#baby"].map(String::from));
    v
}

#[test]
fn every_character_model_survives_export_and_import() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let read = |id: &str| table.by_id(id).and_then(|e| image.read_sectors(e.extent.lba, e.extent.sectors).ok());
    let (mut done, mut faces) = (0, 0u32);
    for id in ids() {
        let (file, kind) = model::source(&id).unwrap();
        let Some(b) = read(&file) else { continue };
        if model_parts(kind, &b).is_none() {
            continue;
        }
        let (obj, _mtl, _bind) = model::export(kind, &b, "m").unwrap_or_else(|e| panic!("{id}: {e}"));
        let mesh = parse_obj(&obj, None).unwrap_or_else(|e| panic!("{id}: {e}"));
        let (out, rep) = model::replace(kind, &b, &mesh, &ImportOptions::default()).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(rep.poses_kept, "{id}: vertices {} → {}", rep.report.vertices_before, rep.report.sizes.vertices);
        let old = Tmd::parse(model_parts(kind, &b).unwrap().main).unwrap();
        let parts = model_parts(kind, &out).unwrap_or_else(|| panic!("{id}: rebuilt file has no sections"));
        let new = Tmd::parse(parts.main).unwrap_or_else(|e| panic!("{id}: {e}"));
        let d = differences(&old.objects[0], &new.objects[0]);
        assert!(d.none(), "{id}: {d:?} (clamped UVs {})", rep.report.clamped_uvs);
        faces += old.objects[0].prims.len() as u32;
        // Everything but the model and its padding is untouched.
        let (a, z) = (model_parts(kind, &b).unwrap(), parts);
        assert_eq!(a.poses.map(|p| lt_core::pose::parse(p).map(|x| x.len)), z.poses.map(|p| lt_core::pose::parse(p).map(|x| x.len)), "{id}: poses");
        if let (Some(t0), Some(t1)) = (a.tim, z.tim) {
            assert_eq!(t0[..t1.len().min(t0.len())], t1[..t1.len().min(t0.len())], "{id}: TIM");
        }
        for bud in &rep.budgets {
            assert!(bud.used <= bud.cap, "{id}: {bud:?}");
        }
        done += 1;
    }
    eprintln!("{done} models, {faces} faces");
    assert!(done > 190, "{done}");
}

/// A box made from scratch replaces toy/0's model: new
/// topology, so every pose becomes a record that moves nothing and the toy holds the box
/// in every animation; the box's UVs land on the toy's own TIM.
#[test]
fn a_new_mesh_replaces_a_toy_with_neutral_poses() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let e = table.by_id("toy/0").unwrap();
    let b = image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
    let (_, bind) = model::model_of(model::Container::Toy, &b).unwrap();
    let bind = bind.unwrap();
    let (tsb, cba) = (bind.tsb, bind.cba);
    let mut src = String::from("mtllib none.mtl\n");
    for (x, y, z) in [(-1, 0, -1), (1, 0, -1), (1, 2, -1), (-1, 2, -1), (-1, 0, 1), (1, 0, 1), (1, 2, 1), (-1, 2, 1)] {
        src += &format!("v {} {} {}\n", x * 200, y * 200, z * 200);
    }
    src += "vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nvn 0 0 -1\nvn 0 0 1\nvn -1 0 0\nvn 1 0 0\nvn 0 1 0\nvn 0 -1 0\n";
    src += &format!("usemtl t{tsb:03x}_c{cba:04x}\n");
    for (f, n) in [([1, 4, 3, 2], 1), ([5, 6, 7, 8], 2), ([1, 5, 8, 4], 3), ([2, 3, 7, 6], 4), ([4, 8, 7, 3], 5), ([1, 2, 6, 5], 6)] {
        src += &format!("f {}/1/{n} {}/2/{n} {}/3/{n} {}/4/{n}\n", f[0], f[1], f[2], f[3]);
    }
    let mesh = parse_obj(&src, None).unwrap();
    let (out, rep) = model::replace(model::Container::Toy, &b, &mesh, &ImportOptions::default()).unwrap();
    assert!(!rep.poses_kept);
    assert_eq!(rep.report.quads, 6);
    assert_eq!(rep.report.classes.get("2C00"), Some(&6), "{:?}", rep.report.classes);
    let parts = model_parts(model::Container::Toy, &out).unwrap();
    let tmd = Tmd::parse(parts.main).unwrap();
    assert_eq!(tmd.objects[0].verts.len(), 8);
    let poses = lt_core::pose::parse(parts.poses.unwrap()).unwrap();
    let before = lt_core::pose::parse(model_parts(model::Container::Toy, &b).unwrap().poses.unwrap()).unwrap();
    assert_eq!(poses.poses.len(), before.poses.len());
    // One zero delta each: the engine's delta loop runs at least once.
    assert!(poses.poses.iter().all(|p| p.count == 1 && p.max_delta == 0 && p.first_vertex == 0));
    // Same TIM and sections 1–6 byte for byte.
    let (s0, s1) = (lt_core::container::sections(&b, 8).unwrap(), lt_core::container::sections(&out, 8).unwrap());
    for i in 1..7 {
        let a = &b[s0[i].offset as usize..(s0[i].offset + s0[i].len) as usize];
        let z = &out[s1[i].offset as usize..(s1[i].offset + s1[i].len) as usize];
        assert_eq!(a, z, "section {i}");
    }
    assert!(out.len() < b.len());
}

/// Polygons the 3D view drops from Group A model `i` of `pack`.
fn m_skipped(b: &[u8], pack: &NpcPack, i: usize) -> u32 {
    mesh::tmd_mesh(pack.model(b, i).unwrap()).unwrap().skipped
}

/// The field VRAM `render3d_model` draws NPC models with on `chunk`: SYS08, the
/// chunk's `fe/k` set, sibling interior pages, the chunk, then SYS14.
fn actor_vram(read: &dyn Fn(&str) -> Option<Vec<u8>>, world_chunk: &dyn Fn(u32) -> Vec<u8>, ids: &[u16], chunk: u32) -> mesh::OwnedVram {
    let set = read(&format!("fe/{}", world::texset_for(ids, chunk)));
    let siblings: Vec<Vec<u8>> = if chunk >= world::GRID_CHUNKS {
        world::interior_group(chunk).into_iter().filter(|&s| s != chunk && world::valid_chunk(s)).map(world_chunk).collect()
    } else {
        Vec::new()
    };
    let mut v = mesh::field_vram(read("sys/7").as_deref(), set.as_deref(), &siblings, &world_chunk(chunk));
    v.paint_tim(&read("sys/2").unwrap());
    v
}

/// Every NPC Group A model (`npc/n#i`, 530 in the 50 table entries) exports
/// through `export_paged` with the VRAM the 3D view draws it with: its chunk's
/// field VRAM plus SYS14, else SYS14 + SYS08 (`npc/49`).
/// - The OBJ keeps every vertex in TMD order, so pose blocks line up, and one
///   face per polygon.
/// - Every textured material maps the PNG of its binding (`m.png` for the
///   dominant one); the UVs are page texels; each PNG shows the texels the 3D
///   view's atlas does. Untextured models get no PNG.
/// - `npc/7#12` (prelit G3/G4/F3/F4, vertex colours) writes OBJ + MTL only;
///   `npc/6#8`, the millstone, maps chunk 77's page with no unowned texel.
#[test]
fn every_npc_group_a_model_exports() {
    let Some(path) = std::env::var_os("LT_TEST_DISC") else { return };
    let image = DiscImage::open(path).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let read = |id: &str| table.by_id(id).and_then(|e| image.read_sectors(e.extent.lba, e.extent.sectors).ok());
    let world_lba = table.by_id("world").unwrap().extent.lba;
    let world_chunk = |id: u32| image.read_sectors(world_lba + id * world::CHUNK_SECTORS, world::CHUNK_SECTORS).unwrap();
    let ids = world::texset_ids(&exe).unwrap_or_default();
    let served = world::pack_table(&read("ovr/2").unwrap());
    let field = mesh::field_model_vram(read("sys/2").as_deref(), read("sys/7").as_deref());
    let mut cache: BTreeMap<u32, mesh::OwnedVram> = BTreeMap::new();
    let (mut models, mut textured, mut pngs, mut most, mut left) = (0, 0, 0, (0, String::new()), Vec::new());
    for p in 0..50u32 {
        let b = read(&format!("npc/{p}")).unwrap();
        let pack = NpcPack::parse(&b).unwrap_or_else(|| panic!("npc/{p} did not parse"));
        let fallback = served.iter().find(|&&(c, q)| u32::from(q) == p && world::valid_chunk(c.into())).map(|&(c, _)| u32::from(c));
        for u in pack.model_uses(&b) {
            let at = format!("npc/{p}#{}", u.index);
            let chunk = u.chunk.or(fallback);
            let vram = match chunk {
                Some(c) => cache.entry(c).or_insert_with(|| actor_vram(&read, &world_chunk, &ids, c)),
                None => &field,
            };
            let tmd = Tmd::parse(pack.model(&b, u.index as usize).unwrap()).unwrap_or_else(|e| panic!("{at}: {e}"));
            let model::PagedExport { obj, mtl, pages, skipped, no_normals } = model::export_paged(&tmd, vram, "m").unwrap_or_else(|e| panic!("{at}: {e}"));
            let mesh_in = parse_obj(&obj, None).unwrap_or_else(|e| panic!("{at}: {e}"));

            // Vertex identity: every vertex, in TMD order, Y negated.
            let verts: Vec<[f32; 3]> = tmd.objects.iter().flat_map(|o| o.verts.iter().map(|v| [v[0] as f32, -(v[1] as f32), v[2] as f32])).collect();
            assert_eq!(mesh_in.positions, verts, "{at}: vertices");
            // One face per polygon, but for the ones the OBJ says it left out.
            let left_out: BTreeSet<(usize, usize)> = obj
                .lines()
                .filter_map(|l| {
                    let w: Vec<&str> = l.strip_prefix("# left out: polygon ")?.split_whitespace().collect();
                    Some((w[2].strip_prefix("object")?.parse().ok()?, w[0].parse().ok()?))
                })
                .collect();
            assert_eq!(left_out.len() as u32, skipped, "{at}");
            let all = tmd.objects.iter().enumerate().flat_map(|(oi, o)| o.prims.iter().enumerate().map(move |(pi, q)| ((oi, pi), q)));
            let prims: Vec<_> = all.filter(|(k, _)| !left_out.contains(k)).map(|(_, q)| q).collect();
            assert_eq!(mesh_in.faces.len(), prims.len(), "{at}: faces");
            assert_eq!(skipped, m_skipped(&b, &pack, u.index as usize), "{at}: the 3D view drops the same polygons");
            assert_eq!(obj.lines().filter(|l| l.starts_with("# no normals: ")).count() as u32, no_normals, "{at}");

            // Each textured material maps its binding's PNG; nothing else maps one.
            let mut maps: BTreeMap<String, String> = BTreeMap::new();
            let mut cur = String::new();
            for l in mtl.lines() {
                match l.split_whitespace().collect::<Vec<_>>()[..] {
                    ["newmtl", m] => cur = m.to_string(),
                    ["map_Kd", f] => {
                        maps.insert(cur.clone(), f.to_string());
                    }
                    _ => {}
                }
            }
            let files: BTreeMap<&str, &model::PagePng> = pages.iter().map(|g| (g.file.as_str(), g)).collect();
            let bindings: BTreeSet<(u16, u16)> = prims.iter().filter(|q| q.mode & 0x04 != 0).map(|q| ((q.words[1] >> 16) as u16, (q.words[0] >> 16) as u16)).collect();
            assert_eq!(pages.len(), bindings.len(), "{at}: one PNG per binding");
            assert_eq!(pages.first().map(|g| g.file.as_str()), (!bindings.is_empty()).then_some("m.png"), "{at}");
            assert_eq!(pages.first().map(|g| (g.tsb, g.cba)), model::dominant_binding(&tmd), "{at}: m.png is the dominant binding");
            for m in &mesh_in.materials {
                match (m.textured, maps.get(&m.name)) {
                    (true, Some(file)) => {
                        let g = files.get(file.as_str()).unwrap_or_else(|| panic!("{at}: {} maps {file}, not written", m.name));
                        assert!(m.name.starts_with(&format!("t{:03x}_c{:04x}", g.tsb, g.cba)), "{at}: {} maps {file}", m.name);
                    }
                    (true, None) => panic!("{at}: {} maps no PNG", m.name),
                    (false, Some(file)) => panic!("{at}: untextured {} maps {file}", m.name),
                    (false, None) => {}
                }
            }

            // UVs are page texels: vt = ((u + .5) / 256, 1 − (v + .5) / 256).
            for (k, (f, q)) in mesh_in.faces.iter().zip(&prims).enumerate() {
                if q.mode & 0x04 == 0 {
                    assert!(f.corners.iter().all(|c| c.uv.is_none()), "{at}: face {k} has UVs");
                    continue;
                }
                let order: &[usize] = if f.corners.len() == 4 { &[0, 1, 3, 2] } else { &[0, 1, 2] };
                for (c, &o) in f.corners.iter().zip(order) {
                    let uv = c.uv.unwrap_or_else(|| panic!("{at}: face {k} has no UVs"));
                    let (u, v) = ((uv[0] * 256.0 - 0.5).round() as u32, ((1.0 - uv[1]) * 256.0 - 0.5).round() as u32);
                    assert_eq!((u, v), (q.words[o] & 0xFF, (q.words[o] >> 8) & 0xFF), "{at}: face {k} corner {o}");
                }
            }

            // Each PNG holds the texels the 3D view's atlas shows: colours where
            // owned, clear where index 0 or unowned.
            let mut m = mesh::tmd_mesh(pack.model(&b, u.index as usize).unwrap()).unwrap();
            let atlas = mesh::build_atlas(&mut m, vram);
            let decoded: BTreeMap<(u16, u16), Vec<u8>> = pages
                .iter()
                .map(|g| {
                    let mut d = png::Decoder::new(std::io::Cursor::new(&g.png)).read_info().unwrap();
                    let mut buf = vec![0; d.output_buffer_size().unwrap()];
                    let info = d.next_frame(&mut buf).unwrap();
                    assert_eq!((info.width, info.height, info.color_type), (256, 256, png::ColorType::Rgba), "{at}: {}", g.file);
                    let key = (g.tsb & 0x19F, if (g.tsb >> 7) & 3 >= 2 { 0 } else { g.cba });
                    (key, buf)
                })
                .collect();
            for t in &atlas.tiles {
                let page = decoded.get(&(t.tpage, t.clut)).unwrap_or_else(|| panic!("{at}: atlas tile {t:?} has no PNG"));
                for y in 0..t.h as usize {
                    for x in 0..t.w as usize {
                        let a = &atlas.rgba[((t.y as usize + y) * atlas.w as usize + t.x as usize + x) * 4..][..4];
                        let png = &page[((t.v0 as usize + y) * 256 + t.u0 as usize + x) * 4..][..4];
                        let want = if matches!(a[3], 0 | 64) { [0; 4] } else { [a[0], a[1], a[2], 255] };
                        assert_eq!(png, want, "{at}: texel ({}, {}) of {t:?}", t.u0 as usize + x, t.v0 as usize + y);
                    }
                }
            }

            if at == "npc/7#12" {
                assert_eq!((u.offset, prims.len(), pages.len()), (0x138B8, 245, 0), "untextured: no PNG");
                assert!(maps.is_empty() && obj.lines().any(|l| l.starts_with("v ") && l.split_whitespace().count() == 7), "vertex colours, no map_Kd");
            }
            if at == "npc/6#8" {
                assert_eq!(chunk, Some(77));
                assert!(!pages.is_empty() && maps.values().all(|f| files.contains_key(f.as_str())));
                assert!(m.polys.iter().all(|q| q.flags & mesh::flag::UNOWNED == 0), "the millstone is fully textured on chunk 77");
            }
            if let Some(dir) = std::env::var_os("LT_TEST_OUT").filter(|_| matches!(at.as_str(), "npc/7#12" | "npc/6#8")) {
                let dir = std::path::PathBuf::from(dir).join(at.replace(['/', '#'], "_"));
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("m.obj"), &obj).unwrap();
                std::fs::write(dir.join("m.mtl"), &mtl).unwrap();
                for g in &pages {
                    std::fs::write(dir.join(&g.file), &g.png).unwrap();
                }
            }
            if skipped + no_normals > 0 {
                left.push(format!("{at}: {skipped} left out, {no_normals} without normals"));
            }
            textured += !pages.is_empty() as u32;
            pngs += pages.len();
            if pages.len() > most.0 {
                most = (pages.len(), at.clone());
            }
            models += 1;
        }
    }
    println!("{models} NPC models exported; {textured} textured, {pngs} page PNGs, most {} ({}); polygons left out: {left:?}", most.0, most.1);
    assert_eq!((models, textured, pngs), (530, 124, 132));
    // npc/12#0 and npc/13#0 declare 222 normals past the end of their TMD; the 3D view lights those faces with zero normals.
    assert_eq!(left, ["npc/12#0: 0 left out, 6 without normals", "npc/13#0: 0 left out, 6 without normals"]);
}
