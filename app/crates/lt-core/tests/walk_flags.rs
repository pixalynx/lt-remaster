//! Walk-script flag reads in the flag index (screen 17 §5.2) against the user's
//! own retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test walk_flags`.

use std::collections::BTreeSet;

use lt_core::flags::{self, FlagIndex, FlagSite};
use lt_core::fs::Kind;
use lt_core::npc::NpcPack;
use lt_core::{DiscImage, FileTable, PsxExe};

/// The index over every distinct pack (npc/17 aliases npc/0), or `None` without a disc.
fn index() -> Option<FlagIndex> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    let mut builder = flags::IndexBuilder::default();
    for e in table.of_kind(Kind::Npc).filter(|e| e.aliases.is_empty() || e.global < e.aliases[0]) {
        let n: u16 = e.id.trim_start_matches("npc/").parse().unwrap();
        let b = image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
        builder.add_pack(n, &b, &NpcPack::parse(&b).unwrap());
    }
    Some(builder.finish())
}

#[test]
fn walk_scripts_read_scene_cues() {
    let Some(index) = index() else { return };
    let has = |flag: u16, s: FlagSite| index.sites.get(&flag).is_some_and(|v| v.contains(&s));

    // Kell (npc/40 actor 23, "???" until named) waits on the bird scene's cues in
    // the walk of its 0x016 branch; 0x017 is set by his own script and read only here.
    assert!(has(0x017, FlagSite('W', 40, 23, 0x402A, 340, 0x16, 0)));
    assert!(has(0x019, FlagSite('W', 40, 23, 0x4034, 340, 0x16, 0)));
    assert!(has(0x018, FlagSite('W', 40, 23, 0x403A, 340, 0x16, 0)));
    let in_40 = |f: u16| index.sites[&f].iter().filter(|s| s.1 == 40).map(|s| s.0).collect::<String>();
    assert_eq!(in_40(0x017), "SW", "set by a script, tested only by the walk");

    // The argument at Isten's gate (npc/2, chunk 46): four walkers wait on 0x028–0x02E.
    let walkers = |f: u16| index.sites[&f].iter().filter(|s| s.0 == 'W' && s.1 == 2).map(|s| s.2).collect::<Vec<_>>();
    assert_eq!(walkers(0x02E), vec![17, 18, 19, 20]);
    assert_eq!(walkers(0x028), vec![17]);

    // The wake-up actor (npc/7 actor 3) is re-placed once 0x00E is set: `B00E`.
    assert!(has(0x00E, FlagSite('w', 7, 3, 0x27D0, 326, 0, 2)));

    let all: Vec<(u16, &FlagSite)> = index.sites.iter().flat_map(|(f, v)| v.iter().map(move |s| (*f, s))).filter(|(_, s)| s.0 == 'W' || s.0 == 'w').collect();
    let distinct: BTreeSet<u16> = all.iter().map(|(f, _)| *f).collect();
    let count = |k: char, lane: u8| all.iter().filter(|(_, s)| s.0 == k && s.6 == lane).count();
    assert_eq!((all.len(), distinct.len()), (298, 75));
    assert_eq!((count('W', 0), count('w', 1), count('w', 2)), (230, 0, 68), "wait · hide · re-place");
    assert!(all.iter().all(|(f, s)| *f != 0 && s.2 >= 2), "no flag-0 words, no party walks");

    // Flags that looked set-only (no test, rung or placement in any pack) and are walk cues.
    let any = |v: &[FlagSite], kinds: &str| v.iter().any(|s| kinds.contains(s.0));
    let cues: Vec<u16> = index.sites.iter().filter(|(_, v)| any(v, "Ss") && !any(v, "TRP") && any(v, "Ww")).map(|(f, _)| *f).collect();
    assert_eq!(cues, vec![0x02D, 0x02E, 0x02F, 0x030, 0x033, 0x034, 0x03E, 0x06D]);
}
