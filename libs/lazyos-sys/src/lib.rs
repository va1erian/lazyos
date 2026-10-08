//! The native LazyOS syscalls, defined once for every userspace program
//! (issue #666).
//!
//! LazyOS has two syscall gates. `syscall` is the Linux ABI a static-musl
//! program's `std` uses; `int 0x80` is the native gate, with the Messenger
//! fabric, the display grant, the credential gate, `spawnv` and the rest.
//! The native gate is dispatched by *task*, not by binary kind, so a musl
//! `std` program (an xui app, `rhai`, LazyRAD) reaches the same code a native
//! `no_std` program (`user`) does. This crate is the one place that knows the
//! numbers, the argument blocks and the `int 0x80` instruction:
//!
//! * [`raw`] issues the instruction; [`nr`] names every number, and a host
//!   test checks them against the kernel's dispatch table
//!   (`kernel/src/process/gate.rs`) and the Messenger op table;
//! * the typed layer wraps each surface: [`msg`] (the fabric, with
//!   [`msg::OwnedHandle`]), [`display`], [`cred`], [`time`], [`spawn`]
//!   (feature `alloc`), [`stats`] and its decoded [`sysinfo`] snapshot,
//!   [`dev`], and the service-only surfaces the
//!   native daemons use ([`input`], [`inet`], [`storage`], [`random`],
//!   [`kill`], [`process`]);
//! * feature `parcel` adds [`msg::parcel`], the `libmessenger` call/registry
//!   helpers, and feature `std` the `io::Error` and `Duration` conveniences.
//!
//! Errors are negative errnos (`Err(-EPERM)`), as the kernel returns them;
//! [`errno`] names them. Linux numbering, so under `std`
//! [`errno::io_error`] is an ordinary [`std::io::Error`].

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod cred;
pub mod detect;
pub mod dev;
pub mod display;
pub mod errno;
pub mod inet;
pub mod input;
pub mod kill;
pub mod msg;
pub mod nr;
pub mod process;
pub mod random;
pub mod raw;
pub mod stats;
pub mod storage;
pub mod time;

#[cfg(feature = "alloc")]
pub mod spawn;
#[cfg(feature = "alloc")]
pub mod sysinfo;

/// `Ok(())` for `0`, otherwise the code as the error (a negative errno).
pub(crate) fn zero(code: i64) -> Result<(), i64> {
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}

/// `Ok(value)` for a non-negative result, `Err(code)` for a negative errno.
pub(crate) fn value(code: i64) -> Result<u64, i64> {
    if code < 0 {
        Err(code)
    } else {
        Ok(code as u64)
    }
}
