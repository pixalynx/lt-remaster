//! Raw-sector headers of a STR stream: the CD-XA subheader every MODE2 sector
//! carries, and the 32-byte chunk header at the start of each video sector.
//!
//! Sector layout (2352 bytes): sync 0x00–0x0B, header 0x0C–0x0F, subheader
//! 0x10–0x17 (file, channel, submode, coding info, then a copy), user data
//! from 0x18: 2048 bytes in Form 1 (video), 2324 in Form 2 (XA audio).
//! Sources: psx-spx "CDROM XA Subheader", jPSXdec "STR Frame Sector Header".

/// Offset of the subheader in a raw sector.
pub const SUBHEADER: usize = 0x10;
/// Offset of the user data in a raw sector.
pub const DATA: usize = 0x18;
/// User data of a Form 2 sector (XA audio).
pub const FORM2_DATA: usize = 2324;
/// The STR chunk header at the start of a video sector's user data.
pub const CHUNK_HEADER: usize = 32;
/// Bitstream bytes carried by one video sector (2048 − 32).
pub const CHUNK_DATA: usize = 2016;

/// Submode bits (subheader byte 2).
pub mod submode {
    pub const EOF: u8 = 0x80;
    pub const REALTIME: u8 = 0x40;
    pub const FORM2: u8 = 0x20;
    pub const TRIGGER: u8 = 0x10;
    pub const DATA: u8 = 0x08;
    pub const AUDIO: u8 = 0x04;
    pub const VIDEO: u8 = 0x02;
    pub const EOR: u8 = 0x01;
}

/// `0x0160` then `0x8001`: an MDEC video chunk (bytes `60 01 01 80`).
pub const STR_MAGIC: u16 = 0x0160;
pub const STR_TYPE_MDEC: u16 = 0x8001;

/// The 4 subheader bytes at 0x10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubHeader {
    /// Interleaved file number.
    pub file: u8,
    /// Channel within the file (XA streams pick one).
    pub channel: u8,
    pub submode: u8,
    /// Coding info; meaningful for audio sectors only (see [`XaCoding`]).
    pub coding: u8,
}

impl SubHeader {
    pub fn parse(raw: &[u8]) -> Self {
        Self { file: raw[SUBHEADER], channel: raw[SUBHEADER + 1], submode: raw[SUBHEADER + 2], coding: raw[SUBHEADER + 3] }
    }

    pub fn is_audio(&self) -> bool {
        self.submode & submode::AUDIO != 0
    }

    pub fn is_form2(&self) -> bool {
        self.submode & submode::FORM2 != 0
    }
}

/// XA audio coding info (subheader byte 3 of an audio sector).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct XaCoding {
    pub stereo: bool,
    /// 37,800 or 18,900 Hz.
    pub rate: u32,
    /// 4 or 8 bits per sample.
    pub bits: u8,
    pub emphasis: bool,
}

impl XaCoding {
    /// Bits 0–1 stereo, 2–3 rate (0 = 37.8 kHz), 4–5 bits (0 = 4-bit), 6 emphasis.
    pub fn from_byte(b: u8) -> Self {
        Self {
            stereo: b & 0x03 == 1,
            rate: if (b >> 2) & 0x03 == 1 { 18_900 } else { 37_800 },
            bits: if (b >> 4) & 0x03 == 1 { 8 } else { 4 },
            emphasis: b & 0x40 != 0,
        }
    }

    pub fn channels(&self) -> u8 {
        if self.stereo {
            2
        } else {
            1
        }
    }

    /// PCM frames one sector decodes to: 18 groups × 28 samples × units/channels.
    pub fn frames_per_sector(&self) -> u32 {
        let units = if self.bits == 8 { 4 } else { 8 };
        18 * 28 * units / self.channels() as u32
    }
}

/// The 32-byte header at the start of a video sector's user data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHeader {
    /// Chunk index within the frame (0..chunks−1).
    pub chunk: u16,
    /// Sectors this frame is spread over.
    pub chunks: u16,
    /// Frame number, starting at 1.
    pub frame: u32,
    /// Bitstream bytes the frame uses after demuxing (rounded up to 4).
    pub bytes: u32,
    pub width: u16,
    pub height: u16,
    /// MDEC codes in the frame, rounded up to 64, halved.
    pub codes: u16,
    /// Copy of the frame header's quantisation scale.
    pub qscale: u16,
    /// Copy of the frame header's bitstream version (2 or 3).
    pub version: u16,
}

impl ChunkHeader {
    /// `user` is the sector's user data (from 0x18). `None` if it isn't an
    /// MDEC chunk (magic `0x0160`, type `0x8001`).
    pub fn parse(user: &[u8]) -> Option<Self> {
        if user.len() < CHUNK_HEADER {
            return None;
        }
        let u16_at = |o: usize| u16::from_le_bytes([user[o], user[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([user[o], user[o + 1], user[o + 2], user[o + 3]]);
        if u16_at(0) != STR_MAGIC || u16_at(2) != STR_TYPE_MDEC {
            return None;
        }
        let h = Self {
            chunk: u16_at(4),
            chunks: u16_at(6),
            frame: u32_at(8),
            bytes: u32_at(12),
            width: u16_at(16),
            height: u16_at(18),
            codes: u16_at(20),
            qscale: u16_at(24),
            version: u16_at(26),
        };
        (h.chunks > 0 && h.chunk < h.chunks).then_some(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_info() {
        let c = XaCoding::from_byte(0x01);
        assert!(c.stereo);
        assert_eq!((c.rate, c.bits, c.channels(), c.frames_per_sector()), (37_800, 4, 2, 2016));
        let m = XaCoding::from_byte(0x04);
        assert_eq!((m.stereo, m.rate, m.frames_per_sector()), (false, 18_900, 4032));
        assert_eq!(XaCoding::from_byte(0x11).frames_per_sector(), 1008);
    }

    #[test]
    fn chunk_header() {
        // movie/8's first sector (user data).
        let hex = "600101800000090001000000e40900004001e000a00600380100020000000000";
        let b: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        let h = ChunkHeader::parse(&b).unwrap();
        assert_eq!((h.chunk, h.chunks, h.frame, h.bytes), (0, 9, 1, 0x9E4));
        assert_eq!((h.width, h.height, h.qscale, h.version), (320, 224, 1, 2));
        assert!(ChunkHeader::parse(&[0u8; 32]).is_none());
    }
}
