//! Bluetooth controller firmware loaders for the LazyOS `btd` service
//! (`docs/bluetooth-plan.md` B7).
//!
//! Real radios (Intel AX2xx, Realtek RTL88xx/RTL8761) come up empty or half
//! alive and need a firmware download before they speak useful HCI. This crate
//! is that download as a pure step function:
//!
//! * [`Vendor`] says which loader a device needs, from its USB ids or from its
//!   `Read_Local_Version` manufacturer ([`vendor`]).
//! * [`Loader`] wraps the [`intel`] and [`realtek`] machines behind one
//!   [`Loader::step`]. `btd` runs it between HCI Reset and `bthci::Init`:
//!   it sends the [`Step::Send`] commands, feeds events back, reads the files
//!   a [`Step::NeedFile`] names through `libs/fwload`, and starts `Init` on
//!   [`Done::Operational`] (or [`Done::NoFirmwareNeeded`]). On
//!   [`Done::Reenumerate`] the controller has reset itself and returns as a
//!   new USB device; `btd` starts over when the new interface appears.
//!
//! No I/O, no clock (the caller passes the time), no panics on hostile bytes:
//! firmware files and controller answers are checked before any length is
//! used. Commands are built with `bthci`'s packet encoder. The protocols are
//! the documented behaviour of the controllers as implemented in Linux's
//! `btintel` and `btrtl` drivers; the code is independent.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
#[macro_use]
extern crate std;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod ids;
pub mod intel;
mod link;
pub mod realtek;
mod step;
#[cfg(test)]
mod tests;
pub mod vendor;

pub use intel::Intel;
pub use realtek::Realtek;
pub use step::{Config, Done, Failure, FileRequest, Input, Reason, Step};
pub use vendor::Vendor;

/// The loader for one controller.
pub enum Loader {
    Intel(Intel),
    Realtek(Realtek),
    /// A controller that needs nothing: finishes at once.
    None,
}

impl Loader {
    pub fn new(vendor: Vendor) -> Loader {
        match vendor {
            Vendor::Intel => Loader::Intel(Intel::new()),
            Vendor::Realtek => Loader::Realtek(Realtek::new()),
            Vendor::Generic => Loader::None,
        }
    }

    /// Advance with one input at time `now_ms` (any monotonic millisecond
    /// clock). Call with [`Input::Start`] first. After [`Step::Done`] or
    /// [`Step::Failed`] every call returns [`Step::Wait`].
    pub fn step(&mut self, now_ms: u64, input: Input<'_>) -> Step {
        match self {
            Loader::Intel(l) => l.step(now_ms, input),
            Loader::Realtek(l) => l.step(now_ms, input),
            Loader::None => match input {
                Input::Start => Step::Done(Done::NoFirmwareNeeded),
                _ => Step::Wait,
            },
        }
    }

    /// When to call [`Loader::step`] with [`Input::Tick`] if nothing arrives
    /// first (a command or an awaited event has timed out by then).
    pub fn deadline(&self) -> Option<u64> {
        match self {
            Loader::Intel(l) => l.deadline(),
            Loader::Realtek(l) => l.deadline(),
            Loader::None => None,
        }
    }
}
