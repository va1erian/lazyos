//! Firmware download for the 3081 CPU (upstream `mac.c`:
//! `__rtw_download_firmware` and the helpers it calls,
//! `download_firmware_to_mem`, `rtw_ddma_to_fw_fifo`).
//!
//! Each section is cut into 4 KiB chunks. A chunk is staged in the TX FIFO
//! through the reserved-page path, then the chip's DDMA engine copies it
//! from the FIFO into the CPU's memory, accumulating a checksum the
//! section's trailing 8 bytes are checked against.

use super::regs::*;
use super::{check_hw_ready, ltecoex_read_reg, ltecoex_reg_write, restore_reg, Backup};
use crate::dev::Dev;
use crate::error::{Error, Result};
use crate::fw::header::{Firmware, FwSection};
use crate::fw::rsvd::write_data_rsvd_page;
use crate::hci::{Hci, RegExt};
use crate::rtw8822c::spec::{DmaMapping, TX_PKT_DESC_SZ};

/// Chunk size of the download (`max_size` in `download_firmware_to_mem`).
const DL_CHUNK: usize = 0x1000;

/// Upstream `wlan_cpu_enable`.
fn wlan_cpu_enable<H: Hci>(hci: &mut H, enable: bool) {
    if enable {
        hci.set8(RSV_CTRL + 1, WLMCU_IOIF);
        hci.set8(SYS_FUNC_EN + 1, FEN_CPUEN);
    } else {
        hci.clr8(SYS_FUNC_EN + 1, FEN_CPUEN);
        hci.clr8(RSV_CTRL + 1, WLMCU_IOIF);
    }
}

/// Upstream `download_firmware_reg_backup`: route the high queue to the
/// high DMA channel, give it 0x200 pages, stop beacons; return what to put
/// back.
fn reg_backup<H: Hci>(hci: &mut H) -> [Backup; 6] {
    let b0 = Backup { len: 1, reg: TXDMA_PQ_MAP + 1, val: u32::from(hci.read8(TXDMA_PQ_MAP + 1)) };
    hci.write8(TXDMA_PQ_MAP + 1, (DmaMapping::High as u8) << 6);

    let b1 = Backup { len: 1, reg: CR, val: u32::from(hci.read8(CR)) };
    let b2 = Backup { len: 4, reg: H2CQ_CSR, val: H2CQ_FULL };
    hci.write8(CR, HCI_TXDMA_EN | TXDMA_EN);
    hci.write32(H2CQ_CSR, H2CQ_FULL);

    let b3 = Backup { len: 2, reg: FIFOPAGE_INFO[0], val: u32::from(hci.read16(FIFOPAGE_INFO[0])) };
    let b4 = Backup { len: 4, reg: RQPN_CTRL_2, val: hci.read32(RQPN_CTRL_2) | LD_RQPN };
    hci.write16(FIFOPAGE_INFO[0], 0x200);
    hci.write32(RQPN_CTRL_2, b4.val);

    let tmp = hci.read8(BCN_CTRL);
    let b5 = Backup { len: 1, reg: BCN_CTRL, val: u32::from(tmp) };
    hci.write8(BCN_CTRL, (tmp & !EN_BCN_FUNCTION) | DIS_TSF_UDT);
    [b0, b1, b2, b3, b4, b5]
}

/// Upstream `download_firmware_reset_platform`.
fn reset_platform<H: Hci>(hci: &mut H) {
    hci.clr8(CPU_DMEM_CON + 2, (WL_PLATFORM_RST >> 16) as u8);
    hci.clr8(SYS_CLK_CTRL + 1, (CPU_CLK_EN >> 8) as u8);
    hci.set8(CPU_DMEM_CON + 2, (WL_PLATFORM_RST >> 16) as u8);
    hci.set8(SYS_CLK_CTRL + 1, (CPU_CLK_EN >> 8) as u8);
}

/// Upstream `iddma_enable`.
fn iddma_enable<H: Hci>(hci: &mut H, src: u32, dst: u32, ctrl: u32) -> Result<()> {
    hci.write32(DDMA_CH0SA, src);
    hci.write32(DDMA_CH0DA, dst);
    hci.write32(DDMA_CH0CTRL, ctrl);
    check_hw_ready(hci, "ddma done", DDMA_CH0CTRL, DDMACH0_OWN, 0)
}

/// Upstream `iddma_download_firmware`: one chunk, checksum continued after
/// the first.
fn iddma_download_firmware<H: Hci>(hci: &mut H, src: u32, dst: u32, len: u32, first: bool) -> Result<()> {
    check_hw_ready(hci, "ddma idle", DDMA_CH0CTRL, DDMACH0_OWN, 0)?;
    let mut ctrl = DDMACH0_CHKSUM_EN | DDMACH0_OWN | (len & MASK_DDMACH0_DLEN);
    if !first {
        ctrl |= DDMACH0_CHKSUM_CONT;
    }
    iddma_enable(hci, src, dst, ctrl)
}

/// Upstream `rtw_ddma_to_fw_fifo`: copy `size` bytes at `ocp_src` into the
/// firmware's RX FIFO (used by the firmware crash dump).
pub fn ddma_to_fw_fifo<H: Hci>(hci: &mut H, ocp_src: u32, size: u32) -> Result<()> {
    check_hw_ready(hci, "ddma idle", DDMA_CH0CTRL, DDMACH0_OWN, 0)?;
    let ctrl = DDMACH0_OWN | DDMACH0_DDMA_MODE | (size & MASK_DDMACH0_DLEN);
    iddma_enable(hci, ocp_src, OCPBASE_RXBUF_FW_88XX, ctrl)
}

/// Upstream `check_fw_checksum`: record the section's result in
/// `MCUFW_CTRL`; IMEM below the DMEM base, DMEM from it.
fn check_fw_checksum<H: Hci>(hci: &mut H, addr: u32) -> Result<()> {
    let mut fw_ctrl = hci.read8(MCUFW_CTRL);
    let imem = addr < OCPBASE_DMEM_88XX;
    let (dw_ok, chksum_ok) =
        if imem { (IMEM_DW_OK as u8, IMEM_CHKSUM_OK as u8) } else { (DMEM_DW_OK as u8, DMEM_CHKSUM_OK as u8) };
    if hci.read32(DDMA_CH0CTRL) & DDMACH0_CHKSUM_STS != 0 {
        fw_ctrl = (fw_ctrl | dw_ok) & !chksum_ok;
        hci.write8(MCUFW_CTRL, fw_ctrl);
        return Err(Error::FirmwareDownload("invalid fw checksum"));
    }
    hci.write8(MCUFW_CTRL, fw_ctrl | dw_ok | chksum_ok);
    Ok(())
}

/// Upstream `download_firmware_to_mem` with `src` 0 (every caller's).
fn download_to_mem<H: Hci>(dev: &mut Dev<H>, sec: &FwSection<'_>) -> Result<()> {
    dev.hci.set32(DDMA_CH0CTRL, DDMACH0_RESET_CHKSUM_STS);
    let mut first = true;
    for (i, chunk) in sec.data.chunks(DL_CHUNK).enumerate() {
        // Page 0 of the TX FIFO (src >> 7 with src 0).
        write_data_rsvd_page(dev, 0, chunk)?;
        let src = OCPBASE_TXBUF_88XX + TX_PKT_DESC_SZ as u32;
        let dst = sec.addr.wrapping_add((i * DL_CHUNK) as u32);
        iddma_download_firmware(&mut dev.hci, src, dst, chunk.len() as u32, first)?;
        first = false;
    }
    check_fw_checksum(&mut dev.hci, sec.addr)
}

/// Upstream `start_download_firmware`: enable download, then DMEM, IMEM
/// and EMEM in that order.
fn start_download<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>) -> Result<()> {
    let val = (dev.hci.read16(MCUFW_CTRL) & 0x3800) | MCUFWDL_EN as u16;
    dev.hci.write16(MCUFW_CTRL, val);
    download_to_mem(dev, &fw.dmem)?;
    download_to_mem(dev, &fw.imem)?;
    if let Some(emem) = &fw.emem {
        download_to_mem(dev, emem)?;
    }
    Ok(())
}

/// Upstream `download_firmware_end_flow`: mark the download ready when both
/// checksums passed.
fn end_flow<H: Hci>(hci: &mut H) {
    hci.write32(TXDMA_STATUS, PAGE_OVF);
    let fw_ctrl = hci.read16(MCUFW_CTRL);
    if u32::from(fw_ctrl) & CHECK_SUM_OK != CHECK_SUM_OK {
        return;
    }
    hci.write16(MCUFW_CTRL, (fw_ctrl | FW_DW_RDY as u16) & !(MCUFWDL_EN as u16));
}

/// Upstream `download_firmware_validate`: wait for the firmware to report
/// itself initialised.
fn validate<H: Hci>(hci: &mut H) -> Result<()> {
    if check_hw_ready(hci, "fw ready", MCUFW_CTRL, FW_READY_MASK, FW_READY).is_err() {
        let key = hci.read32(FW_DBG7) & FW_KEY_MASK;
        if key == ILLEGAL_KEY_GROUP {
            return Err(Error::FirmwareDownload("invalid fw key"));
        }
        return Err(Error::FirmwareDownload("firmware did not report ready"));
    }
    Ok(())
}

/// The steps of `__rtw_download_firmware` between stopping the CPU and
/// validation; any error goes to the caller's `dlfw_fail`.
fn download_body<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>, ltecoex_bckp: u32) -> Result<()> {
    wlan_cpu_enable(&mut dev.hci, false);
    let bckp = reg_backup(&mut dev.hci);
    reset_platform(&mut dev.hci);

    start_download(dev, fw)?;

    restore_reg(&mut dev.hci, &bckp);
    end_flow(&mut dev.hci);
    wlan_cpu_enable(&mut dev.hci, true);

    ltecoex_reg_write(&mut dev.hci, 0x38, ltecoex_bckp)?;
    validate(&mut dev.hci)
}

/// Upstream `rtw_download_firmware` (`__rtw_download_firmware`, 3081): stop
/// the CPU, load every section, restart it and wait for it to come up, then
/// reset the bus rings and the H2C counters.
pub fn download_firmware<H: Hci>(dev: &mut Dev<H>, fw: &Firmware<'_>) -> Result<()> {
    // The image size was checked by Firmware::parse (check_firmware_size).
    let ltecoex_bckp = ltecoex_read_reg(&mut dev.hci, 0x38)?;
    if let Err(e) = download_body(dev, fw, ltecoex_bckp) {
        // dlfw_fail: disable FWDL_EN, leave the CPU enabled.
        dev.hci.clr8(MCUFW_CTRL, MCUFWDL_EN as u8);
        dev.hci.set8(SYS_FUNC_EN + 1, FEN_CPUEN);
        return Err(e);
    }

    // reset desc and index
    dev.hci.setup()?;
    dev.fw.last_box_num = 0;
    dev.fw.seq = 0;
    dev.hal.flags.fw_running = true;
    Ok(())
}
