//! LazyOS bindings for the Rhai scripting engine (issue #319, step R0 of the
//! Rhai plan).
//!
//! The crate is `no_std` + `alloc` on purpose: the musl `std` host
//! (`rhai-host/`) is a thin wrapper today, and a native host stays possible
//! later. Everything that touches the world goes through [`Host`], so the whole
//! surface is tested on the developer's machine with `cargo test` against an
//! in-memory `mock::MockHost`.
//!
//! * [`build_engine`] makes an [`Engine`] with limits, `print`/`debug` routed
//!   to the host, the [`os`] module and (when the host has a fabric) the
//!   [`msg`] module installed. [`msg::install`] also works on an engine built
//!   elsewhere (the LazyRAD player's).
//! * [`eval_source`] runs a script and classifies the result ([`Outcome`]).
//! * [`repl::Repl`] is the line-at-a-time REPL state machine.
//!
//! `sync` must stay off in Rhai: the bindings share the host with `Rc`.
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod engine;
pub mod host;
pub mod limits;
#[cfg(test)]
pub mod mock;
pub mod msg;
pub mod os;
pub mod outcome;
pub mod repl;

pub use engine::{build_engine, Config};
pub use host::{DirEntry, EntryKind, Host, HostError};
pub use limits::Limits;
pub use outcome::{classify, eval_source, Outcome};
pub use rhai::{Dynamic, Engine, Scope};

/// The Rhai engine version this build embeds. Rhai exports no version
/// constant; a test keeps this equal to the exact pin in `Cargo.toml`.
pub const ENGINE_VERSION: &str = "1.26.1";

#[cfg(test)]
mod tests;
