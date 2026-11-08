//! A character file from the Furret conversion (Blender → JSON): a prelit
//! model, six poses laid out like the cats' set (npc/1 set 4: idle, talk,
//! walk, with a four-step trot), and its eye texture for the blank corner of sys/7.
//! `cargo run --release --example furret_make -- <furret_final.json> <disc.bin> <out.ltchar>`
use lt_core::edit::anim::{write_poses, Pose};
use lt_core::edit::character::Character;
use lt_core::edit::charfile::{CharFile, VramPatch};
use lt_core::edit::meshin::{Corner, Face, InMesh, Material};
use lt_core::edit::model::{encode_mapped, ImportOptions, Lighting};
use lt_core::edit::tmdcodec::Tmd;
use lt_core::npc::NpcPack;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};
use serde_json::Value;

fn f(v: &Value) -> f32 {
    v.as_f64().unwrap() as f32
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let j: Value = serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
    let image = DiscImage::open(&a[2]).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };

    let p3 = |v: &Value| -> [f32; 3] { [f(&v[0]), f(&v[1]), f(&v[2])] };
    let eye = &j["eye"];
    let mut mesh = InMesh { indexed: true, ..Default::default() };
    // Vertices nose to tail (front is -z): a pose that moves only the head or only the tail
    // is then one short run of deltas.
    let src_pos: Vec<[f32; 3]> = j["positions"].as_array().unwrap().iter().map(p3).collect();
    let mut order: Vec<usize> = (0..src_pos.len()).collect();
    order.sort_by(|&x, &y| src_pos[x][2].total_cmp(&src_pos[y][2]));
    let mut inv = vec![0usize; order.len()];
    for (k, &o) in order.iter().enumerate() {
        inv[o] = k;
    }
    mesh.positions = order.iter().map(|&o| src_pos[o]).collect();
    mesh.materials.push(Material { name: format!("t{:03x}_c{:04x}", eye["tsb"].as_u64().unwrap(), eye["cba"].as_u64().unwrap()), textured: true, gouraud: true, ..Material::named("") });
    for face in j["faces"].as_array().unwrap() {
        let is_eye = face["eye"].as_bool().unwrap();
        let corners = face["corners"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| Corner {
                pos: inv[c["pos"].as_u64().unwrap() as usize],
                normal: None,
                uv: is_eye.then(|| [f(&c["uv"][0]), f(&c["uv"][1])]),
                color: Some(if is_eye { [128.0 / 255.0; 3] } else { p3(&c["color"]) }),
            })
            .collect();
        mesh.faces.push(Face { corners, material: is_eye.then_some(0) });
    }
    let opt = ImportOptions { scale: 1.0, lighting: Lighting::Prelit };
    let (obj, report, vid) = encode_mapped(&mesh, None, &opt, None).unwrap();
    eprintln!("model: {:?}", report.classes);
    eprintln!("sizes: {:?}, {} tris {} quads, clamped uvs {}", report.sizes, report.triangles, report.quads, report.clamped_uvs);
    let tmd = Tmd::new(vec![obj.clone()]);
    let model = tmd.write();

    // Poses: every one after the rest pose, as deltas over the encoded vertices (game axes: Y down).
    let q = |p: [f32; 3]| [p[0].round() as i32, (-p[1]).round() as i32, p[2].round() as i32];
    let rest: Vec<[i32; 3]> = mesh.positions.iter().map(|&p| q(p)).collect();
    let mut poses = Vec::new();
    for pose in j["poses"].as_array().unwrap().iter().skip(1) {
        let mut dense = vec![[0i16; 3]; obj.verts.len()];
        for (old, v) in pose["positions"].as_array().unwrap().iter().enumerate() {
            let (i, p) = (inv[old], q(p3(v)));
            dense[vid[i]] = [(p[0] - rest[i][0]) as i16, (p[1] - rest[i][1]) as i16, (p[2] - rest[i][2]) as i16];
        }
        // Idle and talk move the tail or the head: drop the few-unit drift far from it, so
        // their one record stays short (the pack has to hold every delta).
        if pose["name"] == "idle" || pose["name"] == "talk" {
            for d in dense.iter_mut().filter(|d| d.iter().all(|c| c.abs() < 3)) {
                *d = [0; 3];
            }
        }
        let pz = Pose::from_dense(&dense);
        eprintln!("pose {}: vertices {}..{}", pose["name"], pz.first, pz.end());
        poses.push(pz);
    }
    let block = write_poses(&poses);

    // The cats' animations (npc/1 set 4: idle, talk, walk) with a four-step trot: an entry
    // lasts ceil(256 / speed) frames, so 26 makes the cycle 40 frames (the FBX's is 46 at 30 fps).
    // Shadow and kind are the cats' too.
    let cats = wd.file("npc/1").unwrap();
    let pack = NpcPack::parse(&cats).unwrap();
    let cat = pack.actors.iter().find(|x| x.slot == 5).unwrap();
    let anims = vec![vec![(0, 13), (1, 13)], vec![(0, 24), (1, 24), (2, 24)], vec![(3, 26), (4, 26), (5, 26), (6, 26)]];
    let looks = (cat.d & 0xFFFF) as u16;
    eprintln!("anims {anims:?} looks {looks:#06x}");

    // Eye texels (4 bpp, 4 per halfword) and its CLUT, both inside sys/7's image.
    let (u, v, w, h) = (eye["u"].as_u64().unwrap() as u16, eye["v"].as_u64().unwrap() as u16, eye["w"].as_u64().unwrap() as u16, eye["h"].as_u64().unwrap() as u16);
    let idx: Vec<u16> = eye["indices"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u16).collect();
    let mut texels = Vec::new();
    for row in 0..h as usize {
        for hw in 0..(w / 4) as usize {
            let at = row * w as usize + hw * 4;
            texels.push(idx[at] | idx[at + 1] << 4 | idx[at + 2] << 8 | idx[at + 3] << 12);
        }
    }
    let page_x = (eye["tsb"].as_u64().unwrap() as u16 & 0xF) * 64;
    let cba = eye["cba"].as_u64().unwrap() as u16;
    let clut: Vec<u16> = eye["clut"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u16).collect();
    let vram = vec![
        VramPatch { x: page_x + u / 4, y: v, w: w / 4, h, data: texels },
        VramPatch { x: (cba & 0x3F) * 16, y: cba >> 6, w: 16, h: 1, data: clut },
    ];

    let character = Character { label: "Furret".into(), model, poses: Some(block), anims, looks };
    eprintln!("built {} B of a 0x5A00 packet slot", character.built());
    let file = CharFile { character, vram };
    let bytes = file.write();
    assert_eq!(CharFile::read(&bytes).unwrap(), file);
    std::fs::write(&a[3], &bytes).unwrap();
    eprintln!("wrote {} ({} B)", a[3], bytes.len());
}
