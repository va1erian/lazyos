//! Digital pre-distortion calibration, DPK (upstream `rtw8822c_do_dpk` and
//! its helpers in `rtw8822c.c`). The gain search state machine is in
//! [`super::dpk_agc`].
//!
//! DPK measures each path's PA compression at the current channel and
//! loads 20 pre-distortion coefficients per path; without it the chip still
//! transmits (with DPD off and the low rates already 6 dB down), just with
//! less clean high-order modulation. Results are kept per channel and
//! reloaded when the same channel comes back (`rtw8822c_dpk_reload`).

use crate::coex::{read_indirect_reg, write_indirect_reg};
use crate::dev::{Dev, RF_PATH_A, RF_PATH_B};
use crate::hci::{Hci, RegExt};
use crate::phy::rf::{read_rf, write_rf, RFREG_MASK};
use crate::rtw8822c::dpk_agc::{gainloss, rxbb_dc_cal, thermal_read};
use crate::rtw8822c::regs::*;
use crate::rtw8822c::tables::{DPK_AFE_IS_DPK, DPK_AFE_NO_DPK, DPK_MAC_BB};
use crate::rtw8822c::util::{check_hw_ready, Backup};

/// Upstream `DPK_CHANNEL_WIDTH_80`.
pub const DPK_BW_80: u8 = 1;
/// Upstream `RTW_BAND_2G` as `dpk_band` holds it.
pub const DPK_BAND_2G: u8 = 1;

/// Upstream `struct rtw_dpk_info`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DpkInfo {
    pub is_dpk_pwr_on: bool,
    pub is_reload: bool,
    /// `dpk_path_ok` bitmap (bit per path).
    pub path_ok: u8,
    pub thermal_dpk: [u8; 2],
    pub gnt_control: u32,
    pub gnt_value: u32,
    pub result: [u8; 2],
    pub dpk_txagc: [u8; 2],
    pub coef: [[u32; 20]; 2],
    pub dpk_gs: [u16; 2],
    pub thermal_dpk_delta: [u8; 2],
    pub pre_pwsf: [u8; 2],
    pub dpk_band: u8,
    pub dpk_ch: u8,
    pub dpk_bw: u8,
}

/// One-shot actions (upstream `enum rtw8822c_dpk_one_shot_action`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    CalPwr,
    GainLoss,
    DoDpk,
    DpkOn,
    Dagc,
}

/// Upstream `rtw8822c_parse_tbl_dpk`: `(addr, mask, data)` writes.
pub fn load_dpk_table<H: Hci>(h: &mut H, rows: &[[u32; 3]]) {
    for &[addr, mask, data] in rows {
        h.write32_mask(addr, mask, data);
    }
}

/// Upstream `rtw8822c_dpk_set_gnt_wl`: force GNT_WL around a one-shot so
/// Bluetooth cannot take the antenna mid-measurement.
fn set_gnt_wl<H: Hci>(dev: &mut Dev<H>, before: bool) {
    if before {
        dev.dm.dpk.gnt_control = dev.hci.read32(0x70);
        dev.dm.dpk.gnt_value = read_indirect_reg(dev, 0x38);
        dev.hci.write32_mask(0x70, bit(26), 0x1);
        write_indirect_reg(dev, 0x38, MASKBYTE1, 0x77);
    } else {
        let (v, c) = (dev.dm.dpk.gnt_value, dev.dm.dpk.gnt_control);
        write_indirect_reg(dev, 0x38, MASKDWORD, v);
        dev.hci.write32(0x70, c);
    }
}

fn subpage(path: usize) -> u32 {
    0x8 | ((path as u32) << 1)
}

/// Upstream `rtw8822c_dpk_get_cmd`.
fn get_cmd(dpk_bw: u8, action: Action, path: usize) -> u16 {
    let bw = if dpk_bw == DPK_BW_80 { 2 } else { 0 };
    let p = path as u16;
    let cmd = match action {
        Action::GainLoss => 0x14 + p,
        Action::DoDpk => 0x16 + p + bw,
        Action::DpkOn => 0x1a + p,
        Action::Dagc => 0x1c + p + bw,
        Action::CalPwr => return 0,
    };
    (cmd << 8) | 0x48
}

/// Upstream `rtw8822c_dpk_one_shot`: 1 if the engine did not finish.
pub(super) fn one_shot<H: Hci>(dev: &mut Dev<H>, path: usize, action: Action) -> u8 {
    let mut result = 0;
    set_gnt_wl(dev, true);
    if action == Action::CalPwr {
        let h = &mut dev.hci;
        h.write32_mask(REG_DPD_CTL0, bit(12), 0x1);
        h.write32_mask(REG_DPD_CTL0, bit(12), 0x0);
        h.write32_mask(REG_RXSRAM_CTL, BIT_RPT_SEL, 0x0);
        h.delay_us(10_000);
        if !check_hw_ready(h, REG_STAT_RPT, bit(31), 0x1) {
            result = 1;
        }
    } else {
        let cmd = u32::from(get_cmd(dev.dm.dpk.dpk_bw, action, path));
        let h = &mut dev.hci;
        h.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
        h.write32_mask(REG_R_CONFIG, BIT_IQ_SWITCH, 0x9);
        h.write32(REG_NCTL0, cmd);
        h.write32(REG_NCTL0, cmd + 1);
        h.delay_us(10_000);
        if !check_hw_ready(h, 0x2d9c, 0xff, 0x55) {
            result = 1;
        }
        h.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
        h.write32_mask(REG_R_CONFIG, BIT_IQ_SWITCH, 0x0);
    }
    set_gnt_wl(dev, false);
    dev.hci.write8(0x1b10, 0x0);
    result
}

/// Upstream `rtw8822c_dpk_information`: band, channel, width from RF 0x18.
fn information<H: Hci>(dev: &mut Dev<H>) {
    let reg = read_rf(dev, RF_PATH_A, 0x18, RFREG_MASK);
    let d = &mut dev.dm.dpk;
    d.dpk_band = 1 << ((reg >> 16) & 1);
    d.dpk_ch = (reg & 0xff) as u8;
    d.dpk_bw = ((reg & 0x3000) >> 12) as u8;
}

/// Upstream `rtw8822c_dpk_tx_pause`: stop every queue, wait (up to 5 ms)
/// for both paths to leave TX mode.
fn tx_pause<H: Hci>(dev: &mut Dev<H>) {
    dev.hci.write8(0x522, 0xff);
    dev.hci.write32_mask(0x1e70, 0xf, 0x2);
    for _ in 0..2500 {
        let a = read_rf(dev, RF_PATH_A, 0x00, 0xf0000);
        let b = read_rf(dev, RF_PATH_B, 0x00, 0xf0000);
        dev.hci.delay_us(2);
        if a != 2 && b != 2 {
            break;
        }
    }
}

/// Upstream `rtw8822c_dpk_pre_setting`.
fn pre_setting<H: Hci>(dev: &mut Dev<H>) {
    for path in 0..usize::from(dev.hal.rf_path_num).min(2) {
        write_rf(dev, path, RF_RXAGC_OFFSET, RFREG_MASK, 0x0);
        let two_g = dev.dm.dpk.dpk_band == DPK_BAND_2G;
        let h = &mut dev.hci;
        h.write32(REG_NCTL0, subpage(path));
        h.write32(
            REG_DPD_CTL1_S1,
            if two_g { 0x1f10_0000 } else { 0x1f0d_0000 },
        );
        h.write32_mask(REG_DPD_LUT0, BIT_GLOSS_DB, 0x4);
        h.write32_mask(REG_IQK_CTL1, BIT_TX_CFIR, 0x3);
    }
    let h = &mut dev.hci;
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
    h.write32(REG_DPD_CTL11, 0x3b23_170b);
    h.write32(REG_DPD_CTL12, 0x775f_5347);
}

/// Upstream `rtw8822c_dpk_result_reset`.
fn result_reset<H: Hci>(dev: &mut Dev<H>) {
    for path in 0..usize::from(dev.hal.rf_path_num).min(2) {
        dev.dm.dpk.path_ok &= !(1 << path);
        dev.hci.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
        dev.hci.write32_mask(0x1b58, 0x7f, 0x0);
        let t = thermal_read(dev, path);
        let d = &mut dev.dm.dpk;
        d.dpk_txagc[path] = 0;
        d.result[path] = 0;
        d.dpk_gs[path] = 0x5b;
        d.pre_pwsf[path] = 0;
        d.thermal_dpk[path] = t;
    }
}

/// The 20 report selectors of upstream `rtw8822c_dpk_get_coef_tbl`.
const GET_COEF_TBL: [u32; 20] = [
    0x000400f0, 0x040400f0, 0x080400f0, 0x010400f0, 0x050400f0, 0x090400f0, 0x020400f0, 0x060400f0,
    0x0a0400f0, 0x030400f0, 0x070400f0, 0x0b0400f0, 0x0c0400f0, 0x100400f0, 0x0d0400f0, 0x110400f0,
    0x0e0400f0, 0x120400f0, 0x0f0400f0, 0x130400f0,
];

/// Upstream `rtw8822c_dpk_get_coef` with `_coef_tbl_apply` and
/// `_coef_transfer`: read the 20 coefficients, Q negated.
fn get_coef<H: Hci>(dev: &mut Dev<H>, path: usize) {
    let h = &mut dev.hci;
    h.write32(REG_NCTL0, 0x0000_000c);
    if path == RF_PATH_A {
        h.write32_mask(REG_DPD_CTL0, bit(24), 0x0);
        h.write32(REG_DPD_CTL0_S0, 0x3000_0080);
    } else {
        h.write32_mask(REG_DPD_CTL0, bit(24), 0x1);
        h.write32(REG_DPD_CTL0_S1, 0x3000_0080);
    }
    for (i, &sel) in GET_COEF_TBL.iter().enumerate() {
        h.write32(REG_RXSRAM_CTL, sel);
        let _ = h.read32(REG_STAT_RPT);
        let ci = h.read32_mask(REG_STAT_RPT, MASKHWORD) & 0x1fff;
        let cq = h.read32_mask(REG_STAT_RPT, MASKLWORD) & 0x1fff;
        let cq = ((0x2000 - cq) & 0x1fff).wrapping_sub(1) & 0xffff;
        dev.dm.dpk.coef[path][i] = (ci << 16) | cq;
    }
}

/// Upstream `rtw8822c_dpk_coef_read`: 0 if any coefficient saturated.
fn coef_read(coef: &[u32; 20]) -> u8 {
    let bad = |c: u32| c == 0x1000 || c == 0x0fff;
    u8::from(
        !coef
            .iter()
            .any(|&c| bad((c & 0x1fff_0000) >> 16) || bad(c & 0x1fff)),
    )
}

/// Upstream `rtw8822c_dpk_coef_write`: the measured coefficients, or the
/// identity (with entry 3 marked) when the path failed.
pub(super) fn coef_write<H: Hci>(dev: &mut Dev<H>, path: usize, ok: bool) {
    let reg = [0x1b0c, 0x1b64][path];
    dev.hci.write32(REG_NCTL0, 0x0000_000c);
    dev.hci.write32(REG_RXSRAM_CTL, 0x0000_00f0);
    for addr in 0..20 {
        let coef = if ok {
            dev.dm.dpk.coef[path][addr]
        } else if addr == 3 {
            0x0400_1fff
        } else {
            0x0000_1fff
        };
        dev.hci.write32(reg + addr as u32 * 4, coef);
    }
}

/// Upstream `rtw8822c_dpk_fill_result`.
fn fill_result<H: Hci>(dev: &mut Dev<H>, dpk_txagc: u32, path: usize, ok: bool) {
    dev.hci.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
    dev.hci.write8(
        REG_DPD_AGC,
        if ok {
            dpk_txagc.wrapping_sub(6) as u8
        } else {
            0
        },
    );
    dev.dm.dpk.result[path] = u8::from(ok);
    dev.dm.dpk.dpk_txagc[path] = dev.hci.read8(REG_DPD_AGC);
    coef_write(dev, path, ok);
}

/// Upstream `rtw8822c_dpk_by_path`: run DPK, read the coefficients.
fn by_path<H: Hci>(dev: &mut Dev<H>, path: usize) -> u8 {
    let mut result = one_shot(dev, path, Action::DoDpk);
    dev.hci.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
    result |= dev.hci.read32_mask(REG_DPD_CTL1_S0, bit(26)) as u8;
    write_rf(dev, path, RF_MODE_TRXAGC, RFREG_MASK, 0x33e14);
    get_coef(dev, path);
    result
}

/// Upstream `rtw8822c_dpk_calibrate` with `_check_pass`.
fn calibrate<H: Hci>(dev: &mut Dev<H>, path: usize) {
    let txagc = gainloss(dev, path);
    let fail = by_path(dev, path) != 0;
    let ok = !fail && coef_read(&dev.dm.dpk.coef[path]) != 0;
    fill_result(dev, txagc, path, ok);
    if dev.dm.dpk.result[path] != 0 {
        dev.dm.dpk.path_ok |= 1 << path;
    }
}

/// Upstream `rtw8822c_dpk_enable_disable`.
fn enable_disable<H: Hci>(dev: &mut Dev<H>) {
    let d = dev.dm.dpk.clone();
    let h = &mut dev.hci;
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
    h.write32_mask(REG_DPD_CTL1_S0, BIT_DPD_EN, u32::from(d.is_dpk_pwr_on));
    h.write32_mask(REG_DPD_CTL1_S1, BIT_DPD_EN, u32::from(d.is_dpk_pwr_on));
    for (path, ctl1, ctl0) in [
        (0, REG_DPD_CTL1_S0, REG_DPD_CTL0_S0),
        (1, REG_DPD_CTL1_S1, REG_DPD_CTL0_S1),
    ] {
        if d.path_ok & (1 << path) != 0 {
            h.write32_mask(ctl1, bit(15) | bit(14), 0x0);
            h.write8(ctl0, d.dpk_gs[path] as u8);
        }
    }
}

/// Upstream `rtw8822c_do_dpk`: back up 18 BB registers and 7 RF registers
/// per path, pause TX, calibrate both paths, restore.
pub fn do_dpk<H: Hci>(dev: &mut Dev<H>) {
    if !dev.dm.dpk.is_dpk_pwr_on {
        return; // upstream: "Skip DPK due to DPD PWR off"
    }
    if super::dpk_agc::reload(dev) {
        return;
    }
    information(dev);
    let bb = Backup::save(
        &mut dev.hci,
        [
            0x520, 0x820, 0x824, 0x1c3c, 0x1d58, 0x1864, 0x4164, 0x180c, 0x410c, 0x186c, 0x416c,
            0x1a14, 0x1e70, 0x80c, 0x1d70, 0x1e7c, 0x18a4, 0x41a4,
        ],
    );
    const RF_REGS: [u32; 7] = [0x0, 0x1a, 0x55, 0x63, 0x87, 0x8f, 0xde];
    let mut rf_bak = [[0u32; 2]; 7];
    for (i, &r) in RF_REGS.iter().enumerate() {
        rf_bak[i] = [
            read_rf(dev, RF_PATH_A, r, RFREG_MASK),
            read_rf(dev, RF_PATH_B, r, RFREG_MASK),
        ];
    }

    tx_pause(dev);
    load_dpk_table(&mut dev.hci, DPK_MAC_BB);
    load_dpk_table(&mut dev.hci, DPK_AFE_IS_DPK);
    pre_setting(dev);
    result_reset(dev);
    // rtw8822c_dpk_path_select
    calibrate(dev, RF_PATH_A);
    calibrate(dev, RF_PATH_B);
    super::dpk_agc::dpk_on(dev, RF_PATH_A);
    super::dpk_agc::dpk_on(dev, RF_PATH_B);
    super::dpk_agc::cal_coef1(dev);
    load_dpk_table(&mut dev.hci, DPK_AFE_NO_DPK);
    enable_disable(dev);

    for (i, &r) in RF_REGS.iter().enumerate() {
        write_rf(dev, RF_PATH_A, r, RFREG_MASK, rf_bak[i][0]);
        write_rf(dev, RF_PATH_B, r, RFREG_MASK, rf_bak[i][1]);
    }
    for path in 0..usize::from(dev.hal.rf_path_num).min(2) {
        rxbb_dc_cal(dev, path);
    }
    bb.restore(&mut dev.hci);
    dev.hci.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
    dev.hci.write32_mask(REG_RXSRAM_CTL, BIT_DPD_CLK, 0x4);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_match_upstream() {
        assert_eq!(get_cmd(0, Action::GainLoss, 0), 0x1448);
        assert_eq!(get_cmd(0, Action::DoDpk, 1), 0x1748);
        assert_eq!(get_cmd(DPK_BW_80, Action::DoDpk, 1), 0x1948);
        assert_eq!(get_cmd(0, Action::DpkOn, 1), 0x1b48);
        assert_eq!(get_cmd(DPK_BW_80, Action::Dagc, 0), 0x1e48);
        assert_eq!(get_cmd(0, Action::CalPwr, 0), 0);
    }

    #[test]
    fn saturated_coefficient_fails_the_path() {
        let mut c = [0x0123_0456u32; 20];
        assert_eq!(coef_read(&c), 1);
        c[7] = 0x1000_0001;
        assert_eq!(coef_read(&c), 0);
        c[7] = 0x0001_0fff;
        assert_eq!(coef_read(&c), 0);
    }
}
