//! xHCI (eXtensible Host Controller Interface 1.2) for `usbd`
//! (`docs/usb-hid-plan.md`, phase U0).
//!
//! Pure `no_std` logic over memory the *caller* mapped (the controller's BAR
//! and DMA buffers from syscall 23), like `libs/virtio`: nothing here makes a
//! syscall, so the layouts and ring state machines run under host
//! `cargo test` against a model controller.
//!
//! * [`regs`]: capability, operational, runtime and port register offsets
//!   and bits, and [`regs::Mmio`], the register access trait.
//! * [`trb`]: the 16-byte Transfer Request Block, its types, completion
//!   codes and builders for the commands and transfers `usbd` issues.
//! * [`ring`]: producer rings (command and transfer, with a Link TRB and the
//!   cycle bit) and the event ring consumer, over [`ring::TrbMem`].
//! * [`context`]: slot, endpoint and input-control contexts (32- or 64-byte).
//! * [`extcap`]: the extended capability list: the BIOS-to-OS handoff and
//!   which root ports are USB 2 and which USB 3.
//! * [`route`]: where a device sits (root port, hub ports): route string,
//!   transaction translator and depth for its slot context.
//! * [`setup`]: the USB control requests (standard, HID, hub).
//!
//! Everything the controller writes (event TRBs, completion pointers,
//! context state) is untrusted: a completion that names a TRB outside the
//! ring it should belong to is an [`Error`], never followed.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod context;
pub mod extcap;
pub mod regs;
pub mod ring;
pub mod route;
pub mod setup;
pub mod trb;

#[cfg(test)]
mod tests;

/// Errors from ring and context operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The ring has no free slot (the controller has not caught up).
    RingFull,
    /// A completion names a TRB outside the ring, misaligned, or not in flight.
    BadPointer,
    /// A ring or context buffer is too small or misaligned for its use.
    BadBuffer,
    /// An argument is out of the range the specification allows.
    BadArgument,
}
