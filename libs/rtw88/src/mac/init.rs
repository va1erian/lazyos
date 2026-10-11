//! MAC configuration after the firmware download (upstream `mac.c`:
//! `rtw_mac_init` with `txdma_queue_mapping`, `priority_queue_cfg`,
//! `init_h2c`, `rtw_drv_info_cfg`; `rtw_set_trx_fifo_info`;
//! `rtw_mac_postinit`; `rtw_set_channel_mac`; `rtw_mac_flush_queues`).

use super::regs::*;
use super::{check_hw_ready, FifoConf};
use crate::bringup::chip_calls;
use crate::dev::{Bw, Dev};
use crate::error::{Error, Result};
use crate::hci::{Hci, RegExt};
use crate::rtw8822c::spec::{
    CSI_BUF_PG_NUM, DMA_MAPPING_MAX, PAGE_TABLE_PCIE, PRIOQ_ADDRS_8822C, RQPN_PCIE,
    RSVD_DRV_PG_NUM, RXFF_SIZE, TXFF_SIZE, TX_PAGE_SIZE, TX_PAGE_SIZE_SHIFT,
};

/// Upstream `txdma_queue_mapping`, PCIe row: map every queue to its DMA
/// channel, then enable the MAC's TX/RX paths.
fn txdma_queue_mapping<H: Hci>(dev: &mut Dev<H>) {
    let rqpn = RQPN_PCIE;
    dev.hal.fifo.rqpn = Some(rqpn);
    let map = txdma_hiq_map(rqpn.dma_map_hi as u16)
        | txdma_mgq_map(rqpn.dma_map_mg as u16)
        | txdma_bkq_map(rqpn.dma_map_bk as u16)
        | txdma_beq_map(rqpn.dma_map_be as u16)
        | txdma_viq_map(rqpn.dma_map_vi as u16)
        | txdma_voq_map(rqpn.dma_map_vo as u16);
    dev.hci.write16(TXDMA_PQ_MAP, map);

    dev.hci.write8(CR, 0);
    dev.hci.write8(CR, MAC_TRX_ENABLE);
    dev.hci.write32(H2CQ_CSR, H2CQ_FULL);
}

/// Upstream `rtw_set_trx_fifo_info` (3081): carve the TX FIFO's top into
/// the firmware's reserved areas, the driver's reserved pages below them.
pub fn set_trx_fifo_info(fifo: &mut FifoConf) -> Result<()> {
    let txff_pg_num = TXFF_SIZE / TX_PAGE_SIZE;
    let rsvd_pg_num = RSVD_DRV_PG_NUM
        + RSVD_PG_H2C_EXTRAINFO_NUM
        + RSVD_PG_H2C_STATICINFO_NUM
        + RSVD_PG_H2CQ_NUM
        + RSVD_PG_CPU_INSTRUCTION_NUM
        + RSVD_PG_FW_TXBUF_NUM
        + CSI_BUF_PG_NUM;
    if rsvd_pg_num > txff_pg_num {
        return Err(Error::NoMemory);
    }
    // Every value below fits 16 bits: TXFF_SIZE / 128 is 2048.
    fifo.rsvd_drv_pg_num = RSVD_DRV_PG_NUM as u16;
    fifo.txff_pg_num = txff_pg_num as u16;
    fifo.rsvd_pg_num = rsvd_pg_num as u16;
    fifo.acq_pg_num = (txff_pg_num - rsvd_pg_num) as u16;
    fifo.rsvd_boundary = fifo.acq_pg_num;

    let mut cur = txff_pg_num as u16;
    cur -= CSI_BUF_PG_NUM as u16;
    fifo.rsvd_csibuf_addr = cur;
    cur -= RSVD_PG_FW_TXBUF_NUM as u16;
    fifo.rsvd_fw_txbuf_addr = cur;
    cur -= RSVD_PG_CPU_INSTRUCTION_NUM as u16;
    fifo.rsvd_cpu_instr_addr = cur;
    cur -= RSVD_PG_H2CQ_NUM as u16;
    fifo.rsvd_h2cq_addr = cur;
    cur -= RSVD_PG_H2C_STATICINFO_NUM as u16;
    fifo.rsvd_h2c_sta_info_addr = cur;
    cur -= RSVD_PG_H2C_EXTRAINFO_NUM as u16;
    fifo.rsvd_h2c_info_addr = cur;
    cur -= fifo.rsvd_drv_pg_num;
    fifo.rsvd_drv_addr = cur;

    if fifo.rsvd_boundary != fifo.rsvd_drv_addr {
        return Err(Error::Invalid("wrong rsvd driver address"));
    }
    Ok(())
}

/// Upstream `priority_queue_cfg` + `__priority_queue_cfg` (3081, PCIe
/// page table): page counts per queue, boundaries, then the automatic
/// link-list table init.
fn priority_queue_cfg<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    set_trx_fifo_info(&mut dev.hal.fifo)?;
    let pg = PAGE_TABLE_PCIE;
    let fifo = &dev.hal.fifo;
    let pubq_num = fifo
        .acq_pg_num
        .checked_sub(pg.hq_num + pg.lq_num + pg.nq_num + pg.exq_num + pg.gapq_num)
        .ok_or(Error::Invalid("page table larger than the FIFO"))?;
    let boundary = fifo.rsvd_boundary;
    let hci = &mut dev.hci;

    hci.write16(FIFOPAGE_INFO[0], pg.hq_num);
    hci.write16(FIFOPAGE_INFO[1], pg.lq_num);
    hci.write16(FIFOPAGE_INFO[2], pg.nq_num);
    hci.write16(FIFOPAGE_INFO[3], pg.exq_num);
    hci.write16(FIFOPAGE_INFO[4], pubq_num);
    hci.set32(RQPN_CTRL_2, LD_RQPN);

    hci.write16(FIFOPAGE_CTRL_2, boundary);
    hci.set8(FWHW_TXQ_CTRL + 2, (EN_WR_FREE_TAIL >> 16) as u8);

    hci.write16(BCNQ_BDNY_V1, boundary);
    hci.write16(FIFOPAGE_CTRL_2 + 2, boundary);
    hci.write16(BCNQ1_BDNY_V1, boundary);
    hci.write32(RXFF_BNDY, RXFF_SIZE - C2H_PKT_BUF - 1);

    hci.set8(AUTO_LLT_V1, AUTO_INIT_LLT_V1);
    check_hw_ready(
        hci,
        "auto llt init",
        AUTO_LLT_V1,
        u32::from(AUTO_INIT_LLT_V1),
        0,
    )?;

    hci.write8(CR + 3, 0);
    Ok(())
}

/// Upstream `init_h2c`: place the H2C queue in its reserved pages and check
/// the hardware sees it empty.
fn init_h2c<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    let h2cq_addr = u32::from(dev.hal.fifo.rsvd_h2cq_addr) << TX_PAGE_SIZE_SHIFT;
    let h2cq_size = RSVD_PG_H2CQ_NUM << TX_PAGE_SIZE_SHIFT;
    let hci = &mut dev.hci;

    let v = (hci.read32(H2C_HEAD) & 0xFFFC_0000) | h2cq_addr;
    hci.write32(H2C_HEAD, v);
    let v = (hci.read32(H2C_READ_ADDR) & 0xFFFC_0000) | h2cq_addr;
    hci.write32(H2C_READ_ADDR, v);
    let v = (hci.read32(H2C_TAIL) & 0xFFFC_0000) | (h2cq_addr + h2cq_size);
    hci.write32(H2C_TAIL, v);

    let v = (hci.read8(H2C_INFO) & 0xFC) | 0x01;
    hci.write8(H2C_INFO, v);
    let v = (hci.read8(H2C_INFO) & 0xFB) | 0x04;
    hci.write8(H2C_INFO, v);
    let v = (hci.read8(TXDMA_OFFSET_CHK + 1) & 0x7f) | 0x80;
    hci.write8(TXDMA_OFFSET_CHK + 1, v);

    let wp = hci.read32(H2C_PKT_WRITEADDR) & 0x3FFFF;
    let rp = hci.read32(H2C_PKT_READADDR) & 0x3FFFF;
    let free = if wp >= rp {
        h2cq_size.wrapping_sub(wp - rp)
    } else {
        rp - wp
    };
    if free != h2cq_size {
        return Err(Error::Invalid("H2C queue mismatch"));
    }
    Ok(())
}

/// Upstream `rtw_drv_info_cfg` (3081): PHY status appended to every RX
/// packet, and the "rxdesc len = 0" workaround.
fn drv_info_cfg<H: Hci>(hci: &mut H) {
    hci.write8(RX_DRVINFO_SZ, PHY_STATUS_SIZE);
    let v = (hci.read8(TRXFF_BNDY + 1) & 0xF0) | 0xF;
    hci.write8(TRXFF_BNDY + 1, v);
    hci.set32(RCR, RCR_APP_PHYSTS);
    hci.clr32(WMAC_OPTION_FUNCTION + 4, (1 << 8) | (1 << 9));
}

/// Upstream `rtw_mac_init`: TRX config, the chip's MAC init, driver info,
/// then the bus's own interface config.
pub fn mac_init<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    txdma_queue_mapping(dev);
    priority_queue_cfg(dev)?;
    init_h2c(dev)?;
    // contract: rtw88-chip `rtw8822c::mac_init(dev) -> Result<()>`.
    chip_calls::mac_init(dev)?;
    drv_info_cfg(&mut dev.hci);
    dev.hci.interface_cfg();
    Ok(())
}

/// Upstream `rtw_mac_postinit`: the 8822C has no `mac_postinit` op.
pub fn mac_postinit<H: Hci>(_dev: &mut Dev<H>) -> Result<()> {
    Ok(())
}

/// Upstream `rtw_set_channel_mac` (3081 path), called by the chip's
/// `set_channel`: sub-channel, RF mode for the width, MAC clock and the
/// CCK check that stops CCK on 5 GHz.
pub fn set_channel_mac<H: Hci>(hci: &mut H, channel: u8, bw: Bw, primary_ch_idx: u8) {
    const SC_20_UPPER: u8 = 1;
    const SC_20_UPMOST: u8 = 3;
    const SC_40_UPPER: u8 = 9;
    const SC_40_LOWER: u8 = 10;

    let txsc20 = primary_ch_idx;
    let mut txsc40 = 0u8;
    if bw == Bw::Mhz80 {
        txsc40 = if txsc20 == SC_20_UPPER || txsc20 == SC_20_UPMOST {
            SC_40_UPPER
        } else {
            SC_40_LOWER
        };
    }
    hci.write8(DATA_SC, (txsc20 & 0xf) | ((txsc40 & 0xf) << 4));

    let mut v = hci.read32(WMAC_TRXPTCL_CTL) & !RFMOD;
    match bw {
        Bw::Mhz80 => v |= RFMOD_80M,
        Bw::Mhz40 => v |= RFMOD_40M,
        Bw::Mhz20 => {}
    }
    hci.write32(WMAC_TRXPTCL_CTL, v);

    let v = (hci.read32(AFE_CTRL1) & !MAC_CLK_SEL) | (MAC_CLK_HW_DEF_80M << SHIFT_MAC_CLK_SEL);
    hci.write32(AFE_CTRL1, v);
    hci.write8(USTIME_TSF, MAC_CLK_SPEED);
    hci.write8(USTIME_EDCA, MAC_CLK_SPEED);

    let mut v = hci.read8(CCK_CHECK) & !CHECK_CCK_EN;
    if is_ch_5g_band(channel) {
        v |= CHECK_CCK_EN;
    }
    hci.write8(CCK_CHECK, v);
}

/// Upstream `IS_CH_5G_BAND`.
pub fn is_ch_5g_band(ch: u8) -> bool {
    matches!(ch, 36..=48 | 52..=64 | 100..=144 | 149..=177)
}

/// Upstream `rtw_mac_flush_all_queues` (every priority queue): wait up to
/// 100 ms per DMA channel for its reserved pages to be free again. A queue
/// that does not drain is left as it is, like upstream.
pub fn flush_queues<H: Hci>(dev: &mut Dev<H>, _drop: bool) {
    for addr in PRIOQ_ADDRS_8822C.iter().take(DMA_MAPPING_MAX) {
        for _ in 0..5 {
            let rsvd = dev.hci.read16(addr.rsvd);
            let avail = dev.hci.read16(addr.avail);
            if rsvd == avail {
                break;
            }
            dev.hci.delay_us(20_000);
        }
    }
}
