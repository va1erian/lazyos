//! `phy_set_param`, `mac_init` and `phy_calibration` on the fake bus: the
//! end state of the registers upstream's sequences leave, worked out from
//! `rtw8822c.c`.

use super::*;
use crate::dev::{Dev, BB_PATH_AB};
use crate::fake::FakeHci;
use crate::hci::Hci;
use crate::phy::cond::parse_phy_cond;
use crate::phy::PhyCond;

fn dev() -> Dev<FakeHci> {
    let mut d = Dev::new(FakeHci::default());
    d.hal.rf_path_num = 2;
    d.hal.rf_phy_num = 2;
    d.hal.cut_version = 3;
    d.hal.antenna_tx = BB_PATH_AB;
    d.hal.antenna_rx = BB_PATH_AB;
    d.efuse.rfe_option = 1;
    d.efuse.crystal_cap = 0x3c;
    d.efuse.thermal_meter_k = 0x1b;
    d.phy.cond = PhyCond::setup(3, 0, 1);
    // Physical efuse reads complete at once (the trims read 0x00).
    d.hci
        .force
        .push((regs::REG_EFUSE_CTRL, regs::BIT_EF_FLAG, regs::BIT_EF_FLAG));
    d
}

fn last_table_write(words: &[u32], cond: &PhyCond, addr: u32) -> Option<u32> {
    let mut v = None;
    parse_phy_cond(cond, words, |a, d| {
        if a == addr {
            v = Some(d);
        }
    });
    v
}

#[test]
fn phy_set_param_end_state() {
    let mut d = dev();
    phy_set_param(&mut d).unwrap();
    let h = &d.hci;
    // BB and RF domains powered.
    assert_eq!(
        h.peek32(0x00) >> 16 & 0x3,
        0x3,
        "SYS_FUNC_EN BB_GLB_RST|BB_RSTB"
    );
    assert_eq!(h.peek32(0x1c) >> 24 & 0x7, 0x7, "RF_CTRL EN|RSTB|SDM_RSTB");
    assert_eq!(h.peek32(0xec) >> 24 & 0x7, 0x7, "WLRF1 BBRF_EN");
    assert_eq!(h.peek32(0xa70) & 0x3ff, 0x3ff, "DPD off on every low rate");
    // Crystal cap 0x3c in both 7-bit fields of 0x1040[23:10].
    assert_eq!(h.peek32(0x1040) >> 10 & 0x3fff, 0x3c | (0x3c << 7));
    // CCK/OFDM blocks on again after the tables (header_file_init(false)).
    assert_eq!(h.peek32(0x1c3c) & 0x3, 0x3);
    assert_eq!(
        h.peek32(0x180c) & 0x1000_0003,
        0x1000_0003,
        "3-wire TX/RX/PI on path A"
    );
    // The BB table landed (0x808 is touched by nothing after it).
    let bb808 = last_table_write(tables::BB, &d.phy.cond, 0x808).unwrap();
    assert_eq!(h.peek32(0x808), bb808);
    // 2T2R: both paths active, OFDM 1SS from A, CCK TX on A, RX on both.
    assert_eq!(h.peek32(0x1800) & 0xfffff, 0x33312);
    assert_eq!(h.peek32(0x4100) & 0xfffff, 0x33312);
    assert_eq!(h.peek32(0x820) & 0xff, 0x31);
    assert_eq!(h.peek32(0x1e2c) & 0xffff, 0x0400);
    assert_eq!(h.peek32(0x1a04) >> 24, 0x81, "CCK TX A (0x8), RX A+B (0x1)");
    let v824 = h.peek32(0x824);
    assert_eq!(
        (v824 >> 24 & 0xf, v824 >> 16 & 0xf),
        (3, 3),
        "OFDM RX paths A|B"
    );
    // CCK gain window from what the BB table wrote: 0x1a98 = 0xacc4c040
    // (upper msb 3, lower msb 1), 0x1aa8 = 0xba0f0004 (upper lsb 0xf),
    // 0x1a70 = 0x8000 (lower lsb 0).
    assert_eq!((d.dm.cck_gi_u_bnd, d.dm.cck_gi_l_bnd), (0x3f, 0x10));
    // EDCCA thresholds at the maximum (0x7f + 0x80) for both edges.
    assert_eq!(h.peek32(0x84c) >> 16, 0xffff);
    // Beamformee defaults (rtw_bf_phy_init).
    assert_eq!(h.peek32(0x14c0) & 0x1_f0ff, 0x1_a000);
    assert_eq!(h.peek32(0x167c) & 0xff, 0x70);
    assert_eq!(h.peek32(0x45c) >> 24, 0x10, "NDPA at 6M");
    // State
    assert!(d.dm.dpk.is_dpk_pwr_on);
    assert_eq!(d.dm.crystal_cap, 0x3c);
    assert!(d.dm.cfo_is_adjust);
    assert_eq!(d.dm.thermal_avg, [0xff, 0xff]);
    assert_eq!(d.dm.thermal_meter_lck, 0x1b);
}

#[test]
fn dac_calibration_measures_then_restores_what_it_kept() {
    let mut d = dev();
    let adc_cal = |d: &Dev<FakeHci>| {
        d.hci
            .writes()
            .iter()
            .any(|&(_, a, v)| a == 0x1c3c && v == 0xa8003)
    };
    phy_set_param(&mut d).unwrap();
    assert!(adc_cal(&d), "nothing saved: DACK measures (path A ADC cal)");
    // The 16 registers DACK borrows are back as the tables left them.
    let igi = last_table_write(tables::BB, &d.phy.cond, 0x1d70);
    assert_eq!(Some(d.hci.peek32(0x1d70)), igi.or(Some(0)));

    // With results kept and the DAC reporting ready (state 6), the next
    // bring-up restores them instead of measuring.
    let mut d2 = dev();
    d2.dm.dack.msbk[0][0][0] = 0x1ab;
    for r in [0x2808, 0x2834, 0x4508, 0x4534] {
        d2.hci.force.push((r, 0x7f_ff8f, 0x7f_ff86));
    }
    phy_set_param(&mut d2).unwrap();
    assert!(!adc_cal(&d2));
    // MSB word 0 of path A's I vector written back into 0x18b4[11:3].
    assert!(d2
        .hci
        .writes()
        .iter()
        .any(|&(_, a, v)| a == 0x18b4 && (v >> 3) & 0x1ff == 0x1ab));
}

#[test]
fn mac_init_end_state() {
    let mut d = dev();
    mac_init(&mut d).unwrap();
    let h = &d.hci;
    assert_eq!(h.peek32(0x608), 0xe400_220e, "RCR");
    assert_eq!(
        h.peek32(0x420) & 0xffff,
        0x1f80,
        "TXQ control, report enable"
    );
    assert_eq!(h.peek32(0x514), 0x100a_0e0a, "SIFS");
    assert_eq!(h.peek32(0x500), 0x002f_a226);
    assert_eq!(h.peek32(0x508), 0x005e_a42b);
    assert_eq!(
        h.peek32(0x60c) & 0xff,
        24,
        "RX packet limit in 512-byte units"
    );
    assert_eq!(h.peek32(0x6a0), 0xffff_ffff, "RX filter maps 0 and 1");
    // RX power-save filter: thresholds 1, checks on, OFDM reset, error 7.
    assert_eq!(h.peek32(0x1610), 0x0550_3f87);
    assert_eq!(h.peek32(0x304), 0x3333_0000);
    assert_eq!(h.peek32(0x718) & 0x40, 0x40, "no VHT-SIG-B CRC check");
}

#[test]
fn antenna_choices_and_ldo() {
    let mut d = dev();
    assert!(
        set_antenna(&mut d, regs::BB_PATH_B, regs::BB_PATH_B).is_err(),
        "B alone cannot receive"
    );
    assert!(set_antenna(&mut d, 0, regs::BB_PATH_A).is_err());
    set_antenna(&mut d, regs::BB_PATH_A, regs::BB_PATH_A).unwrap();
    assert_eq!(d.hci.peek32(0x4100) & 0xfffff, 0x11111, "path B idle");
    assert_eq!(d.hci.peek32(0x820) & 0xff, 0x11);
    cfg_ldo25(&mut d, true);
    assert_eq!(d.hci.peek32(0x28) >> 8 & 1, 1);
    cfg_ldo25(&mut d, false);
    assert_eq!(d.hci.peek32(0x28) >> 8 & 1, 0);
}

#[test]
fn calibration_runs_iqk_through_the_firmware_and_dpk() {
    let mut d = dev();
    // RF 0x18 of path A: 20 MHz, 2.4 GHz, channel 6.
    d.hci.write32(0x3c60, 0x3006);
    d.hci.write32(0x520, 0x1234_5678);
    d.hci.force.push((0x2d9c, 0xff, 0xaa)); // IQK done
    d.dm.dpk.is_dpk_pwr_on = true; // set by the calibration init table load
    phy_calibration(&mut d);
    assert!(d.dm.iqk_done);
    // One H2C packet: IQK (sub-command 0x0e) with "clear" set.
    assert_eq!(d.hci.h2c.len(), 1);
    let pkt = &d.hci.h2c[0][48..];
    assert_eq!((pkt[0], pkt[1], pkt[2], pkt[8] & 1), (0x01, 0xff, 0x0e, 1));
    assert!(d
        .hci
        .writes()
        .iter()
        .any(|&(w, a, v)| w == 1 && a == 0x1b10 && v == 0));
    // DPK read the channel, and gave back the TX pause register.
    assert_eq!(
        (d.dm.dpk.dpk_ch, d.dm.dpk.dpk_band, d.dm.dpk.dpk_bw),
        (6, 1, 3)
    );
    assert!(d
        .hci
        .writes()
        .iter()
        .any(|&(w, a, v)| w == 1 && a == 0x522 && v == 0xff));
    assert_eq!(d.hci.peek32(0x520), 0x1234_5678);
    // The engine never answered 0x55 here: both paths fall back to the
    // identity coefficients and DPD stays off.
    assert_eq!(d.dm.dpk.path_ok, 0);
    assert_eq!(d.hci.peek32(0x1b0c), 0x1fff);
    // A second calibration on the same channel reloads instead.
    d.hci.log.clear();
    phy_calibration(&mut d);
    assert!(d.dm.dpk.is_reload);
}
