//! Reading the efuse (upstream `efuse.c`: `rtw_dump_physical_efuse_map`,
//! `rtw_dump_logical_efuse_map`, `rtw_read8_physical_efuse`,
//! `rtw_parse_efuse_map`; `main.c`: `rtw_dump_hw_feature`).
//!
//! The physical efuse is a log of writes: each record is a 1- or 2-byte
//! header naming a 8-byte logical block and which of its four words
//! follow. Replaying the log over an all-`0xff` map gives the logical map.
//! The physical bytes come from the chip and may be garbage on a damaged
//! card, so every index is bounds-checked.

use alloc::vec;
use alloc::vec::Vec;

use super::{HwCap, HW_CAP_IGNORE, HW_CAP_SUPP_BW40, HW_CAP_SUPP_BW80};
use crate::bringup::chip_calls;
use crate::dev::{Dev, RF_1T1R, BB_PATH_A};
use crate::error::{Error, Result};
use crate::fw::c2h::id::{HW_FEATURE_DUMP, HW_FEATURE_REPORT};
use crate::hci::{Hci, RegExt};
use crate::mac::regs::*;

/// `HW_FEATURE_LEN`.
const HW_FEATURE_LEN: usize = 13;

/// Upstream `rtw_dump_logical_efuse_map`: replay the physical log `phy`
/// (`physical_size` bytes of which the last `protect_size` are not log)
/// into a `logical_size` map that starts all `0xff`.
pub fn logical_map(phy: &[u8], physical_size: u32, protect_size: u32, logical_size: u32) -> Result<Vec<u8>> {
    let limit = (physical_size.saturating_sub(protect_size) as usize).min(phy.len());
    let mut log = vec![0xffu8; logical_size as usize];
    let mut idx = 0usize;
    while idx < limit {
        let hdr1 = phy[idx];
        let hdr2 = phy.get(idx + 1).copied().unwrap_or(0xff);
        // invalid_efuse_header: end of the log.
        if hdr1 == 0xff || ((hdr1 & 0x1f) == 0xf && hdr2 == 0xff) {
            break;
        }
        let (blk, word_en) = if (hdr1 & 0x1f) == 0xf {
            // 2-byte header: block[2:0] in hdr1[7:5], block[6:3] in hdr2[7:4].
            idx += 2;
            ((((hdr2 & 0xf0) >> 1) | ((hdr1 >> 5) & 0x07)) as usize, hdr2 & 0xf)
        } else {
            idx += 1;
            (((hdr1 & 0xf0) >> 4) as usize, hdr1 & 0xf)
        };
        for i in 0..4 {
            // A set bit means the word was not written.
            if word_en & (1 << i) != 0 {
                continue;
            }
            let log_idx = (blk << 3) + (i << 1);
            if idx + 1 > limit || log_idx + 2 > log.len() || idx + 2 > phy.len() {
                return Err(Error::Efuse("efuse record past the end of a map"));
            }
            log[log_idx] = phy[idx];
            log[log_idx + 1] = phy[idx + 1];
            idx += 2;
        }
    }
    Ok(log)
}

/// Upstream `rtw_dump_physical_efuse_map`: read every physical byte through
/// `REG_EFUSE_CTRL`, polling the ready flag up to a million times a byte.
fn dump_physical_efuse_map<H: Hci>(dev: &mut Dev<H>) -> Result<Vec<u8>> {
    let size = dev.efuse.physical_size as usize;
    let mut map = vec![0u8; size];
    // The 8822C has no efuse_grant op. switch_efuse_bank: the Wi-Fi bank.
    dev.hci.write32_mask(LDO_EFUSE_CTRL, MASK_EFUSE_BANK_SEL, 0);
    // disable 2.5V LDO
    chip_calls::cfg_ldo25(dev, false);

    let mut ctl = dev.hci.read32(EFUSE_CTRL);
    for (addr, byte) in map.iter_mut().enumerate() {
        ctl &= !(MASK_EF_DATA | (MASK_EF_ADDR << SHIFT_EF_ADDR));
        ctl |= (addr as u32 & MASK_EF_ADDR) << SHIFT_EF_ADDR;
        dev.hci.write32(EFUSE_CTRL, ctl & !EF_FLAG);
        let mut cnt = 1_000_000u32;
        loop {
            dev.hci.delay_us(1);
            ctl = dev.hci.read32(EFUSE_CTRL);
            cnt -= 1;
            if cnt == 0 {
                return Err(Error::Timeout { step: "efuse read", addr: EFUSE_CTRL });
            }
            if ctl & EF_FLAG != 0 {
                break;
            }
        }
        *byte = (ctl & MASK_EF_DATA) as u8;
    }
    Ok(map)
}

/// Upstream `rtw_read8_physical_efuse`: one physical byte (1 ms polls,
/// 100 ms at most).
pub fn read8_physical_efuse<H: Hci>(hci: &mut H, addr: u16) -> Result<u8> {
    hci.write32_mask(EFUSE_CTRL, 0x3ff00, u32::from(addr));
    hci.clr32(EFUSE_CTRL, EF_FLAG);
    hci.poll32("efuse byte", EFUSE_CTRL, EF_FLAG, EF_FLAG, 1000, 100_000)?;
    Ok(hci.read8(EFUSE_CTRL))
}

/// Upstream `rtw_parse_efuse_map`: physical map, logical map, then the
/// chip's layout parser fills `dev.efuse`.
pub fn parse_efuse_map<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    let phy = dump_physical_efuse_map(dev)?;
    let e = &dev.efuse;
    let log = logical_map(&phy, e.physical_size, e.protect_size, e.logical_size)?;
    // contract: rtw88-chip `rtw8822c::read_efuse(&mut Efuse, log_map: &[u8]) -> Result<()>`.
    chip_calls::read_efuse(&mut dev.efuse, &log)
}

/// Upstream `hw_bw_cap_to_bitamp`.
fn hw_bw_cap_to_bitmap(bw_cap: u8) -> u8 {
    let (b20, b40, b80) = (1 << 0, 1 << 1, 1 << 2);
    match bw_cap {
        HW_CAP_IGNORE | HW_CAP_SUPP_BW80 => b80 | b40 | b20,
        HW_CAP_SUPP_BW40 => b40 | b20,
        _ => b20,
    }
}

/// Upstream `rtw_dump_hw_feature` (+ `rtw_hw_config_rf_ant_num`): read the
/// report the firmware left in `REG_C2HEVT` after
/// [`HW_FEATURE_DUMP`] was requested, and clamp the RF configuration.
pub fn dump_hw_feature<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    if dev.hci.read8(C2HEVT) != HW_FEATURE_REPORT {
        return Err(Error::Efuse("no hw feature report"));
    }
    let mut hw = [0u8; HW_FEATURE_LEN];
    for (i, b) in hw.iter_mut().enumerate() {
        *b = dev.hci.read8(C2HEVT + 2 + i as u32);
    }
    dev.hci.write8(C2HEVT, 0);

    let w1 = u32::from_le_bytes([hw[4], hw[5], hw[6], hw[7]]);
    let field = |hi: u32, lo: u32| ((w1 >> lo) & ((1 << (hi - lo + 1)) - 1)) as u8;
    let cap = HwCap {
        hci: field(3, 0),
        bw: hw_bw_cap_to_bitmap(field(18, 16)),
        nss: field(20, 19),
        ant_num: field(23, 21),
        ptcl: field(27, 26),
    };
    dev.efuse.hw_cap = cap;

    let hal = &mut dev.hal;
    if cap.ant_num != HW_CAP_IGNORE && cap.ant_num < hal.rf_path_num && cap.ant_num == 1 {
        hal.rf_type = RF_1T1R;
        hal.rf_path_num = 1;
        hal.rf_phy_num = 1;
        hal.antenna_tx = BB_PATH_A;
        hal.antenna_rx = BB_PATH_A;
    }
    if cap.nss == HW_CAP_IGNORE || cap.nss > hal.rf_path_num {
        dev.efuse.hw_cap.nss = hal.rf_path_num;
    }
    Ok(())
}

/// What `rtw_chip_efuse_enable` writes before the firmware download so the
/// firmware leaves the feature report (upstream `C2H_HW_FEATURE_DUMP`).
pub fn request_hw_feature<H: Hci>(hci: &mut H) {
    hci.write8(C2HEVT, HW_FEATURE_DUMP);
}
