//! Where every TSQ + TVB pair lives, which sound slot and SPU region it is
//! loaded to, and the caps it has to fit. `docs/screens/14-audio.md` §4.1, §4.7, §6.

use serde::Serialize;

use crate::container::{self, le32};
use crate::fs::{FileTable, Kind};

use super::tvb::{self, Tvb};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BankKind {
    /// `bgm/n`: `u32 tsqOfs (=8), u32 tvbOfs` + TSQ + TVB.
    Music,
    /// `sys/0` SE.TSQ + `sys/1` SE.TVB (two files), loaded at boot.
    SystemSe,
    /// `sys/8` SE2.DAT: TSQ at +8, TVB at +0x408.
    FieldSe,
    /// `npcsnd/n`: the BGM layout.
    NpcSound,
    /// `toy/n`: sections 3 (TSQ) and 5 (TVB) of 8.
    Toy,
    /// `enemy/n` (n ≥ 2): sections 2 and 4 of 7.
    Enemy,
    /// `pb/n`: sections 2 and 3 of 6.
    BabyForm,
}

/// One bank in the catalogue: its canonical id and the files it reads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BankRef {
    /// `bgm/0`, `sys/0` (SE: both files), `sys/8`, `npcsnd/3`, `toy/22`, `enemy/17`, `pb/0`.
    pub id: String,
    pub kind: BankKind,
    pub index: u16,
    /// Global file-table indices: `[tsq, tvb]` for SE, otherwise one file.
    pub files: Vec<u16>,
}

/// Every sound bank on the disc, in list order.
pub fn catalog(table: &FileTable) -> Vec<BankRef> {
    let mut out = Vec::new();
    let mut push = |id: String, kind: BankKind, index: u16, files: Vec<u16>| out.push(BankRef { id, kind, index, files });
    for e in table.of_kind(Kind::Bgm) {
        push(e.id.clone(), BankKind::Music, e.index, vec![e.global]);
    }
    if let (Some(t), Some(v)) = (table.by_id("sys/0"), table.by_id("sys/1")) {
        push("sys/0".into(), BankKind::SystemSe, 0, vec![t.global, v.global]);
    }
    if let Some(e) = table.by_id("sys/8") {
        push("sys/8".into(), BankKind::FieldSe, 8, vec![e.global]);
    }
    for e in table.of_kind(Kind::NpcSnd) {
        push(e.id.clone(), BankKind::NpcSound, e.index, vec![e.global]);
    }
    for e in table.of_kind(Kind::Toy) {
        push(e.id.clone(), BankKind::Toy, e.index, vec![e.global]);
    }
    // enemy/0 and enemy/1 are the battle Knight files (KNIGHT02/03): no bank.
    for e in table.of_kind(Kind::Enemy).filter(|e| e.index >= 2) {
        push(e.id.clone(), BankKind::Enemy, e.index, vec![e.global]);
    }
    for e in table.of_kind(Kind::Pb) {
        push(e.id.clone(), BankKind::BabyForm, e.index, vec![e.global]);
    }
    out
}

/// Resolves a bank id; `sys/1` (SE.TVB) names the same bank as `sys/0`.
pub fn find<'a>(catalog: &'a [BankRef], id: &str) -> Option<&'a BankRef> {
    let id = if id == "sys/1" { "sys/0" } else { id };
    catalog.iter().find(|b| b.id == id)
}

/// A loaded bank: the TSQ and TVB bytes plus where they sat in the file.
#[derive(Debug, Clone)]
pub struct Bank {
    pub id: String,
    pub kind: BankKind,
    pub index: u16,
    pub tsq: Vec<u8>,
    /// The TVB region (header + ADPCM, possibly followed by padding or the
    /// next section).
    pub tvb: Vec<u8>,
    pub tsq_offset: u32,
    pub tvb_offset: u32,
    /// Bytes the file really uses (for BGM-style files: TVB end), and its
    /// sector-padded size on disc. For SE these describe SE.TVB.
    pub file_bytes: u32,
    pub file_sectors: u32,
    /// SE.TSQ only: its own file's sectors.
    pub tsq_file_sectors: Option<u32>,
    /// Battle name from the enemy-layout profile (enemies, Baby forms 1–18).
    pub name: Option<String>,
}

fn sectors_of(len: usize) -> u32 {
    len.div_ceil(2048) as u32
}

/// Splits the file(s) of `r` into a [`Bank`]. `files` are the raw contents of
/// `r.files`, in order. Returns `None` if the layout does not hold.
pub fn split(r: &BankRef, files: &[Vec<u8>]) -> Option<Bank> {
    let f = files.first()?;
    let mut name = None;
    let (tsq, tvb) = match r.kind {
        BankKind::Music | BankKind::NpcSound | BankKind::FieldSe => {
            if f.len() < 8 {
                return None;
            }
            let (a, b) = (le32(f, 0) as usize, le32(f, 4) as usize);
            if a < 8 || b <= a || b + tvb::HEADER > f.len() {
                return None;
            }
            ((a, b - a), (b, f.len() - b))
        }
        BankKind::SystemSe => {
            let v = files.get(1)?;
            let t = Tvb::parse(v)?;
            let file_bytes = t.total_bytes() as u32;
            return Some(Bank {
                id: r.id.clone(),
                kind: r.kind,
                index: r.index,
                tsq: f.clone(),
                tvb: v.clone(),
                tsq_offset: 0,
                tvb_offset: 0,
                file_bytes,
                file_sectors: sectors_of(v.len()),
                tsq_file_sectors: Some(sectors_of(f.len())),
                name: Some("SE.TSQ + SE.TVB".into()),
            });
        }
        BankKind::Toy | BankKind::Enemy | BankKind::BabyForm => {
            let (count, ti, vi, profile) = match r.kind {
                BankKind::Toy => (8, 3, 5, None),
                BankKind::Enemy => (7, 2, 4, Some(6)),
                _ => (6, 2, 3, Some(5)),
            };
            let s = container::sections(f, count)?;
            // pb/0 (the Baby) keeps the player layout, with no name at +2.
            if let Some(p) = profile.filter(|_| !(r.kind == BankKind::BabyForm && r.index == 0)) {
                name = profile_name(&f[s[p].offset as usize..(s[p].offset + s[p].len) as usize]);
            }
            ((s[ti].offset as usize, s[ti].len as usize), (s[vi].offset as usize, s[vi].len as usize))
        }
    };
    let t = Tvb::parse(&f[tvb.0..tvb.0 + tvb.1])?;
    let file_bytes = match r.kind {
        BankKind::Music | BankKind::NpcSound | BankKind::FieldSe => (tvb.0 + t.total_bytes()) as u32,
        _ => f.len() as u32,
    };
    Some(Bank {
        id: r.id.clone(),
        kind: r.kind,
        index: r.index,
        tsq: f[tsq.0..tsq.0 + tsq.1].to_vec(),
        tvb: f[tvb.0..tvb.0 + tvb.1].to_vec(),
        tsq_offset: tsq.0 as u32,
        tvb_offset: tvb.0 as u32,
        file_bytes,
        file_sectors: sectors_of(f.len()),
        tsq_file_sectors: None,
        name,
    })
}

/// `char name[20]` at +2 of a 0x398-byte enemy profile (printable ASCII only).
fn profile_name(p: &[u8]) -> Option<String> {
    let raw = p.get(2..22)?;
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let s = &raw[..end];
    (!s.is_empty() && s.iter().all(|c| (0x20..0x7F).contains(c))).then(|| String::from_utf8_lossy(s).trim().to_string())
}

/// A cap a bank has to fit, as shown on a budget bar.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    pub label: String,
    pub value: u32,
    pub max: u32,
    /// Inferred from buffer adjacency or the SPU map, not a check in code.
    pub derived: bool,
    /// Retail data already exceeds it (bosses, the common bank): over is a
    /// warning, not a block.
    pub soft: bool,
}

/// Where a bank's samples are uploaded and which sound slot plays it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    /// Sound slot in the request word `(a << 24) | (b << 16) | (slot << 8) | cue`.
    pub sound_slot: u8,
    /// e.g. "Area BGM · slot 2".
    pub role: String,
    pub spu_base: u32,
    /// Bytes the SPU region holds for this bank.
    pub spu_cap: u32,
    /// "0x3D710–0x59710".
    pub spu_range: String,
    pub spu_derived: bool,
    pub budgets: Vec<Budget>,
}

const STAGING: u32 = 0x20000;

fn range(base: u32, len: u32) -> String {
    format!("0x{:05X}–0x{:05X}", base, base + len)
}

/// Sound slot, SPU region and caps for a loaded bank.
pub fn placement(b: &Bank, adpcm: u32) -> Placement {
    let tsq = b.tsq.len() as u32;
    let file = b.file_sectors * 2048;
    let bar = |label: String, value: u32, max: u32, derived: bool, soft: bool| Budget { label, value, max, derived, soft };
    let spu = |base: u32, cap: u32, label: &str, derived: bool, soft: bool| {
        bar(format!("SPU {}{label}", range(base, cap)), adpcm, cap, derived, soft)
    };
    match b.kind {
        // Load_BGM_StateMachine: id 0x39 goes to 0x80010000 and SPU 0x21710
        // (slot 1, TSQ → 0x801CEEE8); every other id to staging and 0x3D710
        // (slot 2, TSQ → 0x801D16E8).
        BankKind::Music if b.index == 0x39 => {
            let (base, cap) = (0x21710, 0x38000);
            Placement {
                sound_slot: 1,
                role: "Common BGM · slot 1".into(),
                spu_base: base,
                spu_cap: cap,
                spu_range: range(base, cap),
                spu_derived: true,
                budgets: vec![
                    spu(base, cap, " · slots 1+2", true, true),
                    bar("TSQ buffer 0x801CEEE8 · slots 1+2".into(), tsq, 0x5000, true, true),
                ],
            }
        }
        // bgm/0 is the common bank: the boot loads it (as 0xFF) to slot 1 at 0x21710.
        BankKind::Music if b.index == 0 => {
            let (base, cap) = (0x21710, 0x1C000);
            Placement {
                sound_slot: 1,
                role: "Common BGM · slot 1".into(),
                spu_base: base,
                spu_cap: cap,
                spu_range: range(base, cap),
                spu_derived: false,
                budgets: vec![
                    spu(base, cap, "", false, false),
                    bar(format!("File in staging 0x{STAGING:05X}"), file, STAGING, false, false),
                    bar("TSQ buffer 0x2800".into(), tsq, 0x2800, true, false),
                ],
            }
        }
        BankKind::Music => {
            let (base, cap) = (0x3D710, 0x1C000);
            Placement {
                sound_slot: 2,
                role: "Area BGM · slot 2".into(),
                spu_base: base,
                spu_cap: cap,
                spu_range: range(base, cap),
                spu_derived: false,
                budgets: vec![
                    spu(base, cap, "", false, false),
                    bar(format!("File in staging 0x{STAGING:05X}"), file, STAGING, false, false),
                    bar("TSQ buffer 0x2800".into(), tsq, 0x2800, true, false),
                ],
            }
        }
        // Boot_InitSystemAndLoadSE: SE.TSQ ≤ 0x1000, SE.TVB ≤ 0x20800 → SPU 0x1010.
        BankKind::SystemSe => {
            let (base, cap) = (0x1010, 0x20000);
            Placement {
                sound_slot: 0,
                role: "System SE · slot 0".into(),
                spu_base: base,
                spu_cap: cap,
                spu_range: range(base, cap),
                spu_derived: false,
                budgets: vec![
                    spu(base, cap, "", false, false),
                    bar("SE.TVB file ≤ 0x20800".into(), file, 0x20800, false, false),
                    bar("SE.TSQ file ≤ 0x1000".into(), b.tsq_file_sectors.unwrap_or(0) * 2048, 0x1000, false, false),
                ],
            }
        }
        // SE2 is uploaded to 0x5D710 in the field, where no battle banks are
        // resident; it runs up to the reverb work area (mode 3 from 0x7B7C0).
        BankKind::FieldSe => {
            let (base, cap) = (0x5D710, 0x7B7C0 - 0x5D710);
            Placement {
                sound_slot: 4,
                role: "Field SE · slot 4".into(),
                spu_base: base,
                spu_cap: cap,
                spu_range: range(base, cap),
                spu_derived: true,
                budgets: vec![
                    spu(base, cap, " · field", true, false),
                    bar(format!("File in staging 0x{STAGING:05X}"), file, STAGING, false, false),
                    bar("TSQ buffer 0x400".into(), tsq, 0x400, true, false),
                ],
            }
        }
        BankKind::NpcSound => {
            let (base, cap) = (0x59710, 0x4000);
            Placement {
                sound_slot: 3,
                role: "NPC special · slot 3".into(),
                spu_base: base,
                spu_cap: cap,
                spu_range: range(base, cap),
                spu_derived: false,
                budgets: vec![
                    spu(base, cap, "", false, false),
                    bar(format!("File in staging 0x{STAGING:05X}"), file, STAGING, false, false),
                    bar("TSQ buffer 0x410".into(), tsq, 0x410, true, false),
                ],
            }
        }
        // Load_LivingToyBinAndOverlay: UploadSpuSoundBank(field5, slot+7, 0x69710 + slot·0x6000).
        BankKind::Toy => Placement {
            sound_slot: 7,
            role: "Living Toy · slots 7–9".into(),
            spu_base: 0x69710,
            spu_cap: 0x6000,
            spu_range: "0x69710 + slot·0x6000".into(),
            spu_derived: false,
            budgets: vec![
                bar("SPU 0x69710 + slot·0x6000".into(), adpcm, 0x6000, false, false),
                bar(format!("Package in staging 0x{STAGING:05X}"), file, STAGING, false, false),
            ],
        },
        // Load_EnemyBinAndOverlay: 0x59710 + species·0x4000; boss banks are
        // larger and use the following species' region.
        BankKind::Enemy => Placement {
            sound_slot: 3,
            role: "Enemy · slot 3 + species".into(),
            spu_base: 0x59710,
            spu_cap: 0x4000,
            spu_range: "0x59710 + species·0x4000".into(),
            spu_derived: false,
            budgets: vec![bar("SPU 0x59710 + species·0x4000".into(), adpcm, 0x4000, false, true)],
        },
        // Load_BabyFormBin: UploadSpuSoundBank(field3, 6, 0x65710), 0x4000 before toy slot 0.
        BankKind::BabyForm => Placement {
            sound_slot: 6,
            role: "Baby form · slot 6".into(),
            spu_base: 0x65710,
            spu_cap: 0x4000,
            spu_range: range(0x65710, 0x4000),
            spu_derived: false,
            budgets: vec![spu(0x65710, 0x4000, "", false, true)],
        },
    }
}
