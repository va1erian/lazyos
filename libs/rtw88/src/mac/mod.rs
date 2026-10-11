//! The MAC (upstream `mac.c`, plus the helpers it uses from `util.c`): the
//! power sequence engine (`rtw_pwr_seq_parser`), `rtw_mac_power_on`/`off`,
//! `rtw_mac_init` (TRX configuration, queue pages, DMA mapping, the H2C
//! queue), `rtw_mac_postinit`, `rtw_set_channel_mac`, and the firmware
//! download primitives `mac.c` holds (`rtw_download_firmware`,
//! `rtw_ddma_to_fw_fifo`).
//!
//! Owner: the core/MAC work package.
//!
//! Only the 8822C path is kept: `RTW_WCPU_3081` (never the 8051 "legacy"
//! branches) on PCIe (never the SDIO/USB branches).

pub mod regs;

mod fwdl;
mod init;
mod power;

#[cfg(test)]
mod tests;

pub use fwdl::{ddma_to_fw_fifo, download_firmware};
pub use init::{
    flush_queues, is_ch_5g_band, mac_init, mac_postinit, set_channel_mac, set_trx_fifo_info,
};
pub use power::{power_off, power_on, pwr_seq_parser};

use crate::error::{Error, Result};
use crate::hci::{Hci, RegExt};
use crate::rtw8822c::spec::Rqpn;

/// The TX FIFO layout (upstream `struct rtw_fifo_conf`), computed by
/// [`set_trx_fifo_info`] and read by the firmware code (reserved pages, the
/// H2C queue, the general-info H2C).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FifoConf {
    pub rsvd_drv_pg_num: u16,
    pub txff_pg_num: u16,
    pub acq_pg_num: u16,
    pub rsvd_boundary: u16,
    pub rsvd_drv_addr: u16,
    pub rsvd_h2c_info_addr: u16,
    pub rsvd_h2c_sta_info_addr: u16,
    pub rsvd_h2cq_addr: u16,
    pub rsvd_cpu_instr_addr: u16,
    pub rsvd_fw_txbuf_addr: u16,
    pub rsvd_csibuf_addr: u16,
    pub rsvd_pg_num: u16,
    /// The queue to DMA channel mapping in force (`fifo->rqpn`), `None`
    /// until [`mac_init`] ran.
    pub rqpn: Option<Rqpn>,
}

/// The device flags this driver tracks (upstream `RTW_FLAG_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    /// The MAC is powered (`RTW_FLAG_POWERON`).
    pub power_on: bool,
    /// The firmware runs (`RTW_FLAG_FW_RUNNING`).
    pub fw_running: bool,
    /// `core_start` completed (`RTW_FLAG_RUNNING`).
    pub running: bool,
    /// A scan is in progress (`RTW_FLAG_SCANNING`).
    pub scanning: bool,
}

/// Upstream `check_hw_ready`: poll the field under `mask` (shifted down, as
/// `rtw_read32_mask` returns it) until it equals `target`, 1000 times 10 us.
pub fn check_hw_ready<H: Hci + ?Sized>(
    hci: &mut H,
    step: &'static str,
    addr: u32,
    mask: u32,
    target: u32,
) -> Result<()> {
    for _ in 0..1000 {
        if hci.read32_mask(addr, mask) == target {
            return Ok(());
        }
        hci.delay_us(10);
    }
    Err(Error::Timeout { step, addr })
}

/// One saved register (upstream `struct rtw_backup_info`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backup {
    pub len: u8,
    pub reg: u32,
    pub val: u32,
}

/// Upstream `rtw_restore_reg`: write each saved value back at its width.
pub fn restore_reg<H: Hci + ?Sized>(hci: &mut H, bckp: &[Backup]) {
    for b in bckp {
        match b.len {
            1 => hci.write8(b.reg, b.val as u8),
            2 => hci.write16(b.reg, b.val as u16),
            4 => hci.write32(b.reg, b.val),
            _ => {}
        }
    }
}

/// Upstream `ltecoex_read_reg` over the 8822C's indirect window.
pub fn ltecoex_read_reg<H: Hci + ?Sized>(hci: &mut H, offset: u16) -> Result<u32> {
    use crate::rtw8822c::spec::{LTECOEX_CTRL, LTECOEX_RDATA};
    check_hw_ready(hci, "ltecoex read", LTECOEX_CTRL, regs::LTECOEX_READY, 1)?;
    hci.write32(LTECOEX_CTRL, 0x800F_0000 | u32::from(offset));
    Ok(hci.read32(LTECOEX_RDATA))
}

/// Upstream `ltecoex_reg_write` over the 8822C's indirect window.
pub fn ltecoex_reg_write<H: Hci + ?Sized>(hci: &mut H, offset: u16, value: u32) -> Result<()> {
    use crate::rtw8822c::spec::{LTECOEX_CTRL, LTECOEX_WDATA};
    check_hw_ready(hci, "ltecoex write", LTECOEX_CTRL, regs::LTECOEX_READY, 1)?;
    hci.write32(LTECOEX_WDATA, value);
    hci.write32(LTECOEX_CTRL, 0xC00F_0000 | u32::from(offset));
    Ok(())
}
