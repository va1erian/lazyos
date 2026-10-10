//! The logic of the Realtek RTL8111H / RTL8168H NIC driver, as host-testable
//! `no_std` code (docs/rtl8168-driver-plan.md).
//!
//! The fourth back end of `netdrv` behind `os.lazy.net.nic.v1`, shaped like
//! the 8254x one (`libs/e1000`): a plain PCI function with one
//! memory BAR of registers and two descriptor rings, driven through the same
//! `dev_*` syscall ops, with no new op. The client side (rings, frame policy,
//! receive filter, statistics) is the shared [`nicdrv::Engine`]; this crate is
//! only what differs:
//!
//! * [`regs`]: the registers the driver uses and the width-exact [`Regs`]
//!   accessor (the chip's registers are 8, 16 and 32 bits wide, and some
//!   writes have side effects on their neighbours);
//! * [`setup`]: identify the chip revision, reset, the station address, link
//!   state, interrupt causes, shutdown, and a register dump for diagnosis;
//! * [`phy`] and [`phy_541`]: the MII conversation over `PHYAR` and the
//!   revision-specific steps the box has proven necessary (none yet);
//! * [`rings`]: the descriptor rings as the engine's [`nicdrv::NicRings`];
//! * [`watchdog`]: the transmit-hang rule the binary applies.
//!
//! **One revision.** "RTL8168" is a family told apart by the XID field of
//! `TxConfig`. Only XID `541` (the box's) is driven; [`setup::identify`]
//! refuses every other by name.
//!
//! The binary (`netdrv`) claims the function, maps BAR 2, allocates the DMA
//! block and hands this crate the pointers; nothing here takes a syscall, so
//! the host tests drive it against a model of the chip ([`fake`]) that can lie.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod desc;
pub mod phy;
pub mod phy_541;
pub mod regs;
pub mod rings;
pub mod setup;
pub mod watchdog;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_rings;

pub use regs::{Mmio, Regs};
pub use rings::{Layout, Rings};
pub use setup::{Link, SetupError};
pub use watchdog::TxWatchdog;

/// Realtek's PCI vendor id.
pub const VENDOR: u16 = 0x10EC;

/// The PCI device id of the RTL8111/8168 family; the revision inside is told
/// apart by the XID, not by this id.
pub const DEVICE: u16 = 0x8168;

/// The functions this driver matches by id (`devmatch` keeps its manifest
/// equal to this by test). Whether the chip behind the id is supported is
/// [`setup::identify`]'s answer.
pub const DEVICES: &[(u16, &str)] = &[(DEVICE, "RTL8111H/8168H")];

/// The model name of a function this driver may drive, `None` for anything
/// else.
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
