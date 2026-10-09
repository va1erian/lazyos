//! The wire of `dbgd`, the remote inspection service of a LazyOS box
//! (docs/dbgd-plan.md, issue #701): a PC with no serial port is read over its
//! network card, by a person with `dbgctl` or an agent with the MCP bridge.
//!
//! Everything here is pure `no_std` + `alloc` and host-tested, because every
//! byte of it arrives from the network:
//!
//! * [`json`]: a bounded strict JSON reader and the writers replies use;
//! * [`rpc`]: the JSON-RPC 2.0 request, answer and notification lines;
//! * [`auth`]: the HMAC-SHA256 challenge and the failed-attempt lockout;
//! * [`config`]: the `diag.dbg.*` lines of `/boot/lazyos.cfg`;
//! * [`fsallow`]: the paths `fs.read` may open;
//! * [`methods`]: the method table and its parameter checks;
//! * [`logline`]: a `TAG key=value` log line as a record.
//!
//! [`fuzz::run`] is the entry point shared by the in-tree seeded tests and
//! the cargo-fuzz target.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod auth;
pub mod config;
pub mod fsallow;
pub mod fuzz;
pub mod json;
pub mod logline;
pub mod methods;
pub mod rpc;

#[cfg(test)]
mod tests;
