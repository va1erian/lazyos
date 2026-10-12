//! Interrupts (upstream `rtw_pci_init`'s masks, `rtw_pci_enable_interrupt`,
//! `rtw_pci_disable_interrupt`, `rtw_pci_irq_recognized`,
//! `rtw_pci_interrupt_handler` and `rtw_pci_interrupt_threadfn`).
//!
//! The card has one MSI vector, edge triggered: a cause that is not cleared
//! raises no new message. So [`Pci::interrupt`] does what upstream's hard
//! and threaded handlers do together: mask everything, read and
//! write-1-clear the causes, reclaim TX, then unmask, leaving `ROK` masked
//! when RX is pending until [`Pci::rx_poll`] has drained the ring.

use crate::hci::TxQueue;

use super::regs::*;
use super::{DmaAlloc, Mmio, Pci};

/// Upstream `rtw_pci_init`'s `irq_mask`.
pub(super) fn default_mask() -> [u32; 4] {
    [
        IMR_HIGHDOK
            | IMR_MGNTDOK
            | IMR_BKDOK
            | IMR_BEDOK
            | IMR_VIDOK
            | IMR_VODOK
            | IMR_ROK
            | IMR_BCNDMAINT_E
            | IMR_C2HCMD,
        IMR_TXFOVW,
        0,
        IMR_H2CDOK,
    ]
}

/// What one interrupt carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrqEvents {
    /// The causes as read (and cleared), masked: `HISR0`, `HISR1`, unused,
    /// `HISR3`.
    pub status: [u32; 4],
    /// Received buffers are waiting: call [`Pci::rx_poll`].
    pub rx: bool,
    /// Descriptors reclaimed per queue (indexed by [`TxQueue`] as `u8`).
    pub tx_done: [u32; 8],
    /// The firmware raised `C2HCMD` (upstream `rtw_fw_c2h_cmd_isr`: firmware
    /// recovery when `REG_MCU_TST_CFG` says so; C2H packets themselves come
    /// on the RX ring).
    pub c2h: bool,
    /// The TX FIFO overflowed (`IMR_TXFOVW`).
    pub tx_fifo_overflow: bool,
}

impl IrqEvents {
    /// Nothing this driver unmasks was pending (a shared or spurious
    /// message, or one for a card that went away).
    pub fn is_empty(&self) -> bool {
        self.status == [0; 4]
    }
}

/// The ISR bit that reports TX done for each queue, in upstream's order.
const TX_DONE: [(u32, usize, TxQueue); 7] = [
    (IMR_MGNTDOK, 0, TxQueue::Mgmt),
    (IMR_HIGHDOK, 0, TxQueue::Hi0),
    (IMR_BEDOK, 0, TxQueue::Be),
    (IMR_BKDOK, 0, TxQueue::Bk),
    (IMR_VODOK, 0, TxQueue::Vo),
    (IMR_VIDOK, 0, TxQueue::Vi),
    (IMR_H2CDOK, 3, TxQueue::H2c),
];

impl<M: Mmio, D: DmaAlloc> Pci<M, D> {
    /// Upstream `rtw_pci_enable_interrupt`.
    pub(super) fn enable_interrupt(&mut self, exclude_rx: bool) {
        let imr0_unmask = if exclude_rx { IMR_ROK } else { 0 };
        self.mmio.write32(HIMR0, self.irq_mask[0] & !imr0_unmask);
        self.mmio.write32(HIMR1, self.irq_mask[1]);
        self.mmio.write32(HIMR3, self.irq_mask[3]);
        self.irq_enabled = true;
    }

    /// Upstream `rtw_pci_disable_interrupt`.
    pub(super) fn disable_interrupt(&mut self) {
        if !self.irq_enabled {
            return;
        }
        self.mmio.write32(HIMR0, 0);
        self.mmio.write32(HIMR1, 0);
        self.mmio.write32(HIMR3, 0);
        self.irq_enabled = false;
    }

    /// Upstream `rtw_pci_irq_recognized`: read the causes this driver
    /// unmasks and clear exactly those.
    fn irq_recognized(&mut self) -> [u32; 4] {
        let mut st = [
            self.mmio.read32(HISR0),
            self.mmio.read32(HISR1),
            0,
            self.mmio.read32(HISR3),
        ];
        for (s, m) in st.iter_mut().zip(self.irq_mask) {
            *s &= m;
        }
        self.mmio.write32(HISR0, st[0]);
        self.mmio.write32(HISR1, st[1]);
        self.mmio.write32(HISR3, st[3]);
        st
    }

    /// Service one interrupt message: mask, read and clear the causes,
    /// reclaim TX, unmask (without `ROK` if RX is pending). Call
    /// [`Pci::rx_poll`] when the result says `rx`.
    pub fn interrupt(&mut self) -> IrqEvents {
        self.disable_interrupt();
        let status = self.irq_recognized();
        let mut ev = IrqEvents {
            status,
            rx: status[0] & IMR_ROK != 0,
            c2h: status[0] & IMR_C2HCMD != 0,
            tx_fifo_overflow: status[1] & IMR_TXFOVW != 0,
            ..IrqEvents::default()
        };
        for (bit, reg, queue) in TX_DONE {
            if status[reg] & bit != 0 {
                ev.tx_done[queue as usize] = self.tx_reclaim(queue);
            }
        }
        if self.running {
            self.enable_interrupt(ev.rx);
        }
        ev
    }

    /// Whether interrupts are unmasked (upstream `irq_enabled`).
    pub fn irq_enabled(&self) -> bool {
        self.irq_enabled
    }
}
