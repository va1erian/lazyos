//! Factory RF trims from the physical efuse (upstream `rtw8822c_rf_x2_check`,
//! `rtw8822c_thermal_trim`, `rtw8822c_power_trim`,
//! `rtw8822c_set_power_trim`, `rtw8822c_pa_bias`).
//!
//! These bytes live outside the logical map, so each is read with
//! `rtw_read8_physical_efuse` (0xff, or a timeout, means "not trimmed").

use crate::dev::{Dev, RF_PATH_A, RF_PATH_MAX};
use crate::hci::{Hci, RegExt};
use crate::phy::rf::{read_rf, write_rf, RFREG_MASK};
use crate::rtw8822c::regs::*;
use crate::rtw8822c::standin::read8_physical_efuse;

fn efuse8<H: Hci>(dev: &mut Dev<H>, addr: u16) -> u8 {
    read8_physical_efuse(dev, addr).unwrap_or(EFUSE_READ_FAIL)
}

fn paths<H: Hci>(dev: &Dev<H>) -> usize {
    usize::from(dev.hal.rf_path_num).min(RF_PATH_MAX)
}

/// Upstream `rtw8822c_rf_x2_check`: if the X2 synthesiser calibration is
/// stuck busy, restart it.
pub fn rf_x2_check<H: Hci>(dev: &mut Dev<H>) {
    dev.hci.delay_us(1000);
    if read_rf(dev, RF_PATH_A, 0xb8, bit(15)) == 1 {
        write_rf(dev, RF_PATH_A, 0xb8, RFREG_MASK, 0xc4440);
        write_rf(dev, RF_PATH_A, 0xba, RFREG_MASK, 0x6840d);
        write_rf(dev, RF_PATH_A, 0xb8, RFREG_MASK, 0x80440);
        dev.hci.delay_us(1000);
    }
}

/// Upstream `rtw8822c_thermal_trim`: efuse bit 0 moves to bit 3 and bits
/// 3:1 down by one, into RF 0x43[19:16]. Stops at the first untrimmed path.
pub fn thermal_trim<H: Hci>(dev: &mut Dev<H>) {
    let regs = [PPG_THERMAL_A, PPG_THERMAL_B];
    for (path, &reg) in regs.iter().enumerate().take(paths(dev)) {
        let pg = efuse8(dev, reg);
        if pg == EFUSE_READ_FAIL {
            return;
        }
        let thermal = ((pg >> 1) & 0x7) | ((pg & 1) << 3);
        write_rf(dev, path, 0x43, RF_THEMAL_MASK, u32::from(thermal));
    }
}

/// Upstream `rtw8822c_set_power_trim`: write the 8 gain trims into the RF
/// gain LUT (entries 0x0 to 0xe, some sharing a trim).
fn set_power_trim<H: Hci>(dev: &mut Dev<H>, bb_gain: &[[u8; 8]; 2]) {
    const SEQ: [(u32, usize); 15] = [
        (0x0, 0),
        (0x1, 1),
        (0x2, 2),
        (0x3, 2),
        (0x4, 3),
        (0x5, 4),
        (0x6, 5),
        (0x7, 6),
        (0x8, 7),
        (0x9, 3),
        (0xa, 4),
        (0xb, 5),
        (0xc, 6),
        (0xd, 7),
        (0xe, 7),
    ];
    for path in 0..paths(dev) {
        write_rf(dev, path, 0xee, bit(19), 1);
        for &(seq, idx) in &SEQ {
            write_rf(dev, path, 0x33, RFREG_MASK, seq);
            write_rf(dev, path, 0x3f, RFREG_MASK, u32::from(bb_gain[path][idx]));
        }
        write_rf(dev, path, 0xee, bit(19), 0);
    }
}

/// Upstream `rtw8822c_power_trim`: three 2.4 GHz trims (both paths in one
/// byte) and five 5 GHz trims per path; written only if any is present.
/// Low-rate DPD is disabled again afterwards.
pub fn power_trim<H: Hci>(dev: &mut Dev<H>) {
    const EF_2G: [u16; 3] = [PPG_2GL_TXAB, PPG_2GM_TXAB, PPG_2GH_TXAB];
    const EF_5G: [[u16; 5]; 2] = [
        [PPG_5GL1_TXA, PPG_5GL2_TXA, PPG_5GM1_TXA, PPG_5GM2_TXA, PPG_5GH1_TXA],
        [PPG_5GL1_TXB, PPG_5GL2_TXB, PPG_5GM1_TXB, PPG_5GM2_TXB, PPG_5GH1_TXB],
    ];
    let mut bb_gain = [[0u8; 8]; 2];
    let mut set = false;
    for (i, &reg) in EF_2G.iter().enumerate() {
        let pg = efuse8(dev, reg);
        if pg == EFUSE_READ_FAIL {
            continue;
        }
        set = true;
        bb_gain[0][i] = pg & PPG_2G_A_MASK;
        bb_gain[1][i] = (pg & PPG_2G_B_MASK) >> 4;
    }
    for i in 0..5 {
        for path in 0..paths(dev) {
            let pg = efuse8(dev, EF_5G[path][i]);
            if pg == EFUSE_READ_FAIL {
                continue;
            }
            set = true;
            bb_gain[path][i + EF_2G.len()] = pg & PPG_5G_MASK;
        }
    }
    if set {
        set_power_trim(dev, &bb_gain);
    }
    dev.hci.write32_mask(REG_DIS_DPD, DIS_DPD_MASK, DIS_DPD_RATEALL);
}

/// Upstream `rtw8822c_pa_bias`: the PA bias trims into RF 0x60. A missing
/// 2.4 GHz byte stops the function; the 5 GHz bytes are written unchecked
/// (an unprogrammed 0xff gives bias 0xf), exactly as upstream.
pub fn pa_bias<H: Hci>(dev: &mut Dev<H>) {
    let ef_2g = [PPG_PABIAS_2GA, PPG_PABIAS_2GB];
    let ef_5g = [PPG_PABIAS_5GA, PPG_PABIAS_5GB];
    for (path, &reg) in ef_2g.iter().enumerate().take(paths(dev)) {
        let pg = efuse8(dev, reg);
        if pg == EFUSE_READ_FAIL {
            return;
        }
        write_rf(dev, path, RF_PA, RF_PABIAS_2G_MASK, u32::from(pg & PPG_PABIAS_MASK));
    }
    for (path, &reg) in ef_5g.iter().enumerate().take(paths(dev)) {
        let pg = efuse8(dev, reg);
        write_rf(dev, path, RF_PA, RF_PABIAS_5G_MASK, u32::from(pg & PPG_PABIAS_MASK));
    }
}
