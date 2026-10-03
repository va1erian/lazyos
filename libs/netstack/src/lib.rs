//! The LazyOS network stack (`docs/networking-plan.md`, stage N2): smoltcp over
//! the frame rings shared with the NIC driver, a DHCP client and the ICMP echo
//! path, as host-testable `no_std` + `alloc` code.
//!
//! * [`device`]: a smoltcp `Device` over a receive and a transmit
//!   [`framering`] endpoint; frames are copied out of the ring before smoltcp
//!   parses them, and a corrupt ring poisons the device;
//! * [`stack`]: the interface, the DHCP client (leases are validated before
//!   use), the echo path and its bounded ping table;
//! * [`config`]: DHCP or a static setup, from `confd` values that are parsed
//!   strictly and fall back to DHCP.
//!
//! The service glue (`netd`) owns everything that touches the machine; nothing
//! here takes a syscall or allocates from a client-supplied size.

#![no_std]

extern crate alloc;

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod config;
pub mod device;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod resolvconf;
pub mod stack;
#[cfg(any(test, feature = "fuzz"))]
pub mod testdns;
#[cfg(any(test, feature = "fuzz"))]
pub mod testnet;
#[cfg(any(test, feature = "fuzz"))]
pub mod testpair;

#[cfg(test)]
mod tests;

pub use config::Mode;
pub use device::{DeviceStats, RingDevice};
pub use stack::{
    ready, valid_host_name, Counters, DhcpState, Kind, LookupOutcome, LookupResult, PingError,
    PingOutcome, PingResult, ResolveError, SockAddr, SockError, Source, Stack, State,
};
