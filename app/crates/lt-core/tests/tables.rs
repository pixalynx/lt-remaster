//! Game-data table writers (`edit::tables`, screen 13) against the user's own
//! retail image. Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test tables`.
//!
//! Every record of every table is rewritten from its decoded values and must
//! come back byte for byte; then each kind of edit is applied, the tables are
//! decoded again through the staged files, and the change (and only it) shows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use lt_core::edit::tables::{self, AbilityFields, Edit, Files, ItemFields, PhaseFields, ScriptedFields, TemplateFields, ToyFields, ZoneFields};
use lt_core::edit::EditError;
use lt_core::gamedata::{GameData, Source};
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    exe: PsxExe,
    table: FileTable,
    staged: RefCell<HashMap<String, Vec<u8>>>,
}

impl Files for Disc {
    fn current(&self, id: &str) -> Result<Vec<u8>, EditError> {
        match self.staged.borrow().get(id) {
            Some(b) => Ok(b.clone()),
            None => self.retail(id),
        }
    }
    fn retail(&self, id: &str) -> Result<Vec<u8>, EditError> {
        if id == "exe" {
            return Ok(self.exe.bytes.clone());
        }
        Source::new(&self.image, &self.exe, &self.table).file(id).map_err(|e| EditError::new("READ", e.to_string()))
    }
}

impl Disc {
    fn open() -> Option<Self> {
        let path = std::env::var_os("LT_TEST_DISC")?;
        let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
        let exe = PsxExe::from_disc(&image).unwrap();
        let table = FileTable::parse(&exe, image.sectors()).unwrap();
        Some(Self { image, exe, table, staged: RefCell::default() })
    }

    /// The tables as the staged files have them.
    fn data(&self) -> GameData {
        let staged: lt_core::overlay::Staged = self.staged.borrow().iter().map(|(k, v)| (k.clone(), Arc::new(v.clone()))).collect();
        let exe = staged.get("exe").map(|b| PsxExe::parse(self.exe.lba, b.as_ref().clone()).unwrap());
        let src = Source::new(&self.image, exe.as_ref().unwrap_or(&self.exe), &self.table).with_staged(Some(&staged)).with_retail_exe(&self.exe);
        GameData::load(src).unwrap()
    }

    /// Applies an edit and stages what it changed.
    fn apply(&self, edit: Edit) -> Result<tables::Applied, EditError> {
        let data = self.data();
        let out = tables::apply(self, &data, &edit)?;
        for (id, b) in &out.files {
            self.staged.borrow_mut().insert(id.clone(), b.clone());
        }
        Ok(out)
    }

    fn reset(&self) {
        self.staged.borrow_mut().clear();
    }

    /// Files whose staged bytes differ from the disc's.
    fn changed(&self) -> Vec<String> {
        let mut v: Vec<String> = self.staged.borrow().iter().filter(|(id, b)| self.retail(id).unwrap() != **b).map(|(id, _)| id.clone()).collect();
        v.sort();
        v
    }
}

fn exe_off(exe: &PsxExe, va: u32) -> usize {
    exe.vaddr_to_offset(va).unwrap()
}

#[test]
fn every_record_rewrites_byte_exact() {
    let Some(d) = Disc::open() else { return };
    let data = d.data();
    let exe = d.exe.bytes.clone();

    // Items: the whole record from the decoded values, over zeros (bar the pointers and the unused word).
    let items = exe_off(&d.exe, lt_core::gamedata::items::ITEM_TABLE);
    for it in &data.items.items {
        let raw = &exe[items + it.id as usize * 20..][..20];
        let mut r = raw.to_vec();
        r[4..14].fill(0);
        tables::put_item(&mut r, &ItemFields::of(it));
        assert_eq!(r, raw, "item {}", it.id);
        let (out, also) = tables::set_item(&exe, &exe, it.id, &ItemFields::of(it)).unwrap();
        assert!(out == exe && also.is_empty(), "item {} rewrite", it.id);
    }

    // Roster rows.
    let roster = exe_off(&d.exe, lt_core::gamedata::toys::ROSTER);
    for t in &data.toys.roster {
        let raw = &exe[roster + t.row as usize * 24..][..24];
        let mut r = raw.to_vec();
        r[4..10].fill(0);
        r[0x0B..0x10].fill(0);
        tables::put_toy(&mut r, &ToyFields::of(t));
        assert_eq!(r, raw, "roster row {}", t.row);
        let (out, _) = tables::set_toy(&exe, &exe, t.row, &ToyFields::of(t)).unwrap();
        assert!(out == exe, "roster row {} rewrite", t.row);
    }

    // Party templates and scripted battles.
    let tpl = exe_off(&d.exe, lt_core::gamedata::growth::TEMPLATES);
    for (i, t) in data.growth.templates.iter().enumerate() {
        let raw = &exe[tpl + i * 0x28..][..0x28];
        let mut r = vec![0u8; 0x28];
        tables::put_template(&mut r, &TemplateFields::of(t)).unwrap();
        assert_eq!(r, raw, "template {i}");
        assert!(tables::set_template(&exe, i as u8, &TemplateFields::of(t)).unwrap() == exe);
    }
    let sc = exe_off(&d.exe, lt_core::gamedata::encounters::SCRIPTED_TABLE);
    for (i, s) in data.encounters.scripted.iter().enumerate() {
        let mut r = vec![0u8; 8];
        tables::put_scripted(&mut r, &ScriptedFields { map: s.map, arena: s.arena, species: s.species, area: s.area });
        assert_eq!(r, &exe[sc + i * 8..][..8], "scripted {i}");
    }

    // Drop codes, all 206.
    let ovr4 = d.retail("ovr/4").unwrap();
    for (code, &item) in data.items.drop_remap.iter().enumerate() {
        assert!(tables::set_drop(&ovr4, code as u16, item).unwrap() == ovr4, "drop code {code}");
    }

    // Shops: every list rewritten repacks the pool to the retail bytes.
    let ovr2 = d.retail("ovr/2").unwrap();
    for s in &data.shops.shops {
        let (out, also) = tables::set_shop(&ovr2, Some(&ovr2), s.id, &s.items, false).unwrap();
        assert!(out == ovr2, "shop {}", s.id);
        assert_eq!(also, s.shared_with);
    }

    // Toy ability blocks: phases over zeros (bar the names and bytes +0x15/+0x16), and whole files.
    let mut toys = 0;
    for f in data.toys.roster.iter().filter_map(|t| t.file.as_ref()).chain(&data.toys.unlisted) {
        let file = d.retail(&f.asset).unwrap();
        let s1 = u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize;
        for (k, p) in f.phases.iter().enumerate() {
            let raw = &file[s1 + k * 0x2C..][..0x2C];
            let mut r = raw.to_vec();
            r[0x14] = 0;
            r[0x17..].fill(0);
            tables::put_phase(&mut r, &PhaseFields::of(p)).unwrap();
            assert_eq!(r, raw, "{} phase {k}", f.asset);
        }
        let (out, _) = tables::set_abilities(&file, &AbilityFields::of(f)).unwrap();
        assert!(out == file, "{} abilities", f.asset);
        toys += 1;
    }
    assert_eq!(toys, 73);

    // Table T: every species, zone and story record rewritten changes no file.
    for s in &data.encounters.species {
        let e = Edit::Species { id: s.id, fields: tables::SpeciesFields { group: s.group, level: s.level } };
        assert!(tables::apply(&d, &data, &e).unwrap().files.is_empty(), "species {}", s.id);
    }
    for z in &data.encounters.zones {
        let e = Edit::Zone { id: z.id, fields: ZoneFields { pairs: z.pairs, arena: z.arena, flags: z.flags, area: z.area } };
        assert!(tables::apply(&d, &data, &e).unwrap().files.is_empty(), "zone {}", z.id);
    }
    for b in &data.encounters.story {
        let e = Edit::Story { id: b.id, fields: tables::StoryFields { formation: b.formation, arena: b.arena, flags: b.flags, area: b.area } };
        assert!(tables::apply(&d, &data, &e).unwrap().files.is_empty(), "story {:#x}", b.id);
    }
    eprintln!(
        "byte-exact: {} items, {} roster rows, 2 templates, 2 scripted, {} drop codes, {} shops, {toys} toy blocks, {} species, {} zones, {} story battles",
        data.items.items.len(),
        data.toys.roster.len(),
        data.items.drop_remap.len(),
        data.shops.shops.len(),
        data.encounters.species.len(),
        data.encounters.zones.len(),
        data.encounters.story.len()
    );
}

#[test]
fn item_strings_write_in_place_share_and_repoint() {
    let Some(d) = Disc::open() else { return };
    let retail = d.data();
    let item = |data: &GameData, i: usize| data.items.items[i].clone();

    // A unique name that fits its slot is written over the old one; the pointer stays.
    let candy = item(&retail, 1);
    assert_eq!((candy.name.as_str(), candy.name_shared), ("Candy Bar", 0));
    let mut f = ItemFields::of(&candy);
    f.name = "Toffee".into();
    f.price = 25;
    let out = d.apply(Edit::Item { id: 1, fields: f.clone() }).unwrap();
    assert!(out.also.is_empty());
    let now = item(&d.data(), 1);
    assert_eq!((now.name.as_str(), now.price, now.name_ptr), ("Toffee", 25, candy.name_ptr));
    for i in (0..215).filter(|&i| i != 1) {
        assert_eq!(item(&d.data(), i).name, item(&retail, i).name);
    }
    // Too long, or not ASCII.
    f.name = "x".repeat(candy.name_room as usize + 1);
    assert_eq!(d.apply(Edit::Item { id: 1, fields: f.clone() }).unwrap_err().code, "TEXT_LONG");
    f.name = "Café".into();
    assert_eq!(d.apply(Edit::Item { id: 1, fields: f.clone() }).unwrap_err().code, "TEXT_CHARS");
    // Back to the retail values: the EXE is the disc's again.
    d.apply(Edit::Item { id: 1, fields: ItemFields::of(&candy) }).unwrap();
    assert!(d.changed().is_empty(), "reverted item 1 leaves {:?}", d.changed());

    // A shared description: another table string that reads the same is pointed at…
    let sword = retail.items.items.iter().find(|i| i.desc == "Weapon").unwrap().clone();
    let armor = retail.items.items.iter().find(|i| i.desc == "Armor").unwrap().clone();
    assert!(sword.desc_shared > 40);
    let mut f = ItemFields::of(&sword);
    f.desc = "Armor".into();
    d.apply(Edit::Item { id: sword.id, fields: f.clone() }).unwrap();
    let now = item(&d.data(), sword.id as usize);
    assert_eq!((now.desc.as_str(), now.desc_ptr), ("Armor", armor.desc_ptr));
    assert_eq!(retail.items.items.iter().filter(|i| i.desc == "Weapon").count() - 1, d.data().items.items.iter().filter(|i| i.desc == "Weapon").count());
    // …and back to its own.
    d.apply(Edit::Item { id: sword.id, fields: ItemFields::of(&sword) }).unwrap();
    assert!(d.changed().is_empty());

    // …else it is written over, and every sharer shows it.
    f.desc = "Blade".into();
    let out = d.apply(Edit::Item { id: sword.id, fields: f }).unwrap();
    assert_eq!(out.also.len(), sword.desc_shared as usize);
    assert!(out.also.iter().all(|s| s.starts_with("item ") && s.ends_with("'s description")));
    let data = d.data();
    assert_eq!(data.items.items.iter().filter(|i| i.desc == "Blade").count(), sword.desc_shared as usize + 1);
    d.apply(Edit::Item { id: sword.id, fields: ItemFields::of(&sword) }).unwrap();
    assert!(d.changed().is_empty(), "the shared string written back is the disc's: {:?}", d.changed());
}

#[test]
fn roster_templates_and_scripted_battles() {
    let Some(d) = Disc::open() else { return };
    let retail = d.data();

    // Toy R (row 67) has a 1-character name in a 4-byte slot.
    let r = retail.toys.roster[67].clone();
    assert_eq!((r.name.as_str(), r.name_room), ("R", 3));
    let mut f = ToyFields::of(&r);
    f.name = "Rex".into();
    f.pp = 12;
    d.apply(Edit::Toy { row: 67, fields: f.clone() }).unwrap();
    let now = d.data().toys.roster[67].clone();
    assert_eq!((now.name.as_str(), now.pp, now.row_byte, now.fx), ("Rex", 12, r.row_byte, r.fx));
    f.category = 9;
    assert_eq!(d.apply(Edit::Toy { row: 67, fields: f }).unwrap_err().code, "TOY_CATEGORY");

    let mut t = TemplateFields::of(&retail.growth.templates[0]);
    t.level = 5;
    t.atk = 20;
    t.name = "Hero".into();
    d.apply(Edit::Template { index: 0, fields: t.clone() }).unwrap();
    let k = d.data().growth.templates[0].clone();
    assert_eq!((k.name.as_str(), k.level, k.atk, k.def), ("Hero", 5, 20, retail.growth.templates[0].def));
    t.name = "A twelve char".into();
    assert_eq!(d.apply(Edit::Template { index: 0, fields: t }).unwrap_err().code, "TEXT_LONG");

    let s = &retail.encounters.scripted[0];
    d.apply(Edit::Scripted { index: 0, fields: ScriptedFields { map: s.map, arena: 3, species: 17, area: s.area } }).unwrap();
    let now = &d.data().encounters.scripted[0];
    assert_eq!((now.arena, now.species), (3, 17));
    assert_eq!(d.changed(), ["exe"]);

    d.apply(Edit::Toy { row: 67, fields: ToyFields::of(&r) }).unwrap();
    d.apply(Edit::Template { index: 0, fields: TemplateFields::of(&retail.growth.templates[0]) }).unwrap();
    d.apply(Edit::Scripted { index: 0, fields: ScriptedFields { map: s.map, arena: s.arena, species: s.species, area: s.area } }).unwrap();
    assert!(d.changed().is_empty());
}

#[test]
fn shops_drops_and_abilities() {
    let Some(d) = Disc::open() else { return };
    let retail = d.data();

    // Shop 4's list is shop 6, 8, 10 and 21's too.
    let out = d.apply(Edit::Shop { id: 4, items: vec![1, 2, 3], split: false }).unwrap();
    assert_eq!(out.also, ["shop 6", "shop 8", "shop 10", "shop 21"]);
    let s = d.data().shops;
    for id in [4, 6, 8, 10, 21] {
        assert_eq!(s.shops[id].items, [1, 2, 3], "shop {id}");
    }
    for sh in retail.shops.shops.iter().filter(|s| ![4, 6, 8, 10, 21].contains(&s.id)) {
        assert_eq!(s.shops[sh.id as usize].items, sh.items, "shop {} kept", sh.id);
    }

    // Split shop 6 off with 16 items: the aligned pool no longer fits, the lists pack tight.
    let sixteen: Vec<u8> = (19..35).collect();
    let out = d.apply(Edit::Shop { id: 6, items: sixteen.clone(), split: true }).unwrap();
    assert!(out.also.is_empty());
    let s = d.data().shops;
    assert_eq!(s.shops[6].items, sixteen);
    assert!(s.shops[6].shared_with.is_empty());
    assert_eq!(s.shops[4].shared_with, [8, 10, 21]);
    for sh in retail.shops.shops.iter().filter(|s| ![4, 6, 8, 10, 21].contains(&s.id)) {
        assert_eq!(s.shops[sh.id as usize].items, sh.items, "shop {} kept after the split", sh.id);
    }
    // Shop 6 back to the list it shared on the disc: it shares again, and with shop 4's back too the overlay is the disc's.
    d.apply(Edit::Shop { id: 6, items: retail.shops.shops[6].items.clone(), split: false }).unwrap();
    assert!(d.data().shops.shops[6].shared_with.is_empty(), "shop 4's list still differs");
    d.apply(Edit::Shop { id: 4, items: retail.shops.shops[4].items.clone(), split: false }).unwrap();
    assert_eq!(d.data().shops.shops[6].shared_with, retail.shops.shops[6].shared_with);
    assert!(d.changed().is_empty(), "the split undone leaves {:?}", d.changed());

    // More than the pool holds.
    d.reset();
    let mut err = None;
    for id in 0..23u8 {
        if let Err(e) = d.apply(Edit::Shop { id, items: sixteen.clone(), split: true }) {
            err = Some(e.code);
            break;
        }
    }
    assert_eq!(err, Some("SHOP_POOL"));
    assert_eq!(d.apply(Edit::Shop { id: 0, items: vec![0], split: false }).unwrap_err().code, "SHOP_ITEM");
    assert_eq!(d.apply(Edit::Shop { id: 0, items: vec![1; 17], split: false }).unwrap_err().code, "SHOP_LONG");
    d.reset();

    // Drop code 1 (Candy Bar) gives item 5.
    d.apply(Edit::Drop { code: 1, item: 5 }).unwrap();
    assert_eq!(d.data().items.drop_remap[1], 5);
    assert_eq!(d.changed(), ["ovr/4"]);
    assert_eq!(d.apply(Edit::Drop { code: 206, item: 1 }).unwrap_err().code, "DROP_CODE");
    assert_eq!(d.apply(Edit::Drop { code: 1, item: 300 }).unwrap_err().code, "DROP_ITEM");
    d.apply(Edit::Drop { code: 1, item: retail.items.drop_remap[1] }).unwrap();
    assert!(d.changed().is_empty());

    // Toy 22's first ability renamed and made stronger; its actor taller.
    let file = retail.toys.roster[22].file.clone().unwrap();
    let mut f = AbilityFields::of(&file);
    f.phases[0].name = "Big Hatchet".into();
    f.phases[0].max = 40;
    f.dims[1] += 10;
    let out = d.apply(Edit::Abilities { toy: 22, fields: f.clone() }).unwrap();
    assert!(out.notes.is_empty());
    let now = d.data().toys.roster[22].file.clone().unwrap();
    assert_eq!((now.phases[0].name.as_str(), now.phases[0].max, now.dims[1]), ("Big Hatchet", 40, file.dims[1] + 10));
    let of = |f: &lt_core::gamedata::toys::ToyFile| f.phases[1..].iter().map(PhaseFields::of).collect::<Vec<_>>();
    assert_eq!(of(&now), of(&file));
    f.phases[1].effect = 0x28;
    assert_eq!(d.apply(Edit::Abilities { toy: 22, fields: f }).unwrap().notes, ["Ability 1: effect 0x28 does nothing."]);
    d.apply(Edit::Abilities { toy: 22, fields: AbilityFields::of(&file) }).unwrap();
    assert!(d.changed().is_empty());
}

#[test]
fn encounters_mirror_into_both_ghost_files() {
    let Some(d) = Disc::open() else { return };
    let retail = d.data();
    let z = retail.encounters.zones[65].clone();
    // Knoygn (17) is already here; add a second candidate that fights in the zone's module.
    let fits = retail.enemies.iter().find(|e| e.id != 17 && e.mask & (1 << z.module) != 0 && !z.pairs.iter().flatten().any(|&s| s as u16 == e.id)).unwrap();
    let mut pairs = z.pairs;
    let free = pairs.iter().position(|p| p[0] == 0).unwrap();
    pairs[free] = [fits.id as u8, 17];
    d.apply(Edit::Zone { id: 65, fields: ZoneFields { pairs, arena: z.arena, flags: z.flags, area: z.area } }).unwrap();
    assert_eq!(d.changed(), ["sys/10", "sys/9"]);
    let e = d.data().encounters;
    assert!(e.mirror_identical);
    assert_eq!(e.zones[65].pairs, pairs);
    assert!(d.data().enemy(fits.id).unwrap().homes.iter().any(|h| h.area == 65));

    // A species whose attacks live in another module is refused.
    if let Some(misfit) = retail.enemies.iter().find(|e| e.mask & (1 << z.module) == 0) {
        let mut bad = pairs;
        bad[free] = [misfit.id as u8, 0];
        let err = d.apply(Edit::Zone { id: 65, fields: ZoneFields { pairs: bad, arena: z.arena, flags: z.flags, area: z.area } }).unwrap_err();
        assert_eq!(err.code, "ENCOUNTER_MODULE");
    }
    // A gap in the candidate list, or a species that isn't an enemy.
    let mut gap = z.pairs;
    gap[4] = [17, 0];
    assert_eq!(d.apply(Edit::Zone { id: 65, fields: ZoneFields { pairs: gap, arena: z.arena, flags: z.flags, area: z.area } }).unwrap_err().code, "ZONE_PAIR");
    let mut out_of_range = z.pairs;
    out_of_range[0] = [120, 0];
    assert_eq!(d.apply(Edit::Zone { id: 65, fields: ZoneFields { pairs: out_of_range, arena: z.arena, flags: z.flags, area: z.area } }).unwrap_err().code, "SPECIES_RANGE");

    // Story battle 0x100: one more enemy; four is too many.
    let b = retail.encounters.story[0].clone();
    let mut formation = b.formation;
    formation[1] = [21, 1];
    d.apply(Edit::Story { id: 0x100, fields: tables::StoryFields { formation, arena: b.arena, flags: b.flags, area: b.area } }).unwrap();
    assert_eq!(d.data().encounters.story[0].formation, formation);
    formation[2] = [21, 2];
    assert_eq!(d.apply(Edit::Story { id: 0x100, fields: tables::StoryFields { formation, arena: b.arena, flags: b.flags, area: b.area } }).unwrap_err().code, "STORY_COUNT");

    // @LT-45/3: retail's solo battle 0x105 (Glor for the player, against Ramal) keeps its second enemy.
    let g = retail.encounters.story[5].clone();
    assert!(g.solo() && !retail.encounters.story[9].solo());
    let alone = [g.formation[0], [0, 0], [0, 0]];
    let e = Edit::Story { id: 0x105, fields: tables::StoryFields { formation: alone, arena: g.arena, flags: g.flags, area: g.area } };
    assert_eq!(d.apply(e).unwrap_err().code, "STORY_SOLO_FORMATION");

    // Species record 17's rating level.
    let s = retail.encounters.species[17].clone();
    d.apply(Edit::Species { id: 17, fields: tables::SpeciesFields { group: s.group, level: 9 } }).unwrap();
    assert_eq!(d.data().encounters.species[17].level, 9);

    // Everything back: both files are the disc's again.
    d.apply(Edit::Zone { id: 65, fields: ZoneFields { pairs: z.pairs, arena: z.arena, flags: z.flags, area: z.area } }).unwrap();
    d.apply(Edit::Story { id: 0x100, fields: tables::StoryFields { formation: b.formation, arena: b.arena, flags: b.flags, area: b.area } }).unwrap();
    d.apply(Edit::Species { id: 17, fields: tables::SpeciesFields { group: s.group, level: s.level } }).unwrap();
    assert!(d.changed().is_empty());
}
