//! The initial gain (IGI) writes the scan path uses (upstream `phy.c`:
//! `rtw_phy_dig_write`, `rtw_phy_dig_set_max_coverage`,
//! `rtw_phy_dig_reset`, with the 8822C's `rtw8822c_dig[]` registers).
//!
//! The DIG mechanism itself (adapting the IGI to the false alarm rate) is
//! not run: between scans the IGI stays at what the BB table set, which is
//! where upstream starts. A lower IGI hears weaker stations.

use crate::dev::Dev;
use crate::hci::{Hci, RegExt};

/// Upstream `rtw8822c_dig[]`: one IGI field per path in 0x1d70 (the 8822C
/// has no separate CCK IGI).
const DIG: [(u32, u32); 2] = [(0x1d70, 0x7f), (0x1d70, 0x7f00)];
/// Upstream `DIG_CVRG_MIN`.
pub const DIG_CVRG_MIN: u8 = 0x1c;

/// Upstream `rtw_phy_dig_write`.
pub fn dig_write<H: Hci>(dev: &mut Dev<H>, igi: u8) {
    for &(addr, mask) in DIG.iter().take(usize::from(dev.hal.rf_path_num)) {
        dev.hci.write32_mask(addr, mask, u32::from(igi));
    }
}

/// Upstream `rtw_phy_dig_set_max_coverage`: the lowest IGI while scanning.
pub fn dig_set_max_coverage<H: Hci>(dev: &mut Dev<H>) {
    dig_write(dev, DIG_CVRG_MIN);
}

/// Upstream `rtw_phy_dig_reset`: back to the last IGI the DIG history holds
/// (here the one read after the tables loaded).
pub fn dig_reset<H: Hci>(dev: &mut Dev<H>) {
    let igi = dev.dm.igi_history0;
    dig_write(dev, igi);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeHci;

    #[test]
    fn coverage_and_reset_write_both_paths() {
        let mut d = Dev::new(FakeHci::default());
        d.hal.rf_path_num = 2;
        d.dm.igi_history0 = 0x20;
        d.hci.write32(0x1d70, 0xffff_0000);
        dig_set_max_coverage(&mut d);
        assert_eq!(d.hci.peek32(0x1d70), 0xffff_1c1c);
        dig_reset(&mut d);
        assert_eq!(d.hci.peek32(0x1d70), 0xffff_2020);
    }
}
