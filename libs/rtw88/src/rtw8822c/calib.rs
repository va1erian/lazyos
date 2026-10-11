//! RF calibration after a channel switch, and the false alarm counters
//! (upstream `rtw8822c_phy_calibration`, `rtw8822c_rfk_power_save`,
//! `rtw8822c_do_iqk`, `rtw8822c_do_lck`, `rtw8822c_false_alarm_statistics`).
//!
//! **What runs.** Upstream's sequence is TX gain calibration (TXGAPK),
//! IQK, DPK. IQK runs in the firmware (one H2C, then the driver polls for
//! 0xaa); DPK is ported ([`super::dpk`]). **TXGAPK is not ported yet**: it
//! fine-tunes the TX gain steps (about 650 lines upstream) and is a
//! documented later step; upstream itself skips it when
//! `RTW_DM_CAP_TXGAPK` is set in `dm_flags`, so this is the same as running
//! upstream with that flag. TX works without it.

use crate::dev::{Dev, RF_PATH_A, RF_PATH_B};
use crate::fw::{self, IqkPara};
use crate::hci::{Hci, RegExt};
use crate::phy::rf::{read_rf, write_rf, RFREG_MASK};
use crate::rtw8822c::dpk;
use crate::rtw8822c::regs::*;

/// Upstream `IQK_DONE_8822C`.
const IQK_DONE: u8 = 0xaa;

/// Upstream `rtw8822c_phy_calibration` (without TXGAPK, see the module
/// docs).
pub fn phy_calibration<H: Hci>(dev: &mut Dev<H>) {
    rfk_power_save(dev, false);
    do_iqk(dev);
    dpk::do_dpk(dev);
    rfk_power_save(dev, true);
}

/// Upstream `rtw8822c_rfk_power_save`: the DPD block's power save, per path.
pub fn rfk_power_save<H: Hci>(dev: &mut Dev<H>, power_save: bool) {
    for path in 0..u32::from(dev.hal.rf_path_num).min(2) {
        dev.hci.write32_mask(REG_NCTL0, BIT_SEL_PATH, path);
        dev.hci
            .write32_mask(REG_DPD_CTL1_S0, BIT_PS_EN, u32::from(!power_save));
    }
}

/// Upstream `rtw8822c_do_iqk`: ask the firmware for IQK (clearing old
/// results) and wait up to 300 ms for its done code. Returns whether it
/// finished (upstream only warns).
pub fn do_iqk<H: Hci>(dev: &mut Dev<H>) -> bool {
    // A refused H2C shows up as the poll below timing out.
    let _ = fw::do_iqk(
        dev,
        &IqkPara {
            clear: 1,
            segment_iqk: 0,
        },
    );
    let mut done = false;
    let mut waited = 0;
    loop {
        if dev.hci.read8(REG_RPT_CIP) == IQK_DONE {
            done = true;
            break;
        }
        if waited >= 300_000 {
            break;
        }
        dev.hci.delay_us(20_000);
        waited += 20_000;
    }
    dev.hci.write8(REG_IQKSTAT, 0x0);
    dev.dm.iqk_done = done;
    done
}

/// Upstream `rtw8822c_do_lck`: LO calibration (the synthesiser's AAC loop),
/// run by power tracking when the temperature moved.
pub fn do_lck<H: Hci>(dev: &mut Dev<H>) {
    write_rf(dev, RF_PATH_A, RF_SYN_CTRL, RFREG_MASK, 0x80010);
    write_rf(dev, RF_PATH_A, RF_SYN_PFD, RFREG_MASK, 0x1f0fa);
    dev.hci.delay_us(1);
    write_rf(dev, RF_PATH_A, RF_AAC_CTRL, RFREG_MASK, 0x80000);
    write_rf(dev, RF_PATH_A, RF_SYN_AAC, RFREG_MASK, 0x80001);
    // read_poll_timeout(..., val != 0x1, 1000, 100000, sleep_before_read)
    let mut waited = 0;
    while waited < 100_000 {
        dev.hci.delay_us(1000);
        waited += 1000;
        if read_rf(dev, RF_PATH_A, RF_AAC_CTRL, 0x1000) != 0x1 {
            break;
        }
    }
    write_rf(dev, RF_PATH_A, RF_SYN_PFD, RFREG_MASK, 0x1f0f8);
    write_rf(dev, RF_PATH_B, RF_SYN_CTRL, RFREG_MASK, 0x80010);
    write_rf(dev, RF_PATH_A, RF_FAST_LCK, RFREG_MASK, 0x0f000);
    write_rf(dev, RF_PATH_A, RF_FAST_LCK, RFREG_MASK, 0x4f000);
    dev.hci.delay_us(1);
    write_rf(dev, RF_PATH_A, RF_FAST_LCK, RFREG_MASK, 0x0f000);
}

/// The counters `rtw8822c_false_alarm_statistics` reads into `dm_info`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FalseAlarm {
    pub cck_fa: u32,
    pub ofdm_fa: u32,
    pub total_fa: u32,
    pub cck_ok: u16,
    pub cck_err: u16,
    pub ofdm_ok: u16,
    pub ofdm_err: u16,
    pub ht_ok: u16,
    pub ht_err: u16,
    pub vht_ok: u16,
    pub vht_err: u16,
    pub ofdm_cca: u16,
    pub cck_cca: u16,
    pub total_cca: u32,
}

/// Upstream `rtw8822c_false_alarm_statistics`: read the counters, then
/// reset them. (DIG, which would act on them, is not run: the initial gain
/// the BB table sets is kept, which is what upstream starts from too.)
pub fn false_alarm_statistics<H: Hci>(dev: &mut Dev<H>) -> FalseAlarm {
    let h = &mut dev.hci;
    let cck_enable = h.read32(REG_ENCCK) & BIT_CCK_BLK_EN != 0;
    let cck_fa = u32::from(h.read16(REG_CCK_FACNT));
    let c1 = h.read32(REG_OFDM_FACNT1);
    let c2 = h.read32(REG_OFDM_FACNT2);
    let c3 = h.read32(REG_OFDM_FACNT3);
    let c4 = h.read32(REG_OFDM_FACNT4);
    let c5 = h.read32(REG_OFDM_FACNT5);
    let (hi, lo) = (|v: u32| v >> 16, |v: u32| v & 0xffff);
    // parity, rate illegal, crc8, crc8 VHT-A, mcs, mcs VHT, fast fsync, sb search
    let ofdm_fa = hi(c1) + lo(c2) + hi(c2) + lo(c3) + lo(c4) + hi(c4) + lo(c5) + hi(c5);
    let mut fa = FalseAlarm {
        cck_fa,
        ofdm_fa,
        total_fa: ofdm_fa + if cck_enable { cck_fa } else { 0 },
        ..FalseAlarm::default()
    };
    let split = |v: u32| ((v & 0xffff) as u16, (v >> 16) as u16);
    (fa.cck_ok, fa.cck_err) = split(h.read32(0x2c04));
    (fa.ofdm_ok, fa.ofdm_err) = split(h.read32(0x2c14));
    (fa.ht_ok, fa.ht_err) = split(h.read32(0x2c10));
    (fa.vht_ok, fa.vht_err) = split(h.read32(0x2c0c));
    let (cck_cca, ofdm_cca) = split(h.read32(0x2c08));
    fa.cck_cca = cck_cca;
    fa.ofdm_cca = ofdm_cca;
    fa.total_cca = u32::from(ofdm_cca) + if cck_enable { u32::from(cck_cca) } else { 0 };

    h.write32_mask(REG_CCANRX, BIT_CCK_FA_RST, 0);
    h.write32_mask(REG_CCANRX, BIT_CCK_FA_RST, 2);
    h.write32_mask(REG_CCANRX, BIT_OFDM_FA_RST, 0);
    h.write32_mask(REG_CCANRX, BIT_OFDM_FA_RST, 2);
    // Disable RX clock gating to reset the counters.
    h.clr32(REG_RX_BREAK, BIT_COM_RX_GCK_EN);
    h.set32(REG_CNT_CTRL, BIT_ALL_CNT_RST);
    h.clr32(REG_CNT_CTRL, BIT_ALL_CNT_RST);
    h.set32(REG_RX_BREAK, BIT_COM_RX_GCK_EN);
    dev.dm.fa = fa;
    fa
}
