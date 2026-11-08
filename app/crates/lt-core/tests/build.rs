//! Disc writer checks against the user's retail image, which is only read.
//! Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin`. Outputs go to
//! `LT_TEST_OUT` (default: the system temp folder) and are deleted afterwards;
//! on APFS they are clones of the source, so they cost little space.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lt_core::build::{self, BuildEvent, BuildRequest, DiscInput, PlanOptions, Source, Staged, Verdict};
use lt_core::cdrom;
use lt_core::disc::{RAW_SECTOR, USER_DATA_OFFSET};
use lt_core::project::{DiscRef, Project};
use lt_core::{bps, iso9660, DiscImage, FileTable, PsxExe};

struct Retail {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
}

impl Retail {
    fn src(&self) -> Source<'_> {
        Source { image: &self.image, exe: &self.exe, table: &self.table }
    }
}

fn retail() -> Option<Retail> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Retail { image, exe, table })
}

/// A fresh folder for one test's outputs.
fn out_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("LT_TEST_OUT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("lt-build-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn staged(id: &str, bytes: Vec<u8>) -> Staged {
    Staged { id: id.into(), bytes: Arc::new(bytes), base_sha256: None }
}

/// Plans, writes and verifies `files` into `<dir>/out.bin.partial`; returns the path.
fn write(r: &Retail, files: &[Staged], postgap: bool, dir: &Path) -> (PathBuf, build::Plan, build::WriteReport) {
    let src = r.src();
    let plan = build::plan(&src, files, |_| true, PlanOptions { postgap, ..Default::default() });
    assert!(!plan.blocked_included, "{:?}", plan.items);
    let out = dir.join("out.bin.partial");
    let cancel = AtomicBool::new(false);
    let report = build::write_image(&src, &plan, files, &out, &cancel, &mut |_| {}).unwrap();
    build::verify_image(&out, &src, &plan, files, &report, &cancel, &mut |_| {}).unwrap();
    (out, plan, report)
}

/// Every sector LBA where the two images differ (the output may be longer).
fn diff_sectors(a: &Path, b: &Path) -> Vec<u32> {
    const BATCH: usize = 512;
    let (mut fa, mut fb) = (fs::File::open(a).unwrap(), fs::File::open(b).unwrap());
    let (mut ba, mut bb) = (vec![0u8; RAW_SECTOR * BATCH], vec![0u8; RAW_SECTOR * BATCH]);
    let mut out = Vec::new();
    let mut lba = 0u32;
    loop {
        let na = read_full(&mut fa, &mut ba);
        let nb = read_full(&mut fb, &mut bb);
        if na == 0 && nb == 0 {
            return out;
        }
        if na != nb || ba[..na] != bb[..nb] {
            for i in 0..na.max(nb) / RAW_SECTOR {
                let r = i * RAW_SECTOR..(i + 1) * RAW_SECTOR;
                if r.end > na || r.end > nb || ba[r.clone()] != bb[r] {
                    out.push(lba + i as u32);
                }
            }
        }
        lba += (na.max(nb) / RAW_SECTOR) as u32;
    }
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]).unwrap() {
            0 => break,
            k => n += k,
        }
    }
    n
}

fn crc32_of(path: &Path) -> String {
    let img = DiscImage::open(path).unwrap();
    lt_core::identity::fingerprint(&img, |_, _| true).unwrap().unwrap().crc32
}

#[test]
fn encoder_reproduces_retail_sectors() {
    let Some(r) = retail() else { return };
    let npc7 = r.table.by_id("npc/7").unwrap().extent;
    let mut lbas: Vec<u32> = vec![16, 22, 23];
    lbas.extend(24..24 + 155); // the whole EXE
    lbas.extend(npc7.lba..npc7.lba + npc7.sectors);
    lbas.extend((0..r.image.sectors()).step_by(97));
    let mut checked = 0;
    for lba in lbas {
        let raw = r.image.read_raw(lba).unwrap();
        let sub = cdrom::subheader(&raw);
        // Movies' Form 2 audio and the zeroed postgap aren't Form 1 sectors.
        if cdrom::is_form2(&raw) || (sub == [0; 4] && raw[cdrom::EDC..].iter().all(|&b| b == 0)) {
            continue;
        }
        assert_eq!(cdrom::check_form1(&raw, lba), Ok(()), "sector {lba}");
        let mut again = raw;
        let user = raw[USER_DATA_OFFSET..USER_DATA_OFFSET + 2048].to_vec();
        cdrom::encode_form1(&mut again, &user);
        assert!(again == raw, "re-encoding sector {lba} changed it");
        checked += 1;
    }
    assert!(checked > 2000, "only {checked} sectors checked");
}

#[test]
fn no_staged_files_is_byte_identical() {
    let Some(r) = retail() else { return };
    let dir = out_dir("identity");
    let (out, plan, report) = write(&r, &[], true, &dir);
    assert!(plan.nothing);
    assert_eq!((plan.new_volume, plan.edc_sectors, report.sectors_changed), (185_985, 0, 0));
    assert!(diff_sectors(r.image.path(), &out).is_empty());
    assert_eq!(crc32_of(&out), "cdc72c62");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn in_place_edits_touch_only_their_sectors() {
    let Some(r) = retail() else { return };
    let src = r.src();
    // chunk/39: one byte in the body, one in the TIM.
    let mut chunk = src.retail("chunk/39").unwrap();
    chunk[0x100] ^= 0xFF;
    chunk[0x2E010] ^= 0x55;
    // npc/7: a text byte, same size ("the" → "The" somewhere in the D-block).
    let mut npc = src.retail("npc/7").unwrap();
    let at = npc.windows(5).position(|w| w == b" the ").expect("npc/7 has text") + 1;
    npc[at] = b'T';
    let files = [staged("chunk/39", chunk.clone()), staged("npc/7", npc.clone())];
    let dir = out_dir("inplace");
    let (out, plan, report) = write(&r, &files, true, &dir);

    let world = r.table.by_id("world").unwrap().extent.lba;
    let chunk_lba = world + 39 * 125;
    let npc7 = r.table.by_id("npc/7").unwrap().extent;
    assert!(plan.items.iter().all(|i| i.verdict == Verdict::InPlace));
    assert_eq!(plan.items.iter().find(|i| i.id == "chunk/39").unwrap().lba, chunk_lba);
    assert_eq!((plan.appended, plan.new_volume, plan.exe_sectors.len()), (0, 185_985, 0));

    let diffs = diff_sectors(r.image.path(), &out);
    let expected = vec![chunk_lba, chunk_lba + 0x2E010 / 2048, npc7.lba + (at / 2048) as u32];
    let mut sorted = expected.clone();
    sorted.sort_unstable();
    assert_eq!(diffs, sorted, "only the edited sectors differ");
    assert_eq!(report.sectors_changed, 3);
    let img = DiscImage::open(&out).unwrap();
    for lba in &diffs {
        assert_eq!(cdrom::check_form1(&img.read_raw(*lba).unwrap(), *lba), Ok(()), "EDC/ECC of {lba}");
        // Subheaders are the retail ones.
        assert_eq!(cdrom::subheader(&img.read_raw(*lba).unwrap()), cdrom::subheader(&r.image.read_raw(*lba).unwrap()));
    }

    // A fresh reader sees the edits.
    let exe = PsxExe::from_disc(&img).unwrap();
    let table = FileTable::parse(&exe, img.sectors()).unwrap();
    let e = table.by_id("npc/7").unwrap();
    assert_eq!(e.extent, npc7);
    assert_eq!(img.read_sectors(e.extent.lba, e.extent.sectors).unwrap(), npc);
    assert_eq!(img.read_sectors(table.by_id("world").unwrap().extent.lba + 39 * 125, 125).unwrap(), chunk);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn grown_npc7_relocates_to_the_end() {
    let Some(r) = retail() else { return };
    let src = r.src();
    let mut npc = src.retail("npc/7").unwrap();
    assert_eq!(npc.len(), 196_608);
    npc.extend((0..3_212u32).map(|i| b"GROWN TEXT "[i as usize % 11]));
    assert_eq!(npc.len(), 199_820);
    let files = [staged("npc/7", npc.clone())];
    let dir = out_dir("relocate");
    let (out, plan, report) = write(&r, &files, true, &dir);

    let item = &plan.items[0];
    assert_eq!((item.verdict, item.alloc, item.need, item.new_lba), (Verdict::Relocate, 96, 98, Some(185_985)));
    assert_eq!(item.entries, vec![70]);
    assert_eq!(plan.exe_entries.len(), 1);
    assert_eq!(plan.exe_entries[0].raw, [0x41, 0x21, 0x60, 0x00, 0x62, 0x00, 0x00, 0x00]);
    assert_eq!(plan.exe_sectors, vec![155]);
    // 98 appended + 150 postgap; 98 + EXE 1 + PVD 1 sectors re-encoded.
    assert_eq!((plan.appended, plan.new_volume, plan.edc_sectors), (98, 186_233, 100));

    // Only the EXE sector holding entry #70 and the PVD change below the old end.
    let diffs = diff_sectors(r.image.path(), &out);
    let below: Vec<u32> = diffs.iter().copied().filter(|&l| l < 185_985).collect();
    assert_eq!(below, vec![16, 155]);
    assert_eq!(diffs.len(), 2 + 98 + 150);

    let img = DiscImage::open(&out).unwrap();
    assert_eq!(img.sectors(), 186_233);
    assert_eq!(iso9660::read_volume(&img).unwrap().volume_space_size, 186_233);
    for lba in [16, 155].into_iter().chain(185_985..185_985 + 98) {
        assert_eq!(cdrom::check_form1(&img.read_raw(lba).unwrap(), lba), Ok(()), "EDC/ECC of {lba}");
    }
    assert_eq!(cdrom::subheader(&img.read_raw(185_985).unwrap()), cdrom::SUB_DATA);
    assert_eq!(cdrom::subheader(&img.read_raw(185_985 + 97).unwrap()), cdrom::SUB_LAST);
    for lba in 186_083..186_233 {
        let raw = img.read_raw(lba).unwrap();
        assert_eq!(raw, cdrom::postgap(lba), "postgap {lba}");
        assert_eq!(cdrom::header_lba(&raw), Some(lba));
    }
    assert_eq!(report.postgap, Some((186_083, 150)));

    // A fresh reader finds npc/7 at its new home, and nothing else moved.
    let exe = PsxExe::from_disc(&img).unwrap();
    let table = FileTable::parse(&exe, img.sectors()).unwrap();
    let e = table.by_id("npc/7").unwrap();
    assert_eq!(e.raw, [0x41, 0x21, 0x60, 0x00, 0x62, 0x00, 0x00, 0x00]);
    assert_eq!((e.extent.lba, e.extent.sectors), (185_985, 98));
    assert_eq!(img.read_file_bytes(e.extent.lba, 0, npc.len()).unwrap(), npc);
    for (a, b) in table.entries.iter().zip(&r.table.entries) {
        if a.global != 70 {
            assert_eq!(a.raw, b.raw, "entry #{}", a.global);
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}

/// `build::overlay` (the in-process emulator's disc) holds exactly the sectors a written image
/// changes, with the same bytes, and the same length.
#[test]
fn overlay_matches_the_written_image() {
    let Some(r) = retail() else { return };
    let src = r.src();
    let mut npc7 = src.retail("npc/7").unwrap();
    npc7.extend((0..3_212u32).map(|i| b"GROWN TEXT "[i as usize % 11]));
    let mut npc6 = src.retail("npc/6").unwrap();
    npc6[0x1D24] ^= 0x10;
    let files = [staged("npc/6", npc6), staged("npc/7", npc7)];
    let dir = out_dir("overlay");
    let (out, plan, _) = write(&r, &files, true, &dir);
    let (written, sectors) = build::overlay(&src, &plan, &files).unwrap();
    let img = DiscImage::open(&out).unwrap();
    assert_eq!(sectors, img.sectors());
    let diffs = diff_sectors(r.image.path(), &out);
    assert_eq!(written.keys().copied().collect::<Vec<_>>(), diffs);
    for (lba, raw) in &written {
        assert_eq!(&img.read_raw(*lba).unwrap(), raw, "sector {lba}");
    }
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn new_areas_land_at_their_fixed_lba_before_relocations() {
    let Some(r) = retail() else { return };
    let src = r.src();
    let world = r.table.by_id("world").unwrap().extent.lba;
    // A blank new area (1330) and a grown npc/7, which goes after it.
    let area = lt_core::edit::chunk::Chunk::new_area(1330).serialize().unwrap();
    assert_eq!(area.len(), 0x3E800);
    let mut npc = src.retail("npc/7").unwrap();
    npc.extend(std::iter::repeat_n(0x55u8, 4096));
    let files = [staged("chunk/1330", area.clone()), staged("npc/7", npc.clone())];
    let dir = out_dir("new-area");
    let (out, plan, report) = write(&r, &files, true, &dir);

    let at = world + 1330 * 125;
    assert_eq!(at, 186_261);
    let a = plan.items.iter().find(|i| i.id == "chunk/1330").unwrap();
    assert_eq!((a.verdict, a.lba, a.alloc, a.new_lba), (Verdict::InPlace, at, 125, None));
    assert!(plan.exe_entries.iter().all(|e| e.id == "npc/7"), "a new area needs no table entry");
    let n = plan.items.iter().find(|i| i.id == "npc/7").unwrap();
    assert_eq!(n.new_lba, Some(at + 125));
    assert_eq!(plan.cursor_end, at + 125 + n.need);
    assert_eq!(plan.new_volume, plan.cursor_end + 150);
    assert!(report.log.iter().any(|l| l.contains("chunk/1330") && l.ends_with("(new area), 125 changed")), "{:?}", report.log);

    let img = DiscImage::open(&out).unwrap();
    assert_eq!(img.sectors(), plan.new_volume);
    // The gap up to the area is blank postgap; the area is world-archive data.
    for lba in 185_985..at {
        assert_eq!(img.read_raw(lba).unwrap(), cdrom::postgap(lba), "gap {lba}");
    }
    for lba in at..at + 125 {
        assert_eq!(cdrom::check_form1(&img.read_raw(lba).unwrap(), lba), Ok(()), "EDC/ECC of {lba}");
    }
    assert_eq!(cdrom::subheader(&img.read_raw(at).unwrap()), cdrom::SUB_DATA);
    assert_eq!(cdrom::subheader(&img.read_raw(at + 124).unwrap()), cdrom::SUB_LAST);
    assert_eq!(img.read_file_bytes(at, 0, area.len()).unwrap(), area);
    // The game's reader: world entry LBA + id · 125.
    let exe = PsxExe::from_disc(&img).unwrap();
    let table = FileTable::parse(&exe, img.sectors()).unwrap();
    assert_eq!(table.by_id("world").unwrap().extent.lba + 1330 * 125, at);
    assert_eq!(table.by_id("npc/7").unwrap().extent.lba, at + 125);
    assert_eq!(img.read_file_bytes(at + 125, 0, npc.len()).unwrap(), npc);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn alias_group_is_repointed_together() {
    let Some(r) = retail() else { return };
    let src = r.src();
    let npc0 = r.table.by_id("npc/0").unwrap();
    assert_eq!(npc0.aliases.len(), 1, "npc/0 = npc/17");
    let alloc = npc0.extent.sectors;
    assert!(alloc < 98);
    let mut bytes = src.retail("npc/0").unwrap();
    bytes.extend(std::iter::repeat_n(0x5A, 2048 + 7)); // two sectors more
    let files = [staged("npc/0", bytes.clone())];
    let dir = out_dir("alias");
    let (out, plan, _) = write(&r, &files, false, &dir);
    assert_eq!(plan.items[0].entries, vec![63, 80]);
    assert_eq!(plan.exe_entries.iter().map(|e| e.global).collect::<Vec<_>>(), vec![63, 80]);
    // No postgap this time.
    assert_eq!(plan.new_volume, 185_985 + alloc + 2);

    let img = DiscImage::open(&out).unwrap();
    let table = FileTable::parse(&PsxExe::from_disc(&img).unwrap(), img.sectors()).unwrap();
    for id in ["npc/0", "npc/17"] {
        let e = table.by_id(id).unwrap();
        assert_eq!((e.extent.lba, e.extent.sectors), (185_985, alloc + 2), "{id}");
    }
    assert_eq!(table.alias_entries, 4);
    assert_eq!(img.read_file_bytes(185_985, 0, bytes.len()).unwrap(), bytes);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn refusals_block_the_plan() {
    let Some(r) = retail() else { return };
    let src = r.src();
    let mut over = src.retail("npc/2").unwrap();
    over.resize(98 * 2048 + 1, 0); // 99 sectors: one over the NPC cap
    let mut exe = r.exe.bytes.clone();
    exe.push(0);
    let mut table_edit = r.exe.bytes.clone();
    table_edit[0x41D34] ^= 1;
    let files = [
        staged("npc/2", over),
        staged("exe", exe),
        staged("movie/3", vec![0; 2048]),
        staged("chunk/2", vec![0; 0x3E801]),
        staged("new/hook", vec![0; 16]),
    ];
    let plan = build::plan(&src, &files, |_| true, PlanOptions { postgap: true, ..Default::default() });
    let code = |id: &str| plan.items.iter().find(|i| i.id == id).unwrap().refusal.as_ref().map(|e| e.code.clone());
    assert_eq!(code("npc/2").as_deref(), Some("BUDGET_NPC_CAP"));
    assert_eq!(code("exe").as_deref(), Some("EXE_SIZE_CHANGED"));
    // A movie is staged as whole 2336-byte Mode 2 payloads.
    assert_eq!(code("movie/3").as_deref(), Some("BUILD_MOVIE_SECTORS"));
    assert_eq!(code("chunk/2").as_deref(), Some("BUDGET_CHUNK"));
    assert_eq!(code("new/hook").as_deref(), Some("NEW_FILE_NEEDS_EXE_GROWTH"));
    assert!(plan.blocked_included && plan.nothing);
    let edit = [staged("exe", table_edit)];
    let plan = build::plan(&src, &edit, |_| true, PlanOptions { postgap: true, ..Default::default() });
    assert_eq!(plan.items[0].refusal.as_ref().unwrap().code, "EXE_TABLE_EDITED");
    // Excluding the blocked file unblocks the build.
    let plan = build::plan(&src, &files[..1], |_| false, PlanOptions { postgap: true, ..Default::default() });
    assert!(!plan.blocked_included && plan.nothing);
}

fn project(dir: &Path, r: &Retail) -> Project {
    let disc = DiscRef {
        title: "Guardian's Crusade (USA)".into(),
        serial: "SLUS-00811".into(),
        verified: "redump".into(),
        fingerprint: None,
        hint_path: r.image.path().display().to_string(),
    };
    Project::create(dir, "Build test", disc, Some(r.image.path().display().to_string())).unwrap()
}

fn verified(r: &Retail) -> DiscInput {
    DiscInput {
        path: r.image.path().display().to_string(),
        sectors: r.image.sectors(),
        crc32: Some("cdc72c62".into()),
        sha1: Some("d7eda1f08b91721842511348aff58818e12f486b".into()),
        verified: true,
    }
}

#[test]
fn project_build_with_bps_reproduces_the_image() {
    let Some(r) = retail() else { return };
    let src = r.src();
    let dir = out_dir("project");
    let p = project(&dir, &r);
    let mut npc = src.retail("npc/7").unwrap();
    npc.extend(std::iter::repeat_n(b'!', 3_212));
    let files = [staged("npc/7", npc)];
    let req = BuildRequest { name: "english v0.4".into(), out_dir: None, excluded: vec![], image: true, bps: true, postgap: true, new_area_first: None, bps_source: None };
    let mut steps = Vec::new();
    let mut done = Vec::new();
    let rec = build::run(&p, &src, verified(&r), &files, &req, &AtomicBool::new(false), &mut |e| match e {
        BuildEvent::Started { steps: s, .. } => steps = s,
        BuildEvent::StepDone { step, .. } => done.push(step),
        BuildEvent::Progress { .. } => {}
    });
    assert_eq!(rec.status, "built", "{:?}", rec.error);
    assert_eq!(steps.len(), 7);
    assert_eq!(done, vec![0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(rec.number, 1);
    let out = rec.result.as_ref().unwrap();
    let bin = PathBuf::from(&out.bin.as_ref().unwrap().path);
    assert!(bin.ends_with("builds/0001/english v0.4.bin"));
    assert_eq!(out.volume_to, 186_233);
    assert_eq!(out.entries_rewritten, 1);
    assert!(!bin.with_extension("bin.partial").exists());
    let cue = fs::read_to_string(out.cue.as_ref().unwrap()).unwrap();
    assert!(cue.starts_with("FILE \"english v0.4.bin\" BINARY"));
    assert!(fs::read_to_string(out.readme.as_ref().unwrap()).unwrap().contains("cdc72c62"));
    assert_eq!(crc32_of(&bin), out.crc32);

    // build.json, log and history.
    let json: build::BuildRecord = serde_json::from_str(&fs::read_to_string(p.root.join("builds/0001/build.json")).unwrap()).unwrap();
    assert_eq!((json.status.as_str(), json.number, json.label.as_str()), ("built", 1, "english v0.4"));
    assert_eq!(json.plan[0].to, Some(build::Span { lba: 185_985, sectors: 98 }));
    assert!(json.inputs.snapshot.as_deref().unwrap().contains("pre-build-1"));
    assert!(fs::read_to_string(p.root.join("builds/0001/log.txt")).unwrap().contains("file table: npc/7 (#70) -> LBA 185985 (98 sectors)"));
    let kinds: Vec<String> = p.history(10).unwrap().into_iter().map(|h| h.kind).collect();
    assert_eq!(&kinds[..3], ["build.finished", "snapshot.created", "build.started"]);
    assert_eq!(build::list(&p.root).len(), 1);
    assert_eq!(build::next_number(&p.root), 2);

    // The BPS applies to the source and reproduces the build exactly.
    let patch = fs::read(&out.bps.as_ref().unwrap().path).unwrap();
    let info = bps::inspect(&patch).unwrap();
    assert_eq!((info.source_crc32.as_str(), info.target_crc32.as_str()), ("cdc72c62", out.crc32.as_str()));
    assert!(info.metadata.contains("\"build\":1"));
    let source = fs::read(r.image.path()).unwrap();
    let applied = bps::apply(&patch, &source).unwrap();
    drop(source);
    let built = fs::read(&bin).unwrap();
    assert!(applied == built, "BPS output differs from the build");
    // Relocated sectors and the postgap are copied from similar source sectors:
    // the patch holds the 3,212 new bytes plus headers and checksums.
    assert!(patch.len() < 32_000, "patch is {} bytes", patch.len());
    eprintln!("BPS {} bytes · build {} ms · CRC32 {}", patch.len(), out.duration_ms, out.crc32);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn failed_and_cancelled_builds_leave_no_image() {
    let Some(r) = retail() else { return };
    let src = r.src();
    let dir = out_dir("fail");
    let p = project(&dir, &r);
    let req = BuildRequest { name: "x".into(), out_dir: None, excluded: vec![], image: true, bps: false, postgap: true, new_area_first: None, bps_source: None };

    // Blocked: npc/2 over its cap.
    let mut over = src.retail("npc/2").unwrap();
    over.resize(99 * 2048, 1);
    let rec = build::run(&p, &src, verified(&r), &[staged("npc/2", over)], &req, &AtomicBool::new(false), &mut |_| {});
    assert_eq!(rec.status, "failed");
    assert_eq!(rec.error.as_ref().unwrap().code, "BUDGET_NPC_CAP");
    assert!(!p.root.join("builds/0001/x.bin").exists());

    // Cancelled once the copy is done.
    let mut npc = src.retail("npc/7").unwrap();
    npc[5000] ^= 1;
    let cancel = AtomicBool::new(false);
    let rec = build::run(&p, &src, verified(&r), &[staged("npc/7", npc.clone())], &req, &cancel, &mut |e| {
        if let BuildEvent::StepDone { step: build::STEP_COPY, .. } = e {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
    assert_eq!((rec.status.as_str(), rec.number), ("cancelled", 2));
    let leftovers: Vec<_> = fs::read_dir(p.root.join("builds/0002")).unwrap().map(|e| e.unwrap().file_name()).collect();
    let mut names: Vec<String> = leftovers.iter().map(|n| n.to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["build.json", "log.txt"]);

    // The source is still the retail image.
    assert_eq!(crc32_of(r.image.path()), "cdc72c62");
    let finished: Vec<String> = p
        .history(20)
        .unwrap()
        .into_iter()
        .filter(|h| h.kind == "build.finished")
        .map(|h| h.fields["status"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(finished, ["cancelled", "failed"]);
    fs::remove_dir_all(&dir).unwrap();
}

/// A reopened project vouches for its disc by size (`DiscRef::trusted_fingerprint`),
/// so the BPS step checks the CRC32 of the source it actually reads.
#[test]
fn bps_refuses_a_source_that_is_not_the_redump_dump() {
    let Some(r) = retail() else { return };
    let dir = out_dir("not-redump");
    // Same size as the retail image, one byte different (the last sector's ECC).
    let copy = dir.join("other.bin");
    fs::copy(r.image.path(), &copy).unwrap();
    let mut f = fs::OpenOptions::new().read(true).write(true).open(&copy).unwrap();
    let mut last = [0u8; 1];
    f.seek(SeekFrom::End(-1)).unwrap();
    f.read_exact(&mut last).unwrap();
    f.seek(SeekFrom::End(-1)).unwrap();
    f.write_all(&[!last[0]]).unwrap();
    drop(f);
    let image = DiscImage::open(&copy).unwrap();
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let other = Retail { image, exe, table };
    assert_eq!(other.image.size(), r.image.size());

    let p = project(&dir, &other);
    let mut npc = other.src().retail("npc/7").unwrap();
    npc[5000] ^= 1;
    let req = BuildRequest { name: "x".into(), out_dir: None, excluded: vec![], image: true, bps: true, postgap: true, new_area_first: None, bps_source: None };
    // `verified(..)` claims the Redump CRC32, as a trusted project record does.
    let rec = build::run(&p, &other.src(), verified(&other), &[staged("npc/7", npc)], &req, &AtomicBool::new(false), &mut |_| {});
    assert_eq!(rec.status, "failed");
    let err = rec.error.as_ref().unwrap();
    assert_eq!(err.code, "BUILD_BPS_UNVERIFIED");
    assert!(err.message.contains("not cdc72c62"), "{}", err.message);
    let mut names: Vec<String> =
        fs::read_dir(p.root.join("builds/0001")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["build.json", "log.txt"]);
    fs::remove_dir_all(&dir).unwrap();
}
