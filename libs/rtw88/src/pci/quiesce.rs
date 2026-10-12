//! Silencing a chip a previous driver left running, before bus mastering is
//! switched on again.
//!
//! When a driver dies (a panic, an exit, `dbgctl reload`), the kernel turns
//! bus mastering off and returns its DMA blocks to the pool, but the chip
//! stays powered: its ring base registers still hold the old blocks'
//! addresses and its RX DMA is still enabled. A new driver that turned bus
//! mastering back on before reprogramming the rings would let the chip
//! write received frames into pages that now belong to someone else.
//!
//! [`quiesce`] runs on the register BAR alone (no DMA memory, so before
//! [`super::Pci::new`]) and leaves the chip unable to master the bus:
//!
//! 1. every interrupt mask cleared (upstream `rtw_pci_disable_interrupt`,
//!    plus `HIMR2`);
//! 2. if the MAC is on (`CR` is not the powered-off 0xea): the host and MAC
//!    TX/RX DMA enables in `CR` cleared and the PCIe DMA interface reset
//!    (the bit upstream's `rtw_pci_dma_reset` sets), so DMA stops at once
//!    even if step 3 times out;
//! 3. the MAC powered off by the card-disable flow (upstream
//!    `rtw_mac_power_off`), so the bring-up's `rtw_mac_power_on` starts
//!    from a cold MAC;
//! 4. the interrupt causes cleared (write 1 to clear).

use crate::dev::Dev;
use crate::error::{Error, Result};
use crate::hci::{Hci, TxQueue};
use crate::mac::regs::{
    CR, HCI_RXDMA_EN, HCI_TXDMA_EN, MASK_CHIP_VER, RXDMA_EN, SHIFT_CHIP_VER, SYS_CFG1, TXDMA_EN,
};

use super::regs::{
    BIT_RST_TRXDMA_INTF, HIMR0, HIMR1, HIMR2, HIMR3, HISR0, HISR1, HISR2, HISR3, PCI_CTRL,
};
use super::Mmio;

/// What `CR` reads while the MAC is powered off (upstream
/// `rtw_mac_power_switch`'s test).
pub const CR_POWERED_OFF: u8 = 0xea;

/// The `CR` bits that let the chip move data to or from host memory.
const CR_DMA: u8 = HCI_TXDMA_EN | HCI_RXDMA_EN | TXDMA_EN | RXDMA_EN;

/// What [`quiesce`] found and left, for the driver's evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quiesced {
    /// `CR` as found: anything but [`CR_POWERED_OFF`] means a previous
    /// driver left the MAC on.
    pub cr_before: u8,
    /// `CR` after the power-off flow ([`CR_POWERED_OFF`] when it worked).
    pub cr_after: u8,
}

impl Quiesced {
    /// The MAC was on when found.
    pub fn was_running(&self) -> bool {
        self.cr_before != CR_POWERED_OFF
    }
}

/// Stop every interrupt and DMA source of the chip behind `mmio` and power
/// its MAC off (see the module docs). Safe on a chip already off.
pub fn quiesce<M: Mmio>(mmio: &M) -> Quiesced {
    for imr in [HIMR0, HIMR1, HIMR2, HIMR3] {
        mmio.write32(imr, 0);
    }
    let cr_before = mmio.read8(CR);
    if cr_before != CR_POWERED_OFF {
        mmio.write8(CR, cr_before & !CR_DMA);
        let ctrl = mmio.read32(PCI_CTRL);
        mmio.write32(PCI_CTRL, ctrl | BIT_RST_TRXDMA_INTF);
        let mut dev = Dev::new(Regs(mmio));
        let sys_cfg1 = mmio.read32(SYS_CFG1);
        dev.hal.cut_version = ((sys_cfg1 >> SHIFT_CHIP_VER) & MASK_CHIP_VER) as u8;
        crate::mac::power_off(&mut dev);
    }
    for isr in [HISR0, HISR1, HISR2, HISR3] {
        let pending = mmio.read32(isr);
        mmio.write32(isr, pending);
    }
    Quiesced {
        cr_before,
        cr_after: mmio.read8(CR),
    }
}

/// The registers alone, as an [`Hci`] for the power sequence: nothing here
/// can queue a packet or touch DMA.
struct Regs<'a, M: Mmio>(&'a M);

impl<M: Mmio> Hci for Regs<'_, M> {
    fn read8(&mut self, addr: u32) -> u8 {
        self.0.read8(addr)
    }
    fn read16(&mut self, addr: u32) -> u16 {
        self.0.read16(addr)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.0.read32(addr)
    }
    fn write8(&mut self, addr: u32, val: u8) {
        self.0.write8(addr, val)
    }
    fn write16(&mut self, addr: u32, val: u16) {
        self.0.write16(addr, val)
    }
    fn write32(&mut self, addr: u32, val: u32) {
        self.0.write32(addr, val)
    }
    fn delay_us(&mut self, us: u32) {
        self.0.delay_us(us)
    }
    fn setup(&mut self) -> Result<()> {
        Err(NO_DMA)
    }
    fn start(&mut self) -> Result<()> {
        Err(NO_DMA)
    }
    fn stop(&mut self) {}
    fn interface_cfg(&mut self) {}
    fn link_ps(&mut self, _enter: bool) {}
    fn tx_write(&mut self, _queue: TxQueue, _pkt: &[u8]) -> Result<()> {
        Err(NO_DMA)
    }
    fn tx_kick_off(&mut self) {}
    fn write_data_rsvd_page(&mut self, _pkt: &[u8]) -> Result<()> {
        Err(NO_DMA)
    }
    fn write_data_h2c(&mut self, _pkt: &[u8]) -> Result<()> {
        Err(NO_DMA)
    }
}

const NO_DMA: Error = Error::Unsupported("quiesce: registers only, no DMA");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac::regs::MAC_TRX_ENABLE;
    use crate::pci::fake::FakeMmio;
    use crate::rtw8822c::spec::CARD_DISABLE_FLOW_8822C;

    /// Every register the card-disable flow writes.
    fn disable_flow_regs() -> std::vec::Vec<u32> {
        CARD_DISABLE_FLOW_8822C
            .iter()
            .flat_map(|cmds| cmds.iter())
            .filter(|c| c.cmd == crate::pwrseq::PwrCmd::Write)
            .map(|c| u32::from(c.offset))
            .collect()
    }

    #[test]
    fn a_running_chip_is_masked_stopped_and_powered_off() {
        let mmio = FakeMmio::default();
        mmio.set32(HIMR0, 0xffff_ffff);
        mmio.set32(HIMR1, 0xffff_ffff);
        mmio.set32(HIMR3, 0xffff_ffff);
        mmio.set32(HISR0, 0x1);
        mmio.set32(CR, u32::from(MAC_TRX_ENABLE));
        let q = quiesce(&mmio);
        assert!(q.was_running());
        for imr in [HIMR0, HIMR1, HIMR2, HIMR3] {
            assert_eq!(mmio.peek32(imr), 0, "HIMR {imr:#x} masked");
        }
        let w = mmio.writes();
        let at = |pred: &dyn Fn(&(u8, u32, u32)) -> bool| w.iter().position(pred);
        let last_mask = at(&|x| x.1 == HIMR3).unwrap();
        let dma_off = at(&|x| x.0 == 1 && x.1 == CR).expect("CR written");
        assert_eq!(w[dma_off].2 as u8 & CR_DMA, 0, "DMA enables cleared");
        assert!(last_mask < dma_off, "interrupts masked first");
        let reset = at(&|x| x.1 == PCI_CTRL).expect("DMA interface reset");
        assert_ne!(w[reset].2 & BIT_RST_TRXDMA_INTF, 0);
        let flow = disable_flow_regs();
        let first_flow = at(&|x| x.1 != CR && flow.contains(&x.1)).expect("disable flow ran");
        assert!(
            dma_off < first_flow,
            "DMA stopped before the power sequence"
        );
        // The stale cause is cleared (written back).
        assert!(w.iter().any(|x| x.1 == HISR0 && x.2 == 1));
    }

    #[test]
    fn a_chip_already_off_is_only_masked() {
        let mmio = FakeMmio::default();
        mmio.set32(CR, u32::from(CR_POWERED_OFF));
        let q = quiesce(&mmio);
        assert!(!q.was_running());
        assert_eq!(q.cr_after, CR_POWERED_OFF);
        let w = mmio.writes();
        assert!(w.iter().all(|x| x.1 != CR && x.1 != PCI_CTRL));
        let flow = disable_flow_regs();
        assert!(w.iter().all(|x| !flow.contains(&x.1)), "no power sequence");
        assert_eq!(mmio.peek32(HIMR0), 0);
    }
}
