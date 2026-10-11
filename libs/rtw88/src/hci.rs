//! The bus seam (upstream `hci.h`, `struct rtw_hci_ops`).
//!
//! Everything above the bus talks to the chip through [`Hci`]: register
//! reads and writes, and handing finished packets to a TX queue. The PCIe
//! implementation is [`crate::pci::Pci`]; the host tests use
//! [`crate::fake::FakeHci`].
//!
//! **One difference from upstream.** Upstream's `tx_write`,
//! `write_data_rsvd_page` and `write_data_h2c` fill the TX packet descriptor
//! inside the bus driver. Here the caller builds the whole packet (the
//! descriptor from [`crate::tx`], then the payload) and the bus only moves
//! bytes, so the bus code never needs the device state. `pkt` passed to
//! [`Hci::tx_write`] is therefore `TX_PKT_DESC_SZ` bytes of descriptor
//! followed by the payload.

use crate::error::Result;

/// The chip's TX queues (upstream `enum rtw_tx_queue_type`); the order of
/// the AC queues matters (it is the hardware's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TxQueue {
    Bk = 0,
    Be = 1,
    Vi = 2,
    Vo = 3,
    Bcn = 4,
    Mgmt = 5,
    Hi0 = 6,
    H2c = 7,
}

impl TxQueue {
    /// Every queue, in hardware order.
    pub const ALL: [TxQueue; 8] = [
        TxQueue::Bk,
        TxQueue::Be,
        TxQueue::Vi,
        TxQueue::Vo,
        TxQueue::Bcn,
        TxQueue::Mgmt,
        TxQueue::Hi0,
        TxQueue::H2c,
    ];
}

/// The bus under the chip. Register widths matter (side effects on
/// neighbours), so there is no generic "write N bytes".
pub trait Hci {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, val: u8);
    fn write16(&mut self, addr: u32, val: u16);
    fn write32(&mut self, addr: u32, val: u32);

    /// Busy-wait `us` microseconds (upstream `udelay`/`usleep_range`/`msleep`).
    fn delay_us(&mut self, us: u32);

    /// Bus bring-up before the MAC is powered (upstream `setup`): for PCIe,
    /// program the ring base addresses and the PHY parameters.
    fn setup(&mut self) -> Result<()>;
    /// Start DMA and interrupts after the MAC is configured (upstream `start`).
    fn start(&mut self) -> Result<()>;
    /// Stop DMA and interrupts (upstream `stop`).
    fn stop(&mut self);
    /// Bus-specific interface configuration after MAC init (upstream
    /// `interface_cfg`).
    fn interface_cfg(&mut self);
    /// Enter or leave the bus's link power save (upstream `link_ps`); a
    /// driver that never enables ASPM may make this a no-op.
    fn link_ps(&mut self, enter: bool);

    /// Queue one finished packet (descriptor + payload, see the module docs).
    /// The packet is copied; the slice is free on return.
    fn tx_write(&mut self, queue: TxQueue, pkt: &[u8]) -> Result<()>;
    /// Ring the doorbells of every queue written since the last kick.
    fn tx_kick_off(&mut self);
    /// Send one packet on the beacon queue into the reserved page area and
    /// wait until the hardware took it (upstream `write_data_rsvd_page`;
    /// `pkt` is descriptor + payload). Used for firmware download and
    /// reserved pages.
    fn write_data_rsvd_page(&mut self, pkt: &[u8]) -> Result<()>;
    /// Send one H2C packet on the H2C queue (upstream `write_data_h2c`;
    /// `pkt` is descriptor + payload).
    fn write_data_h2c(&mut self, pkt: &[u8]) -> Result<()>;
}

/// The register helpers upstream keeps as inline functions in `main.h` and
/// `util.c`, for every [`Hci`].
pub trait RegExt: Hci {
    fn set8(&mut self, addr: u32, bits: u8) {
        let v = self.read8(addr);
        self.write8(addr, v | bits);
    }
    fn clr8(&mut self, addr: u32, bits: u8) {
        let v = self.read8(addr);
        self.write8(addr, v & !bits);
    }
    fn set16(&mut self, addr: u32, bits: u16) {
        let v = self.read16(addr);
        self.write16(addr, v | bits);
    }
    fn clr16(&mut self, addr: u32, bits: u16) {
        let v = self.read16(addr);
        self.write16(addr, v & !bits);
    }
    fn set32(&mut self, addr: u32, bits: u32) {
        let v = self.read32(addr);
        self.write32(addr, v | bits);
    }
    fn clr32(&mut self, addr: u32, bits: u32) {
        let v = self.read32(addr);
        self.write32(addr, v & !bits);
    }
    /// Upstream `rtw_read32_mask`: the field under `mask`, shifted down.
    fn read32_mask(&mut self, addr: u32, mask: u32) -> u32 {
        if mask == 0 {
            return 0;
        }
        (self.read32(addr) & mask) >> mask.trailing_zeros()
    }
    /// Upstream `rtw_write32_mask`: replace the field under `mask`.
    fn write32_mask(&mut self, addr: u32, mask: u32, data: u32) {
        if mask == 0 {
            return;
        }
        let shift = mask.trailing_zeros();
        let v = (self.read32(addr) & !mask) | ((data << shift) & mask);
        self.write32(addr, v);
    }
    /// Upstream `rtw_write8_mask`.
    fn write8_mask(&mut self, addr: u32, mask: u8, data: u8) {
        if mask == 0 {
            return;
        }
        let shift = mask.trailing_zeros();
        let v = (self.read8(addr) & !mask) | ((data << shift) & mask);
        self.write8(addr, v);
    }
    /// Poll `addr` every `step_us` until `(read & mask) == want`, for at
    /// most `timeout_us`. Upstream `read_poll_timeout`/`check_hw_ready`.
    fn poll32(
        &mut self,
        step: &'static str,
        addr: u32,
        mask: u32,
        want: u32,
        step_us: u32,
        timeout_us: u32,
    ) -> Result<()> {
        let mut waited = 0u32;
        loop {
            if self.read32(addr) & mask == want {
                return Ok(());
            }
            if waited >= timeout_us {
                return Err(crate::Error::Timeout { step, addr });
            }
            self.delay_us(step_us.max(1));
            waited = waited.saturating_add(step_us.max(1));
        }
    }
    /// The 8-bit form of [`RegExt::poll32`].
    fn poll8(
        &mut self,
        step: &'static str,
        addr: u32,
        mask: u8,
        want: u8,
        step_us: u32,
        timeout_us: u32,
    ) -> Result<()> {
        let mut waited = 0u32;
        loop {
            if self.read8(addr) & mask == want {
                return Ok(());
            }
            if waited >= timeout_us {
                return Err(crate::Error::Timeout { step, addr });
            }
            self.delay_us(step_us.max(1));
            waited = waited.saturating_add(step_us.max(1));
        }
    }
}

impl<T: Hci + ?Sized> RegExt for T {}
