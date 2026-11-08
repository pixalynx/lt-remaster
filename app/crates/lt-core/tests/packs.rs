//! Content packs (`lt_core::pack`) against the user's own retail image: opt in with
//! `LT_TEST_DISC=<Guardian's Crusade (USA) .bin>`. The image is opened read-only. The patches are
//! read from the pack's folder (`data/packs/<id>/`, where `make_pack` writes them; they stay out of
//! git) or from `LT_TEST_PACKS=<folder with <id>/<patch>>`, a download of the published ones.

use std::path::PathBuf;

use lt_core::flyover::{self, Waypoint};
use lt_core::pack::{self, PackFile, PackState};
use lt_core::release::Release;
use lt_core::world::grid_cell;
use lt_core::worldmap::WorldDisc;
use lt_core::{DiscImage, FileTable, PsxExe};

fn disc() -> Option<(DiscImage, PsxExe, FileTable)> {
    let path = std::env::var("LT_TEST_DISC").ok()?;
    let image = DiscImage::open(&path).expect("open LT_TEST_DISC");
    let exe = PsxExe::from_disc(&image).expect("exe");
    let table = FileTable::parse(&exe, image.sectors()).expect("table");
    Some((image, exe, table))
}

fn patch_dir(id: &str) -> PathBuf {
    match std::env::var("LT_TEST_PACKS") {
        Ok(d) => PathBuf::from(d).join(id),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/packs").join(id),
    }
}

#[test]
fn the_jp_prologue_pack_builds_from_the_usa_overlays() {
    let Some((image, exe, table)) = disc() else { return };
    let pack = pack::find("jp-prologue").unwrap();
    let dir = patch_dir("jp-prologue");
    let patches = |f: &PackFile| std::fs::read(dir.join(&f.patch)).ok().filter(|b| pack::patch_ok(f, b));
    if pack.manifest.files.iter().any(|f| patches(f).is_none()) {
        eprintln!("skipped: no checked patches in {}", dir.display());
        return;
    }
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = |id: &str| wd.file(id).ok();
    let built = pack.build(&retail, &patches).expect("the patches apply to the retail overlays");
    assert_eq!(built.len(), 2);
    let ovr1 = &built[0].1;
    let ovr25 = &built[1].1;
    // New Game's marker: two immediates, nothing else, and the overlay keeps its size.
    let r1 = retail("ovr/1").unwrap();
    assert_eq!(ovr1.len(), r1.len());
    assert_eq!(ovr1.iter().zip(&r1).filter(|(a, b)| a != b).count(), 2);
    // OVR25 stays inside its slot (0x8014F000 up to the staging buffer at 0x80153000).
    assert!(ovr25.len() <= 0x4000, "OVR25 is {} bytes", ovr25.len());

    // The flight: JP's 20 waypoints and the four English pages.
    let t = pack.flyover(ovr25).expect("flyover table");
    assert_eq!((t.room.waypoints, t.room.text_bytes, t.room.lines), (48, 1024, 8));
    let f = &t.flyover;
    assert_eq!(f.waypoints.len(), 20);
    assert_eq!(f.waypoints[0], Waypoint { mode: 2, page: 0, chunk: 36, x: -272, h: -300, z: 1252, pitch: 512, yaw: 1024, roll: 0 });
    assert_eq!(f.waypoints[13].mode, flyover::MODE_LOOK_AROUND);
    assert_eq!(f.pages.len(), 4);
    assert_eq!(f.pages[0][0], "\"When the devil of black wings");
    assert_eq!(f.pages[0].len(), 8);
    assert_eq!(f.pages[2][2], "a sweet fairy girl named Nehani,");
    assert_eq!(f.pages[3].last().unwrap(), "is about to begin...");
    let chunk_ok = |c: u16| grid_cell(c as u32).is_some();
    flyover::check(f, &t.room, &chunk_ok).expect("the shipped flight fits its own checks");

    // States: retail, applied, flight edited, other edits, another release, no patches.
    let state = |current: &dyn Fn(&str) -> Option<Vec<u8>>| pack.state(Release::Usa, &retail, &retail, current, &patches);
    assert_eq!(state(&retail), PackState::Available);
    let applied = |id: &str| built.iter().find(|(f, _)| f == id).map(|(_, b)| b.clone()).or_else(|| retail(id));
    assert_eq!(state(&applied), PackState::Applied { edited: false });

    let mut g = f.clone();
    g.pages[2][3] = "and a baby monster called Baby.".into();
    g.pages.push(vec!["A fifth page.".into()]);
    g.waypoints[19].z += 100;
    g.waypoints.push(Waypoint { mode: 0, page: 5, chunk: 78, x: 0, h: -500, z: 0, pitch: 512, yaw: 0, roll: 0 });
    let edited_ovr25 = pack.with_flyover(ovr25, &g, &chunk_ok).expect("edit the flight");
    assert_eq!(pack.flyover(&edited_ovr25).unwrap().flyover, g);
    assert_eq!(edited_ovr25.len(), ovr25.len());
    let edited = |id: &str| if id == "ovr/25" { Some(edited_ovr25.clone()) } else { applied(id) };
    assert_eq!(state(&edited), PackState::Applied { edited: true });

    let half = |id: &str| if id == "ovr/1" { retail(id) } else { applied(id) };
    assert!(matches!(state(&half), PackState::Conflict { ref file, .. } if file == "ovr/1"));
    let other = |id: &str| {
        let mut b = applied(id)?;
        if id == "ovr/25" {
            b[0x100] ^= 1;
        }
        Some(b)
    };
    assert!(matches!(state(&other), PackState::Conflict { ref file, .. } if file == "ovr/25"));
    assert!(matches!(pack.state(Release::Jp, &retail, &retail, &retail, &patches), PackState::Unavailable { .. }));

    // Without the patches, the CRCs still tell retail, applied and an edited flight apart.
    let none = |_: &PackFile| None;
    let blind = |current: &dyn Fn(&str) -> Option<Vec<u8>>| pack.state(Release::Usa, &retail, &retail, current, &none);
    assert_eq!(blind(&retail), PackState::Available);
    assert_eq!(blind(&applied), PackState::Applied { edited: false });
    assert_eq!(blind(&edited), PackState::Applied { edited: true });
    assert_eq!(pack.build(&retail, &none).unwrap_err().code, "PACK_NOT_DOWNLOADED");

    // The font has no '{'; a waypoint must be on the overworld.
    let mut bad = f.clone();
    bad.pages[0][0] = "{".into();
    assert_eq!(pack.with_flyover(ovr25, &bad, &chunk_ok).unwrap_err().code, "PACK_FLYOVER");
    let mut bad = f.clone();
    bad.waypoints[3].chunk = 400;
    assert_eq!(pack.with_flyover(ovr25, &bad, &chunk_ok).unwrap_err().code, "PACK_FLYOVER");
}

#[test]
fn the_darkbeat_ibkee_pack_builds_from_the_usa_disc() {
    let Some((image, exe, table)) = disc() else { return };
    let pack = pack::find("darkbeat-ibkee").unwrap();
    assert_eq!(pack.manifest.play_as.as_ref().map(|a| (a.hero.as_str(), a.partner.as_str())), Some(("Darkbeat", "Ibkee")));
    let dir = patch_dir("darkbeat-ibkee");
    let patches = |f: &PackFile| std::fs::read(dir.join(&f.patch)).ok().filter(|b| pack::patch_ok(f, b));
    if pack.manifest.files.iter().any(|f| patches(f).is_none()) {
        eprintln!("skipped: no checked patches in {}", dir.display());
        return;
    }
    let wd = WorldDisc { image: &image, table: &table, exe: &exe };
    let retail = |id: &str| if id == "exe" { Some(exe.bytes.clone()) } else { wd.file(id).ok() };
    let built = pack.build(&retail, &patches).expect("the patches apply to the USA files and their sources");
    let ids: Vec<&str> = built.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["sys/6", "sys/5", "sys/14", "sys/13", "knight/0", "enemy/0", "enemy/1", "pb/0"]);
    let file = |id: &str| &built.iter().find(|(f, _)| f == id).unwrap().1;
    let verts = |b: &[u8], at: usize| u32::from_le_bytes(b[at + 0x10..at + 0x14].try_into().unwrap());
    // The field packet: Darkbeat's 219-vertex body in the Knight's place, Ibkee's 158 in the Baby's.
    let f = file("sys/6");
    let off = |k: usize| u32::from_le_bytes(f[4 * k..4 * k + 4].try_into().unwrap()) as usize;
    assert_eq!((verts(f, off(0)), verts(f, off(1))), (219, 158));
    // The Knight's three battle files are the same Darkbeat; the Baby's is Ibkee, her moves named.
    assert_eq!(file("knight/0"), file("enemy/0"));
    assert_eq!(file("knight/0"), file("enemy/1"));
    let pb = file("pb/0");
    let prof = u32::from_le_bytes(pb[20..24].try_into().unwrap()) as usize;
    assert_eq!(&pb[prof + 2..prof + 11], b"Tail Whip");
    // The partner's name and actions (no transformations), by their retail addresses, and back.
    let exe0 = retail("exe").unwrap();
    let on = pack.exe_write(&exe0, true).unwrap();
    let at = exe.vaddr_to_offset(0x801B_5554).unwrap();
    assert_eq!(&on[at..at + 6], b"Ibkee\0");
    assert_eq!(on.iter().zip(&exe0).filter(|(a, b)| a != b).count(), 5 + 2);
    let bits = exe.vaddr_to_offset(0x801B_557C).unwrap();
    assert_eq!(&on[bits..bits + 4], &[0x3F, 0, 8, 0]);
    assert_eq!(pack.exe_write(&on, false).unwrap(), exe0);
    // States: retail, in, taken out; the names alone count as half in.
    let state = |current: &dyn Fn(&str) -> Option<Vec<u8>>| pack.state(Release::Usa, &retail, &retail, current, &patches);
    assert_eq!(state(&retail), PackState::Available);
    let applied = |id: &str| if id == "exe" { Some(on.clone()) } else { built.iter().find(|(f, _)| f == id).map(|(_, b)| b.clone()).or_else(|| retail(id)) };
    assert_eq!(state(&applied), PackState::Applied { edited: false });
    let names_only = |id: &str| if id == "exe" { Some(on.clone()) } else { retail(id) };
    assert!(matches!(state(&names_only), PackState::Conflict { .. }));
}
