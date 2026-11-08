//! Editor encoders against the user's own retail image (screens 10, 12, 13).
//! Opt in with `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test edits`.
//! The image is opened read-only. `LT_EDIT_DUMP=<dir>` also writes the enemy
//! profiles, a set of field edits and the Rust encodings as JSON there, so
//! `encode.ts` can be checked against the same cases.

use std::collections::BTreeMap;

use lt_core::edit::enemy::{self, EnemyFields};
use lt_core::edit::placement::{self as place_edit, Place, PlaceWrite};
use lt_core::edit::text as text_edit;
use lt_core::fs::Kind;
use lt_core::gamedata::{enemy::EnemyProfile, GameData, Source};
use lt_core::npc::NpcPack;
use lt_core::placement::{self, Body};
use lt_core::text;
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
    fn read(&self, id: &str) -> Vec<u8> {
        let e = self.table.by_id(id).unwrap();
        self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
    }

    /// Every distinct pack (npc/17 aliases npc/0).
    fn packs(&self) -> Vec<(String, Vec<u8>)> {
        self.table
            .of_kind(Kind::Npc)
            .filter(|e| e.aliases.is_empty() || e.global < e.aliases[0])
            .map(|e| (e.id.clone(), self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()))
            .collect()
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/* ───────────────────────── 13 · enemy profiles ───────────────────────── */

/// Deterministic field edits for profile `p` (every field changes in some case).
fn cases(p: &EnemyProfile) -> Vec<EnemyFields> {
    let base = EnemyFields::of(p);
    let n = p.id as i32;
    let mut out = vec![base.clone()];
    let mut a = base.clone();
    a.name = format!("Test {n:03} {}", "x".repeat((n % 9) as usize));
    a.hp_base = a.hp_base.wrapping_add(6);
    a.chance_b = a.chance_b.wrapping_add(31);
    a.level = (n as u16 % 99) + 1;
    out.push(a);
    let mut b = base.clone();
    b.size ^= 0x00F0;
    b.height = b.height.wrapping_add(1);
    b.depth = b.depth.wrapping_sub(1);
    b.width = 0xFFFF;
    b.spd = -12;
    b.atk = 300;
    b.def = i16::MIN;
    b.stat = i16::MAX;
    b.ai ^= 1;
    b.aff = b.aff.rotate_left(4) ^ 0x0101_0000;
    b.imm ^= 0x00A5;
    b.gold = -1;
    b.exp = 12_345;
    b.drop_a = (n as u16 * 7) % 206;
    b.drop_b = (n as u16 * 3) % 206;
    b.chance_a = 255;
    out.push(b);
    out
}

#[test]
fn every_enemy_profile_round_trips() {
    let Some(disc) = disc() else { return };
    let data = GameData::load(Source::new(&disc.image, &disc.exe, &disc.table)).unwrap();
    assert_eq!(data.enemies.len(), 110);
    let mut dump = Vec::new();
    for p in &data.enemies {
        let head = enemy::head_len(p);
        let raw = &p.raw[..head];
        let f = EnemyFields::of(p);
        // encode(decode(x)) == x, for the TS port and for the patcher.
        assert_eq!(enemy::encode_head(p, &f), raw, "enemy/{} encode_head", p.id);
        assert_eq!(enemy::patch(p, raw, &f).unwrap(), raw, "enemy/{} patch", p.id);

        let file = disc.read(&format!("enemy/{}", p.id));
        for (k, f) in cases(p).iter().enumerate() {
            let patched = enemy::patch(p, raw, f).unwrap();
            if !p.proto {
                assert_eq!(patched, enemy::encode_head(p, f), "enemy/{} case {k}: patch and the encode.ts port disagree", p.id);
            }
            // Staged file re-decodes to the edited fields.
            let staged = enemy::patch_file(p, &file, f).unwrap();
            assert_eq!(staged.len(), file.len());
            let effects = lt_core::gamedata::modules::EffectTable::parse(&disc.read("ovr/4"));
            let section = &staged[p.prof_ofs as usize..];
            let back = lt_core::gamedata::enemy::parse(p.id, p.size, p.prof_ofs, section, &effects).unwrap();
            assert_eq!(EnemyFields::of(&back), *f, "enemy/{} case {k} reads back", p.id);
            // Only the head changed.
            let at = p.prof_ofs as usize;
            assert_eq!(staged[..at], file[..at]);
            assert_eq!(staged[at + head..], file[at + head..]);
            if !p.proto {
                dump.push(serde_json::json!({ "id": p.id, "case": k, "fields": f, "bytes": hex(&patched) }));
            }
        }
    }
    assert_eq!(dump.len(), 107 * 3);
    // Validation.
    let p = data.enemy(17).unwrap();
    let mut f = EnemyFields::of(p);
    f.name = "A name that is too long".into();
    assert_eq!(enemy::patch(p, &p.raw, &f).unwrap_err().code, "ENEMY_NAME_LONG");
    f.name = "Knoygné".into();
    assert_eq!(enemy::patch(p, &p.raw, &f).unwrap_err().code, "ENEMY_NAME_CHARS");

    if let Some(dir) = std::env::var_os("LT_EDIT_DUMP") {
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("enemies.json"), serde_json::to_string(&data.enemies).unwrap()).unwrap();
        std::fs::write(dir.join("rust-encodings.json"), serde_json::to_string(&dump).unwrap()).unwrap();
        eprintln!("wrote {} cases to {}", dump.len(), dir.display());
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/* ───────────────────────── 12 · dialogue text ───────────────────────── */

/// Swaps the case of the first ASCII letter outside control codes (same length).
fn recase(markup: &str) -> Option<String> {
    let mut out = String::new();
    let mut done = false;
    let mut in_code = false;
    let mut chars = markup.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            in_code = true;
        } else if c == '}' {
            in_code = false;
        } else if !in_code && !done && (c == '$' || c == '#' || c == '~') {
            // Skip the insert's key so it stays an insert.
            out.push(c);
            if let Some(n) = chars.next() {
                out.push(n);
            }
            continue;
        } else if !in_code && !done && c.is_ascii_alphabetic() {
            out.push(if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() });
            done = true;
            continue;
        }
        out.push(c);
    }
    done.then_some(out)
}

#[test]
fn every_pack_string_re_encodes_in_place() {
    let Some(disc) = disc() else { return };
    let (mut strings, mut packs) = (0, 0);
    for (id, b) in disc.packs() {
        let pack = NpcPack::parse(&b).unwrap();
        let d = pack.d_block(&b);
        let pool = pack.pool(&b);
        // decode → markup → encode is byte-identical for every string.
        for s in &pool {
            let raw = text::c_string(d, s.offset as usize);
            assert_eq!(text_edit::encode_markup(&s.markup).unwrap(), raw, "{id}@{:04X}", s.offset);
            strings += 1;
        }
        // Applying no edits (or every line unchanged) gives the retail bytes back.
        let same: BTreeMap<u32, String> = pool.iter().map(|s| (s.offset, s.markup.clone())).collect();
        let (out, report) = text_edit::apply(&b, &b, &same).unwrap();
        assert_eq!(out, b, "{id}: unchanged edit set");
        assert!(report.relocated.is_empty() && report.in_place.is_empty());

        // Every line rewritten in place (same length, one letter recased).
        let edits: BTreeMap<u32, String> = pool.iter().filter_map(|s| recase(&s.markup).map(|m| (s.offset, m))).collect();
        if edits.is_empty() {
            continue;
        }
        let (out, report) = text_edit::apply(&b, &b, &edits).unwrap();
        assert_eq!(out.len(), b.len(), "{id}: in-place edits keep the size");
        assert!(report.relocated.is_empty(), "{id}: nothing relocates");
        assert_eq!(report.in_place.len(), edits.len());
        let back = text_edit::edit_set(&b, &out).unwrap();
        assert_eq!(back, edits, "{id}: the edits read back");
        // And reverting them restores retail exactly.
        let (rev, _) = text_edit::apply(&b, &out, &BTreeMap::new()).unwrap();
        assert_eq!(rev, b, "{id}: revert");
        packs += 1;
    }
    eprintln!("{strings} strings round-trip; {packs} packs rewritten in place and reverted");
    assert!(strings > 2_700);
}

#[test]
fn a_grown_mayor_line_relocates() {
    let Some(disc) = disc() else { return };
    let b = disc.read("npc/7");
    let pack = NpcPack::parse(&b).unwrap();
    let pool = pack.pool(&b);
    let mayor = pool.iter().find(|s| s.offset == 0x07A8).unwrap();
    assert!(mayor.markup.contains("Mayor Kaylan"));
    let grown = mayor.markup.replace("Mayor Kaylan", "Mayor Kalan, keeper of the village");
    assert!(grown.len() > mayor.markup.len() + 8);
    let refs = text_edit::text_refs(&pack, &b);
    let sites = refs[&0x07A8].clone();
    assert!(!sites.is_empty());

    let edits = BTreeMap::from([(0x07A8u32, grown.clone())]);
    let (out, report) = text_edit::apply(&b, &b, &edits).unwrap();
    let first_model = pack.models.iter().copied().filter(|&m| m > pack.d).min().unwrap();
    assert_eq!(report.relocated.len(), 1);
    let moved = &report.relocated[0];
    assert_eq!(moved.off, 0x07A8);
    assert_eq!(moved.at, first_model - pack.d, "appended at the end of the D-span");
    assert_eq!(moved.sites, sites);
    assert_eq!(report.appended % 4, 0);
    assert_eq!(report.d_span, pack.d_span + report.appended);
    assert!(out.len() <= 0x31000 && out.len() % 2048 == 0);

    // Re-disassemble: every site points at the new offset, and the text decodes.
    let p2 = NpcPack::parse(&out).unwrap();
    let d2 = p2.d_block(&out);
    let refs2 = text_edit::text_refs(&p2, &out);
    assert!(!refs2.contains_key(&0x07A8), "no operand still points at the old slot");
    assert_eq!(refs2[&moved.at], sites);
    assert_eq!(text::markup(&text::decode(text::c_string(d2, moved.at as usize))), grown);
    // The old slot keeps the retail text; every other string is unchanged.
    assert_eq!(text::c_string(d2, 0x07A8), text::c_string(pack.d_block(&b), 0x07A8));
    for (&off, s) in &refs {
        if off != 0x07A8 {
            assert_eq!(refs2[&off], *s);
            assert_eq!(text::c_string(d2, off as usize), text::c_string(pack.d_block(&b), off as usize));
        }
    }
    // Models moved by the inserted bytes; everything before the D-span end is as before.
    for (i, (m1, m2)) in pack.models.iter().zip(&p2.models).enumerate() {
        let want = if *m1 >= first_model { m1 + report.appended } else { *m1 };
        assert_eq!(*m2, want, "model {i}");
        assert_eq!(le32(&out, 8 + i * 4), want);
    }
    let fm = first_model as usize;
    let a = report.appended as usize;
    assert_eq!(out[fm + a..fm + a + 0x1000], b[fm..fm + 0x1000], "model bytes moved intact");
    // Same programs and trigger tables.
    assert_eq!(p2.actors.len(), pack.actors.len());
    assert_eq!(p2.triggers(&out).len(), pack.triggers(&b).len());

    // Reading back, re-saving the same set, and reverting.
    let lines = text_edit::edited_lines(&b, &out).unwrap();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].relocated && lines[0].markup == grown);
    let (again, _) = text_edit::apply(&b, &out, &edits).unwrap();
    assert_eq!(again, out, "saving the same edits again is stable");
    let (rev, _) = text_edit::apply(&b, &out, &BTreeMap::new()).unwrap();
    assert_eq!(rev, b, "revert gives the retail pack");

    // Limits: the loader cap.
    let huge = format!("{{box:0}}{}", "A very long line. ".repeat(500));
    let err = text_edit::apply(&b, &b, &BTreeMap::from([(0x07A8u32, huge)])).unwrap_err();
    assert_eq!(err.code, "NPC_CAP");
    eprintln!("grown Mayor line: +{} B appended, {} sectors, {}", report.appended, report.sectors, err.message);
}

/* ───────────────────────── 10 · placement ───────────────────────── */

#[test]
fn a_moved_fisher_reads_back() {
    let Some(disc) = disc() else { return };
    let b = disc.read("npc/4");
    let pack = NpcPack::parse(&b).unwrap();
    let roster = placement::roster(&pack, &b);
    let fisher = roster.iter().find(|a| a.slot == 5).unwrap();
    assert_eq!(fisher.speaker.as_deref(), Some("Fisher"));
    let branch = fisher.branches.iter().find(|br| matches!(br.body, Body::Place { .. })).unwrap();
    let to = Place { x: 448, h: -129, z: -320, dir: 1024 };
    let out = place_edit::apply(&b, &b, &[PlaceWrite { at: branch.at, place: Some(to) }]).unwrap();
    assert_eq!(out.len(), b.len());
    let diff = out.iter().zip(&b).filter(|(x, y)| x != y).count();
    assert!(diff > 0 && diff <= 8, "only the four words change ({diff} bytes)");

    let p2 = NpcPack::parse(&out).unwrap();
    let back = placement::roster(&p2, &out);
    let f2 = back.iter().find(|a| a.slot == 5).unwrap();
    let b2 = f2.branches.iter().find(|br| br.at == branch.at).unwrap();
    assert_eq!(b2.body, Body::Place { x: 448, h: -129, z: -320, dir: 1024 });
    assert_eq!(b2.walk, branch.walk, "the walk script is untouched");
    let moved = place_edit::moved(&b, &out);
    assert!(moved.iter().any(|(slot, m)| *slot == 5 && m.at == branch.at && m.place == to));

    // Composes with a text edit in the same pack; reverting each leaves the other.
    let line = pack.pool(&b).into_iter().find(|s| s.kind == "say").unwrap();
    let edits = BTreeMap::from([(line.offset, format!("{} Grown to move it somewhere else.", line.markup))]);
    let (both, _) = text_edit::apply(&b, &out, &edits).unwrap();
    let pb = NpcPack::parse(&both).unwrap();
    assert!(place_edit::moved(&b, &both).iter().any(|(slot, m)| *slot == 5 && m.place == to));
    assert_eq!(text_edit::edit_set(&b, &both).unwrap(), edits);
    assert_eq!(pb.d, pack.d);
    let reverted = place_edit::apply(&b, &both, &[PlaceWrite { at: branch.at, place: None }]).unwrap();
    assert!(place_edit::moved(&b, &reverted).is_empty());
    let (clean, _) = text_edit::apply(&b, &reverted, &BTreeMap::new()).unwrap();
    assert_eq!(clean, b, "both reverted");

    // Absent branches have no position.
    if let Some(absent) = roster.iter().flat_map(|a| &a.branches).find(|br| br.body == Body::Absent) {
        let err = place_edit::apply(&b, &b, &[PlaceWrite { at: absent.at, place: Some(to) }]).unwrap_err();
        assert_eq!(err.code, "PLACE_NOT_A_BRANCH");
    }
}

/* ───────────────────────── The project overlay ───────────────────────── */

/// Saves through a real project folder, rebuilds the overlay from its
/// manifest (as the app does) and reads the edits back through the
/// overlay-aware sources: game data, the world index and the text state.
#[test]
fn staged_edits_read_back_through_the_overlay() {
    use lt_core::overlay::{self, Staged};
    use lt_core::project::{DiscRef, Project};
    use lt_core::worldmap::{self, ProjectWorld, WorldDisc, WorldFiles};
    use std::sync::Arc;

    let Some(disc) = disc() else { return };
    let parent = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("lt-edits-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent).unwrap();
    let disc_ref = DiscRef {
        title: "Guardian's Crusade (USA)".into(),
        serial: "SLUS-00811".into(),
        verified: "redump".into(),
        fingerprint: None,
        hint_path: String::new(),
    };
    let mut project = Project::create(&parent, "overlay test", disc_ref, None).unwrap();
    let overlay_of = |p: &Project| -> Staged {
        p.manifest
            .entries
            .iter()
            .flat_map(|(id, e)| {
                let b = Arc::new(p.blobs().get(&e.current.sha256).unwrap());
                e.alias_group.iter().chain(std::iter::once(id)).map(move |a| (a.clone(), b.clone())).collect::<Vec<_>>()
            })
            .collect()
    };

    // 13: Knoygn's HP base 0 → 6.
    let data = GameData::load(Source::new(&disc.image, &disc.exe, &disc.table)).unwrap();
    let knoygn = data.enemy(17).unwrap();
    let mut f = EnemyFields::of(knoygn);
    f.hp_base = 6;
    let file = disc.read("enemy/17");
    project.stage("enemy/17", vec!["enemy/17".into()], &file, &enemy::patch_file(knoygn, &file, &f).unwrap(), "editor").unwrap();

    // 12 + 10 in one pack: the Mayor's grown line and the Fisher's move (npc/4 serves chunk 39; the Mayor is npc/7).
    let npc7 = disc.read("npc/7");
    let grown = BTreeMap::from([(0x07A8u32, NpcPack::parse(&npc7).unwrap().pool(&npc7).into_iter().find(|s| s.offset == 0x07A8).unwrap().markup.replace("Kaylan", "Kalan, keeper of the village"))]);
    let (npc7_edit, _) = text_edit::apply(&npc7, &npc7, &grown).unwrap();
    project.stage("npc/7", vec!["npc/7".into()], &npc7, &npc7_edit, "editor").unwrap();
    let npc4 = disc.read("npc/4");
    let p4 = NpcPack::parse(&npc4).unwrap();
    let fisher_branch = placement::roster(&p4, &npc4)[5].branches.iter().find(|b| matches!(b.body, Body::Place { .. })).unwrap().at;
    let to = Place { x: 448, h: -129, z: -320, dir: 1024 };
    let npc4_edit = place_edit::apply(&npc4, &npc4, &[PlaceWrite { at: fisher_branch, place: Some(to) }]).unwrap();
    project.stage("npc/4", vec!["npc/4".into()], &npc4, &npc4_edit, "editor").unwrap();

    // Reopen from disk, as after a relaunch.
    let reopened = Project::open(&project.root).unwrap();
    assert_eq!(reopened.manifest.entries.len(), 3);
    let kinds: Vec<String> = reopened.history(10).unwrap().iter().map(|h| h.kind.clone()).collect();
    assert_eq!(kinds.iter().filter(|k| *k == "autosave").count(), 3);
    let staged = overlay_of(&reopened);

    let src = Source::new(&disc.image, &disc.exe, &disc.table).with_staged(Some(&staged));
    let projected = GameData::load(src).unwrap();
    assert_eq!(projected.enemy(17).unwrap().hp_base, 6, "game data reads the staged enemy/17");
    assert_eq!(projected.enemy(18).unwrap().hp_base, data.enemy(18).unwrap().hp_base);

    let w = ProjectWorld { disc: WorldDisc { image: &disc.image, table: &disc.table, exe: &disc.exe }, staged: &staged };
    let b4 = w.file("npc/4").unwrap();
    let r4 = placement::roster(&NpcPack::parse(&b4).unwrap(), &b4);
    assert!(r4[5].branches.iter().any(|b| b.body == Body::Place { x: 448, h: -129, z: -320, dir: 1024 }), "the world roster reads the moved Fisher");
    let packs = worldmap::packs(&w);
    assert_eq!(packs[7].row.bytes as usize, npc7_edit.len());

    let current7 = overlay::file(&disc.image, &disc.table, Some(&staged), "npc/7").unwrap();
    assert_eq!(text_edit::edit_set(&npc7, &current7).unwrap(), grown, "the text state reads the relocated line");

    // Revert: staging the retail bytes removes the entries.
    let mut p = reopened;
    p.stage("enemy/17", vec!["enemy/17".into()], &file, &file, "editor").unwrap();
    let (back, _) = text_edit::apply(&npc7, &current7, &BTreeMap::new()).unwrap();
    p.stage("npc/7", vec!["npc/7".into()], &npc7, &back, "editor").unwrap();
    assert_eq!(p.manifest.entries.keys().collect::<Vec<_>>(), vec!["npc/4"]);
    std::fs::remove_dir_all(&parent).unwrap();
}

/* ───────────────────────── 07 · palette colours ───────────────────────── */

#[test]
fn every_sys14_palette_colour_round_trips() {
    use lt_core::edit::tim as tim_edit;
    use lt_core::tim::{bgr555_to_hex, Tim};
    let Some(disc) = disc() else { return };
    let file = disc.read("sys/2");
    let t = Tim::parse(&file).unwrap();
    let c = t.info.clut.unwrap();
    let mut n = 0;
    for row in 0..c.h as usize {
        for e in 0..c.w as usize {
            let at = tim_edit::clut_entry_offset(&file, 0, row, e).unwrap();
            let old = u16::from_le_bytes([file[at], file[at + 1]]);
            let (out, word) = tim_edit::set_color(&file, 0, row, e, bgr555_to_hex(old).as_deref()).unwrap();
            assert_eq!((word, out == file), (old, true), "row {row} entry {e}");
            n += 1;
        }
    }
    // One colour changes exactly two bytes.
    let (out, word) = tim_edit::set_color(&file, 0, 0, 1, Some("#FF0000")).unwrap();
    assert_eq!(word & 0x7FFF, 0x001F);
    let changed = out.iter().zip(&file).filter(|(a, b)| a != b).count();
    assert!((1..=2).contains(&changed), "{changed} bytes changed");
    eprintln!("{n} SYS14 palette entries round-trip");
}
