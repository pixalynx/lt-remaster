//! Which release of the game a disc is: Guardian's Crusade (USA, SLUS-00811), Guardian's
//! Crusade (Europe, SLES-01793) or Knight & Baby (Japan, SLPS-01531), and what differs between
//! them (`docs/research/09-releases.md`).
//!
//! Living Tool names everything the USA way: tables by their USA address, files by their USA
//! canonical ids. The decomp builds the EU and JP releases from the same source
//! (`gc_decomp/docs/VERSIONS.md`) and its port places every USA symbol in them
//! (`ver/<eu|jp>/config/port_symbols.json`). `scripts/release-maps.py` turns that into the
//! built-in maps in `data/release_<eu|jp>.json`: the same [`Layout`] ranges a shifted HackerGC base
//! carries, so [`crate::exe::PsxExe::va`] finds main's tables, plus one list per code overlay for
//! the tables read out of overlay files ([`crate::exe::PsxExe::ovr_va`]).
//!
//! The EU and JP releases open read-only: editing and builds are USA only for now.

use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use crate::disc::DiscImage;
use crate::error::{LtError, Result};
use crate::fs::{SubTable, JP_SUB_TABLES, SUB_TABLES};
use crate::iso9660::{self, DirEntry};
use crate::layout::Layout;
use crate::text::Encoding;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Release {
    #[default]
    Usa,
    Eu,
    Jp,
}

/// What Living Tool knows about one release.
#[derive(Debug)]
pub struct ReleaseInfo {
    pub release: Release,
    /// The dump's name, as the projects list shows it: "Guardian's Crusade (USA)".
    pub title: &'static str,
    /// "USA" | "Europe" | "Japan" (the part of the title in brackets).
    pub region: &'static str,
    pub serial: &'static str,
    /// The boot EXE in the ISO9660 root.
    pub exe_name: &'static str,
    /// The EXE header's region marker at 0x4C ("Sony Computer Entertainment Inc. for … area").
    exe_marker: &'static [u8],
    /// The known good dump: its CRC32 and size, and whose list it is on.
    pub crc32: u32,
    pub sectors: u32,
    pub dump: &'static str,
    /// Entries in the EXE's hidden file table and its named slices.
    pub table_len: usize,
    pub sub_tables: &'static [SubTable],
    /// LBA of the world archive (`world`), the base of every chunk id.
    pub world_lba: u32,
    /// Memory-card file names: the prefix before `nn`.
    pub save_prefix: &'static str,
    /// The BIOS version string's last letter for this release's consoles.
    pub bios_region: char,
    pub text: Encoding,
    /// PAL: 256-line screen and 50 Hz timing.
    pub pal: bool,
}

static USA: ReleaseInfo = ReleaseInfo {
    release: Release::Usa,
    title: "Guardian's Crusade (USA)",
    region: "USA",
    serial: "SLUS-00811",
    exe_name: "SLUS_008.11",
    exe_marker: b"Sony Computer Entertainment Inc. for North America area",
    crc32: 0xcdc7_2c62,
    sectors: 185_985,
    dump: "Redump",
    table_len: 493,
    sub_tables: SUB_TABLES,
    world_lba: 20_011,
    save_prefix: "BASLUSP00811KNIGHT",
    bios_region: 'A',
    text: Encoding::Ascii,
    pal: false,
};

static EU: ReleaseInfo = ReleaseInfo {
    release: Release::Eu,
    title: "Guardian's Crusade (Europe)",
    region: "Europe",
    serial: "SLES-01793",
    exe_name: "SLES_017.93",
    exe_marker: b"Sony Computer Entertainment Inc. for Europe area",
    crc32: 0xb1a3_672f,
    sectors: 186_066,
    dump: "Trurip",
    table_len: 493,
    sub_tables: SUB_TABLES,
    world_lba: 19_999,
    save_prefix: "BESLES-01793KNIGHT",
    bios_region: 'E',
    text: Encoding::Ascii,
    pal: true,
};

static JP: ReleaseInfo = ReleaseInfo {
    release: Release::Jp,
    title: "Knight & Baby (Japan)",
    region: "Japan",
    serial: "SLPS-01531",
    exe_name: "SLPS_015.31",
    exe_marker: b"Sony Computer Entertainment Inc. for Japan area",
    crc32: 0xde78_f968,
    sectors: 172_194,
    dump: "Redump",
    table_len: 491,
    sub_tables: JP_SUB_TABLES,
    world_lba: 20_103,
    save_prefix: "BISLPS-01531KNIGHT",
    bios_region: 'J',
    text: Encoding::ShiftJis,
    pal: false,
};

impl Release {
    pub const ALL: [Release; 3] = [Release::Usa, Release::Eu, Release::Jp];

    pub fn info(self) -> &'static ReleaseInfo {
        match self {
            Release::Usa => &USA,
            Release::Eu => &EU,
            Release::Jp => &JP,
        }
    }

    /// Living Tool edits and builds this release (the USA one only, for now).
    pub fn editable(self) -> bool {
        self == Release::Usa
    }

    /// The release a PS-X EXE was built for, by its header's region marker.
    pub fn of_exe(bytes: &[u8]) -> Option<Release> {
        Self::ALL.into_iter().find(|r| {
            let m = r.info().exe_marker;
            bytes.get(0x4C..0x4C + m.len()) == Some(m)
        })
    }

    /// By boot EXE name (`SLES_017.93`, any case).
    pub fn of_exe_name(name: &str) -> Option<Release> {
        Self::ALL.into_iter().find(|r| r.info().exe_name.eq_ignore_ascii_case(name))
    }

    /// By serial (`SLES-01793`; the dash is optional, as Redux reports it).
    pub fn of_serial(serial: &str) -> Option<Release> {
        let bare = |s: &str| s.replace('-', "").to_ascii_uppercase();
        Self::ALL.into_iter().find(|r| bare(r.info().serial) == bare(serial))
    }

    /// The release whose known good dump has this CRC32.
    pub fn of_crc32(crc32: u32) -> Option<Release> {
        Self::ALL.into_iter().find(|r| r.info().crc32 == crc32)
    }

    /// The built-in address map of an EU or JP EXE (`None` for USA, whose addresses are the names).
    pub fn layout(self) -> Option<Arc<Layout>> {
        static EU_MAP: OnceLock<Arc<Layout>> = OnceLock::new();
        static JP_MAP: OnceLock<Arc<Layout>> = OnceLock::new();
        let (cell, json) = match self {
            Release::Usa => return None,
            Release::Eu => (&EU_MAP, include_str!("../data/release_eu.json")),
            Release::Jp => (&JP_MAP, include_str!("../data/release_jp.json")),
        };
        Some(cell.get_or_init(|| Arc::new(parse_map(json))).clone())
    }
}

impl ReleaseInfo {
    /// The slice of the file table holding `kind`.
    pub fn sub_table(&self, kind: crate::fs::Kind) -> &'static SubTable {
        self.sub_tables.iter().find(|t| t.kind == kind).expect("every kind has a sub-table")
    }

    /// The serial as PCSX-Redux reports it (`SLUS00811`).
    pub fn redux_id(&self) -> String {
        self.serial.replace('-', "")
    }

    /// The first new-area chunk id (research 03 §10): chunk `id` is read at `world_lba + id·125`
    /// with no bound, so new areas start at the first id past the end of the disc, rounded up to a
    /// group of four (the four chunk slots). USA 1328, EU 1332, JP 1220.
    pub fn new_area_first(&self) -> u32 {
        (self.sectors - self.world_lba).div_ceil(125).div_ceil(4) * 4
    }

    pub fn summary(&self) -> ReleaseSummary {
        ReleaseSummary {
            id: self.release,
            title: self.title,
            region: self.region,
            serial: self.serial,
            exe_name: self.exe_name,
            dump: self.dump,
            crc32: format!("{:08x}", self.crc32),
            sectors: self.sectors,
            table_len: self.table_len,
            editable: self.release.editable(),
            pal: self.pal,
        }
    }
}

/// A release as the app shows it (the open disc's, `DiscSummary.release`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSummary {
    pub id: Release,
    pub title: &'static str,
    pub region: &'static str,
    pub serial: &'static str,
    pub exe_name: &'static str,
    pub dump: &'static str,
    pub crc32: String,
    pub sectors: u32,
    pub table_len: usize,
    pub editable: bool,
    pub pal: bool,
}

#[derive(Deserialize)]
struct MapFile {
    main: Vec<(u32, u32, i32)>,
    ovr: std::collections::BTreeMap<u8, Vec<(u32, u32, i32)>>,
}

fn parse_map(json: &str) -> Layout {
    let m: MapFile = serde_json::from_str(json).expect("built-in release map");
    Layout { ranges: m.main, overlays: m.ovr, ..Default::default() }
}

/// The boot file `SYSTEM.CNF` names (`BOOT = cdrom:\SLUS_008.11;1` → `SLUS_008.11`).
pub fn boot_name(cnf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(cnf);
    let line = text.lines().find(|l| l.trim_start().to_ascii_uppercase().starts_with("BOOT"))?;
    let path = line.split_once('=')?.1.trim();
    let name = path.rsplit(['\\', ':', '/']).next()?.split(';').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// SHA-1 of the USA retail boot EXE (SLUS_008.11) as the disc holds it.
pub const USA_EXE_SHA1: &str = "207358f97ac5577fc3585e7a003a87153c2f47e2";

/// Whether `exe` is the USA retail boot EXE byte for byte: what Living Tool Remaster draws (its
/// hooks and tables are at that EXE's addresses), so a project that changes the EXE, or runs on a
/// base, plays on the Play screen only.
pub fn is_usa_retail_exe(exe: &[u8]) -> bool {
    sha1_smol::Sha1::from(exe).digest().to_string() == USA_EXE_SHA1
}

/// Finds the disc's boot EXE and the release it is: the file `SYSTEM.CNF` boots, else any known
/// release's EXE in the root. A disc that boots another game's EXE is `DISC_OTHER_GAME`.
pub fn find_exe(disc: &DiscImage) -> Result<(Release, DirEntry)> {
    let vol = iso9660::read_volume(disc)?;
    let root = iso9660::read_dir(disc, vol.root_lba, vol.root_size)?;
    let file = |name: &str| root.iter().find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(name)).cloned();
    let boot = file("SYSTEM.CNF")
        .and_then(|e| disc.read_file_bytes(e.lba, 0, e.size.min(2048) as usize).ok())
        .and_then(|b| boot_name(&b));
    if let Some(name) = boot {
        return match (Release::of_exe_name(&name), file(&name)) {
            (Some(r), Some(e)) => Ok((r, e)),
            (Some(_), None) => Err(LtError::FileNotFound(name)),
            (None, _) => Err(LtError::OtherGame(name)),
        };
    }
    Release::ALL
        .into_iter()
        .find_map(|r| file(r.info().exe_name).map(|e| (r, e)))
        .ok_or_else(|| LtError::FileNotFound(Release::ALL.map(|r| r.info().exe_name).join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_retail_exe_is_retail() {
        assert!(!is_usa_retail_exe(b"PS-X EXE and something else"));
        assert_eq!(USA_EXE_SHA1.len(), 40);
    }

    #[test]
    fn new_area_first_follows_the_end_of_each_disc() {
        assert_eq!(USA.new_area_first(), 1328);
        assert_eq!(USA.new_area_first(), crate::world::NEW_AREA_FIRST);
        assert_eq!(EU.new_area_first(), 1332);
        assert_eq!(JP.new_area_first(), 1220);
    }

    #[test]
    fn reads_the_boot_line() {
        assert_eq!(boot_name(b"BOOT = cdrom:\\SLUS_008.11;1\r\nTCB = 4\r\n").as_deref(), Some("SLUS_008.11"));
        assert_eq!(boot_name(b"BOOT=cdrom:\\SLPS_015.31;1\nEVENT = 10").as_deref(), Some("SLPS_015.31"));
        assert_eq!(boot_name(b"TCB = 4").as_deref(), None);
        assert_eq!(Release::of_exe_name("sles_017.93"), Some(Release::Eu));
        assert_eq!(Release::of_serial("SLPS01531"), Some(Release::Jp));
        assert_eq!(Release::of_serial("SLUS-00811"), Some(Release::Usa));
        assert_eq!(USA.redux_id(), "SLUS00811");
    }

    #[test]
    fn tells_the_exes_apart_by_their_header() {
        let mut h = vec![0u8; 0x800];
        h[..8].copy_from_slice(b"PS-X EXE");
        assert_eq!(Release::of_exe(&h), None);
        for r in Release::ALL {
            let m = r.info().exe_marker;
            h[0x4C..0x4C + m.len()].copy_from_slice(m);
            assert_eq!(Release::of_exe(&h), Some(r));
            h[0x4C..0x4C + m.len()].fill(0);
        }
    }

    /// Where the decomp's port puts main's tables (`gc_decomp/docs/VERSIONS.md`).
    #[test]
    fn built_in_maps_place_the_tables() {
        assert!(Release::Usa.layout().is_none());
        let eu = Release::Eu.layout().unwrap();
        let jp = Release::Jp.layout().unwrap();
        // The file table (D_801B4304) and its overlay group (FileTable_AreaOverlays).
        assert_eq!(eu.map(0x801B_4304), 0x801B_461C);
        assert_eq!(jp.map(0x801B_4304), 0x801B_4F50);
        assert_eq!(eu.map(0x801B_442C), 0x801B_4744);
        assert_eq!(jp.map(0x801B_442C), 0x801B_5068);
        // g_EventFlags.
        assert_eq!(eu.map(0x801C_E810), 0x801C_E980);
        assert_eq!(jp.map(0x801C_E810), 0x801C_F1D0);
        // OVR02's shop table; OVR04's drop remap, found by its bytes (no relocation names it).
        assert_eq!(eu.map_overlay(2, 0x801F_CE00), 0x801F_CDC0);
        assert_eq!(jp.map_overlay(2, 0x801F_CE00), 0x801F_C6C0);
        assert_eq!(eu.map_overlay(4, 0x801F_DAFC), 0x801F_DC18);
        assert_eq!(jp.map_overlay(4, 0x801F_DAFC), 0x801F_E6A8);
        assert_eq!(eu.map_overlay(4, 0x801F_DC98), 0x801F_DDB4);
    }
}
