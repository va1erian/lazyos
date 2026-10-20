//! Byte builders for fixtures: EDID blocks, CEA extensions, ELDs through
//! [`crate::eld`], and VBT tables.
//!
//! Real-world dumps cannot be reconstructed byte-exactly from memory, and the
//! ones from the box are still to be captured (`docs/compat/kabylake/gpu/`),
//! so the tests and fuzz seeds use these synthesised blocks. They encode the
//! *layouts* the parsers claim to read, from the other direction.

use std::vec;
use std::vec::Vec;

use crate::cea::Sad;
use crate::edid::{Timing, BLOCK, HEADER};

/// Fix byte `len - 1` so the block sums to 0 mod 256.
pub fn seal(block: &mut [u8]) {
    let n = block.len();
    block[n - 1] = 0;
    let sum = block.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    block[n - 1] = 0u8.wrapping_sub(sum);
}

/// Encode a detailed timing descriptor (inverse of `Timing::parse`).
pub fn dtd(t: &Timing) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[..2].copy_from_slice(&((t.pixel_clock_khz / 10) as u16).to_le_bytes());
    d[2] = t.h_active as u8;
    d[3] = t.h_blank as u8;
    d[4] = ((t.h_active >> 8) as u8) << 4 | (t.h_blank >> 8) as u8;
    d[5] = t.v_active as u8;
    d[6] = t.v_blank as u8;
    d[7] = ((t.v_active >> 8) as u8) << 4 | (t.v_blank >> 8) as u8;
    d[8] = t.h_front as u8;
    d[9] = t.h_sync as u8;
    d[10] = ((t.v_front & 0xF) as u8) << 4 | (t.v_sync & 0xF) as u8;
    d[11] = ((t.h_front >> 8) as u8) << 6
        | (((t.h_sync >> 8) & 3) as u8) << 4
        | (((t.v_front >> 4) & 3) as u8) << 2
        | ((t.v_sync >> 4) & 3) as u8;
    d[12] = t.h_size_mm as u8;
    d[13] = t.v_size_mm as u8;
    d[14] = ((t.h_size_mm >> 8) as u8) << 4 | (t.v_size_mm >> 8) as u8;
    d[17] = u8::from(t.interlaced) << 7
        | 0x18
        | u8::from(t.v_sync_positive) << 2
        | u8::from(t.h_sync_positive) << 1;
    d
}

/// A 13-byte-text display descriptor (0xFC name, 0xFF serial, 0xFE text).
pub fn text_descriptor(tag: u8, text: &str) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[3] = tag;
    let mut t = [0x20u8; 13];
    let bytes = text.as_bytes();
    let n = bytes.len().min(13);
    t[..n].copy_from_slice(&bytes[..n]);
    if n < 13 {
        t[n] = 0x0A;
    }
    d[5..18].copy_from_slice(&t);
    d
}

/// The 2560x1440 CVT reduced-blanking timing of a 27-inch panel: 241.5 MHz,
/// 2720 x 1481 total, about 59.95 Hz.
pub fn timing_2560x1440() -> Timing {
    Timing {
        pixel_clock_khz: 241_500,
        h_active: 2560,
        h_blank: 160,
        h_front: 48,
        h_sync: 32,
        v_active: 1440,
        v_blank: 41,
        v_front: 3,
        v_sync: 5,
        h_size_mm: 597,
        v_size_mm: 336,
        interlaced: false,
        h_sync_positive: true,
        v_sync_positive: false,
    }
}

/// CEA-861 1080p60 (VIC 16): 148.5 MHz, 2200 x 1125.
pub fn timing_1080p60() -> Timing {
    Timing {
        pixel_clock_khz: 148_500,
        h_active: 1920,
        h_blank: 280,
        h_front: 88,
        h_sync: 44,
        v_active: 1080,
        v_blank: 45,
        v_front: 4,
        v_sync: 5,
        h_size_mm: 597,
        v_size_mm: 336,
        interlaced: false,
        h_sync_positive: true,
        v_sync_positive: true,
    }
}

/// Fields of a base block a test varies.
pub struct BaseSpec<'a> {
    /// Three PNP letters.
    pub manufacturer: [u8; 3],
    pub product: u16,
    pub serial: u32,
    pub timings: &'a [Timing],
    pub name: Option<&'a str>,
    pub monitor_serial: Option<&'a str>,
    pub extensions: u8,
}

/// A sealed 128-byte EDID 1.4 base block.
pub fn base_block(s: &BaseSpec) -> [u8; BLOCK] {
    let mut b = [0u8; BLOCK];
    b[..8].copy_from_slice(&HEADER);
    let l = |c: u8| u16::from(c - b'A' + 1) & 0x1F;
    let m = l(s.manufacturer[0]) << 10 | l(s.manufacturer[1]) << 5 | l(s.manufacturer[2]);
    b[8..10].copy_from_slice(&m.to_be_bytes());
    b[10..12].copy_from_slice(&s.product.to_le_bytes());
    b[12..16].copy_from_slice(&s.serial.to_le_bytes());
    b[16] = 12; // week
    b[17] = 33; // 2023
    b[18] = 1;
    b[19] = 4;
    b[20] = 0xA5; // digital, 8 bpc, DisplayPort-ish; only bit 7 is read
    b[21] = 60;
    b[22] = 34;
    // Standard timings: 1280x1024@60 (0x81 0x80 is 1280 5:4 60) and unused.
    b[38] = 0x81;
    b[39] = 0x80;
    b[40..54].fill(0x01);
    let mut at = 54;
    for t in s.timings.iter().take(2) {
        b[at..at + 18].copy_from_slice(&dtd(t));
        at += 18;
    }
    if let Some(n) = s.name {
        b[at..at + 18].copy_from_slice(&text_descriptor(0xFC, n));
        at += 18;
    }
    if let Some(n) = s.monitor_serial {
        b[at..at + 18].copy_from_slice(&text_descriptor(0xFF, n));
        at += 18;
    }
    if at + 18 <= 126 {
        // Range limits: 48-75 Hz, 30-90 kHz, 300 MHz.
        let mut r = [0u8; 18];
        r[3] = 0xFD;
        r[5..10].copy_from_slice(&[48, 75, 30, 90, 30]);
        b[at..at + 18].copy_from_slice(&r);
    }
    b[126] = s.extensions;
    seal(&mut b);
    b
}

/// The audio part of a CEA extension and the rest of its data blocks.
#[derive(Default)]
pub struct CeaSpec {
    pub basic_audio: bool,
    pub svds: Vec<u8>,
    pub sads: Vec<Sad>,
    pub speakers: Option<u8>,
    /// `(physical address, flags byte, max TMDS in 5 MHz units, audio
    /// latency raw byte if latency fields should be present)`.
    pub hdmi: Option<(u16, u8, u8, Option<u8>)>,
    /// HDMI Forum block: `(version, max TMDS in 5 MHz units, flags)`.
    pub forum: Option<(u8, u8, u8)>,
    pub dtds: Vec<Timing>,
}

fn data_block(out: &mut Vec<u8>, tag: u8, payload: &[u8]) {
    out.push(tag << 5 | payload.len() as u8);
    out.extend_from_slice(payload);
}

/// A sealed revision 3 CEA-861 extension block.
pub fn cea_block(s: &CeaSpec) -> [u8; BLOCK] {
    let mut blocks = Vec::new();
    if !s.svds.is_empty() {
        data_block(&mut blocks, 2, &s.svds);
    }
    if !s.sads.is_empty() {
        let p: Vec<u8> = s.sads.iter().flat_map(|a| a.to_bytes()).collect();
        data_block(&mut blocks, 1, &p);
    }
    if let Some(sp) = s.speakers {
        data_block(&mut blocks, 4, &[sp, 0, 0]);
    }
    if let Some((phys, flags, tmds, latency)) = s.hdmi {
        let mut p = vec![0x03, 0x0C, 0x00];
        p.extend_from_slice(&phys.to_be_bytes());
        p.push(flags);
        p.push(tmds);
        if let Some(a) = latency {
            // Latency fields present, video latency 0 (unknown).
            p.extend_from_slice(&[0x80, 0, a]);
        }
        data_block(&mut blocks, 3, &p);
    }
    if let Some((ver, tmds, flags)) = s.forum {
        data_block(&mut blocks, 3, &[0xD8, 0x5D, 0xC4, ver, tmds, flags]);
    }
    let mut b = [0u8; BLOCK];
    b[0] = 0x02;
    b[1] = 3;
    let d = 4 + blocks.len();
    assert!(
        d + 18 * s.dtds.len() < 127,
        "fixture does not fit one block"
    );
    b[2] = d as u8;
    b[3] = if s.basic_audio { 0xC0 } else { 0x80 } | s.dtds.len().min(15) as u8;
    b[4..d].copy_from_slice(&blocks);
    for (i, t) in s.dtds.iter().enumerate() {
        b[d + 18 * i..d + 18 * (i + 1)].copy_from_slice(&dtd(t));
    }
    seal(&mut b);
    b
}

/// The LPCM, AC-3 pair a typical HDMI monitor lists: stereo LPCM at 32, 44.1
/// and 48 kHz in 16/20/24 bits, and 6-channel AC-3 to 48 kHz.
pub fn typical_sads() -> Vec<Sad> {
    vec![
        Sad::from_bytes([0x09, 0x07, 0x07]),
        Sad::from_bytes([0x15, 0x07, 0x50]),
    ]
}

/// A 27-inch 2560x1440 HDMI monitor with stereo speakers: a base block with
/// the 241.5 MHz timing and a 1080p60 second timing, and one CEA extension
/// with audio. 256 bytes.
pub fn monitor_2560x1440() -> Vec<u8> {
    let base = base_block(&BaseSpec {
        manufacturer: *b"LZY",
        product: 0x2560,
        serial: 0x1234_5678,
        timings: &[timing_2560x1440(), timing_1080p60()],
        name: Some("LZY QHD27"),
        // Two timings, the name and the range limits fill the four slots.
        monitor_serial: None,
        extensions: 1,
    });
    let cea = cea_block(&CeaSpec {
        basic_audio: true,
        svds: vec![0x90, 4, 31, 32, 0x85],
        sads: typical_sads(),
        speakers: Some(0x01),
        hdmi: Some((0x1000, 0x38, 60, Some(21))),
        forum: None,
        dtds: vec![],
    });
    [base.as_slice(), cea.as_slice()].concat()
}

/// A DVI-style monitor: one base block, no extension, no audio.
pub fn dvi_monitor_1080p() -> Vec<u8> {
    base_block(&BaseSpec {
        manufacturer: *b"DVI",
        product: 1,
        serial: 0,
        timings: &[timing_1080p60()],
        name: Some("DVI MON"),
        monitor_serial: None,
        extensions: 0,
    })
    .to_vec()
}

/// One VBT child device.
#[derive(Clone, Copy)]
pub struct ChildSpec {
    pub handle: u16,
    pub device_type: u16,
    pub dvo_port: u8,
    pub i2c_pin: u8,
    pub ddc_pin: u8,
    /// Byte 23: efp_routed 0, lane_reversal 1, lspcon 2, iboost 3.
    pub flags23: u8,
    /// Byte 24: hdmi 0, dp 1, tmds 2.
    pub flags24: u8,
    pub aux: u8,
}

/// Child entry size used by the builder (BDB 2xx boards use 33 to 38).
pub const CHILD_SIZE: usize = 38;

fn child_bytes(c: &ChildSpec) -> [u8; CHILD_SIZE] {
    let mut d = [0u8; CHILD_SIZE];
    d[0..2].copy_from_slice(&c.handle.to_le_bytes());
    d[2..4].copy_from_slice(&c.device_type.to_le_bytes());
    d[16] = c.dvo_port;
    d[17] = c.i2c_pin;
    d[19] = c.ddc_pin;
    d[23] = c.flags23;
    d[24] = c.flags24;
    d[25] = c.aux;
    d
}

/// A VBT with BDB `bdb_version`: a features block, a skip block, then the
/// general definitions with the given children, and a valid checksum.
pub fn vbt(bdb_version: u16, children: &[ChildSpec]) -> Vec<u8> {
    let mut bdb = Vec::new();
    bdb.extend_from_slice(b"BIOS_DATA_BLOCK ");
    bdb.extend_from_slice(&bdb_version.to_le_bytes());
    bdb.extend_from_slice(&22u16.to_le_bytes());
    bdb.extend_from_slice(&[0, 0]); // bdb_size, patched below
    let mut push = |id: u8, data: &[u8]| {
        bdb.push(id);
        bdb.extend_from_slice(&(data.len() as u16).to_le_bytes());
        bdb.extend_from_slice(data);
    };
    push(1, &[0u8; 8]);
    push(254, &[0xAA; 5]);
    let mut defs = vec![5, 0, 0, 0, CHILD_SIZE as u8];
    for c in children {
        defs.extend_from_slice(&child_bytes(c));
    }
    push(2, &defs);
    let bdb_len = bdb.len() as u16;
    bdb[20..22].copy_from_slice(&bdb_len.to_le_bytes());

    let mut v = Vec::new();
    v.extend_from_slice(b"$VBT SKYLAKE        ");
    v.extend_from_slice(&155u16.to_le_bytes());
    v.extend_from_slice(&48u16.to_le_bytes());
    v.extend_from_slice(&[0, 0]); // vbt_size
    v.extend_from_slice(&[0, 0]); // checksum, reserved
    v.extend_from_slice(&48u32.to_le_bytes());
    v.extend_from_slice(&[0u8; 16]);
    v.extend_from_slice(&bdb);
    let size = v.len() as u16;
    v[24..26].copy_from_slice(&size.to_le_bytes());
    let sum = v.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    v[26] = 0u8.wrapping_sub(sum);
    v
}

/// Two ports a typical board lists: DDI B HDMI (native TMDS, DDC pin 5 =
/// DPB's GMBUS pin) and DDI C DisplayPort with AUX channel C.
pub fn typical_children() -> Vec<ChildSpec> {
    vec![
        ChildSpec {
            handle: 0x0004,
            device_type: crate::vbt::device_type::HDMI,
            dvo_port: crate::vbt::dvo_port::HDMIB,
            i2c_pin: 0,
            ddc_pin: 5,
            flags23: 0x01,
            flags24: 0x05,
            aux: 0,
        },
        ChildSpec {
            handle: 0x0020,
            device_type: crate::vbt::device_type::DP,
            dvo_port: crate::vbt::dvo_port::DPC,
            i2c_pin: 0,
            ddc_pin: 4,
            flags23: 0x01,
            flags24: 0x02,
            aux: 0x20,
        },
    ]
}
