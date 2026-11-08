//! Event flags: 512 bits at `g_EventFlags` 0x801CE810 (16 × u32, flag n = word
//! n/32, bit n%32), the bundled catalogue, and a cross-reference index over
//! every NPC pack. Spec: `docs/screens/17-event-flags.md` §4–5.
//!
//! The catalogue is `GuardiansCrusade_Docs/tools/data/event_flags.json`
//! (from `flag_catalogue.py`), copied read-only into `crates/lt-core/data/`.
//! Its file names are the decomp's 0-based `NPCnn` (`NPC07` = `npc/7`); five
//! harvested ids above 0x1FF are misparse artefacts and are dropped.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::npc::NpcPack;
use crate::placement::{self, WalkRead};
use crate::text;
use crate::tier2::{self, Operand};
use crate::trigger::{Outcome, TriggerRecord};

pub const COUNT: u16 = 512;
/// The most a HackerGC base can have: its extra flags 0x200–0x29F live in the
/// save block's 20 spare bytes (gc_decomp `src/hgc/hgc_flags.c`; a base says
/// how many it has in `base.json` `profile.flagCount`).
pub const MAX_COUNT: u16 = 0x2A0;

/// Flags the open engine has: [`COUNT`] on retail, a base's `flagCount`.
pub fn count() -> u16 {
    (crate::engine::active().flag_count as u16).clamp(COUNT, MAX_COUNT)
}
/// Bits `ResetAllEventFlags` sets at a new game.
pub const NEW_GAME: [u16; 3] = [0xFC, 0xFE, 0xFF];
/// Chapter flags in the order the game checks them (first set wins), with their number.
pub const CHAPTERS: [(u16, u8); 4] = [(0xAB, 4), (0x9D, 3), (0x86, 2), (0xF9, 1)];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Band {
    /// 0–63: reused by every area; `SET 0xFD` wipes them.
    Scratch,
    /// 64–239.
    Story,
    /// 0xF0–0xFF: read by the engine.
    Engine,
    /// 0x100–0x1FF.
    StoryHigh,
}

pub fn band(n: u16) -> Band {
    match n {
        0..=63 => Band::Scratch,
        0xF0..=0xFF => Band::Engine,
        64..=0xEF => Band::Story,
        _ => Band::StoryHigh,
    }
}

/// The game's chapter: the first of 0xAB, 0x9D, 0x86, 0xF9 that is set (0 = prologue).
pub fn chapter(set: impl Fn(u16) -> bool) -> u8 {
    CHAPTERS.iter().find(|(f, _)| set(*f)).map_or(0, |c| c.1)
}

/* ───────────────────────── Catalogue ───────────────────────── */

const CATALOGUE_JSON: &str = include_str!("../data/event_flags.json");

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogueEntry {
    /// `[file, text]`: lines after which the flag is set.
    #[serde(default)]
    pub set_after: Vec<(String, String)>,
    /// `[file, text]`: lines the flag gates.
    #[serde(default)]
    pub gates: Vec<(String, String)>,
    /// `[file, item]`: chests that use the flag.
    #[serde(default)]
    pub chest: Vec<(String, i64)>,
    #[serde(default)]
    pub files: Vec<String>,
    /// Hand-written note for engine-read flags.
    #[serde(default)]
    pub known: Option<String>,
}

/// The catalogue keyed by flag id (ids ≥ 512 dropped).
pub fn catalogue() -> BTreeMap<u16, CatalogueEntry> {
    let raw: BTreeMap<String, CatalogueEntry> = serde_json::from_str(CATALOGUE_JSON).unwrap_or_default();
    raw.into_iter()
        .filter_map(|(k, v)| {
            let id = u16::from_str_radix(k.trim_start_matches("0x"), 16).ok()?;
            (id < COUNT).then_some((id, v))
        })
        .collect()
}

/// `NPC07` → 7.
fn catalogue_pack(file: &str) -> Option<u16> {
    file.strip_prefix("NPC")?.parse().ok()
}

/// One-line summary of a catalogue entry (the screen's "catalogue hint").
pub fn hint(e: &CatalogueEntry) -> String {
    let mut h = match e.set_after.first() {
        Some((file, text)) => {
            let first = text.split("\\n").next().unwrap_or("").replace(" / ", " ");
            let pack = catalogue_pack(file).map(|p| format!(" (npc/{p})")).unwrap_or_default();
            let mut s = format!("Set after “{}”{pack}", first.trim());
            let more = e.set_after.len() - 1;
            if more > 0 {
                s.push_str(&format!(" and {more} more line{}", if more > 1 { "s" } else { "" }));
            }
            s.push('.');
            s
        }
        None => "No set site in the catalogue.".to_string(),
    };
    if !e.gates.is_empty() {
        let n = e.gates.len();
        h.push_str(&format!(" Gates {n} line{}.", if n != 1 { "s" } else { "" }));
    }
    if !e.chest.is_empty() {
        h.push_str(&format!(" Chest flag ({}).", e.chest.len()));
    }
    h
}

/* ───────────────────────── Curated names ───────────────────────── */

/// The app owns the file (`app/src/data/`, where the dev server watches it).
const CURATED_JSON: &str = include_str!("../../../src/data/flag-catalogue.json");

/// One curated meaning (`app/src/data/flag-catalogue.json`, the app's Flag
/// list). Scratch flags are keyed per pack (`0x004@npc/7`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Meaning {
    /// DSL-safe label (`[a-z][a-z0-9_]*`, ≤ 32 chars).
    pub name: String,
    /// The state that holds while the flag is set (≤ 60 chars).
    pub summary: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub area: String,
    #[serde(default)]
    pub chapter: Option<u8>,
    /// `confirmed`, `likely` or `guess`.
    pub confidence: String,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub related: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CuratedFile {
    #[serde(default)]
    flags: BTreeMap<String, Meaning>,
    #[serde(default)]
    places: BTreeMap<String, String>,
}

/// `"0x004@npc/7"` → (4, Some(7)); `"0x081"` → (0x81, None).
pub fn parse_meaning_key(key: &str) -> Option<(u16, Option<u16>)> {
    let (id, pack) = match key.split_once('@') {
        Some((id, pack)) => (id, Some(pack.strip_prefix("npc/")?.parse().ok()?)),
        None => (key, None),
    };
    let flag = u16::from_str_radix(id.strip_prefix("0x")?, 16).ok()?;
    (flag < count()).then_some((flag, pack))
}

/// The curated meanings keyed by (flag, pack); unparseable keys are dropped.
pub fn meanings() -> BTreeMap<(u16, Option<u16>), Meaning> {
    let file: CuratedFile = serde_json::from_str(CURATED_JSON).unwrap_or_default();
    file.flags.into_iter().filter_map(|(k, v)| Some((parse_meaning_key(&k)?, v))).collect()
}

/// The place each pack serves ("Orgo", "Isten", …): the catalogue's place
/// description up to its " — ". Packs of one place share scratch flags, which
/// last until a script runs `SET 0xFD`.
pub fn places() -> BTreeMap<u16, String> {
    let file: CuratedFile = serde_json::from_str(CURATED_JSON).unwrap_or_default();
    file.places
        .into_iter()
        .filter_map(|(k, v)| Some((k.strip_prefix("npc/")?.parse().ok()?, v.split(" — ").next().unwrap_or(&v).to_string())))
        .collect()
}

/* ───────────────────────── Cross-reference index ───────────────────────── */

/// One reference to a flag, as a compact row:
/// `[kind, pack, actor, at, chunk, extra, lane]`.
///
/// - kind `S` set · `C` clear · `T` test (IF_FLAG / IF_FLAG2) · `H` chest (extra = item) ·
///   `R` trigger rung condition (actor = talk target or −1, at = rung number 1-based,
///   extra = rungs in the ladder) · `s`/`c` Tier-1 set/clear record · `P` placement
///   condition (at = D-offset of the condition word, extra = 0 placed · 1 absent · 2 on arrival) ·
///   `W` walk-script wait (`0nnn`) · `w` walk-script hide (`7nnn`) or re-place (`Bnnn`)
///   (at = D-offset of the walk word, extra = the condition of the ladder branch that walks, 0 always)
/// - `at` is the D-relative instruction offset for S/C/T/H
/// - `chunk` is the actor's home chunk (record `b`) or the trigger group
/// - `extra` for S/C/T is an index into [`FlagIndex::says`]: the line before a
///   SET/CLEAR, or the first line after a test (−1 when there is none)
/// - `lane` (S/C/H): 0 outside any IF, 1 then lane, 2 else lane; (W/w): 0 wait · 1 hide · 2 re-place
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlagSite(pub char, pub u16, pub i32, pub i32, pub i32, pub i32, pub u8);

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagIndex {
    /// Packs indexed (`npc/17` is skipped: it aliases `npc/0`).
    pub packs: Vec<u16>,
    /// One letter per flag: S scratch (0–63), R engine (0xF0–0xFF), T catalogued,
    /// r referenced by data but not catalogued, U neither.
    pub cats: String,
    /// Sites per flag id (only flags with sites).
    pub sites: BTreeMap<u16, Vec<FlagSite>>,
    /// First page of nearby lines (markup, no box prefix), deduplicated.
    pub says: Vec<String>,
    /// First speaker of each actor script, keyed `"pack:actor"`.
    pub speakers: BTreeMap<String, String>,
    /// Per pack: [script `SET 0xFD` sites, Tier-1 records that set 0xFD].
    pub wipes: BTreeMap<u16, [u32; 2]>,
    /// Catalogue hint per flag.
    pub hints: BTreeMap<u16, String>,
    /// The catalogue's notes on engine-read flags.
    pub known: BTreeMap<u16, String>,
    /// Distinct quest-log objectives (opcode 0x44): text and `[pack, actor, D-offset]` sites.
    pub objectives: Vec<(String, Vec<[u32; 3]>)>,
}

/// Markup without the leading `{box:n}`.
fn body(markup: &str) -> &str {
    if markup.starts_with("{box:") {
        if let Some(end) = markup.find('}') {
            return &markup[end + 1..];
        }
    }
    markup
}

#[derive(Default)]
pub struct IndexBuilder {
    index: FlagIndex,
    say_ids: BTreeMap<String, i32>,
    objective_ids: BTreeMap<String, usize>,
}

impl IndexBuilder {
    fn say(&mut self, d: &[u8], at: Option<u16>) -> i32 {
        let Some(at) = at else { return -1 };
        let m = text::markup_at(d, at as usize);
        let first: String = body(&m).split("{wait}").next().unwrap_or("").chars().take(110).collect();
        if let Some(&id) = self.say_ids.get(&first) {
            return id;
        }
        let id = self.index.says.len() as i32;
        self.say_ids.insert(first.clone(), id);
        self.index.says.push(first);
        id
    }

    fn site(&mut self, flag: u16, s: FlagSite) {
        if flag < count() {
            self.index.sites.entry(flag).or_default().push(s);
        }
    }

    /// Adds every Tier-2, chest, placement, walk-script and Tier-1 reference of one pack.
    pub fn add_pack(&mut self, index: u16, bytes: &[u8], pack: &NpcPack) {
        let d = pack.d_block(bytes);
        let mut wipes = [0u32; 2];
        self.index.packs.push(index);
        for a in &pack.actors {
            let chunk = (a.b & 0xFFFF) as i32;
            let Ok(code) = tier2::disassemble(d, a.script) else { continue };
            let speaker = code.iter().find_map(|i| match i.operands.first() {
                Some(Operand::Name(t)) => Some(body(&text::markup_at(d, *t as usize)).to_string()),
                _ => None,
            });
            if let Some(s) = speaker.filter(|s| !s.is_empty()) {
                self.index.speakers.insert(format!("{index}:{}", a.slot), s);
            }
            let mut last_say: Option<u16> = None;
            let mut lanes: Vec<u8> = Vec::new();
            for (k, i) in code.iter().enumerate() {
                let lane = lanes.last().copied().unwrap_or(0);
                let u = match i.operands.first() {
                    Some(Operand::Text(t)) | Some(Operand::Flag(t)) | Some(Operand::Objective(t)) => Some(*t),
                    _ => None,
                };
                match i.op {
                    0x01..=0x03 => last_say = u,
                    0x04 | 0x05 | 0x0B | 0x2D => {
                        let flag = u.unwrap_or(0);
                        if i.op == 0x04 && flag == 0xFD {
                            wipes[0] += 1;
                        }
                        let (kind, say_at) = match i.op {
                            0x04 => ('S', last_say),
                            0x05 => ('C', last_say),
                            // Tests: the first line after the test says more than the one before it.
                            _ => (
                                'T',
                                code[k + 1..]
                                    .iter()
                                    .find(|j| (0x01..=0x03).contains(&j.op))
                                    .and_then(|j| match j.operands.first() {
                                        Some(Operand::Text(t)) => Some(*t),
                                        _ => None,
                                    })
                                    .or(last_say),
                            ),
                        };
                        let say = self.say(d, say_at);
                        self.site(flag, FlagSite(kind, index, a.slot as i32, i.at as i32, chunk, say, if kind == 'T' { 0 } else { lane }));
                    }
                    0x28 => {
                        if let [Operand::Flag(f), Operand::Item(item)] = i.operands[..] {
                            self.site(f, FlagSite('H', index, a.slot as i32, i.at as i32, chunk, item as i32, lane));
                        }
                    }
                    0x44 => {
                        if let Some(t) = u {
                            let text = body(&text::markup_at(d, t as usize)).to_string();
                            let n = self.index.objectives.len();
                            let id = *self.objective_ids.entry(text.clone()).or_insert(n);
                            if id == n {
                                self.index.objectives.push((text, Vec::new()));
                            }
                            self.index.objectives[id].1.push([index as u32, a.slot, i.at]);
                        }
                    }
                    _ => {}
                }
                match i.op {
                    0x0D => lanes.push(1),
                    0x0E => {
                        if let Some(top) = lanes.last_mut() {
                            *top = 2;
                        }
                    }
                    0x0F => {
                        lanes.pop();
                    }
                    _ => {}
                }
            }
            for br in crate::trigger::placement_ladder(d, a.placement) {
                if br.cond > 0 && br.cond < count() {
                    let outcome = match br.outcome {
                        Outcome::Place { .. } => 0,
                        Outcome::Absent => 1,
                        Outcome::Arrival => 2,
                    };
                    self.site(br.cond, FlagSite('P', index, a.slot as i32, br.at as i32, chunk, outcome, 0));
                }
            }
            // Walk scripts. The party (slots 0, 1) and type-4/5 objects never run theirs.
            let typ = (a.d >> 8) as u8;
            if a.slot >= 2 && !matches!(typ, 4 | 5) {
                for w in placement::walk_flags(d, a.placement, typ) {
                    let (kind, lane) = match w.read {
                        WalkRead::Wait => ('W', 0),
                        WalkRead::Hide => ('w', 1),
                        WalkRead::Replace => ('w', 2),
                    };
                    self.site(w.flag, FlagSite(kind, index, a.slot as i32, w.at as i32, chunk, w.cond as i32, lane));
                }
            }
        }
        for g in pack.triggers(bytes) {
            let n = g.rungs.len() as i32;
            for (k, r) in g.rungs.iter().enumerate() {
                if r.flag != 0 {
                    let talk = r.records.iter().find_map(|x| match x {
                        TriggerRecord::Talk { actor, .. } => Some(*actor as i32),
                        _ => None,
                    });
                    self.site(r.flag, FlagSite('R', index, talk.unwrap_or(-1), k as i32 + 1, g.chunk as i32, n, 0));
                }
                for x in &r.records {
                    match *x {
                        TriggerRecord::Set { flag, .. } => {
                            if flag == 0xFD {
                                wipes[1] += 1;
                            }
                            self.site(flag, FlagSite('s', index, -1, -1, g.chunk as i32, -1, 0));
                        }
                        TriggerRecord::Clear { flag, .. } => self.site(flag, FlagSite('c', index, -1, -1, g.chunk as i32, -1, 0)),
                        _ => {}
                    }
                }
            }
        }
        self.index.wipes.insert(index, wipes);
    }

    /// Adds the catalogue's hints and the categories.
    pub fn finish(mut self) -> FlagIndex {
        let cat = catalogue();
        for (&id, e) in &cat {
            self.index.hints.insert(id, hint(e));
            if let Some(k) = &e.known {
                self.index.known.insert(id, k.clone());
            }
        }
        self.index.cats = (0..count())
            .map(|f| match f {
                0..=63 => 'S',
                0xF0..=0xFF => 'R',
                f if cat.contains_key(&f) => 'T',
                f if self.index.sites.contains_key(&f) => 'r',
                _ => 'U',
            })
            .collect();
        self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_loads() {
        let c = catalogue();
        assert_eq!(c.len(), 297, "302 keys, five artefacts above 0x1FF dropped");
        assert!(c[&0xF8].known.as_deref().unwrap().contains("no-op"));
        assert_eq!(band(0x0E), Band::Scratch);
        assert_eq!(band(0x186), Band::StoryHigh);
        assert_eq!(chapter(|f| f == 0x86 || f == 0xF9), 2);
        assert!(hint(&c[&0x01]).starts_with("Set after “You're lucky you survived!” (npc/1) and"));
    }

    /// The curated list parses, and its names work as DSL labels: unique among the
    /// global meanings, unique per pack for scratch ones, short summaries.
    #[test]
    fn curated_meanings_are_well_formed() {
        let raw: serde_json::Value = serde_json::from_str(CURATED_JSON).expect("flag-catalogue.json parses");
        let keys: Vec<&String> = raw["flags"].as_object().expect("a flags object").keys().collect();
        let m = meanings();
        assert_eq!(m.len(), keys.len(), "every key parses: {keys:?}");
        let mut seen = std::collections::BTreeSet::new();
        for (&(flag, pack), e) in &m {
            let key = format!("0x{flag:03X}{}", pack.map(|p| format!("@npc/{p}")).unwrap_or_default());
            assert!(keys.iter().any(|k| **k == key), "{key}: keys are three upper-case hex digits");
            assert!(pack.is_none() || flag < 0x40, "{key}: only scratch flags are per pack");
            let ok = e.name.len() <= 32
                && e.name.starts_with(|c: char| c.is_ascii_lowercase())
                && e.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            assert!(ok, "{key}: name {:?} isn't a DSL label", e.name);
            assert!(seen.insert((pack, e.name.clone())), "{key}: name {:?} is used twice in one scope", e.name);
            assert!(e.summary.chars().count() <= 60, "{key}: summary over 60 characters");
            assert!(["confirmed", "likely", "guess"].contains(&e.confidence.as_str()), "{key}: confidence {:?}", e.confidence);
            assert!(e.chapter.is_none_or(|c| (1..=4).contains(&c)), "{key}: chapter");
            for r in &e.related {
                assert!(parse_meaning_key(r).is_some(), "{key}: related {r:?} isn't a key");
            }
        }
    }
}
