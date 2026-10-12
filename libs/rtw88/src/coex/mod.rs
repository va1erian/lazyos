//! Bluetooth coexistence, the minimum (upstream `coex.c`).
//!
//! Owner: the chip work package. The RTL8822CE is a Wi-Fi + Bluetooth
//! combo whose two radios share the antenna through a PTA (packet traffic
//! arbiter) and two grant signals, GNT_WL and GNT_BT. LazyOS has no
//! Bluetooth stack and runs no coexistence mechanism (no TDMA tables, no BT
//! profiles), so this is only what the chip needs to give the antenna and
//! the RF switch to Wi-Fi: `rtw_coex_power_on_setting`,
//! `rtw_coex_init_hw_config`, `rtw_coex_power_off_setting`, the antenna
//! path phases they use (`rtw_coex_set_ant_path`), the scoreboard, the PTA
//! table, and the `rtw8822c_coex_cfg_*` hooks ([`chip`]).
//!
//! Contract used by [`crate::bringup`]: [`power_on_setting`]`(dev)` then
//! [`init_hw_config`]`(dev, wifi_only)` after the PHY is up, and
//! [`power_off_setting`]`(dev)` on the way down.
//!
//! **Call `init_hw_config` with `wifi_only = true`.** Upstream passes
//! `!efuse.btcoex`, which is `false` on a combo card, and relies on its
//! coexistence mechanism to move the grants to the PTA once Bluetooth
//! reports in; with Bluetooth alive and no mechanism, the `false` path
//! leaves GNT_WL forced low and Wi-Fi deaf. With `true`, the end state is:
//!
//! | Where | Value | Meaning |
//! |---|---|---|
//! | LTE coex indirect 0x38 bits 15:14, 11:10 | 01, 01 | GNT_BT forced low (software) |
//! | LTE coex indirect 0x38 bits 13:12, 9:8 | 11, 11 | GNT_WL forced high (software) |
//! | LTE coex indirect 0x38 bit 7 | 0 | LTE coexistence off |
//! | LTE coex indirect 0xa0, 0xa4 low halves | 0xffff | WL/BT TRX not masked by LTE |
//! | 0x73 bit 2 (`BIT_LTE_MUX_CTRL_PATH`) | 1 | the antenna path is controlled by Wi-Fi |
//! | 0xaa (`REG_WIFI_BT_INFO`) | 0x8003 | scoreboard: Wi-Fi active and on, BT interrupt enabled |
//! | 0x6c0/0x6c4/0x6c8 | 0x55555555 x2, 0xf0ffffff | PTA table case 1 (shared antenna only) |
//! | 0x6cc bits 3, 4; 0x6cf bit 3 | 1 | ACK/BA/CTS, beacons, beacon queue at high priority |
//! | 0x40 bits 5, 9 | 1 | PTA enabled, BT PTA pins on |
//! | RF path B 0x01 | 0x40000 | the RF parameter upstream sets "to avoid RF parameter error" |
//!
//! The 8822C has no external antenna switch (`ant_switch_exist` is false
//! for every RFE type), so no RFE pin is driven here.

pub mod chip;
mod indirect;

#[cfg(test)]
mod tests;

pub use indirect::{read_indirect_reg, write_indirect_reg};

use crate::dev::Dev;
use crate::fw;
use crate::hci::Hci;
use crate::rtw8822c::regs::*;
use crate::rtw8822c::util::{set8, write8_mask};

/// Upstream `COEX_SCBD_*` scoreboard bits.
pub const SCBD_ACTIVE: u16 = 1 << 0;
pub const SCBD_ONOFF: u16 = 1 << 1;
pub const SCBD_BT_RFK: u16 = 1 << 5;
pub const SCBD_TDMA: u16 = 1 << 9;
pub const SCBD_ALL: u16 = 0xffff;

/// Upstream `COEX_GNT_SET_*`.
pub const GNT_SET_HW_PTA: u32 = 0x0;
pub const GNT_SET_SW_LOW: u32 = 0x1;
pub const GNT_SET_SW_HIGH: u32 = 0x3;

/// Antenna phases (upstream `COEX_SET_ANT_*`, the ones init uses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AntPhase {
    Init,
    WlanOnly,
    WlanOff,
    PowerOn,
}

/// What the coex code keeps (the parts of upstream `struct rtw_coex`,
/// `rtw_coex_stat`, `rtw_coex_dm` and `rtw_coex_rfe` it uses).
#[derive(Clone, Debug, Default)]
pub struct CoexState {
    pub stop_dm: bool,
    pub wl_rf_off: bool,
    pub freeze: bool,
    pub bt_disabled: bool,
    pub bt_iqk_state: u8,
    pub score_board: u16,
    pub kt_ver: u8,
    pub wl_slot_extend: bool,
    pub tdma_timer_base: u8,
    pub cur_table: u8,
    pub cur_ps_tdma: u8,
    pub cur_ps_tdma_on: bool,
    pub ps_tdma_para: [u8; 5],
    pub cur_ant_pos: Option<AntPhase>,
    pub wl_force_lps_ctrl: bool,
    pub wl_slot_toggle: bool,
    pub wl_slot_toggle_change: bool,
    pub cur_wl_rx_low_gain_en: bool,
    /// `coex_rfe.rfe_module_type` and `wlg_at_btg`.
    pub rfe_module_type: u8,
    pub wlg_at_btg: bool,
    /// Indirect-window reads or writes that failed (the LTE coex block was
    /// not ready); upstream only logs them.
    pub indirect_errors: u32,
    /// What [`check_wifi_grant`] found after the last `init_hw_config`.
    pub grant: Option<GrantCheck>,
}

/// Whether Wi-Fi holds the shared antenna after a Wi-Fi-only init: the
/// grant bits read back from the LTE coex window and the path owner. Not in
/// upstream (it trusts its writes); a write that silently failed here would
/// leave the radio deaf with nothing in the log, so `wifid` prints this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantCheck {
    /// LTE coex indirect 0x38 as read back (`None`: the read failed).
    pub lte_ctrl: Option<u32>,
    /// 0x70 bit 26 (`BIT_LTE_MUX_CTRL_PATH`): the path is Wi-Fi's.
    pub path_is_wifi: bool,
    pub indirect_errors: u32,
}

impl GrantCheck {
    /// GNT_BT forced low and GNT_WL forced high (bits 15:8 = 0x77), the
    /// path owned by Wi-Fi, and no failed indirect access on the way.
    pub fn ok(&self) -> bool {
        self.lte_ctrl.is_some_and(|v| v & 0xff00 == 0x7700)
            && self.path_is_wifi
            && self.indirect_errors == 0
    }
}

/// Read the grants back after `init_hw_config(dev, true)`.
pub fn check_wifi_grant<H: Hci>(dev: &mut Dev<H>) -> GrantCheck {
    let lte_ctrl =
        crate::mac::ltecoex_read_reg(&mut dev.hci, crate::rtw8822c::regs::LTE_COEX_CTRL).ok();
    let path_is_wifi = dev.hci.read32(crate::rtw8822c::regs::REG_SYS_SDIO_CTRL)
        & crate::rtw8822c::regs::BIT_LTE_MUX_CTRL_PATH
        != 0;
    let check = GrantCheck {
        lte_ctrl,
        path_is_wifi,
        indirect_errors: dev.coex.indirect_errors,
    };
    dev.coex.grant = Some(check);
    check
}

/// Upstream `rtw_coex_read_scbd`.
fn read_scbd<H: Hci>(dev: &mut Dev<H>) -> u16 {
    dev.hci.read16(REG_WIFI_BT_INFO) & !(BIT_BT_INT_EN as u16)
}

/// Upstream `rtw_coex_write_scbd` (the 8822C has `new_scbd10_def`, so bit
/// 10 is an ordinary bit).
pub fn write_scbd<H: Hci>(dev: &mut Dev<H>, bitpos: u16, set: bool) {
    let mut val = 0x2 | dev.coex.score_board;
    if set {
        val |= bitpos;
    } else {
        val &= !bitpos;
    }
    if val != dev.coex.score_board {
        dev.coex.score_board = val;
        dev.hci
            .write16(REG_WIFI_BT_INFO, val | BIT_BT_INT_EN as u16);
    }
}

/// Upstream `rtw_coex_monitor_bt_enable` (scoreboard chips): Bluetooth is
/// off unless it set its on/off scoreboard bit.
fn monitor_bt_enable<H: Hci>(dev: &mut Dev<H>) {
    let bt_disabled = read_scbd(dev) & SCBD_ONOFF == 0;
    dev.coex.bt_disabled = bt_disabled;
}

/// Upstream `rtw_coex_coex_ctrl_owner`: who drives the antenna path mux
/// (the 8822C has no `btg_reg`).
fn ctrl_owner<H: Hci>(dev: &mut Dev<H>, wifi: bool) {
    let bits = BIT_LTE_MUX_CTRL_PATH >> 24;
    if wifi {
        set8(&mut dev.hci, REG_SYS_SDIO_CTRL + 3, bits);
    } else {
        crate::rtw8822c::util::clr8(&mut dev.hci, REG_SYS_SDIO_CTRL + 3, bits);
    }
}

/// Upstream `rtw_coex_set_gnt_bt`.
pub fn set_gnt_bt<H: Hci>(dev: &mut Dev<H>, state: u32) {
    write_indirect_reg(dev, LTE_COEX_CTRL, 0xc000, state);
    write_indirect_reg(dev, LTE_COEX_CTRL, 0x0c00, state);
}

/// Upstream `rtw_coex_set_gnt_wl`.
pub fn set_gnt_wl<H: Hci>(dev: &mut Dev<H>, state: u32) {
    write_indirect_reg(dev, LTE_COEX_CTRL, 0x3000, state);
    write_indirect_reg(dev, LTE_COEX_CTRL, 0x0300, state);
}

/// Upstream `rtw_coex_check_rfk`: on a shared antenna, wait (up to 600 ms)
/// for a running BT or Wi-Fi RF calibration before moving the antenna.
fn check_rfk<H: Hci>(dev: &mut Dev<H>) {
    if !dev.coex.wlg_at_btg || dev.coex.bt_iqk_state == 0xff {
        return;
    }
    for _ in 0..600 / 10 {
        let btk = read_scbd(dev) & SCBD_BT_RFK != 0;
        let wlk = dev.hci.read8(REG_ARFR4) & BIT_WL_RFK as u8 != 0;
        if !btk && !wlk {
            return;
        }
        dev.hci.delay_us(10_000);
    }
    dev.coex.bt_iqk_state = 0xff;
}

/// Upstream `rtw_coex_set_ant_path` for the init phases.
pub fn set_ant_path<H: Hci>(dev: &mut Dev<H>, force: bool, phase: AntPhase) {
    if !force && dev.coex.cur_ant_pos == Some(phase) {
        return;
    }
    dev.coex.cur_ant_pos = Some(phase);
    check_rfk(dev);
    match phase {
        AntPhase::PowerOn => {
            let bt_off = dev.coex.bt_disabled;
            ctrl_owner(dev, bt_off);
        }
        AntPhase::Init => {
            if dev.coex.bt_disabled {
                set_gnt_bt(dev, GNT_SET_SW_LOW);
                set_gnt_wl(dev, GNT_SET_SW_HIGH);
            } else {
                set_gnt_bt(dev, GNT_SET_SW_HIGH);
                set_gnt_wl(dev, GNT_SET_SW_LOW);
            }
            ctrl_owner(dev, true);
        }
        AntPhase::WlanOnly => {
            set_gnt_bt(dev, GNT_SET_SW_LOW);
            set_gnt_wl(dev, GNT_SET_SW_HIGH);
            ctrl_owner(dev, true);
        }
        AntPhase::WlanOff => ctrl_owner(dev, false),
    }
    // No antenna switch on the 8822C (`ant_switch_exist` is false).
}

/// Upstream `rtw_coex_table`: the PTA table for `case`. A non-shared
/// antenna numbers its cases from 100; upstream's `type - 100` on a `u8`
/// wraps for the small cases init uses, so nothing is written then.
pub fn table<H: Hci>(dev: &mut Dev<H>, case: u8) {
    dev.coex.cur_table = case;
    let entry = if dev.efuse.share_ant {
        chip::TABLE_SANT.get(usize::from(case))
    } else {
        chip::TABLE_NSANT.get(usize::from(case.wrapping_sub(100)))
    };
    if let Some(&[bt, wl]) = entry {
        dev.hci.write32(REG_BT_COEX_TABLE0, bt);
        dev.hci.write32(REG_BT_COEX_TABLE1, wl);
        dev.hci.write32(REG_BT_COEX_BRK_TABLE, 0xf0ff_ffff);
    }
}

/// Upstream `rtw_coex_tdma(force, 0)` with `rtw_coex_set_tdma` for case 0
/// (TDMA off): the timer base is already 2-slot (so no H2C for it), the
/// scoreboard's TDMA bit is cleared (Wi-Fi is not busy), and with a shared
/// antenna the firmware is told "no TDMA" (H2C 0x60 with zeros). For a
/// non-shared antenna upstream's `0 - 100` on a `u8` is past the table, so
/// nothing is sent.
pub fn tdma_off<H: Hci>(dev: &mut Dev<H>) {
    write_scbd(dev, SCBD_TDMA, false);
    dev.coex.cur_ps_tdma_on = false;
    dev.coex.cur_ps_tdma = 0;
    if dev.efuse.share_ant {
        // COEX_PS_WIFI_NATIVE: no forced LPS (this driver has no LPS).
        dev.coex.wl_force_lps_ctrl = false;
        dev.coex.ps_tdma_para = [0; 5];
        // Upstream sends and forgets H2Cs here; so does this.
        let _ = fw::coex_tdma_type(dev, [0; 5]);
        dev.coex.wl_slot_toggle_change = dev.coex.wl_slot_toggle;
        dev.coex.wl_slot_toggle = false;
    }
}

/// Upstream `rtw_coex_power_on_setting`: BB on (so 0x948-area writes
/// land), the RFE type, the antenna path mux to whoever is alive, PTA table
/// case 1, then the debug pins off.
pub fn power_on_setting<H: Hci>(dev: &mut Dev<H>) {
    dev.coex.stop_dm = true;
    dev.coex.wl_rf_off = false;
    set8(
        &mut dev.hci,
        REG_SYS_FUNC_EN,
        BIT_FEN_BB_GLB_RST | BIT_FEN_BB_RSTB,
    );
    monitor_bt_enable(dev);
    chip::cfg_rfe_type(dev);
    set_ant_path(dev, true, AntPhase::PowerOn);
    table(dev, 1);
    // "red x issue"
    dev.hci.write8(0xff1a, 0x0);
    chip::cfg_gnt_debug(dev);
}

/// Upstream `__rtw_coex_init_hw_config`. See the module docs for why
/// LazyOS passes `wifi_only = true`.
pub fn init_hw_config<H: Hci>(dev: &mut Dev<H>, wifi_only: bool) {
    // rtw_coex_init_coex_var: clear the statistics and mechanism state,
    // keep the top-level flags and the RFE facts.
    let c = &dev.coex;
    dev.coex = CoexState {
        stop_dm: c.stop_dm,
        wl_rf_off: c.wl_rf_off,
        freeze: c.freeze,
        rfe_module_type: c.rfe_module_type,
        wlg_at_btg: c.wlg_at_btg,
        ..CoexState::default()
    };
    dev.coex.kt_ver = dev.hci.read8(0xf1) >> 4;
    monitor_bt_enable(dev);
    // rtw_coex_wl_slot_extend(false): H2C 0x69 op 0xc, "disable 5 ms".
    dev.coex.wl_slot_extend = false;
    let _ = fw::bt_wifi_control(dev, 0xc, [0x1, 0, 0, 0, 0]);
    set8(&mut dev.hci, REG_BCN_CTRL, BIT_EN_BCN_FUNCTION);
    chip::cfg_rfe_type(dev);
    chip::cfg_init(dev);
    // TX response, beacon and beacon queue at high PTA priority
    // (rtw_coex_set_wl_pri_mask, COEX_WLPRI_TX_RSP/_BEACON/_BEACONQ).
    for bitmap in [3u32, 4, 27] {
        write8_mask(
            &mut dev.hci,
            REG_BT_COEX_TABLE_H + bitmap / 8,
            1 << (bitmap % 8),
            1,
        );
    }
    if dev.coex.wl_rf_off {
        set_ant_path(dev, true, AntPhase::WlanOff);
        write_scbd(dev, SCBD_ALL, false);
        dev.coex.stop_dm = true;
    } else if wifi_only {
        set_ant_path(dev, true, AntPhase::WlanOnly);
        write_scbd(dev, SCBD_ACTIVE | SCBD_ONOFF, true);
        dev.coex.stop_dm = true;
    } else {
        set_ant_path(dev, true, AntPhase::Init);
        write_scbd(dev, SCBD_ACTIVE | SCBD_ONOFF, true);
        dev.coex.stop_dm = false;
        dev.coex.freeze = true;
    }
    table(dev, 1);
    tdma_off(dev);
    // rtw_coex_query_bt_info
    if !dev.coex.bt_disabled {
        let _ = fw::query_bt_info(dev);
    }
}

/// Upstream `rtw_coex_power_off_setting`: clear the scoreboard (Wi-Fi off)
/// but keep the BT interrupt.
pub fn power_off_setting<H: Hci>(dev: &mut Dev<H>) {
    dev.hci.write16(REG_WIFI_BT_INFO, BIT_BT_INT_EN as u16);
}
