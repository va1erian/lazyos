//! The RX side of the rings (upstream `rtw_pci_get_hw_rx_ring_nr`,
//! `rtw_pci_rx_napi`, `rtw_pci_napi_poll`, `rtw_pci_dma_check`,
//! `rtw_pci_sync_rx_desc_device`).
//!
//! Each RX descriptor owns one 12 KiB slot for good. A buffer the hardware
//! hands back is copied out of DMA memory into a private scratch buffer
//! before anything in it is read, parsed by [`crate::rx::query_rx_desc`]
//! (which refuses any length that does not fit what was copied), handed to
//! the caller, and its descriptor re-armed with the driver's own address;
//! nothing the device writes is used as an index or an address.

use core::mem;
use core::sync::atomic::{fence, Ordering};

use crate::rtw8822c::spec::{RX_BUF_DESC_SZ, RX_PKT_DESC_SZ};
use crate::rx::{self, RxLayout, RxPktStat};

use super::regs::*;
use super::ring::{self, RX_BUF_SIZE, RX_SLOT};
use super::{DmaAlloc, Mmio, Pci};

/// The RX tag counts modulo this (upstream `RX_TAG_MAX`).
const RX_TAG_MAX: u16 = 8192;

/// One received buffer, valid for the callback only.
#[derive(Clone, Copy, Debug)]
pub struct RxPacket<'a> {
    /// RX descriptor, driver info (PHY status), shift and the frame.
    pub buf: &'a [u8],
    pub stat: &'a RxPktStat,
    pub layout: RxLayout,
}

impl<'a> RxPacket<'a> {
    /// The 802.11 frame (with its FCS: the chip keeps it, upstream sets
    /// `RX_INCLUDES_FCS`), or the C2H packet when `stat.is_c2h`.
    pub fn payload(&self) -> &'a [u8] {
        self.layout.payload(self.buf)
    }
}

impl<M: Mmio, D: DmaAlloc> Pci<M, D> {
    /// Point every RX descriptor at its slot.
    pub(super) fn arm_all_rx(&mut self) {
        for i in 0..self.rx.len {
            self.arm_rx(i);
        }
    }

    fn arm_rx(&mut self, idx: u32) {
        let off = self.layout.rx_bd + idx as usize * RX_BUF_DESC_SZ;
        let dma = (self.rxbuf.phys + u64::from(idx) * RX_SLOT as u64) as u32;
        ring::arm_rx_bd(&self.desc, off, dma);
    }

    /// Buffers the hardware has filled and the host not yet read
    /// (upstream `rtw_pci_get_hw_rx_ring_nr`).
    pub fn rx_pending(&mut self) -> u32 {
        let tmp = self.mmio.read32(RXBD_IDX_MPDUQ);
        let cur_wp = (tmp >> TRX_BD_HW_IDX_SHIFT) & TRX_BD_IDX_MASK;
        if cur_wp >= self.rx.len {
            self.stats.rx_bad_hw_index += 1;
            return 0;
        }
        self.rx.distance(self.rx.wp, cur_wp)
    }

    /// Upstream `rtw_pci_dma_check`: the hardware stamps each buffer's BD
    /// with a running tag; a gap means a PCIe timeout (a warning upstream).
    fn dma_check(&mut self, idx: u32) {
        let off = self.layout.rx_bd + idx as usize * RX_BUF_DESC_SZ;
        let (_, total_pkt_size, _) = ring::read_rx_bd(&self.desc, off);
        if total_pkt_size != self.rx_tag {
            self.stats.rx_tag_mismatch += 1;
        }
        self.rx_tag = (self.rx_tag + 1) % RX_TAG_MAX;
    }

    /// Copy slot `idx` into `scratch` and parse it; `None` drops it.
    fn read_slot(
        &mut self,
        idx: u32,
        scratch: &mut [u8],
        stat: &mut RxPktStat,
    ) -> Option<RxLayout> {
        let base = idx as usize * RX_SLOT;
        ring::copy_out(&self.rxbuf, base, &mut scratch[..RX_PKT_DESC_SZ]);
        let total = match rx::rx_desc_extent(&scratch[..RX_PKT_DESC_SZ]) {
            Ok(total) if total <= RX_BUF_SIZE => total,
            _ => return None,
        };
        ring::copy_out(
            &self.rxbuf,
            base + RX_PKT_DESC_SZ,
            &mut scratch[RX_PKT_DESC_SZ..total],
        );
        rx::query_rx_desc(&self.phy_ctx, &scratch[..total], stat).ok()
    }

    /// Upstream `rtw_pci_rx_napi`: hand at most `limit` buffers to `f`,
    /// re-arm them and move the host index. Returns the buffers processed
    /// (delivered or dropped) and the ones delivered.
    fn rx_napi(&mut self, limit: u32, f: &mut impl FnMut(RxPacket<'_>)) -> (u32, u32) {
        let count = self.rx_pending().min(limit);
        if count == 0 {
            return (0, 0);
        }
        // Read the hardware's index before the buffers it covers.
        fence(Ordering::SeqCst);
        let mut scratch = mem::take(&mut self.scratch);
        let mut cur = self.rx.rp;
        let mut delivered = 0;
        for _ in 0..count {
            self.dma_check(cur);
            let mut stat = RxPktStat::default();
            match self.read_slot(cur, &mut scratch, &mut stat) {
                Some(layout) => {
                    f(RxPacket {
                        buf: &scratch[..layout.end()],
                        stat: &stat,
                        layout,
                    });
                    delivered += 1;
                }
                None => self.stats.rx_dropped += 1,
            }
            self.arm_rx(cur);
            cur = self.rx.advance(cur);
        }
        self.scratch = scratch;
        self.stats.rx_delivered += u64::from(delivered);
        self.rx.rp = cur;
        // `rp`, the last position read, is where the next count starts.
        self.rx.wp = cur;
        // The re-armed BDs are in memory before the hardware may reuse them.
        fence(Ordering::SeqCst);
        self.mmio.write16(RXBD_IDX_MPDUQ, cur as u16);
        (count, delivered)
    }

    /// Upstream `rtw_pci_napi_poll`: deliver up to `budget` received
    /// buffers (802.11 frames and C2H packets alike) to `f`, then, if the
    /// ring was drained, unmask RX again. Returns the buffers delivered.
    /// Upstream reschedules itself when more arrived meanwhile; here the
    /// caller checks [`Pci::rx_pending`] or waits for the next interrupt.
    pub fn rx_poll(&mut self, budget: u32, mut f: impl FnMut(RxPacket<'_>)) -> u32 {
        let (mut work, mut delivered) = (0, 0);
        while work < budget {
            let (done, got) = self.rx_napi(budget - work, &mut f);
            if done == 0 {
                break;
            }
            work += done;
            delivered += got;
        }
        if work < budget && self.running {
            self.enable_interrupt(false);
        }
        delivered
    }
}
