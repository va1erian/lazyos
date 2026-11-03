//! The Realtek "epatch" firmware container, versions 1 and 2.
//!
//! A `_fw.bin` file holds one patch per ROM revision of a chip family, a
//! signature `Realtech` (v1) or `RTBTCore` (v2) up front, and a tail whose
//! last four octets are `51 04 fd 77` preceded by instructions that name the
//! chip's "project id". The layout and the selection rule are the documented
//! behaviour of Linux's `rtlbt_parse_firmware` / `rtlbt_parse_firmware_v2`
//! (`drivers/bluetooth/btrtl.c`), checked against `rtl8761bu_fw.bin` (v1) and
//! `rtl8822cu_fw.bin` (v2) from linux-firmware; this parser is independent.
//!
//! ```text
//! v1: "Realtech" fw_version:u32 num_patches:u16
//!     chip_id:u16 x n, length:u16 x n, offset:u32 x n        (patch at offset)
//!     ... patches ... instructions, 51 04 fd 77
//! v2: "RTBTCore" fw_version:[u8;8] num_sections:u32
//!     { opcode:u32 length:u32 section } x num_sections ... instructions, sig
//!     section: num:u16 reserved:u16 { eco prio cb:[u8;2] length:u32 data } x num
//! ```
//!
//! The file is untrusted: every offset and length is checked against what is
//! present before use, and the output can never be larger than the input.

use alloc::vec::Vec;

const SIG_V1: &[u8; 8] = b"Realtech";
const SIG_V2: &[u8; 8] = b"RTBTCore";
const TAIL: [u8; 4] = [0x51, 0x04, 0xfd, 0x77];
const V1_HEADER: usize = 14;
const V2_HEADER: usize = 20;
/// Octets cut from the end of a v2 file before its sections are walked: the
/// tail signature and the project-id instruction.
const V2_TAIL_CUT: usize = 7;

const SECTION_SNIPPETS: u32 = 1;
const SECTION_DUMMY: u32 = 2;
const SECTION_SECURITY: u32 = 3;

/// The project ids a patch file names, with the ROM family (LMP subversion)
/// each belongs to.
const PROJECTS: &[(u16, u8)] = &[
    (0x1200, 0),
    (0x8723, 1),
    (0x8821, 2),
    (0x8761, 3),
    (0x8703, 7),
    (0x8822, 8),
    (0x8723, 9),
    (0x8821, 10),
    (0x8822, 13),
    (0x8761, 14),
    (0x8852, 18),
    (0x8852, 20),
    (0x8852, 25),
    (0x8851, 36),
    (0x8922, 44),
    (0x8852, 47),
    (0x8761, 51),
];

/// Whether `fw` starts like an epatch file (versus the plain image of the
/// oldest ROMs).
pub(crate) fn is_epatch(fw: &[u8]) -> bool {
    fw.starts_with(SIG_V1) || fw.starts_with(SIG_V2)
}

/// The patch for a ROM: `lmp_subver` and `rom_version` from the controller,
/// `key_id` from its security project register.
pub(crate) fn build(
    fw: &[u8],
    lmp_subver: u16,
    rom_version: u8,
    key_id: u8,
) -> Result<Vec<u8>, &'static str> {
    let v2 = fw.starts_with(SIG_V2);
    if !v2 && !fw.starts_with(SIG_V1) {
        return Err("bad epatch signature");
    }
    let header = if v2 { V2_HEADER } else { V1_HEADER };
    if fw.len() < header + TAIL.len() + 3 {
        return Err("epatch file too short");
    }
    if fw[fw.len() - TAIL.len()..] != TAIL {
        return Err("epatch tail signature mismatch");
    }
    let project = project_id(fw, header)?;
    if !PROJECTS.contains(&(lmp_subver, project)) {
        return Err("firmware is for a different chip");
    }
    if v2 {
        v2_patch(fw, rom_version, key_id)
    } else {
        v1_patch(fw, rom_version)
    }
}

/// Walk the instructions stored backwards before the tail signature (each
/// `data, length, opcode` read from the end) until the project-id one.
fn project_id(fw: &[u8], header: usize) -> Result<u8, &'static str> {
    let mut end = fw.len() - TAIL.len();
    while end >= header + 3 {
        let (data, length, opcode) = (fw[end - 3], fw[end - 2], fw[end - 1]);
        end -= 3;
        if opcode == 0xff {
            break;
        }
        if length == 0 {
            return Err("epatch instruction of length 0");
        }
        if opcode == 0 && length == 1 {
            return Ok(data);
        }
        end = end
            .checked_sub(usize::from(length))
            .ok_or("epatch instruction runs off the start")?;
    }
    Err("no project id in the epatch file")
}

fn le16(fw: &[u8], at: usize) -> Option<u16> {
    let b = fw.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn le32(fw: &[u8], at: usize) -> Option<u32> {
    let b = fw.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn v1_patch(fw: &[u8], rom_version: u8) -> Result<Vec<u8>, &'static str> {
    let count = usize::from(le16(fw, 12).ok_or("short v1 header")?);
    // The three tables (2 + 2 + 4 octets per patch) must lie in the file.
    if fw.len() < V1_HEADER + TAIL.len() + 3 + 8 * count {
        return Err("patch tables run past the file");
    }
    let wanted = u16::from(rom_version) + 1;
    let ids = V1_HEADER;
    let lengths = ids + 2 * count;
    let offsets = lengths + 2 * count;
    let mut found = None;
    for i in 0..count {
        if le16(fw, ids + 2 * i) == Some(wanted) {
            let length = le16(fw, lengths + 2 * i).ok_or("short length table")?;
            let offset = le32(fw, offsets + 4 * i).ok_or("short offset table")?;
            found = Some((usize::from(length), offset as usize));
            break;
        }
    }
    // An offset of 0 is "no patch" (the header sits there).
    let (length, offset) = found
        .filter(|&(_, o)| o != 0)
        .ok_or("no patch for this ROM")?;
    if length < 4 || offset > fw.len() || length > fw.len() - offset {
        return Err("patch lies outside the file");
    }
    // The patch's last four octets are replaced with the file's version.
    let mut out = Vec::with_capacity(length);
    out.extend_from_slice(&fw[offset..offset + length - 4]);
    out.extend_from_slice(&fw[8..12]);
    Ok(out)
}

fn v2_patch(fw: &[u8], rom_version: u8, key_id: u8) -> Result<Vec<u8>, &'static str> {
    let count = le32(fw, 16).ok_or("short v2 header")?;
    let mut rest = &fw[V2_HEADER..fw.len() - V2_TAIL_CUT];
    let mut subs: Vec<(u8, &[u8])> = Vec::new();
    for _ in 0..count {
        let (Some(opcode), Some(length)) = (le32(rest, 0), le32(rest, 4)) else {
            break;
        };
        let Some(section) = rest.get(8..).and_then(|r| r.get(..length as usize)) else {
            break;
        };
        rest = &rest[8 + section.len()..];
        match opcode {
            SECTION_SNIPPETS | SECTION_DUMMY => {}
            // A chip with no key id ignores the signed sections.
            SECTION_SECURITY if key_id != 0 => {}
            _ => continue,
        }
        collect(section, opcode, rom_version, key_id, &mut subs)?;
    }
    let out: Vec<u8> = subs
        .iter()
        .flat_map(|(_, data)| data.iter().copied())
        .collect();
    if out.is_empty() {
        return Err("no patch for this ROM");
    }
    Ok(out)
}

/// Add the sub-sections of one section that match the ROM (and, for signed
/// ones, the key) to `subs`, ordered by priority: a new entry goes before the
/// first existing one of equal or higher priority.
fn collect<'a>(
    section: &'a [u8],
    opcode: u32,
    rom_version: u8,
    key_id: u8,
    subs: &mut Vec<(u8, &'a [u8])>,
) -> Result<(), &'static str> {
    let count = le16(section, 0).ok_or("short section header")?;
    let mut rest = &section[4.min(section.len())..];
    for _ in 0..count {
        let Some(head) = rest.get(..8) else { break };
        let length = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let Some(data) = rest.get(8..).and_then(|r| r.get(..length)) else {
            break;
        };
        rest = &rest[8 + length..];
        let (eco, prio, cb0) = (head[0], head[1], head[2]);
        if u16::from(eco) != u16::from(rom_version) + 1 {
            continue;
        }
        if opcode == SECTION_SECURITY && cb0 != key_id {
            continue;
        }
        let at = subs
            .iter()
            .position(|(p, _)| *p >= prio)
            .unwrap_or(subs.len());
        subs.insert(at, (prio, data));
    }
    Ok(())
}
