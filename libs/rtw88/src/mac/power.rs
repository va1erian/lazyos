//! MAC power (upstream `mac.c`: `rtw_pwr_seq_parser`, `rtw_mac_power_on`,
//! `rtw_mac_power_off` and the steps around them).

use super::regs::*;
use crate::dev::Dev;
use crate::error::{Error, Result};
use crate::hci::{Hci, RegExt};
use crate::pwrseq::{intf, PwrBase, PwrCmd, PwrSeqCmd};
use crate::rtw8822c::spec::{
    CARD_DISABLE_FLOW_8822C, CARD_ENABLE_FLOW_8822C, RPWM_ADDR, SYS_FUNC_EN as CHIP_SYS_FUNC_EN,
};

/// Upstream `cut_version_to_mask`.
fn cut_version_to_mask(cut: u8) -> u8 {
    // A cut past G has no mask bit; upstream would shift into bit 8 and
    // match nothing, which `checked_shl` reproduces as 0.
    1u32.checked_shl(u32::from(cut) + 1).map_or(0, |m| m as u8)
}

/// Upstream `rtw_mac_pre_system_cfg`, the 3081/PCIe path: pin mux, BB and
/// RF held in reset before the power sequence runs.
fn pre_system_cfg<H: Hci>(hci: &mut H) {
    hci.write8(RSV_CTRL, 0);
    hci.set32(HCI_OPT_CTRL, USB_SUS_DIS);

    // config PIN Mux
    hci.set32(PAD_CTRL1, PAPE_WLBT_SEL | LNAON_WLBT_SEL);
    hci.clr32(LED_CFG, PAPE_SEL_EN | LNAON_SEL_EN);
    hci.set32(GPIO_MUXCFG, WLRFE_4_5_EN);

    // disable BB/RF
    hci.clr8(SYS_FUNC_EN, FEN_BB_RSTB | FEN_BB_GLB_RST);
    hci.clr8(RF_CTRL, RF_SDM_RSTB | RF_RSTB | RF_EN);
    hci.clr32(WLRF1, WLRF1_BBRF_EN);
}

/// Upstream `do_pwr_poll_cmd`: 50 us steps, `RTW_PWR_POLLING_CNT` of them.
fn do_pwr_poll<H: Hci>(hci: &mut H, addr: u32, mask: u8, target: u8) -> Result<()> {
    hci.poll8(
        "pwr seq poll",
        addr,
        mask,
        target & mask,
        50,
        50 * PWR_POLLING_CNT,
    )
}

/// Upstream `rtw_pwr_cmd_polling`: on PCIe a failed poll is retried once
/// after toggling `BIT_PFM_WOWL`.
fn pwr_cmd_polling<H: Hci>(hci: &mut H, cmd: &PwrSeqCmd) -> Result<()> {
    let offset = u32::from(cmd.offset);
    if do_pwr_poll(hci, offset, cmd.mask, cmd.value).is_ok() {
        return Ok(());
    }
    let value = hci.read8(SYS_PW_CTRL);
    hci.write8(SYS_PW_CTRL, value | PFM_WOWL);
    hci.write8(SYS_PW_CTRL, value & !PFM_WOWL);
    do_pwr_poll(hci, offset, cmd.mask, cmd.value)
}

/// Upstream `rtw_sub_pwr_seq_parser`: run one transition.
fn sub_pwr_seq_parser<H: Hci>(
    hci: &mut H,
    intf_mask: u8,
    cut_mask: u8,
    cmds: &[PwrSeqCmd],
) -> Result<()> {
    for cmd in cmds {
        if cmd.cmd == PwrCmd::End {
            break;
        }
        if cmd.intf_mask & intf_mask == 0 || cmd.cut_mask & cut_mask == 0 {
            continue;
        }
        if cmd.base != PwrBase::Mac {
            // SDIO-local and USB addressing only appear in steps masked to
            // those buses; reaching one on PCIe means a broken table.
            return Err(Error::Invalid("power sequence step for another bus"));
        }
        match cmd.cmd {
            PwrCmd::Write => {
                let offset = u32::from(cmd.offset);
                let v = hci.read8(offset);
                hci.write8(offset, (v & !cmd.mask) | (cmd.value & cmd.mask));
            }
            PwrCmd::Polling => pwr_cmd_polling(hci, cmd)?,
            PwrCmd::Delay => {
                // RTW_PWR_DELAY_US is 0; anything else is milliseconds.
                let us = if cmd.value == 0 {
                    u32::from(cmd.offset)
                } else {
                    u32::from(cmd.offset).saturating_mul(1000)
                };
                hci.delay_us(us);
            }
            PwrCmd::End => break,
        }
    }
    Ok(())
}

/// Upstream `rtw_pwr_seq_parser`: run every transition of `seq` for this
/// chip's cut on PCIe.
pub fn pwr_seq_parser<H: Hci>(dev: &mut Dev<H>, seq: &[&[PwrSeqCmd]]) -> Result<()> {
    let cut_mask = cut_version_to_mask(dev.hal.cut_version);
    for cmds in seq {
        sub_pwr_seq_parser(&mut dev.hci, intf::PCI, cut_mask, cmds)?;
    }
    Ok(())
}

/// What a power switch did (upstream returns `-EALREADY` for `Already`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Switch {
    Done,
    Already,
}

/// Upstream `rtw_mac_power_switch` for the 3081 CPU on PCIe.
fn power_switch<H: Hci>(dev: &mut Dev<H>, pwr_on: bool) -> Result<Switch> {
    let rpwm = dev.hci.read8(RPWM_ADDR);
    // Firmware still alive: toggle RPWM so it lets go of the power state.
    if dev.hci.read16(MCUFW_CTRL) == 0xC078 {
        dev.hci
            .write8(RPWM_ADDR, (rpwm ^ RPWM_TOGGLE) & RPWM_TOGGLE);
    }

    let cur_pwr = dev.hci.read8(CR) != 0xea;
    if pwr_on == cur_pwr {
        return Ok(Switch::Already);
    }
    if !pwr_on {
        dev.hal.flags.power_on = false;
    }
    let seq: &[&[PwrSeqCmd]] = if pwr_on {
        &CARD_ENABLE_FLOW_8822C
    } else {
        &CARD_DISABLE_FLOW_8822C
    };
    pwr_seq_parser(dev, seq)?;
    if pwr_on {
        dev.hal.flags.power_on = true;
    }
    Ok(Switch::Done)
}

/// Upstream `__rtw_mac_init_system_cfg` (the 3081 path): DDMA on, the
/// chip's function enables, and boot-from-flash off so the driver's
/// firmware download is the one that runs.
fn init_system_cfg<H: Hci>(hci: &mut H) {
    hci.set32(CPU_DMEM_CON, WL_PLATFORM_RST | DDMA_EN);
    hci.set8(SYS_FUNC_EN + 1, CHIP_SYS_FUNC_EN);
    let v = (hci.read8(CR_EXT + 3) & 0xF0) | 0x0C;
    hci.write8(CR_EXT + 3, v);

    let tmp = hci.read32(MCUFW_CTRL);
    if tmp & BOOT_FSPI_EN != 0 {
        hci.write32(MCUFW_CTRL, tmp & !BOOT_FSPI_EN);
        let v = hci.read32(GPIO_MUXCFG) & !FSPI_EN;
        hci.write32(GPIO_MUXCFG, v);
    }
}

/// Upstream `rtw_mac_power_on`. A chip found already powered (a previous
/// driver left it on) is powered off and on again.
pub fn power_on<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    pre_system_cfg(&mut dev.hci);
    if power_switch(dev, true)? == Switch::Already {
        // Upstream ignores the power-off result here too.
        let _ = power_switch(dev, false);
        pre_system_cfg(&mut dev.hci);
        if power_switch(dev, true)? == Switch::Already {
            return Err(Error::Bus("mac power on: chip still reports powered"));
        }
    }
    init_system_cfg(&mut dev.hci);
    Ok(())
}

/// Upstream `rtw_mac_power_off`.
pub fn power_off<H: Hci>(dev: &mut Dev<H>) {
    // Upstream discards the result: power-off is best effort.
    let _ = power_switch(dev, false);
}
