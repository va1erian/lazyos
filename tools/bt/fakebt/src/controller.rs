//! The seam between the USB side of the fake dongle and the Bluetooth
//! controller behind it: the trait of `docs/bluetooth-plan.md` ("The
//! simulator"), which `libs/btsim` defines and implements. `fakebt` serves
//! whichever controller `main.rs` builds (today the simulator with its HID
//! peers, wrapped by `world.rs`).
//!
//! Packets are HCI packets **without** the H4 indicator: a command is its
//! opcode, length and parameters, an event its code, length and parameters,
//! ACL data its header and data.

pub use btsim::{Controller, HciIn};
