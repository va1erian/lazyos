//! The host logic of the LazyOS `btd` service (`docs/bluetooth-plan.md` B3): a
//! sans-IO Bluetooth BR/EDR HID host. It brings a controller up, admits
//! only bonded devices, pairs while a window is open, and turns a bonded,
//! authenticated and encrypted HID link into input events.
//!
//! There is no I/O, no clock and no `unsafe` here. The shell
//! (`user/src/bin/btd/`) feeds [`Host`] the HCI events and ACL packets that
//! arrive and the current time, and drains three queues: HCI commands, ACL
//! packets and [`Effect`]s (register or close an input source, publish an
//! event, save the bond file, print an evidence line). Time is a plain
//! number of milliseconds the shell passes in, and [`Host::next_deadline`]
//! says when it needs to be called again, so the shell sleeps on the
//! transport's doorbell and wakes for nothing else (docs/bluetooth-plan.md
//! decision 11).
//!
//! The pieces, one file each: `host.rs` (state, entry points, bring-up),
//! `conn.rs` (links, authentication, encryption), `pair.rs` (Secure Simple
//! Pairing, the bond store), `chan.rs` (SDP, HIDP, the typing device),
//! `paging.rs` (the host's own pages, bounded). The libraries it is built on
//! are `bthci` (packets, flow control, bring-up), `btl2cap`, `btsdp`,
//! `bthid` and `btbond`. Everything from the radio is hostile: tables and
//! queues are bounded and nothing indexes unchecked.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod chan;
pub mod config;
mod conn;
mod ctl;
mod ctl_pair;
pub mod ctl_types;
mod discover;
pub mod effect;
mod host;
mod le;
mod le_gatt;
mod le_link;
mod le_sec;
mod link;
mod paging;
mod pair;
pub mod store;
#[cfg(test)]
mod tests;

pub use config::{Config, PairPolicy};
pub use ctl::{MAX_DISCOVERY_S, UI_WINDOW_MS};
pub use effect::{Effect, SourceKind};
pub use host::{Host, Stats};
pub use pair::{key_authenticated, key_type_accepted};
