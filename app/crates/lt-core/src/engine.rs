//! The retail engine profile: ids the C code reacts to by number (research
//! 06 §4.2). A new story that reuses one of them gets the retail behaviour
//! too, so the editors warn about them. The list lives in
//! `src/data/engine-reserved.json`, shared with the frontend; a modded base
//! would ship its own (research 06 §6.4, `base.json` `reserved`).

use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Deserializer, Serialize};

use crate::tier2::ast::{self, Flat, Kind, Val};
use crate::tier2::dsl::Diag;

const RESERVED_JSON: &str = include_str!("../../../src/data/engine-reserved.json");

/// What a reserved id is used for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reserved {
    pub from: u32,
    pub to: u32,
    pub why: String,
    /// Where the decomp reads it (`file:line`).
    pub code: String,
}

impl Reserved {
    fn holds(&self, v: u32) -> bool {
        (self.from..=self.to).contains(&v)
    }
}

/// Every reserved id of the retail engine.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub flags: Vec<Reserved>,
    pub chunks: Vec<Reserved>,
    pub battles: Vec<Reserved>,
    pub packs: Vec<Reserved>,
    /// The HackerGC EXT opcode's version (0x4A, [`crate::tier2::OP_EXT`]): 0
    /// on retail, where the VM hangs on it; a base sets it (`profile.tier2Ext`).
    pub tier2_ext: u32,
    /// Event flags the engine has: 512 on retail; a HackerGC base sets more
    /// (`profile.flagCount`, at most [`crate::flags::MAX_COUNT`]).
    pub flag_count: u32,
}

fn id<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        N(u32),
        S(String),
    }
    match Raw::deserialize(d)? {
        Raw::N(n) => Ok(n),
        Raw::S(s) => {
            let t = s.trim();
            let parsed = match t.strip_prefix("0x") {
                Some(h) => u32::from_str_radix(h, 16),
                None => t.parse(),
            };
            parsed.map_err(|_| serde::de::Error::custom(format!("not an id: {s}")))
        }
    }
}

fn opt_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    id(d).map(Some)
}

#[derive(Deserialize)]
struct RawItem {
    #[serde(deserialize_with = "id")]
    from: u32,
    #[serde(default, deserialize_with = "opt_id")]
    to: Option<u32>,
    why: String,
    code: String,
}

#[derive(Deserialize)]
struct RawProfile {
    flags: Vec<RawItem>,
    chunks: Vec<RawItem>,
    battles: Vec<RawItem>,
    packs: Vec<RawItem>,
}

fn items(raw: Vec<RawItem>) -> Vec<Reserved> {
    raw.into_iter().map(|r| Reserved { from: r.from, to: r.to.unwrap_or(r.from), why: r.why, code: r.code }).collect()
}

/// The retail profile.
pub fn retail() -> &'static Profile {
    static P: OnceLock<Profile> = OnceLock::new();
    P.get_or_init(|| {
        let raw: RawProfile = serde_json::from_str(RESERVED_JSON).expect("engine-reserved.json parses");
        Profile { flags: items(raw.flags), chunks: items(raw.chunks), battles: items(raw.battles), packs: items(raw.packs), tier2_ext: 0, flag_count: crate::flags::COUNT as u32 }
    })
}

static ACTIVE: RwLock<Option<Arc<Profile>>> = RwLock::new(None);

/// The profile the editors check against: retail, plus the reserved ids of
/// the open project's base ([`set_active`]).
pub fn active() -> Arc<Profile> {
    if let Some(p) = ACTIVE.read().expect("profile lock").as_ref() {
        return p.clone();
    }
    Arc::new(retail().clone())
}

/// Sets (or with `None` clears) the open project's profile.
pub fn set_active(p: Option<Profile>) {
    *ACTIVE.write().expect("profile lock") = p.map(Arc::new);
}

impl Profile {
    /// This profile plus the ids `base.json` reserves (`reserved`).
    pub fn with_base(&self, m: &crate::base::BaseManifest) -> Profile {
        let mut p = self.clone();
        // Retail entries the base released, whole: a band the base only partly freed stays.
        let release = |list: &mut Vec<Reserved>, ranges: &[String]| {
            let freed = crate::base::parse_ranges(ranges);
            list.retain(|r| !freed.iter().any(|&(a, b)| a <= r.from && r.to <= b));
        };
        release(&mut p.flags, &m.released.flags);
        release(&mut p.battles, &m.released.battles);
        release(&mut p.chunks, &m.released.chunks);
        release(&mut p.packs, &m.released.packs);
        let add = |list: &mut Vec<Reserved>, ranges: &[String]| {
            for (from, to) in crate::base::parse_ranges(ranges) {
                list.push(Reserved { from, to, why: format!("Reserved by the base {}", m.tag()), code: "base.json".into() });
            }
        };
        add(&mut p.flags, &m.reserved.flags);
        add(&mut p.battles, &m.reserved.battles);
        add(&mut p.chunks, &m.reserved.chunks);
        p.tier2_ext = m.profile.get("tier2Ext").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if let Some(n) = m.profile.get("flagCount").and_then(|v| v.as_u64()) {
            p.flag_count = (n as u32).clamp(crate::flags::COUNT as u32, crate::flags::MAX_COUNT as u32);
        }
        p
    }
}

/// The narrowest entry holding `v` (a single id beats the band around it).
fn find(list: &[Reserved], v: u32) -> Option<&Reserved> {
    list.iter().filter(|r| r.holds(v)).min_by_key(|r| r.to - r.from)
}

impl Profile {
    pub fn flag(&self, f: u32) -> Option<&Reserved> {
        find(&self.flags, f)
    }
    pub fn chunk(&self, c: u32) -> Option<&Reserved> {
        find(&self.chunks, c)
    }
    pub fn battle(&self, b: u32) -> Option<&Reserved> {
        find(&self.battles, b)
    }
    pub fn pack(&self, p: u32) -> Option<&Reserved> {
        find(&self.packs, p)
    }

    /// Errors for HackerGC's EXT (0x4A) where this engine can't run it: on
    /// retail at all, and sub-ops past the ones its version has.
    pub fn ext_checks(&self, stmts: &[ast::Stmt]) -> Vec<Diag> {
        let mut out = Vec::new();
        for f in ast::flatten(stmts) {
            let Flat::Op { op, args, line } = f else { continue };
            if op != crate::tier2::OP_EXT {
                continue;
            }
            if self.tier2_ext == 0 {
                out.push(Diag::error(line, 0, "EXT_NEEDS_HACKERGC", "`ext` runs only on a HackerGC base: the retail VM hangs on opcode 0x4A."));
                continue;
            }
            if let Some(Val::Num(sub)) = args.first() {
                if *sub != crate::tier2::EXT_RUN_OVERLAY as i64 {
                    out.push(Diag::error(line, 0, "EXT_SUB", format!("This base's `ext` has no sub-op {sub} (0: run an overlay).")));
                }
            }
        }
        out
    }

    /// Info diagnostics for the reserved flags, chunks and story battles a
    /// program names. Engine flags a scene is meant to drive (0x0FC, 0x0FE,
    /// 0x0FF) are named too, so the author sees what they do.
    pub fn script_notes(&self, stmts: &[ast::Stmt]) -> Vec<Diag> {
        let mut out = Vec::new();
        let num = |v: &Val| match v {
            Val::Num(n) if *n >= 0 => Some(*n as u32),
            _ => None,
        };
        for f in ast::flatten(stmts) {
            let Flat::Op { op, args, line } = f else { continue };
            for ((_, kind), v) in ast::mnemonic(op).args.iter().zip(args.iter()) {
                let Some(n) = num(v) else { continue };
                match kind {
                    Kind::Flag => {
                        if let Some(r) = self.flag(n) {
                            out.push(Diag::info(line, "ENGINE_FLAG", format!("Flag 0x{n:03X} is read by the engine: {}.", r.why)));
                        }
                    }
                    Kind::Chunk => {
                        if let Some(r) = self.chunk(n) {
                            out.push(Diag::info(line, "ENGINE_CHUNK", format!("chunk/{n} has code of its own: {}.", r.why)));
                        }
                    }
                    Kind::MapId if op == 0x08 => {
                        if let Some(r) = self.battle(n) {
                            out.push(Diag::info(line, "ENGINE_BATTLE", format!("Battle 0x{n:03X}: {}.", r.why)));
                        }
                    }
                    _ => {}
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_retail_profile_parses_and_finds_the_narrowest_entry() {
        let p = retail();
        assert!(p.flags.len() > 20 && p.chunks.len() > 15 && !p.battles.is_empty() && !p.packs.is_empty());
        assert!(p.flag(0x0FF).unwrap().why.contains("Knight is hidden"));
        assert!(p.flag(0x0F0).unwrap().why.contains("Cult-raid"), "a single id beats the 0x0EE–0x0F7 band");
        assert!(p.flag(0x0F1).unwrap().why.contains("Engine band"));
        assert!(p.flag(0x18).is_some() && p.flag(0x1D).is_none());
        assert_eq!(p.chunk(326).unwrap().from, 326);
        assert!(p.chunk(271).is_some() && p.chunk(78).is_none());
        assert!(p.battle(0x11E).unwrap().why.contains("ending"));
        assert!(p.battle(0x105).unwrap().why.contains("without the Baby"));
        assert!(p.pack(10).is_some());
    }

    #[test]
    fn ext_is_refused_on_retail_and_runs_on_a_base_that_has_it() {
        let (stmts, d) = crate::tier2::dsl::parse("ext 0 3 -20\nend\n");
        assert!(d.is_empty(), "{d:?}");
        let any = |_: &crate::tier2::ast::TextRef| Ok(0u16);
        let bytes = crate::tier2::asm::assemble(&stmts, 0, &any).unwrap();
        assert_eq!(bytes, [0x4A, 0x00, 0x03, 0x00, 0xEC, 0xFF, 0x00]);
        assert_eq!(retail().ext_checks(&stmts).iter().map(|d| d.code).collect::<Vec<_>>(), ["EXT_NEEDS_HACKERGC"]);
        let hgc = Profile { tier2_ext: 1, ..retail().clone() };
        assert!(hgc.ext_checks(&stmts).is_empty());
        let (bad, _) = crate::tier2::dsl::parse("ext 7 0 0\n");
        assert_eq!(hgc.ext_checks(&bad).iter().map(|d| d.code).collect::<Vec<_>>(), ["EXT_SUB"]);
    }

    #[test]
    fn a_base_releases_the_retail_ids_it_took_out() {
        let mut m = crate::base::manifest_template("hackergc-blank", "0.5.0");
        m.released.flags = vec!["0x0E6".into(), "0x0F0".into()];
        m.released.chunks = vec!["39".into(), "270-272".into(), "413".into()];
        m.released.packs = vec!["4".into()];
        m.reserved.chunks = vec!["39".into()];
        let p = retail().with_base(&m);
        assert!(retail().flag(0xE6).is_some() && p.flag(0xE6).is_none());
        // 0xF0 sits in the engine band 0xEE-0xF7 too: releasing the single id leaves the band.
        assert!(p.flag(0xF0).is_some_and(|r| r.from == 0xEE));
        assert!(p.chunk(270).is_none() && p.chunk(413).is_none() && p.pack(4).is_none() && p.pack(6).is_some());
        // What the base reserves for itself still shows, even on an id it released from retail.
        assert!(p.chunk(39).is_some_and(|r| r.code == "base.json"));
        assert!(p.chunk(414).is_some(), "414 wasn't released");
    }

    #[test]
    fn a_base_with_more_flags_takes_them_and_no_more() {
        let (stmts, d) = crate::tier2::dsl::parse("set 0x210\nif flag(0x29F) {\n clear 0x29F\n} else {\n}\nset 0x2A0\n");
        assert!(d.is_empty(), "{d:?}");
        let codes = |flags: u32| {
            let lim = crate::tier2::asm::Limits { actors: 1, pool: &|_| true, code: &|_| true, d_span: 0, flags, areas: &[] };
            crate::tier2::asm::validate(&stmts, &lim).into_iter().map(|d| (d.line, d.code)).collect::<Vec<_>>()
        };
        assert_eq!(codes(512), [(1, "FLAG_RANGE"), (2, "FLAG_RANGE"), (3, "FLAG_RANGE"), (6, "FLAG_RANGE")]);
        assert_eq!(codes(0x2A0), [(6, "FLAG_RANGE")]);
        assert_eq!(retail().flag_count, 512);
        let mut f = crate::scene::world::Flags::default();
        f.set(0x210);
        assert!(!f.get(0x210), "the retail scene player has 512 flags");
    }
}
