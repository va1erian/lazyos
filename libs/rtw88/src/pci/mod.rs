//! The PCIe bus (upstream `pci.c`, `pci.h`): TX/RX buffer-descriptor rings
//! in DMA memory, the interrupt mask and status (`HIMR`/`HISR`), the PCIe
//! PHY parameters over DBI and MDIO, and [`crate::hci::Hci`] for all of it.
//!
//! The binary gives this module two things it cannot make itself: the mapped
//! BAR ([`Mmio`], which also provides the busy-wait) and DMA memory
//! ([`DmaAlloc`]). Neither is a syscall here.
//!
//! | Here | Upstream |
//! |---|---|
//! | [`Pci::new`] | `rtw_pci_init`, `rtw_pci_init_trx_ring` |
//! | [`Hci::setup`] | `rtw_pci_setup`: `rtw_pci_reset_buf_desc`, `rtw_pci_dma_reset` |
//! | [`Hci::start`] / [`Hci::stop`] | `rtw_pci_start` / `rtw_pci_stop` |
//! | [`Pci::interrupt`] | `rtw_pci_interrupt_handler` + `_threadfn`, `rtw_pci_tx_isr` |
//! | [`Pci::rx_poll`] | `rtw_pci_napi_poll`, `rtw_pci_rx_napi` |
//! | [`Hci::tx_write`] ... | `rtw_pci_tx_write_data`, `_tx_kick_off`, `_write_data_rsvd_page`, `_write_data_h2c` |
//! | [`Pci::phy_cfg`] | `rtw_pci_phy_cfg`, `rtw_pci_link_cfg`, `rtw_dbi_*`, `rtw_mdio_write` |
//!
//! **Use from `wifid`.** First [`quiesce`] the chip over the BAR alone, as
//! a previous driver may have left it running with its rings pointing at
//! memory it no longer owns; only then switch bus mastering on. `Pci::new`
//! once (it allocates the three DMA blocks of [`ring`]'s budget), then the
//! bring-up: after the chip info is read
//! (`bringup`'s `chip_info_setup`), call [`Pci::phy_cfg`] with the cut and
//! RFE option, as upstream's probe does, before powering on for real. On
//! every MSI message call [`Pci::interrupt`]; when it reports `rx`, call
//! [`Pci::rx_poll`], which unmasks RX again (upstream masks `ROK` until NAPI
//! has drained the ring).

mod irq;
mod phycfg;
mod quiesce;
pub mod regs;
pub mod ring;
mod rxq;
mod txq;

#[cfg(any(test, feature = "fuzz"))]
pub mod fake;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_rx;

use alloc::vec;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::hci::{Hci, TxQueue};

pub use irq::IrqEvents;
pub use phycfg::{IntfPhyPara, IpSel, PCIE_GEN1_PARA_8822C, PCIE_GEN2_PARA_8822C};
pub use quiesce::{quiesce, Quiesced, CR_POWERED_OFF};
pub use ring::{Layout, RingSizes};
pub use rxq::{RxPacket, RxPoll};

use ring::Ring;

/// The chip's memory-mapped registers (BAR 2, 64 KiB), and the platform's
/// busy-wait, which every register poll needs.
pub trait Mmio {
    fn read8(&self, offset: u32) -> u8;
    fn read16(&self, offset: u32) -> u16;
    fn read32(&self, offset: u32) -> u32;
    fn write8(&self, offset: u32, val: u8);
    fn write16(&self, offset: u32, val: u16);
    fn write32(&self, offset: u32, val: u32);
    /// Busy-wait `us` microseconds.
    fn delay_us(&self, us: u32);
}

/// A block of DMA memory: where the CPU sees it and where the device does.
#[derive(Clone, Copy, Debug)]
pub struct DmaBuf {
    pub virt: *mut u8,
    pub phys: u64,
    pub len: usize,
}

/// Where DMA memory comes from (`dev::dma_alloc` in the binary, the heap in
/// tests). The kernel allows 16 blocks per claim, 4 MiB each; this driver
/// asks for three ([`ring`]).
///
/// # Safety
/// Every [`DmaBuf`] returned must be a live, writable mapping of `len`
/// bytes at `virt`, page aligned, physically contiguous at device address
/// `phys`, used by nothing else, and valid for the life of the driver (a
/// driver never frees DMA while its device runs, docs/architecture/audio.md).
/// [`Pci::new`] checks the alignment and that the block lies below 4 GiB
/// (the 8822C's buffer descriptors hold 32-bit addresses), not the rest.
pub unsafe trait DmaAlloc {
    fn alloc(&mut self, len: usize) -> Result<DmaBuf>;
}

/// What went wrong in a way the rings survived, for `wifid`'s evidence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PciStats {
    pub tx_reclaimed: u64,
    /// A TX read pointer outside the ring or past what was queued (ignored).
    pub tx_bad_hw_index: u64,
    pub rx_delivered: u64,
    /// RX buffers dropped: a descriptor `rx::query_rx_desc` refused, or a
    /// length past the slot.
    pub rx_dropped: u64,
    /// An RX write index outside the ring (ignored).
    pub rx_bad_hw_index: u64,
    /// The RX tag the hardware wrote did not follow ours (upstream warns
    /// "pci bus timeout, check dma status" and goes on).
    pub rx_tag_mismatch: u64,
}

/// The 8822CE on PCIe.
pub struct Pci<M: Mmio, D: DmaAlloc> {
    mmio: M,
    /// Kept so the allocator (and whatever it holds) lives as long as the
    /// rings it handed out.
    _dma: D,
    layout: Layout,
    desc: DmaBuf,
    txbuf: DmaBuf,
    rxbuf: DmaBuf,
    tx: [Ring; 8],
    rx: Ring,
    /// Queues written since the last kick (upstream `tx_queued`).
    tx_queued: u8,
    irq_mask: [u32; 4],
    irq_enabled: bool,
    running: bool,
    rx_tag: u16,
    /// From [`Pci::phy_cfg`]; [`Hci::interface_cfg`] needs it.
    cut_version: Option<u8>,
    /// Private copy of the RX slot being parsed.
    scratch: Vec<u8>,
    stats: PciStats,
    /// From [`Hci::set_phy_status_ctx`], for the PHY status parser.
    phy_ctx: crate::rtw8822c::PhyStatusCtx,
}

// SAFETY: the raw pointers in the three `DmaBuf`s are the only handles to
// DMA memory this `Pci` owns exclusively (the `DmaAlloc` contract); moving
// the `Pci` to another thread moves that ownership with it.
unsafe impl<M: Mmio + Send, D: DmaAlloc + Send> Send for Pci<M, D> {}

fn check_block(buf: &DmaBuf, want: usize) -> Result<()> {
    let end = buf
        .phys
        .checked_add(buf.len as u64)
        .ok_or(Error::NoMemory)?;
    if buf.len < want || end > 1 << 32 {
        return Err(Error::Unsupported("pci: DMA block above 4 GiB or short"));
    }
    if !(buf.virt as usize).is_multiple_of(4096) || !buf.phys.is_multiple_of(4096) {
        return Err(Error::Unsupported("pci: DMA block not page aligned"));
    }
    Ok(())
}

impl<M: Mmio, D: DmaAlloc> Pci<M, D> {
    /// The rings of [`RingSizes::DEFAULT`] over `mmio`, memory from `dma`.
    pub fn new(mmio: M, dma: D) -> Result<Self> {
        Self::with_sizes(mmio, dma, RingSizes::DEFAULT)
    }

    /// As [`Pci::new`] with chosen ring sizes (upstream `rtw_pci_init`):
    /// allocates the three blocks, zeroes them and arms every RX descriptor.
    /// Nothing is written to the chip until [`Hci::setup`].
    pub fn with_sizes(mmio: M, mut dma: D, sizes: RingSizes) -> Result<Self> {
        let layout = Layout::new(sizes)?;
        let desc = dma.alloc(layout.desc_len)?;
        check_block(&desc, layout.desc_len)?;
        let txbuf = dma.alloc(layout.txbuf_len)?;
        check_block(&txbuf, layout.txbuf_len)?;
        let rxbuf = dma.alloc(layout.rxbuf_len)?;
        check_block(&rxbuf, layout.rxbuf_len)?;
        ring::zero(&desc, 0, layout.desc_len);
        ring::zero(&txbuf, 0, layout.txbuf_len);
        ring::zero(&rxbuf, 0, layout.rxbuf_len);
        let mut pci = Pci {
            mmio,
            _dma: dma,
            layout,
            desc,
            txbuf,
            rxbuf,
            tx: sizes.tx.map(Ring::new),
            rx: Ring::new(sizes.rx),
            tx_queued: 0,
            irq_mask: irq::default_mask(),
            irq_enabled: false,
            running: false,
            rx_tag: 0,
            cut_version: None,
            scratch: vec![0; ring::RX_BUF_SIZE],
            stats: PciStats::default(),
            phy_ctx: crate::rtw8822c::PhyStatusCtx::default(),
        };
        pci.arm_all_rx();
        Ok(pci)
    }

    /// Where everything sits.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn stats(&self) -> PciStats {
        self.stats
    }

    /// The register window (for `wifid`'s register dumps).
    pub fn mmio(&self) -> &M {
        &self.mmio
    }

    /// Descriptors free in `queue`'s ring (upstream `avail_desc`).
    pub fn tx_avail(&self, queue: TxQueue) -> u32 {
        self.tx[queue as usize].avail()
    }

    /// Upstream `rtw_pci_reset_buf_desc`: program every ring's base and
    /// size, clear the indices, and start the software rings over.
    fn reset_buf_desc(&mut self) {
        let tmp = self.mmio.read8(regs::PCI_CTRL + 3);
        self.mmio.write8(regs::PCI_CTRL + 3, tmp | 0xf7);

        let bcn = self.ring_dma(TxQueue::Bcn);
        self.mmio.write32(regs::TXBD_DESA_BCNQ, bcn);

        // The 8822C's WLAN CPU is a 3081, so the H2C queue exists.
        for q in [
            TxQueue::H2c,
            TxQueue::Bk,
            TxQueue::Be,
            TxQueue::Vo,
            TxQueue::Vi,
            TxQueue::Mgmt,
            TxQueue::Hi0,
        ] {
            let (num, desa) = txq::num_desa(q);
            let ring = &mut self.tx[q as usize];
            ring.rp = 0;
            ring.wp = 0;
            let len = ring.len;
            self.mmio.write16(num, (len & regs::TRX_BD_IDX_MASK) as u16);
            let dma = self.ring_dma(q);
            self.mmio.write32(desa, dma);
        }
        self.tx[TxQueue::Bcn as usize].rp = 0;
        self.tx[TxQueue::Bcn as usize].wp = 0;

        self.rx.rp = 0;
        self.rx.wp = 0;
        self.mmio.write16(
            regs::RXBD_NUM_MPDUQ,
            (self.rx.len & regs::TRX_BD_IDX_MASK) as u16,
        );
        let rx_dma = (self.desc.phys + self.layout.rx_bd as u64) as u32;
        self.mmio.write32(regs::RXBD_DESA_MPDUQ, rx_dma);

        // Reset the read/write pointers, and the H2C queue's in one write.
        self.mmio.write32(regs::TXBD_RWPTR_CLR, 0xffff_ffff);
        let csr = self.mmio.read32(regs::TXBD_H2CQ_CSR);
        self.mmio.write32(
            regs::TXBD_H2CQ_CSR,
            csr | regs::BIT_CLR_H2CQ_HOST_IDX | regs::BIT_CLR_H2CQ_HW_IDX,
        );
        self.tx_queued = 0;
    }

    /// Device address of `queue`'s BD ring.
    fn ring_dma(&self, queue: TxQueue) -> u32 {
        (self.desc.phys + self.layout.tx_bd[queue as usize] as u64) as u32
    }

    /// Upstream `rtw_pci_dma_reset`: reset TRX DMA and the RX tag.
    fn dma_reset(&mut self) {
        let v = self.mmio.read32(regs::PCI_CTRL);
        self.mmio.write32(
            regs::PCI_CTRL,
            v | regs::BIT_RST_TRXDMA_INTF | regs::BIT_RX_TAG_EN,
        );
        self.rx_tag = 0;
    }

    /// Upstream `rtw_pci_dma_release`: rings back to empty. Nothing to
    /// unmap: the slots are this driver's for good.
    fn dma_release(&mut self) {
        self.reset_buf_desc();
        for q in TxQueue::ALL {
            for i in 0..self.tx[q as usize].len {
                ring::clear_tx_bd(&self.desc, self.tx_bd_off(q, i));
            }
        }
        self.arm_all_rx();
    }

    fn tx_bd_off(&self, queue: TxQueue, idx: u32) -> usize {
        self.layout.tx_bd[queue as usize] + idx as usize * crate::rtw8822c::spec::TX_BUF_DESC_SZ
    }
}

/// What tests and the fuzzer look at from outside the module.
#[cfg(any(test, feature = "fuzz"))]
impl<M: Mmio, D: DmaAlloc> Pci<M, D> {
    pub(crate) fn dma(&self) -> &D {
        &self._dma
    }
    pub(crate) fn desc_phys(&self) -> u64 {
        self.desc.phys
    }
    pub(crate) fn rxbuf_phys(&self) -> u64 {
        self.rxbuf.phys
    }
    pub(crate) fn txbuf_phys(&self) -> u64 {
        self.txbuf.phys
    }
    pub(crate) fn rx_rp(&self) -> u32 {
        self.rx.rp
    }
}

impl<M: Mmio, D: DmaAlloc> Hci for Pci<M, D> {
    fn read8(&mut self, addr: u32) -> u8 {
        self.mmio.read8(addr)
    }
    fn read16(&mut self, addr: u32) -> u16 {
        self.mmio.read16(addr)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.mmio.read32(addr)
    }
    fn write8(&mut self, addr: u32, val: u8) {
        self.mmio.write8(addr, val)
    }
    fn write16(&mut self, addr: u32, val: u16) {
        self.mmio.write16(addr, val)
    }
    fn write32(&mut self, addr: u32, val: u32) {
        self.mmio.write32(addr, val)
    }
    fn delay_us(&mut self, us: u32) {
        self.mmio.delay_us(us)
    }

    /// Upstream `rtw_pci_setup`.
    fn setup(&mut self) -> Result<()> {
        self.reset_buf_desc();
        self.dma_reset();
        Ok(())
    }

    /// Upstream `rtw_pci_start`.
    fn start(&mut self) -> Result<()> {
        self.running = true;
        self.enable_interrupt(false);
        Ok(())
    }

    /// Upstream `rtw_pci_stop`.
    fn stop(&mut self) {
        self.running = false;
        self.disable_interrupt();
        self.dma_release();
    }

    /// Upstream `rtw_pci_interface_cfg` (8822C: from cut D, the EMAC
    /// power-down aux clock switches to the fast clock).
    fn interface_cfg(&mut self) {
        if self
            .cut_version
            .is_some_and(|cut| cut >= regs::CHIP_VER_CUT_D)
        {
            let v = self.mmio.read32(regs::REG_HCI_MIX_CFG);
            self.mmio.write32(
                regs::REG_HCI_MIX_CFG,
                v | regs::BIT_PCIE_EMAC_PDN_AUX_TO_FAST_CLK,
            );
        }
    }

    /// Upstream `rtw_pci_link_ps` turns the chip's ASPM on in power save;
    /// this driver keeps ASPM and CLKREQ off ([`Pci::phy_cfg`]) and never
    /// enters it, so there is nothing to do.
    fn link_ps(&mut self, _enter: bool) {}

    fn tx_write(&mut self, queue: TxQueue, pkt: &[u8]) -> Result<()> {
        self.tx_write_data(queue, pkt)
    }

    fn tx_kick_off(&mut self) {
        for q in TxQueue::ALL {
            if self.tx_queued & (1 << q as u8) != 0 {
                self.kick_off_queue(q);
            }
        }
        self.tx_queued = 0;
    }

    fn write_data_rsvd_page(&mut self, pkt: &[u8]) -> Result<()> {
        self.write_rsvd_page(pkt)
    }

    fn write_data_h2c(&mut self, pkt: &[u8]) -> Result<()> {
        self.tx_write_data(TxQueue::H2c, pkt)?;
        self.kick_off_queue(TxQueue::H2c);
        self.tx_queued &= !(1 << TxQueue::H2c as u8);
        Ok(())
    }
    fn set_phy_status_ctx(&mut self, ctx: crate::rtw8822c::PhyStatusCtx) {
        self.phy_ctx = ctx;
    }
}
