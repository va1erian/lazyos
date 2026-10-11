//! The reserved pages (upstream `fw.c`: `rtw_fw_write_data_rsvd_page`,
//! `rtw_download_drv_rsvd_page`, `rtw_build_rsvd_page`,
//! `rtw_fw_download_rsvd_page`; `tx.c`: `rtw_tx_rsvd_page_pkt_info_update`,
//! `rtw_tx_write_data_rsvd_page_get`).
//!
//! The TX FIFO's top pages hold packets the firmware sends on its own
//! (PS-Poll, null data). They are written through the beacon queue with the
//! FIFO's beacon head pointed at the target page; the firmware download
//! uses the same path to stage each chunk at page 0.

use alloc::vec;
use alloc::vec::Vec;

use crate::dev::{Band, Dev};
use crate::error::{Error, Result};
use crate::fw::txdesc::{self, QSEL_BEACON, QSEL_MGMT};
use crate::hci::Hci;
use crate::mac::check_hw_ready;
use crate::mac::regs::*;
use crate::rtw8822c::spec::TX_PAGE_SIZE;
use crate::tx::{TxPktInfo, TX_PKT_DESC_SZ};

/// The reserved packet kinds (upstream `enum rtw_rsvd_packet_type`), the
/// ones a station without LPS uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RsvdType {
    Beacon,
    Dummy,
    PsPoll,
    Null,
    QosNull,
}

/// `DESC_RATE1M`, `DESC_RATE6M`, `RTW_RATEID_B_20M`, `RTW_RATEID_G`.
const DESC_RATE1M: u8 = 0x00;
const DESC_RATE6M: u8 = 0x04;
const RATEID_B_20M: u8 = 8;
const RATEID_G: u8 = 7;

/// Upstream `rtw_tx_pkt_info_update_rate` with `ignore_rate`: the band's
/// lowest rate, no fallback.
pub fn lowest_rate(info: &mut TxPktInfo, band: Band) {
    let (rate_id, rate) = match band {
        Band::Ghz2 => (RATEID_B_20M, DESC_RATE1M),
        Band::Ghz5 => (RATEID_G, DESC_RATE6M),
    };
    info.rate_id = rate_id;
    info.rate = rate;
    info.use_rate = true;
    info.dis_rate_fallback = true;
}

/// Is the frame's first address broadcast or multicast? Upstream reads
/// `addr1` of whatever is in the buffer (a firmware chunk too); a buffer too
/// short to have one is treated as unicast.
fn addr1_is_bmc(frame: &[u8]) -> bool {
    frame.get(4).is_some_and(|b| b & 1 != 0)
}

/// Upstream `rtw_tx_rsvd_page_pkt_info_update` for `frame` of `kind`.
pub fn rsvd_page_pkt_info(frame: &[u8], kind: RsvdType, band: Band) -> TxPktInfo {
    let mut info = TxPktInfo::default();
    // The first page (beacon or dummy) gets its queue select from the bus.
    if kind != RsvdType::Beacon && kind != RsvdType::Dummy {
        info.qsel = QSEL_MGMT;
    }
    lowest_rate(&mut info, band);
    info.bmc = addr1_is_bmc(frame);
    info.tx_pkt_size = frame.len() as u32;
    info.offset = TX_PKT_DESC_SZ as u8;
    info.ls = true;
    if kind == RsvdType::PsPoll {
        info.nav_use_hdr = true;
    } else {
        info.dis_qselseq = true;
        info.en_hwseq = true;
        info.hw_ssn_sel = 0;
    }
    if kind == RsvdType::QosNull {
        info.bt_null = true;
    }
    info
}

/// Descriptor + `frame` as one buffer.
fn with_desc(info: &TxPktInfo, frame: &[u8]) -> Vec<u8> {
    let mut pkt = vec![0u8; TX_PKT_DESC_SZ + frame.len()];
    txdesc::fill_tx_desc(info, &mut pkt[..TX_PKT_DESC_SZ]);
    pkt[TX_PKT_DESC_SZ..].copy_from_slice(frame);
    pkt
}

/// Upstream `rtw_fw_write_data_rsvd_page`: write `buf` into the TX FIFO at
/// page `pg_addr` through the beacon queue, then put the beacon head back
/// at the reserved boundary.
pub fn write_data_rsvd_page<H: Hci>(dev: &mut Dev<H>, pg_addr: u16, buf: &[u8]) -> Result<()> {
    if buf.is_empty() {
        return Err(Error::Invalid("empty reserved page"));
    }
    let hci = &mut dev.hci;
    let bcn_ctrl = hci.read8(BCN_CTRL);
    hci.write16(FIFOPAGE_CTRL_2, (pg_addr & MASK_BCN_HEAD_1_V1) | BCN_VALID_V1);

    let cr1 = hci.read8(CR + 1);
    hci.write8(CR + 1, cr1 | (ENSWBCN >> 8) as u8);
    hci.write8(BCN_CTRL, (bcn_ctrl & !EN_BCN_FUNCTION) | DIS_TSF_UDT);
    let txq2 = hci.read8(FWHW_TXQ_CTRL + 2);
    hci.write8(FWHW_TXQ_CTRL + 2, txq2 & !((EN_BCNQ_DL >> 16) as u8));

    // rtw_tx_write_data_rsvd_page_get: the whole buffer is one "beacon",
    // and the PCIe bus sets the beacon queue select.
    let mut info = rsvd_page_pkt_info(buf, RsvdType::Beacon, dev.hal.current_band_type);
    info.qsel = QSEL_BEACON;
    let pkt = with_desc(&info, buf);
    let mut ret = hci.write_data_rsvd_page(&pkt);
    if ret.is_ok() {
        ret = check_hw_ready(hci, "beacon valid", u32::from(FIFOPAGE_CTRL_2), u32::from(BCN_VALID_V1), 1);
    }

    let head = dev.hal.fifo.rsvd_boundary;
    hci.write16(FIFOPAGE_CTRL_2, head | BCN_VALID_V1);
    hci.write8(BCN_CTRL, bcn_ctrl);
    hci.write8(FWHW_TXQ_CTRL + 2, txq2);
    hci.write8(CR + 1, cr1);
    ret
}

/// Upstream `rtw_download_drv_rsvd_page`: `buf` must fit the driver's
/// reserved pages.
pub fn download_drv_rsvd_page<H: Hci>(dev: &mut Dev<H>, buf: &[u8]) -> Result<()> {
    let pg_num = buf.len().div_ceil(TX_PAGE_SIZE as usize);
    if pg_num > usize::from(dev.hal.fifo.rsvd_drv_pg_num) {
        return Err(Error::NoMemory);
    }
    let pg_addr = dev.hal.fifo.rsvd_drv_addr;
    write_data_rsvd_page(dev, pg_addr, buf)
}

/// Where each station reserved packet landed (page numbers relative to the
/// driver's reserved area), for [`crate::fw::cmds::send_rsvd_page_h2c`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RsvdLocations {
    pub ps_poll: u8,
    pub qos_null: u8,
    pub null: u8,
}

/// A PS-Poll (IEEE 802.11-2020 9.3.1.5): AID with the two top bits set,
/// BSSID, our address.
fn ps_poll(mac: &[u8; 6], bssid: &[u8; 6], aid: u16) -> Vec<u8> {
    let mut f = vec![0xA4, 0x00];
    f.extend_from_slice(&(aid | 0xC000).to_le_bytes());
    f.extend_from_slice(bssid);
    f.extend_from_slice(mac);
    f
}

/// A (QoS) Null data frame to the AP (ToDS): BSSID, us, BSSID. The QoS
/// form carries TID 0 in its QoS Control field.
fn null_frame(mac: &[u8; 6], bssid: &[u8; 6], qos: bool) -> Vec<u8> {
    let mut f = vec![if qos { 0xC8 } else { 0x48 }, 0x01, 0, 0];
    f.extend_from_slice(bssid);
    f.extend_from_slice(mac);
    f.extend_from_slice(bssid);
    f.extend_from_slice(&[0, 0]);
    if qos {
        f.extend_from_slice(&[0, 0]);
    }
    f
}

/// Upstream `rtw_build_rsvd_page` for a station (`rtw_add_rsvd_page_sta`
/// without the LPS-PG pages, see [`download_sta_rsvd_page`]): a dummy first
/// page, then PS-Poll, QoS Null and Null, each with its own descriptor and
/// starting on a page boundary.
pub fn build_sta_rsvd_page(mac: &[u8; 6], bssid: &[u8; 6], aid: u16, band: Band) -> (Vec<u8>, RsvdLocations) {
    let page = TX_PAGE_SIZE as usize;
    let margin = page - TX_PKT_DESC_SZ;
    let pkts = [
        (RsvdType::PsPoll, ps_poll(mac, bssid, aid)),
        (RsvdType::QosNull, null_frame(mac, bssid, true)),
        (RsvdType::Null, null_frame(mac, bssid, false)),
    ];
    // The dummy page is one byte without descriptor; the TX path adds the
    // descriptor that makes it a full first page.
    let mut total_page = (1 + TX_PKT_DESC_SZ).div_ceil(page);
    let mut built = Vec::new();
    let mut loc = RsvdLocations::default();
    for (kind, frame) in pkts {
        let pkt = with_desc(&rsvd_page_pkt_info(&frame, kind, band), &frame);
        let at = total_page as u8;
        match kind {
            RsvdType::PsPoll => loc.ps_poll = at,
            RsvdType::QosNull => loc.qos_null = at,
            _ => loc.null = at,
        }
        total_page += pkt.len().div_ceil(page);
        built.push((at as usize, pkt));
    }
    let mut buf = vec![0u8; (total_page - 1) * page + margin];
    // Page 0 holds the dummy byte (zero); page p >= 1 starts at margin.
    for (at, pkt) in built {
        let start = margin + page * (at - 1);
        buf[start..start + pkt.len()].copy_from_slice(&pkt);
    }
    (buf, loc)
}

/// Upstream `rtw_fw_download_rsvd_page` + `rtw_send_rsvd_page_h2c` for the
/// station: write the pages, rewrite the first ("beacon") page alone so
/// its descriptor is its own, and tell the firmware where they are.
///
/// Deviation: upstream also reserves the LPS-PG DPK and info pages; they
/// only matter in LPS deep power-gating mode, which this driver never
/// enters, so they are left out.
pub fn download_sta_rsvd_page<H: Hci>(dev: &mut Dev<H>, mac: &[u8; 6], bssid: &[u8; 6], aid: u16) -> Result<RsvdLocations> {
    let (buf, loc) = build_sta_rsvd_page(mac, bssid, aid, dev.hal.current_band_type);
    download_drv_rsvd_page(dev, &buf)?;
    download_drv_rsvd_page(dev, &[0u8])?;
    crate::fw::cmds::send_rsvd_page_h2c(dev, 0, loc.ps_poll, loc.null, loc.qos_null)?;
    Ok(loc)
}
