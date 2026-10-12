//! Small helpers upstream keeps in `util.c` (`check_hw_ready`,
//! `rtw_restore_reg`) and `main.h` (`rtw_write8_set`, ...), shaped for the
//! chip code.

use crate::hci::{Hci, RegExt};

/// Upstream `check_hw_ready` (`crate::mac::check_hw_ready`) as the `bool`
/// the calibration code tests: it logs and carries on after a timeout, as
/// upstream does.
pub fn check_hw_ready<H: Hci>(hci: &mut H, addr: u32, mask: u32, target: u32) -> bool {
    crate::mac::check_hw_ready(hci, "rtw8822c", addr, mask, target).is_ok()
}

/// `rtw_write8_set`: OR `bits` (the low byte of a 32-bit field constant)
/// into a byte register.
pub fn set8<H: Hci>(hci: &mut H, addr: u32, bits: u32) {
    hci.set8(addr, bits as u8);
}

/// `rtw_write8_clr`.
pub fn clr8<H: Hci>(hci: &mut H, addr: u32, bits: u32) {
    hci.clr8(addr, bits as u8);
}

/// `rtw_write8_mask` with a 32-bit field constant shifted into the byte.
pub fn write8_mask<H: Hci>(hci: &mut H, addr: u32, mask: u32, data: u8) {
    hci.write8_mask(addr, mask as u8, data);
}

/// Registers saved before a calibration and written back after it
/// (upstream `struct rtw_backup_info` arrays with `len == 4`).
pub struct Backup<const N: usize> {
    regs: [u32; N],
    vals: [u32; N],
}

impl<const N: usize> Backup<N> {
    /// Read every register in `regs` (upstream's `*_backup_reg` loops).
    pub fn save<H: Hci>(hci: &mut H, regs: [u32; N]) -> Self {
        let mut vals = [0u32; N];
        for (v, &r) in vals.iter_mut().zip(regs.iter()) {
            *v = hci.read32(r);
        }
        Backup { regs, vals }
    }

    /// Upstream `rtw_restore_reg`: write them back in order.
    pub fn restore<H: Hci>(&self, hci: &mut H) {
        for (&r, &v) in self.regs.iter().zip(self.vals.iter()) {
            hci.write32(r, v);
        }
    }
}
