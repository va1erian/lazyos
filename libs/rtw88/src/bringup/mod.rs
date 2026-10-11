//! The bring-up order (upstream `main.c`: `rtw_core_init` (the defaults),
//! `rtw_chip_info_setup` with `rtw_chip_parameter_setup`,
//! `rtw_chip_efuse_info_setup` and `rtw_chip_board_info_setup`;
//! `rtw_power_on`, `rtw_core_start`, `rtw_power_off`, `rtw_core_stop`).
//!
//! Owner: the core/MAC work package. This is the one place that knows the
//! order; every step it calls lives in its own module.
//!
//! The binary does, once:
//!
//! ```text
//! let mut dev = Dev::new(pci);
//! dev.chip_info_setup(&fw_image)?;   // chip id, efuse (MAC address), RF type
//! dev.hci.phy_cfg(dev.hal.cut_version, dev.efuse.rfe_option)?; // PCIe PHY, as upstream's probe
//! dev.core_start(&fw_image)?;        // power, firmware, MAC/BB/RF init, DMA on
//! dev.add_station(dev.mac_addr())?;  // then crate::station
//! ```
//!
//! and [`Dev::core_stop`] on the way out. `Pci::phy_cfg` (the bus package)
//! is the binary's call, between the two: upstream's `rtw_pci_probe` runs
//! `rtw_pci_phy_cfg` after `rtw_chip_info_setup` and before the first
//! `rtw_core_start`. The firmware image is passed in each time rather than
//! kept: the binary owns the file's bytes.

pub(crate) mod chip_calls;

#[cfg(test)]
mod tests;

use crate::dev::{Dev, BB_PATH_A, BB_PATH_AB, RF_1T1R, RF_2T2R};
use crate::efuse;
use crate::error::{Error, Result};
use crate::fw::{self, Firmware};
use crate::hci::Hci;
use crate::mac::{self, regs::*};
use crate::rtw8822c::spec::{LOG_EFUSE_SIZE, PHY_EFUSE_SIZE, PTCT_EFUSE_SIZE, RFE_DEFS_SIZE};

/// The address used when the efuse holds none (upstream draws a random
/// one; `no_std` has no RNG here). Locally administered, so it cannot
/// collide with a vendor's; the binary may pass its own to
/// [`Dev::add_station`] instead.
pub const FALLBACK_MAC: [u8; 6] = [0x02, 0x4c, 0x5a, 0x88, 0x22, 0xc0];

/// Upstream `rtw_core_init`, the register-free part: the default receive
/// filter and the first channel.
pub fn core_init<H: Hci>(dev: &mut Dev<H>) {
    dev.hal.rcr = RCR_APP_FCS
        | RCR_APP_MIC
        | RCR_APP_ICV
        | RCR_PKTCTL_DLEN
        | RCR_HTC_LOC_CTRL
        | RCR_APP_PHYSTS
        | RCR_AB
        | RCR_AM
        | RCR_APM;
    dev.hal.current_channel = 1;
}

/// Upstream `rtw_chip_parameter_setup` (PCIe): chip version, cut and RF
/// type from `REG_SYS_CFG1`, the efuse sizes.
pub fn chip_parameter_setup<H: Hci>(dev: &mut Dev<H>) {
    let hal = &mut dev.hal;
    hal.chip_version = dev.hci.read32(SYS_CFG1);
    hal.cut_version = ((hal.chip_version >> SHIFT_CHIP_VER) & MASK_CHIP_VER) as u8;
    hal.mp_chip = u8::from(hal.chip_version & RTL_ID == 0);
    if hal.chip_version & RF_TYPE_ID != 0 {
        hal.rf_type = RF_2T2R;
        hal.rf_path_num = 2;
        hal.antenna_tx = BB_PATH_AB;
        hal.antenna_rx = BB_PATH_AB;
    } else {
        hal.rf_type = RF_1T1R;
        hal.rf_path_num = 1;
        hal.antenna_tx = BB_PATH_A;
        hal.antenna_rx = BB_PATH_A;
    }
    // The 8822C has no fix_rf_phy_num.
    hal.rf_phy_num = hal.rf_path_num;

    dev.efuse.physical_size = PHY_EFUSE_SIZE;
    dev.efuse.logical_size = LOG_EFUSE_SIZE;
    dev.efuse.protect_size = PTCT_EFUSE_SIZE;
    // default use ack
    hal.rcr |= RCR_VHT_DACK;
}

/// Upstream `rtw_chip_efuse_enable`: power the MAC and run the firmware
/// once, asking it to leave the hardware feature report.
fn chip_efuse_enable<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>) -> Result<()> {
    dev.hci.setup()?;
    mac::power_on(dev)?;
    efuse::read::request_hw_feature(&mut dev.hci);
    if let Err(e) = mac::download_firmware(dev, fw) {
        mac::power_off(dev);
        return Err(e);
    }
    Ok(())
}

/// The part of `rtw_chip_efuse_info_setup` between enable and disable.
fn read_efuse_info<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    efuse::parse_efuse_map(dev)?;
    efuse::dump_hw_feature(dev)?;
    // rtw_check_supported_rfe: every 8822C RFE definition has its tables.
    if dev.efuse.rfe_option >= RFE_DEFS_SIZE {
        return Err(Error::Unsupported("rfe option"));
    }
    if !dev.efuse.apply_defaults() {
        dev.efuse.addr = FALLBACK_MAC;
    }
    Ok(())
}

/// Upstream `rtw_chip_efuse_info_setup`: power on, read the efuse and the
/// feature report, power off again (whatever happened in between).
pub fn chip_efuse_info_setup<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>) -> Result<()> {
    chip_efuse_enable(dev, fw)?;
    let ret = read_efuse_info(dev);
    // rtw_chip_efuse_disable
    dev.hci.stop();
    mac::power_off(dev);
    ret
}

/// Upstream `rtw_chip_info_setup`, after `rtw_core_init` and the firmware
/// load callback (`update_firmware_info`).
pub fn chip_info_setup<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>) -> Result<()> {
    core_init(dev);
    dev.fw.update(fw);
    chip_parameter_setup(dev);
    chip_efuse_info_setup(dev, fw)?;
    // contract: rtw88-chip `phy::setup_board_info(dev)` (`rtw_chip_board_info_setup`).
    chip_calls::board_info_setup(dev)
}

/// The steps of `rtw_power_on` after the MAC is powered; an error powers
/// it off again (`err_off`).
fn power_on_steps<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>) -> Result<()> {
    mac::download_firmware(dev, fw)?;
    // config mac after firmware downloaded
    mac::mac_init(dev)?;
    // contract: rtw88-chip `rtw8822c::phy_set_param(dev) -> Result<()>`.
    chip_calls::phy_set_param(dev)?;
    mac::mac_postinit(dev)?;
    dev.hci.start()?;
    // send H2C after HCI has started
    fw::send_general_info(dev)?;
    fw::send_phydm_info(dev)?;
    let wifi_only = !dev.efuse.btcoex;
    // contract: rtw88-chip `coex::power_on_setting(dev)`, `coex::init_hw_config(dev, wifi_only)`.
    chip_calls::coex_power_on_setting(dev);
    chip_calls::coex_init_hw_config(dev, wifi_only);
    Ok(())
}

/// Upstream `rtw_power_on`.
pub fn power_on<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>) -> Result<()> {
    dev.hci.setup()?;
    // power on MAC before firmware downloaded
    mac::power_on(dev)?;
    if let Err(e) = power_on_steps(dev, fw) {
        mac::power_off(dev);
        return Err(e);
    }
    Ok(())
}

/// Upstream `rtw_core_start` (`chip->ops->power_on` is `rtw_power_on`): the
/// security engine on and the receive filter written, since power-on reset
/// it. LPS deep mode is never selected: this driver does not use LPS.
pub fn core_start<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>) -> Result<()> {
    power_on(dev, fw)?;
    dev.sec.enable_sec_engine(&mut dev.hci);
    dev.hci.write32(RCR, dev.hal.rcr);
    dev.hal.flags.running = true;
    Ok(())
}

/// Upstream `rtw_power_off`.
pub fn power_off<H: Hci>(dev: &mut Dev<H>) {
    dev.hci.stop();
    // contract: rtw88-chip `coex::power_off_setting(dev)`.
    chip_calls::coex_power_off_setting(dev);
    mac::power_off(dev);
}

/// Upstream `rtw_core_stop`.
pub fn core_stop<H: Hci>(dev: &mut Dev<H>) {
    dev.hal.flags.running = false;
    dev.hal.flags.fw_running = false;
    power_off(dev);
}

impl<H: Hci> Dev<H> {
    /// Identify the chip and read its efuse (MAC address, RF options):
    /// upstream's probe-time `rtw_chip_info_setup`. Leaves the chip off.
    pub fn chip_info_setup(&mut self, fw_image: &[u8]) -> Result<()> {
        let fw = Firmware::parse(fw_image)?;
        chip_info_setup(self, &fw)
    }

    /// Power on and download the firmware without the `core_start`
    /// extras (upstream `rtw_power_on`).
    pub fn power_on(&mut self, fw_image: &[u8]) -> Result<()> {
        let fw = Firmware::parse(fw_image)?;
        power_on(self, &fw)
    }

    /// Bring the chip up for traffic (upstream `rtw_core_start`).
    pub fn core_start(&mut self, fw_image: &[u8]) -> Result<()> {
        let fw = Firmware::parse(fw_image)?;
        self.fw.update(&fw);
        core_start(self, &fw)
    }

    /// Upstream `rtw_power_off`.
    pub fn power_off(&mut self) {
        power_off(self);
    }

    /// Upstream `rtw_core_stop`.
    pub fn core_stop(&mut self) {
        core_stop(self);
    }
}
