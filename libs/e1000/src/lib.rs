//! The logic of the Intel 8254x ("e1000") NIC driver, as host-testable
//! `no_std` code (issue #497, driver-plan stage D7).
//!
//! The second NIC behind `os.lazy.net.nic.v1`, and the proof that the device
//! core is not virtio-shaped: a plain PCI function with one memory BAR of
//! registers and two legacy descriptor rings, driven through the same `dev_*`
//! syscall ops as virtio-net (claim, `map_bar`, `cfg_write` for decode and bus
//! mastering, `dma_alloc`, `irq_enable`/`irq_ack`), with no new op. The
//! client side (rings, frame policy, receive filter, statistics) is the shared
//! [`nicdrv::Engine`]; this crate is only what differs:
//!
//! * [`regs`]: the registers the driver uses and the [`Regs`] accessor;
//! * [`setup`]: reset, the station address, link state, interrupt causes;
//! * [`rings`]: the descriptor rings as the engine's [`nicdrv::NicRings`].
//!
//! The binary (`netdrv`) claims the function, maps BAR 0, allocates the DMA
//! block and hands this crate the pointers; nothing here takes a syscall, so
//! the host tests drive it against a model of the card ([`fake`]) that can lie.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod desc;
pub mod regs;
pub mod rings;
pub mod setup;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use regs::{Mmio, Regs};
pub use rings::{Layout, Rings};
pub use setup::SetupError;

/// Intel's PCI vendor id.
pub const VENDOR: u16 = 0x8086;

/// The 8254x functions this driver knows: the ones QEMU models (`e1000` is
/// the 82540EM, `e1000-82544gc` and `e1000-82545em` the others) and the
/// common 82545/82546 parts that share their register layout and EEPROM
/// read interface. The 82541/82547 families and the PCIe `e1000e` parts
/// differ in exactly those places and are left out on purpose.
pub const DEVICES: &[(u16, &str)] = &[
    (0x100E, "82540EM"),
    (0x100F, "82545EM"),
    (0x1008, "82544EI"),
    (0x1010, "82546EB"),
    (0x1011, "82545EM fiber"),
    (0x1026, "82545GM"),
];

/// The model name of a supported function, `None` for anything else.
pub fn model(vendor: u16, device: u16) -> Option<&'static str> {
    (vendor == VENDOR)
        .then(|| {
            DEVICES
                .iter()
                .find(|(id, _)| *id == device)
                .map(|(_, name)| *name)
        })
        .flatten()
}
