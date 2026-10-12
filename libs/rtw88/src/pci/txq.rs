//! The TX side of the rings (upstream `rtw_pci_tx_write_data`,
//! `rtw_pci_tx_kick_off_queue`, `rtw_pci_tx_isr`,
//! `rtw_pci_write_data_rsvd_page`).
//!
//! Each descriptor owns one slot in `txbuf`; a packet (TX descriptor and
//! payload, built by the caller with [`crate::tx`]) is copied into the slot
//! at the write index, the BD's two segments point at the descriptor and at
//! the payload right behind it, and the host index moves on the next kick.
//! The hardware's read pointer, read on a TX-done interrupt, frees slots.
//! A read pointer outside the ring or past what was queued is ignored and
//! counted, never trusted.

use core::sync::atomic::{fence, Ordering};

use crate::error::{Error, Result};
use crate::hci::{RegExt, TxQueue};
use crate::rtw8822c::spec::TX_PKT_DESC_SZ;

use super::regs::*;
use super::ring::{self, TxBd};
use super::{DmaAlloc, Mmio, Pci};

/// Polls of `BIT_BCN_VALID_V1` after a reserved-page write, 10 us apart
/// (upstream `check_hw_ready` waits up to 50 ms).
const BCN_VALID_TIMEOUT_US: u32 = 50_000;

/// `(size register, base register)` of a queue's ring; the beacon queue
/// has no size register (upstream `rtw_pci_reset_buf_desc`).
pub(super) fn num_desa(queue: TxQueue) -> (u32, u32) {
    match queue {
        TxQueue::Bk => (TXBD_NUM_BKQ, TXBD_DESA_BKQ),
        TxQueue::Be => (TXBD_NUM_BEQ, TXBD_DESA_BEQ),
        TxQueue::Vi => (TXBD_NUM_VIQ, TXBD_DESA_VIQ),
        TxQueue::Vo => (TXBD_NUM_VOQ, TXBD_DESA_VOQ),
        TxQueue::Mgmt => (TXBD_NUM_MGMTQ, TXBD_DESA_MGMTQ),
        TxQueue::Hi0 => (TXBD_NUM_HI0Q, TXBD_DESA_HI0Q),
        TxQueue::H2c => (TXBD_NUM_H2CQ, TXBD_DESA_H2CQ),
        TxQueue::Bcn => (0, TXBD_DESA_BCNQ),
    }
}

/// A queue's index register (upstream `rtw_pci_tx_queue_idx_addr`); the
/// beacon queue has none.
pub(super) fn idx_addr(queue: TxQueue) -> Option<u32> {
    Some(match queue {
        TxQueue::Bk => TXBD_IDX_BKQ,
        TxQueue::Be => TXBD_IDX_BEQ,
        TxQueue::Vi => TXBD_IDX_VIQ,
        TxQueue::Vo => TXBD_IDX_VOQ,
        TxQueue::Mgmt => TXBD_IDX_MGMTQ,
        TxQueue::Hi0 => TXBD_IDX_HI0Q,
        TxQueue::H2c => TXBD_IDX_H2CQ,
        TxQueue::Bcn => return None,
    })
}

impl<M: Mmio, D: DmaAlloc> Pci<M, D> {
    /// Upstream `rtw_pci_tx_write_data`, with the descriptor already in
    /// `pkt`. On a full ring the hardware's read pointer is read once first
    /// (this driver may run with interrupts off during bring-up); still full
    /// is [`Error::Full`].
    pub(super) fn tx_write_data(&mut self, queue: TxQueue, pkt: &[u8]) -> Result<()> {
        if pkt.len() <= TX_PKT_DESC_SZ {
            return Err(Error::Invalid("pci: tx packet without payload"));
        }
        if pkt.len() > ring::tx_slot_size(queue) {
            return Err(Error::Invalid("pci: tx packet larger than its slot"));
        }
        let q = queue as usize;
        if queue != TxQueue::Bcn && self.tx[q].avail() == 0 {
            self.tx_reclaim(queue);
            if self.tx[q].avail() == 0 {
                return Err(Error::Full("pci: tx ring"));
            }
        }
        // The beacon queue has one slot, always 0, re-used on every write
        // (upstream releases the previous rsvd page first).
        let idx = self.tx[q].wp;
        let slot = self.layout.tx_buf[q] + idx as usize * ring::tx_slot_size(queue);
        ring::copy_in(&self.txbuf, slot, pkt);
        let dma = (self.txbuf.phys + slot as u64) as u32;
        let bd = TxBd::new(pkt.len(), dma, queue == TxQueue::Bcn);
        // The packet is in memory before the BD that points at it.
        fence(Ordering::SeqCst);
        ring::write_tx_bd(&self.desc, self.tx_bd_off(queue, idx), &bd);
        if queue == TxQueue::Bcn {
            return Ok(());
        }
        self.tx_queued |= 1 << queue as u8;
        self.tx[q].wp = self.tx[q].advance(idx);
        Ok(())
    }

    /// Upstream `rtw_pci_tx_kick_off_queue`: hand the hardware everything
    /// up to the write index.
    pub(super) fn kick_off_queue(&mut self, queue: TxQueue) {
        let Some(addr) = idx_addr(queue) else { return };
        // Every BD is in memory before the hardware may fetch it.
        fence(Ordering::SeqCst);
        let wp = self.tx[queue as usize].wp & TRX_BD_IDX_MASK;
        self.mmio.write16(addr, wp as u16);
    }

    /// Upstream `rtw_pci_tx_isr`: free what the hardware has sent. Returns
    /// how many descriptors came back.
    pub fn tx_reclaim(&mut self, queue: TxQueue) -> u32 {
        let Some(addr) = idx_addr(queue) else {
            return 0;
        };
        let q = queue as usize;
        let bd_idx = self.mmio.read32(addr);
        let cur_rp = (bd_idx >> TRX_BD_HW_IDX_SHIFT) & TRX_BD_IDX_MASK;
        let ring = self.tx[q];
        let in_flight = ring.distance(ring.rp, ring.wp);
        if cur_rp >= ring.len || ring.distance(ring.rp, cur_rp) > in_flight {
            self.stats.tx_bad_hw_index += 1;
            return 0;
        }
        let count = ring.distance(ring.rp, cur_rp);
        // Read the index before reusing anything it covers.
        fence(Ordering::SeqCst);
        self.tx[q].rp = cur_rp;
        self.stats.tx_reclaimed += u64::from(count);
        count
    }

    /// Upstream `rtw_pci_write_data_rsvd_page`, then the wait upstream's
    /// caller (`rtw_fw_write_data_rsvd_page`) does: `BIT_BCN_VALID_V1` set
    /// in `REG_FIFOPAGE_CTRL_2` means the page landed. The caller is
    /// expected to have cleared that flag (written it as 1) with the page
    /// address and switched `REG_FWHW_TXQ_CTRL`'s `BIT_EN_BCNQ_DL` off, as
    /// `rtw_fw_write_data_rsvd_page` does; those need the page address and
    /// the boundary, which the bus does not know.
    pub(super) fn write_rsvd_page(&mut self, pkt: &[u8]) -> Result<()> {
        self.tx_write_data(TxQueue::Bcn, pkt)?;
        fence(Ordering::SeqCst);
        let work = self.mmio.read8(TXBD_BCN_WORK);
        self.mmio.write8(TXBD_BCN_WORK, work | BIT_PCI_BCNQ_FLAG);
        let mask = u32::from(BIT_BCN_VALID_V1);
        self.poll32(
            "rsvd page",
            REG_FIFOPAGE_CTRL_2,
            mask,
            mask,
            10,
            BCN_VALID_TIMEOUT_US,
        )
    }

    /// The TX BD at `idx` of `queue`, as written (tests and diagnostics).
    pub fn tx_bd(&self, queue: TxQueue, idx: u32) -> Option<TxBd> {
        (idx < self.tx[queue as usize].len)
            .then(|| ring::read_tx_bd(&self.desc, self.tx_bd_off(queue, idx)))
    }
}
