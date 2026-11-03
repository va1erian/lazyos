//! Synthetic firmware images for the firmware-phase tests: the container
//! layouts `libs/btfw` parses, small enough to read, and the real files from
//! `target/firmware` when `tools/firmware/fetch.py --switch bt` fetched them.

use std::path::PathBuf;
use std::vec::Vec;

fn command(opcode: u16, params: &[u8]) -> Vec<u8> {
    let mut out = opcode.to_le_bytes().to_vec();
    out.push(params.len() as u8);
    out.extend_from_slice(params);
    out
}

/// The command stream of an Intel image, in 4-octet-aligned groups: boot
/// parameters plus a NOP, three big commands, then small ones.
pub fn intel_body() -> Vec<u8> {
    let mut params = 0x0010_0800u32.to_le_bytes().to_vec();
    params.extend_from_slice(&[3, 14, 26]);
    let mut out = command(0xfc0e, &params);
    out.extend(command(0xfc8e, &[0, 0, 0]));
    for (i, n) in [253usize, 253, 249].into_iter().enumerate() {
        out.extend(command(0xfc0b, &std::vec![0x50 + i as u8; n]));
    }
    out.extend(command(0xfc8e, &[1, 2, 3]));
    out.extend(command(0xfc8e, &[4, 5, 6]));
    out.extend(command(0xfc8e, &[0]));
    out
}

/// A `.sfi`: the RSA block, the ECDSA block when asked, then the stream.
pub fn intel_sfi(ecdsa: bool) -> Vec<u8> {
    let mut file: Vec<u8> = (0..644).map(|i| (i % 251) as u8 ^ 0x40).collect();
    file[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    if ecdsa {
        let mut block: Vec<u8> = (0..320).map(|i| (i % 241) as u8 ^ 0x20).collect();
        block[0] = 0x06;
        block[8..12].copy_from_slice(&0x0002_0000u32.to_le_bytes());
        file.extend(block);
    }
    file.extend(intel_body());
    file
}

/// Two device-configuration records, as the real `.ddc` files carry.
pub const DDC: [u8; 9] = [0x03, 0x28, 0x01, 0x18, 0x04, 0x29, 0x01, 0x03, 0x00];

/// A v1 Realtek patch file with one patch for ROM version 1 (chip id 2) of
/// the RTL8761 family (project 14), enough fragments to wrap nothing.
pub fn realtek_patch() -> Vec<u8> {
    let content: Vec<u8> = (0..700).map(|i| (i % 253) as u8).collect();
    let mut out = b"Realtech".to_vec();
    out.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&((content.len() + 4) as u16).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());
    out.extend_from_slice(&content);
    out.extend_from_slice(&[0xEE; 4]);
    out.extend_from_slice(&[14, 1, 0]);
    out.extend_from_slice(&[0x51, 0x04, 0xfd, 0x77]);
    out
}

/// A real file fetched by `tools/firmware/fetch.py --switch bt`, if present.
pub fn real(name: &str) -> Option<Vec<u8>> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "../../target/firmware", name]
        .iter()
        .collect();
    let data = std::fs::read(path).ok();
    if data.is_none() {
        std::eprintln!("bthost: {name} not fetched, real-file check skipped");
    }
    data
}
