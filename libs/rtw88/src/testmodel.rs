//! A healthy RTL8822CE as the host tests see it: [`FakeHci`] with the
//! forced bits and write hooks a real chip's registers would give the
//! bring-up code (power state in `REG_CR`, polls that complete, DDMA that
//! finishes, an efuse, the firmware's feature report). Test-only.
//!
//! Each behaviour is the minimum the upstream sequences wait for, so a
//! test that removes one sees the step that depends on it fail.

use std::vec;
use std::vec::Vec;

use crate::fake::FakeHci;

/// `REG_SYS_CFG1` of a 2T2R chip, cut B (`BIT_RF_TYPE_ID`, chip version 1).
pub const SYS_CFG1_2T2R_CUT_B: u32 = (1 << 27) | (1 << 12);

/// The feature report's second word: 2 streams, 80 MHz, 2 antennas, VHT.
pub const HW_FEATURE_W1: u32 = (2 << 19) | (7 << 16) | (2 << 21) | (3 << 26);

/// A chip that is powered off (`REG_CR` reads 0xEA) and does everything
/// the bring-up waits for. `efuse_phy` is its physical efuse.
pub fn healthy_chip(efuse_phy: Vec<u8>) -> FakeHci {
    let mut f = FakeHci::default();
    f.regs[0x100] = 0xEA;
    f.regs[0xF0..0xF4].copy_from_slice(&SYS_CFG1_2T2R_CUT_B.to_le_bytes());
    // 0x0006 bit 1 (power ready) set; 0x0005 bits 0-1 (power commands)
    // self-clearing.
    f.force.push((0x0004, 0x0002_0300, 0x0002_0000));
    // DDMA never busy, checksum always good.
    f.force.push((0x1208, (1 << 31) | (1 << 27), 0));
    // The firmware reports ready (FW_READY under FW_READY_MASK).
    f.force.push((0x0080, 0xCFFF, 0xC078));
    // LTE coex indirect window ready.
    f.force.push((0x1700, 1 << 29, 1 << 29));
    // Automatic LLT init done.
    f.force.push((0x0208, 1, 0));

    // The power sequences: APFM_ONMAC (0x05 bit 0) leaves "card disabled",
    // APFM_OFFMAC (0x05 bit 1) returns to it.
    f.hooks.push(std::boxed::Box::new(|regs: &mut [u8], width, addr, val| {
        if width == 1 && addr == 0x05 {
            if val & 0x01 != 0 {
                regs[0x100] = 0x00;
            }
            if val & 0x02 != 0 {
                regs[0x100] = 0xEA;
            }
        }
    }));
    // The efuse: a read command (flag clear) makes the byte and the flag
    // appear.
    f.hooks.push(std::boxed::Box::new(move |regs: &mut [u8], width, addr, val| {
        if width == 4 && addr == 0x30 && val & (1 << 31) == 0 {
            let a = ((val >> 8) & 0x3ff) as usize;
            regs[0x30] = efuse_phy.get(a).copied().unwrap_or(0xff);
            regs[0x33] |= 0x80;
        }
    }));
    // The firmware answers a feature dump request at once.
    f.hooks.push(std::boxed::Box::new(|regs: &mut [u8], width, addr, val| {
        if width == 1 && addr == 0x1A0 && val == 0xfd {
            regs[0x1A0] = 0x19;
            regs[0x1A2 + 4..0x1A2 + 8].copy_from_slice(&HW_FEATURE_W1.to_le_bytes());
        }
    }));
    f
}

/// An efuse with nothing written (every byte 0xff).
pub fn blank_efuse() -> Vec<u8> {
    vec![0xff; 512]
}

/// A firmware image with the given section sizes (checksums excluded),
/// version 9.9.15, signed, the 8822C's load addresses.
pub fn synth_fw(dmem: usize, imem: usize, emem: Option<usize>) -> Vec<u8> {
    let mut h = vec![0u8; 64];
    h[0..2].copy_from_slice(&0x8822u16.to_le_bytes());
    h[4..6].copy_from_slice(&9u16.to_le_bytes());
    h[6] = 9;
    h[7] = 15;
    h[0x0C..0x10].copy_from_slice(&0x1e7u32.to_le_bytes());
    h[0x18] = if emem.is_some() { 0x18 } else { 0x08 };
    h[0x1C..0x1E].copy_from_slice(&0x0fu16.to_le_bytes());
    h[0x20..0x24].copy_from_slice(&0x8020_0000u32.to_le_bytes());
    h[0x24..0x28].copy_from_slice(&(dmem as u32).to_le_bytes());
    h[0x30..0x34].copy_from_slice(&(imem as u32).to_le_bytes());
    h[0x34..0x38].copy_from_slice(&(emem.unwrap_or(0) as u32).to_le_bytes());
    h[0x38..0x3C].copy_from_slice(&0x8010_0000u32.to_le_bytes());
    h[0x3C..0x40].copy_from_slice(&0x8003_0000u32.to_le_bytes());
    let mut img = h;
    for (n, len) in [Some(dmem), Some(imem), emem].into_iter().flatten().enumerate() {
        img.extend((0..len + 8).map(|i| (i as u8).wrapping_add(n as u8 * 0x40)));
    }
    img
}

/// The real firmware when `RTW88_FW` names it (never committed: it is
/// linux-firmware's `rtw88/rtw8822c_fw.bin`).
pub fn real_fw() -> Option<Vec<u8>> {
    let path = std::env::var_os("RTW88_FW")?;
    std::fs::read(path).ok()
}
