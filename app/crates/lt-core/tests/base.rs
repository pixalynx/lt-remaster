//! Bases (research 06 phase 2) against the user's retail image, which is only
//! read: a hand-made base (retail plus one appended area, chunk 1328, so its
//! new areas start at 1329) is packaged, materialised, built on, and its
//! project's patch is made against retail. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test base`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use lt_core::base::{self, BasePackage};
use lt_core::build::{self, BuildRequest, DiscInput, PlanOptions, Source, Staged, Verdict};
use lt_core::cdrom;
use lt_core::edit::chunk::Chunk;
use lt_core::project::{DiscRef, Project};
use lt_core::world::CHUNK_SECTORS;
use lt_core::{bps, DiscImage, FileTable, PsxExe};

fn out_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("LT_TEST_OUT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("lt-base-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn staged(id: &str, bytes: Vec<u8>) -> Staged {
    Staged { id: id.into(), bytes: Arc::new(bytes), base_sha256: None }
}

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

impl Disc {
    fn open(path: &Path) -> Self {
        let image = DiscImage::open(path).unwrap();
        let exe = PsxExe::from_disc(&image).unwrap();
        let table = FileTable::parse(&exe, image.sectors()).unwrap();
        Self { image, exe, table }
    }
    fn src(&self) -> Source<'_> {
        Source { image: &self.image, exe: &self.exe, table: &self.table }
    }
}

/// Retail with chunk 326's bytes appended as chunk 1328 (after the blank gap
/// up to its fixed LBA), as a base's own area would be.
fn hand_made_base(retail: &Path, out: &Path) -> (u32, Vec<u8>) {
    let r = Disc::open(retail);
    let world = r.table.by_id("world").unwrap().extent.lba;
    let chunk = r.image.read_sectors(world + 326 * CHUNK_SECTORS, CHUNK_SECTORS).unwrap();
    let at = world + 1328 * CHUNK_SECTORS;
    fs::copy(retail, out).unwrap();
    let mut f = fs::OpenOptions::new().append(true).open(out).unwrap();
    for lba in r.image.sectors()..at {
        f.write_all(&cdrom::postgap(lba)).unwrap();
    }
    for k in 0..CHUNK_SECTORS {
        let mut raw = cdrom::blank(at + k, if k == CHUNK_SECTORS - 1 { cdrom::SUB_LAST } else { cdrom::SUB_DATA });
        cdrom::encode_form1(&mut raw, &chunk[k as usize * 2048..(k as usize + 1) * 2048]);
        f.write_all(&raw).unwrap();
    }
    (at, chunk)
}

#[test]
fn a_base_packages_materialises_and_builds_against_retail() {
    let Some(retail) = std::env::var_os("LT_TEST_DISC").map(PathBuf::from) else { return };
    let dir = out_dir("synthetic");
    let cancel = AtomicBool::new(false);

    // 1. The base image and its package.
    let img = dir.join("hand-made.bin");
    let (at, chunk) = hand_made_base(&retail, &img);
    assert_eq!(at, 186_011);
    let extras = dir.join("extras");
    fs::create_dir_all(&extras).unwrap();
    fs::write(extras.join("flags.json"), r#"{"version":1,"flags":{"0x0EE":{"name":"base_intro_seen","summary":"The base's opening scene ran","confidence":"confirmed"},"0x0EF":{"name":"not a name"}}}"#).unwrap();
    let mut m = base::manifest_template("test-base", "0.1.0");
    m.name = Some("Test base".into());
    m.layout.new_area_first = Some(1329);
    m.reserved.flags = vec!["0x120-0x12F".into()];
    m.catalogues.flags = Some("flags.json".into());
    let pkg = base::make_package(&retail, &img, &dir.join("package"), m, Some(&extras), &cancel).unwrap();
    let pm = &pkg.manifest;
    assert_eq!((pm.source.crc32.as_str(), pm.source.sectors, pm.new_area_first()), ("cdc72c62", 185_985, 1329));
    // @LT-64/1: the package always says where its new areas start, even when the manifest left it
    // out: past the area the hand-made image already holds (1328), so 1329.
    let plain = base::make_package(&retail, &img, &dir.join("package-default"), base::manifest_template("plain-base", "0.1.0"), None, &cancel).unwrap();
    assert_eq!(plain.manifest.layout.new_area_first, Some(1329), "written, not left to the 1328 default");
    let bps_len = fs::metadata(pkg.file(&pm.patch.bps)).unwrap().len();
    // The gap and the area are stored whole: 151 sectors, about 355 KB.
    assert!(bps_len < 400_000, "base patch is {bps_len} bytes");
    let reread = BasePackage::open(&dir.join("package")).unwrap();
    assert_eq!(reread.manifest, *pm);

    // 2. Materialised once, then from the cache.
    let cache = dir.join("cache");
    let t = Instant::now();
    let mat = base::materialise(&pkg, &retail, &cache, &cancel, |_, _| {}).unwrap();
    eprintln!("materialised {} ({} sectors) in {:?}", mat.tag, mat.sectors, t.elapsed());
    assert_eq!((mat.sectors, mat.sha1.as_str()), (at + CHUNK_SECTORS, pm.patch.target_sha1.as_str()));
    assert!(mat.image.starts_with(base::cache_dir(&cache, pm)));
    assert!(fs::read_to_string(&mat.cue).unwrap().starts_with("FILE \"base.bin\" BINARY"));
    let t = Instant::now();
    assert_eq!(base::materialise(&pkg, &retail, &cache, &cancel, |_, _| {}).unwrap(), mat);
    assert!(t.elapsed().as_millis() < 500, "a cached base opens without re-applying");
    assert_eq!(base::find_cached(&cache, &mat.crc32.to_uppercase()), Some(mat.clone()));
    assert_eq!(base::find_cached(&cache, "cdc72c62"), None);
    // The patch only applies to retail.
    let e = base::materialise(&pkg, &img, &dir.join("cache2"), &cancel, |_, _| {}).unwrap_err();
    assert_eq!(e.code, "BASE_WRONG_RETAIL", "{e}");

    // 3. A project on the base carries the package.
    let disc = DiscRef {
        title: "Guardian's Crusade (USA)".into(),
        serial: "SLUS-00811".into(),
        verified: "redump".into(),
        fingerprint: None,
        hint_path: retail.display().to_string(),
    };
    let mut p = Project::create(&dir, "On a base", disc, Some(retail.display().to_string())).unwrap();
    assert_eq!(p.new_area_first(), 1328);
    p.set_base(&pkg).unwrap();
    let p = Project::open(&p.root).unwrap();
    assert_eq!((p.file.base.as_ref().unwrap().tag(), p.new_area_first()), ("test-base@0.1.0".to_string(), 1329));
    let copy = p.base_package().unwrap().unwrap();
    assert!(copy.file("flags.json").is_file() && copy.file(&copy.manifest.patch.bps).is_file());
    // Its flag names seed the project's catalogue; its reserved ids join the engine profile.
    let cat = lt_core::catalogue::from_base(&copy.file("flags.json")).unwrap();
    assert_eq!((cat.base, cat.flags.keys().map(String::as_str).collect::<Vec<_>>()), (lt_core::catalogue::Base::Empty, vec!["0x0EE"]));
    let prof = lt_core::engine::retail().with_base(&copy.manifest);
    assert!(prof.flag(0x125).unwrap().why.contains("test-base@0.1.0"));
    assert_eq!(prof.flags.len(), lt_core::engine::retail().flags.len() + 1);

    // 4. Planning on the base: chunk 1328 is the base's own area, read from the
    // image and edited in place; the project's first new area is 1329.
    let b = Disc::open(&mat.image);
    let src = b.src();
    assert_eq!(src.retail("chunk/1328").unwrap(), chunk);
    let mut edited = chunk.clone();
    edited[0x40] ^= 0x01;
    let area = Chunk::new_area(1329).serialize().unwrap();
    let files = [staged("chunk/1328", edited.clone()), staged("chunk/1329", area.clone())];
    let opts = PlanOptions { postgap: true, new_area_first: p.new_area_first() };
    let plan = build::plan(&src, &files, |_| true, opts);
    assert!(!plan.blocked_included, "{:?}", plan.items.iter().map(|i| (&i.id, &i.refusal)).collect::<Vec<_>>());
    let i1328 = plan.items.iter().find(|i| i.id == "chunk/1328").unwrap();
    assert_eq!((i1328.verdict, i1328.lba, i1328.new_lba), (Verdict::InPlace, at, None));
    // Numbered as on retail, the project's first area would land on the base's.
    let taken = build::plan(&src, &files[..1], |_| true, PlanOptions { postgap: true, ..Default::default() });
    assert_eq!(taken.items[0].refusal.as_ref().map(|e| e.code.as_str()), Some("BUILD_AREA_TAKEN"));
    // Ids below newAreaFirst that the base doesn't have are reserved.
    let reserved = build::plan(&src, &[staged("chunk/1350", Chunk::new_area(1350).serialize().unwrap())], |_| true, PlanOptions { postgap: true, new_area_first: 1400 });
    assert_eq!(reserved.items[0].refusal.as_ref().map(|e| e.code.as_str()), Some("BUILD_AREA_RESERVED"));

    // 5. The build: image from the base, BPS against retail.
    let retail_input = DiscInput { path: retail.display().to_string(), sectors: 185_985, crc32: Some("cdc72c62".into()), sha1: None, verified: true };
    let base_input = DiscInput { path: mat.image.display().to_string(), sectors: mat.sectors, crc32: Some(mat.crc32.clone()), sha1: Some(mat.sha1.clone()), verified: false };
    let req = BuildRequest {
        name: "on a base".into(),
        out_dir: None,
        excluded: vec![],
        image: true,
        bps: true,
        postgap: true,
        new_area_first: Some(p.new_area_first()),
        bps_source: Some(retail_input),
    };
    let rec = build::run(&p, &src, base_input.clone(), &files, &req, &cancel, &mut |_| {});
    assert_eq!(rec.status, "built", "{:?}", rec.error);
    let out = rec.result.as_ref().unwrap();
    let bin = PathBuf::from(&out.bin.as_ref().unwrap().path);
    let built = DiscImage::open(&bin).unwrap();
    assert_eq!(built.read_sectors(at, CHUNK_SECTORS).unwrap(), edited);
    assert_eq!(built.read_file_bytes(at + CHUNK_SECTORS, 0, area.len()).unwrap(), area);
    let patch = fs::read(&out.bps.as_ref().unwrap().path).unwrap();
    let info = bps::inspect(&patch).unwrap();
    assert_eq!((info.source_crc32.as_str(), info.source_size), ("cdc72c62", 185_985 * 2352));
    assert!(info.metadata.contains("\"base\""), "{}", info.metadata);
    // The base's area and the new one are stored; the gap and postgap copy retail's blank last sector.
    eprintln!("retail patch: {} bytes", patch.len());
    assert!(patch.len() < 700_000, "{} bytes", patch.len());
    assert!(fs::read_to_string(out.readme.as_ref().unwrap()).unwrap().contains("cdc72c62"));
    let applied = bps::apply(&patch, &fs::read(&retail).unwrap()).unwrap();
    assert!(applied == fs::read(&bin).unwrap(), "the retail patch doesn't reproduce the build");

    // Without a BPS source the base build can't make a patch (the base isn't Redump).
    let req2 = BuildRequest { name: "no retail".into(), bps_source: None, ..req };
    let rec2 = build::run(&p, &src, base_input, &files, &req2, &cancel, &mut |_| {});
    assert_eq!(rec2.error.as_ref().map(|e| e.code.as_str()), Some("BUILD_BPS_UNVERIFIED"));
    fs::remove_dir_all(&dir).unwrap();
}

/// A real base package (`LT_TEST_BASE=<package dir>`, e.g. HackerGC's from
/// `examples/make_hackergc.rs`): it materialises against retail, its image
/// reads as the game, and a blank base's boot pack has the starter scene.
#[test]
fn a_real_base_package_materialises() {
    let (Some(retail), Some(dir)) = (std::env::var_os("LT_TEST_DISC").map(PathBuf::from), std::env::var_os("LT_TEST_BASE").map(PathBuf::from)) else { return };
    let pkg = BasePackage::open(&dir).unwrap();
    let out = out_dir("real");
    let mat = base::materialise(&pkg, &retail, &out, &AtomicBool::new(false), |_, _| {}).unwrap();
    assert_eq!(mat.sha1, pkg.manifest.patch.target_sha1);
    let b = Disc::open(&mat.image);
    let npc7 = b.src().retail("npc/7").unwrap();
    // The starter scene and the blank slate are additions relative to retail's pack.
    let retail7 = Disc::open(&retail).src().retail("npc/7").unwrap();
    let starter = lt_core::edit::blank::find_starter(&retail7, &npc7).unwrap();
    eprintln!("{}: {} sectors, newAreaFirst {}, starter scene {:?}", mat.tag, mat.sectors, pkg.manifest.new_area_first(), starter.as_ref().map(|s| s.slot));
    if pkg.manifest.id.contains("blank") {
        // A blank base releases retail's pack swaps and chunk effects (and, shift-safe, the switch checks).
        let prof = lt_core::engine::retail().with_base(&pkg.manifest);
        assert!(lt_core::engine::retail().chunk(414).is_some() && prof.chunk(414).is_none(), "414's rain is released");
        assert!(prof.pack(22).is_none() && prof.flag(0xBA).is_some(), "0xBA stays: OVR24's ending reads it");
        if pkg.manifest.exe_layout().is_some_and(|l| l.boot.is_some()) && !pkg.manifest.released.flags.is_empty() {
            assert!(prof.chunk(39).is_none() && prof.chunk(346).is_none() && prof.flag(0xE6).is_none(), "the switch checks are released");
        }
        assert!(starter.is_some(), "a blank base boots into the starter scene");
        assert_eq!(lt_core::edit::blank::report(&retail7, &npc7).unwrap().actors_left, 0);
    }
    // A base from the shift-safe build moved main's tables: read through its layout they hold
    // retail's contents (the blank profile changes code, not tables).
    if pkg.manifest.exe_layout().is_some() {
        let r = Disc::open(&retail);
        assert!(b.exe.layout.is_some() && b.exe.va(lt_core::fs::TABLE_VADDR) != lt_core::fs::TABLE_VADDR, "the base's EXE reads through its layout");
        assert!(r.exe.layout.is_none(), "retail keeps its own layout");
        let sizes = |d: &Disc| d.table.entries.iter().filter(|e| e.kind != lt_core::fs::Kind::Spare).map(|e| (e.id.clone(), e.extent.sectors)).collect::<Vec<_>>();
        assert_eq!(sizes(&b), sizes(&r));
        let toys = |d: &Disc| lt_core::gamedata::toys::Toys::load(lt_core::gamedata::Source::new(&d.image, &d.exe, &d.table)).unwrap().roster.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        assert_eq!(toys(&b), toys(&r));
        let mb = lt_core::scene::math::Tables::from_exe(&b.exe).unwrap();
        let mr = lt_core::scene::math::Tables::from_exe(&r.exe).unwrap();
        assert_eq!((mb.sin(300), mb.ratan2(5, 9), &mb.items, &mb.knight), (mr.sin(300), mr.ratan2(5, 9), &mr.items, &mr.knight));
        eprintln!("shifted base: file table at 0x{:08X}, {} toys and the scene tables read as retail's", b.exe.va(lt_core::fs::TABLE_VADDR), toys(&b).len());
    }
    // The project's first new area lands past everything the base appended (its spare files).
    // @LT-64/2: a package made since LT-64 carries the id; 1.0.0 (published before) reads 1328.
    let first = pkg.manifest.new_area_first();
    match pkg.manifest.layout.new_area_first {
        Some(n) => assert_eq!(n, first, "base.json's newAreaFirst is the id the project uses"),
        None => eprintln!("{}: base.json has no newAreaFirst (made before LT-64); reads {first}", pkg.manifest.tag()),
    }
    let area = [staged(&format!("chunk/{first}"), Chunk::new_area(first).serialize().unwrap())];
    let plan = build::plan(&b.src(), &area, |_| true, PlanOptions { postgap: false, new_area_first: first });
    assert!(!plan.blocked_included, "chunk/{first}: {:?}", plan.items.iter().map(|i| (&i.id, &i.refusal)).collect::<Vec<_>>());
    // A base with HackerGC's file registry: spare entries are `new/N`; a project's new file
    // goes into a free one, appended to the disc with its entry written.
    if pkg.manifest.exe_layout().is_some_and(|l| l.spare_files.is_some()) {
        let spare: Vec<_> = b.table.of_kind(lt_core::fs::Kind::Spare).collect();
        let used: Vec<_> = spare.iter().filter(|e| e.extent.sectors > 0).map(|e| e.id.as_str()).collect();
        eprintln!("spare entries: {} ({} used: {:?})", spare.len(), used.len(), used);
        let free = spare.iter().find(|e| e.extent.sectors == 0).expect("a free spare entry").id.clone();
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
        let dir = out_dir("spare");
        let src = b.src();
        let files = [staged(&free, data.clone())];
        let plan = build::plan(&src, &files, |_| true, PlanOptions { postgap: false, ..Default::default() });
        assert!(!plan.blocked_included, "{:?}", plan.items.iter().map(|i| (&i.id, &i.refusal)).collect::<Vec<_>>());
        let img = dir.join("spare.bin");
        let cancel = AtomicBool::new(false);
        let rep = build::write_image(&src, &plan, &files, &img, &cancel, &mut |_| {}).unwrap();
        build::verify_image(&img, &src, &plan, &files, &rep, &cancel, &mut |_| {}).unwrap();
        let built = Disc::open(&img);
        let e = built.table.by_id(&free).expect("the new file's entry");
        assert_eq!(e.extent.sectors, 3);
        assert_eq!(built.image.read_file_bytes(e.extent.lba, 0, data.len()).unwrap(), data);
        eprintln!("{free}: placed at LBA {} ({} sectors), read back through the layout", e.extent.lba, e.extent.sectors);
        fs::remove_dir_all(&dir).unwrap();
        // Ids past the spare entries are still refused.
        let p2 = build::plan(&src, &[staged("new/99", vec![1; 10])], |_| true, PlanOptions::default());
        assert_eq!(p2.items[0].refusal.as_ref().map(|r| r.code.as_str()), Some("NEW_FILE_NEEDS_EXE_GROWTH"));
    }
    fs::remove_dir_all(&out).unwrap();
}

/// A blank base's packs (`LT_TEST_BASE`, HackerGC's from `examples/make_hackergc.rs`) are retail's
/// plus Living Tool's own edits, so the project's edits are read against retail's pack (LT-42):
/// every pack reads as edits, and an actor, a scene and a door go into npc/7, npc/23 and npc/2
/// with the base's own edits (removed records, freed models, the starter scene) kept.
#[test]
fn a_blank_bases_packs_take_edits_against_retail() {
    use lt_core::edit::{actor, blank, door, pack, scene};
    use lt_core::trigger::TriggerRecord;
    let (Some(retail), Some(dir)) = (std::env::var_os("LT_TEST_DISC").map(PathBuf::from), std::env::var_os("LT_TEST_BASE").map(PathBuf::from)) else { return };
    let pkg = BasePackage::open(&dir).unwrap();
    if !pkg.manifest.id.contains("blank") {
        return;
    }
    let out = out_dir("blank-refs");
    let mat = base::materialise(&pkg, &retail, &out, &AtomicBool::new(false), |_, _| {}).unwrap();
    let (b, r) = (Disc::open(&mat.image), Disc::open(&retail));
    let (bs, rs) = (b.src(), r.src());
    for n in (0..50u32).filter(|&n| n != 17) {
        let id = format!("npc/{n}");
        assert!(pack::reads_as_edits(&rs.retail(&id).unwrap(), &bs.retail(&id).unwrap()), "{id} reads as retail's plus edits");
    }
    for n in [7u32, 23, 2] {
        let id = format!("npc/{n}");
        let (rp, bp) = (rs.retail(&id).unwrap(), bs.retail(&id).unwrap());
        let had = pack::Edits::recover(&rp, &bp).unwrap();
        let chunk = lt_core::npc::NpcPack::parse(&bp).unwrap().triggers(&bp).into_iter().find(|g| g.pass == 1).unwrap().chunk as u16;
        // A visible copy brings back its source's model and pose block, nothing else; scenes and
        // doors leave the freed sections alone.
        let kept = |out: &[u8], what: &str| {
            let now = pack::Edits::recover(&rp, out).unwrap_or_else(|e| panic!("{id} {what}: {e}"));
            assert_eq!(now.removed, had.removed, "{id} {what}: the base's removed records");
            if what == "actor" {
                let src = lt_core::npc::NpcPack::parse(&rp).unwrap().actors[2].b;
                let (model, pose) = (((src >> 16) & 0xFF) as u32, ((src >> 24) & 0xFF) as u32);
                let back: Vec<u32> = had.freed.models.difference(&now.freed.models).map(|&m| m as u32).collect();
                assert!(now.freed.models.is_subset(&had.freed.models) && back.iter().all(|&m| m == model), "{id}: models back {back:?}, source model {model}");
                assert!(now.freed.poses.is_subset(&had.freed.poses) && had.freed.poses.difference(&now.freed.poses).all(|&p| p as u32 == pose), "{id}: poses");
            } else {
                assert_eq!(now.freed, had.freed, "{id} {what}: the base's freed models");
            }
            if n == 7 {
                assert!(blank::find_starter(&rp, out).unwrap().is_some(), "{id} {what}: the base's starter scene");
            }
        };
        let mut actors = actor::recover(&rp, &bp).unwrap();
        actors.push(actor::AddedActor { from: 2, home: chunk, x: 0x40, h: -0x20, z: 0x40, dir: 0, hidden: false, own: None, character: None, spot: false, branches: Vec::new(), otherwise_hidden: false, walk: Vec::new(), house: None, smoke: false });
        kept(&actor::apply(&rp, &bp, &actors).unwrap_or_else(|e| panic!("{id} actor: {e}")).0, "actor");
        let program = blank::starter_program_at(chunk, 0x80, 0x80, 0);
        let (with_scene, slot) = scene::create(&rp, &bp, &scene::NewScene { chunk, x: 0x80, h: -0x20, z: 0x80, cue: 0xFF }, &program, &[]).unwrap_or_else(|e| panic!("{id} scene: {e}"));
        kept(&with_scene, "scene");
        assert!(scene::list(&rp, &with_scene).unwrap().iter().any(|s| s.slot == slot && s.chunk == chunk), "{id}: the new scene lists");
        let mut doors = door::recover(&rp, &bp).unwrap();
        doors.push(door::Added::always(chunk as i16, TriggerRecord::Door { at: 0, x: 0x100, z: 0x100, facing: 0, to: chunk, tx: 0x100, tz: 0x180, flags: 0x8000, pack: n as u8, music: 0 }));
        kept(&door::apply(&rp, &bp, &doors).unwrap_or_else(|e| panic!("{id} door: {e}")).0, "door");
    }
    fs::remove_dir_all(&out).unwrap();
}
