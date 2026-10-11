//! Saving and restoring DACK results (upstream `rtw8822c_dac_cal_backup`,
//! `_backup_path`, `_backup_vec`, `_backup_dck`, `rtw8822c_dac_cal_restore`,
//! `_restore_prepare`, `_restore_path`, `_restore_wait`, `_restore_dck`).
//!
//! The kept state is [`DackState`]: 15 MSB calibration words per path and
//! I/Q vector, the DCK nibbles, and the ADC compensation.

use crate::dev::{Dev, RF_PATH_A, RF_PATH_B};
use crate::hci::{Hci, RegExt};
use crate::rtw8822c::dack::{read_addr, write_addr};
use crate::rtw8822c::regs::*;
use crate::rtw8822c::util::check_hw_ready;

/// Upstream `DACK_MSBK_BACKUP_NUM`.
pub const MSBK_NUM: usize = 0xf;

/// DACK results (upstream `dm_info->dack_adck`, `dack_msbk`, `dack_dck`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DackState {
    pub adck: [u32; 2],
    /// `[path][vector: 0 = I, 1 = Q][index]`.
    pub msbk: [[[u16; MSBK_NUM]; 2]; 2],
    /// `[path][..][..]` in upstream's (asymmetric) indexing, see
    /// [`backup_dck`].
    pub dck: [[[u8; 2]; 2]; 2],
}

fn backup_vec<H: Hci>(dev: &mut Dev<H>, path: usize, vec: usize, w_addr: u32, r_addr: u32) {
    for i in 0..MSBK_NUM {
        dev.hci.write32_mask(w_addr, 0xf000_0000, i as u32);
        dev.dm.dack.msbk[path][vec][i] = dev.hci.read32_mask(r_addr, 0x7fc_0000) as u16;
    }
}

fn backup_path<H: Hci>(dev: &mut Dev<H>, path: usize) {
    let (w, r) = (write_addr(path) + 0xb0, read_addr(path) + 0x10);
    backup_vec(dev, path, 0, w, r);
    backup_vec(dev, path, 1, w + 0x1c, r + 0x2c);
}

/// Upstream `rtw8822c_dac_cal_backup_dck`. Path B stores its I_1 and Q_0
/// nibbles crosswise (`[1][0]` and `[0][1]`) while [`restore_dck`] reads
/// them straight; kept as upstream has it.
fn backup_dck<H: Hci>(dev: &mut Dev<H>) {
    let h = &mut dev.hci;
    let d = &mut dev.dm.dack.dck;
    d[RF_PATH_A][0][0] = h.read32_mask(REG_DCKA_I_0, 0xf000_0000) as u8;
    d[RF_PATH_A][0][1] = h.read32_mask(REG_DCKA_I_1, 0xf) as u8;
    d[RF_PATH_A][1][0] = h.read32_mask(REG_DCKA_Q_0, 0xf000_0000) as u8;
    d[RF_PATH_A][1][1] = h.read32_mask(REG_DCKA_Q_1, 0xf) as u8;
    d[RF_PATH_B][0][0] = h.read32_mask(REG_DCKB_I_0, 0xf000_0000) as u8;
    d[RF_PATH_B][1][0] = h.read32_mask(REG_DCKB_I_1, 0xf) as u8;
    d[RF_PATH_B][0][1] = h.read32_mask(REG_DCKB_Q_0, 0xf000_0000) as u8;
    d[RF_PATH_B][1][1] = h.read32_mask(REG_DCKB_Q_1, 0xf) as u8;
}

/// Upstream `rtw8822c_dac_cal_backup`.
pub fn dac_cal_backup<H: Hci>(dev: &mut Dev<H>) {
    let h = &mut dev.hci;
    let temp = [h.read32(0x1860), h.read32(0x4160), h.read32(0x9b4)];
    h.write32(0x9b4, 0xdb66_db00);
    h.clr32(0x1830, bit(30));
    h.write32_mask(0x1860, 0xfc00_0000, 0x3c);
    backup_path(dev, RF_PATH_A);
    dev.hci.clr32(0x4130, bit(30));
    dev.hci.write32_mask(0x4160, 0xfc00_0000, 0x3c);
    backup_path(dev, RF_PATH_B);
    backup_dck(dev);
    let h = &mut dev.hci;
    h.set32(0x1830, bit(30));
    h.set32(0x4130, bit(30));
    h.write32(0x1860, temp[0]);
    h.write32(0x4160, temp[1]);
    h.write32(0x9b4, temp[2]);
}

/// Upstream `rtw8822c_dac_cal_restore_dck`.
fn restore_dck<H: Hci>(dev: &mut Dev<H>) {
    let d = dev.dm.dack.dck;
    let h = &mut dev.hci;
    let regs = [
        (RF_PATH_A, REG_DCKA_I_0, REG_DCKA_I_1, 0),
        (RF_PATH_A, REG_DCKA_Q_0, REG_DCKA_Q_1, 1),
        (RF_PATH_B, REG_DCKB_I_0, REG_DCKB_I_1, 0),
        (RF_PATH_B, REG_DCKB_Q_0, REG_DCKB_Q_1, 1),
    ];
    for (path, r0, r1, v) in regs {
        h.set32(r0, bit(19));
        h.write32_mask(r0, 0xf000_0000, u32::from(d[path][v][0]));
        h.write32_mask(r1, 0xf, u32::from(d[path][v][1]));
    }
}

/// Upstream `rtw8822c_dac_cal_restore_prepare`.
fn restore_prepare<H: Hci>(dev: &mut Dev<H>) {
    let h = &mut dev.hci;
    h.write32(0x9b4, 0xdb66_db00);
    for r in [0x18b0, 0x18cc, 0x41b0, 0x41cc] {
        h.write32_mask(r, bit(27), 0x0);
    }
    for (anapar, ctl, a, b) in [(0x1830, 0x1860, 0x18b4, 0x18d0), (0x4130, 0x4160, 0x41b4, 0x41d0)] {
        h.write32_mask(anapar, bit(30), 0x0);
        h.write32_mask(ctl, 0xfc00_0000, 0x3c);
        h.write32_mask(a, bit(0), 0x1);
        h.write32_mask(b, bit(0), 0x1);
    }
    h.write32_mask(0x18b0, 0xf00, 0x0);
    h.write32_mask(0x18c0, bit(14), 0x0);
    h.write32_mask(0x18cc, 0xf00, 0x0);
    h.write32_mask(0x18dc, bit(14), 0x0);
    h.write32_mask(0x18b0, bit(0), 0x0);
    h.write32_mask(0x18cc, bit(0), 0x0);
    h.write32_mask(0x18b0, bit(0), 0x1);
    h.write32_mask(0x18cc, bit(0), 0x1);
    restore_dck(dev);
    let h = &mut dev.hci;
    for r in [0x18c0, 0x18dc, 0x41c0, 0x41dc] {
        h.write32_mask(r, 0x38000, 0x7);
    }
    h.write32_mask(0x18b8, bit(26) | bit(25), 0x1);
    h.write32_mask(0x18d4, bit(26) | bit(25), 0x1);
    h.write32_mask(0x41b0, 0xf00, 0x0);
    h.write32_mask(0x41c0, bit(14), 0x0);
    h.write32_mask(0x41cc, 0xf00, 0x0);
    h.write32_mask(0x41dc, bit(14), 0x0);
    h.write32_mask(0x41b0, bit(0), 0x0);
    h.write32_mask(0x41cc, bit(0), 0x0);
    h.write32_mask(0x41b0, bit(0), 0x1);
    h.write32_mask(0x41cc, bit(0), 0x1);
    h.write32_mask(0x41b8, bit(26) | bit(25), 0x1);
    h.write32_mask(0x41d4, bit(26) | bit(25), 0x1);
}

/// Upstream `rtw8822c_dac_cal_restore_wait`: toggle until the target
/// reports state 6, at most 101 tries.
fn restore_wait<H: Hci>(h: &mut H, target: u32, toggle: u32) -> bool {
    for _ in 0..=100 {
        h.write32_mask(toggle, bit(26) | bit(25), 0x0);
        h.write32_mask(toggle, bit(26) | bit(25), 0x2);
        if h.read32_mask(target, 0xf) == 0x6 {
            return true;
        }
    }
    false
}

fn restore_vec<H: Hci>(dev: &mut Dev<H>, path: usize, vec: usize, w: u32) {
    for i in 0..MSBK_NUM {
        let v = u32::from(dev.dm.dack.msbk[path][vec][i]);
        let h = &mut dev.hci;
        h.write32_mask(w + 0x4, bit(2), 0x0);
        h.write32_mask(w + 0x4, 0xff8, v);
        h.write32_mask(w, 0xf000_0000, i as u32);
        h.write32_mask(w + 0x4, bit(2), 0x1);
    }
    dev.hci.write32_mask(w + 0x4, bit(2), 0x0);
}

/// Upstream `rtw8822c_dac_cal_restore_path`.
fn restore_path<H: Hci>(dev: &mut Dev<H>, path: usize) -> bool {
    let w_i = write_addr(path) + 0xb0;
    let r_i = read_addr(path) + 0x08;
    let (w_q, r_q) = (w_i + 0x1c, r_i + 0x2c);
    if !restore_wait(&mut dev.hci, r_i, w_i + 0x8) {
        return false;
    }
    restore_vec(dev, path, 0, w_i);
    if !restore_wait(&mut dev.hci, r_q, w_q + 0x8) {
        return false;
    }
    restore_vec(dev, path, 1, w_q);
    let h = &mut dev.hci;
    h.write32_mask(w_i + 0x8, bit(26) | bit(25), 0x0);
    h.write32_mask(w_q + 0x8, bit(26) | bit(25), 0x0);
    h.write32_mask(w_i + 0x4, bit(0), 0x0);
    h.write32_mask(w_q + 0x4, bit(0), 0x0);
    true
}

/// Upstream `rtw8822c_dac_cal_restore`: `false` (calibrate instead) when
/// nothing was saved or the hardware does not get ready.
pub fn dac_cal_restore<H: Hci>(dev: &mut Dev<H>) -> bool {
    let m = &dev.dm.dack.msbk;
    if m[RF_PATH_A][0][0] == 0 && m[RF_PATH_A][1][0] == 0 && m[RF_PATH_B][0][0] == 0 && m[RF_PATH_B][1][0] == 0 {
        return false;
    }
    let h = &mut dev.hci;
    let temp = [h.read32(0x1860), h.read32(0x4160), h.read32(0x9b4)];
    restore_prepare(dev);
    let h = &mut dev.hci;
    if !check_hw_ready(h, 0x2808, 0x7f_ff80, 0xffff)
        || !check_hw_ready(h, 0x2834, 0x7f_ff80, 0xffff)
        || !check_hw_ready(h, 0x4508, 0x7f_ff80, 0xffff)
        || !check_hw_ready(h, 0x4534, 0x7f_ff80, 0xffff)
    {
        return false;
    }
    if !restore_path(dev, RF_PATH_A) || !restore_path(dev, RF_PATH_B) {
        return false;
    }
    let h = &mut dev.hci;
    h.write32_mask(0x1830, bit(30), 0x1);
    h.write32_mask(0x4130, bit(30), 0x1);
    h.write32(0x1860, temp[0]);
    h.write32(0x4160, temp[1]);
    for r in [0x18b0, 0x18cc, 0x41b0, 0x41cc] {
        h.write32_mask(r, bit(27), 0x1);
    }
    h.write32(0x9b4, temp[2]);
    true
}
