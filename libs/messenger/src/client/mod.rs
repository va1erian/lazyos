//! The client runtime for Messenger calls, shared by every program
//! (`docs/client-api-plan.md` 3.4): the [`Transport`] a runtime implements
//! (native `user`, std over `lazyos-sys`, the host [`Mock`]), the
//! [`Connection`] that resolves a service by name, caches the handle and
//! survives a restart, the [`CallError`] every failure becomes, and the
//! named [`Errno`] and [`Deadline`] they speak in, and the [`Retry`] policy
//! that says which calls may be resent to a restarted service.
//!
//! Generated clients are generic over [`Transport`]; hand-written code never
//! builds a request header or scans a reply for the error field.

mod connection;
mod deadline;
mod errno;
mod error;
mod policy;
#[cfg(any(test, feature = "mock"))]
mod test_mock;
#[cfg(test)]
mod tests;
mod waiting;

pub use connection::{is_peer_gone, Connection, Handle, Transport, CONNECT_BOUND, POLL_HINT_NS};
pub use deadline::{Deadline, TICK_MILLIS};
pub use errno::Errno;
pub use error::{remote_error, CallError, Detail, ERROR_FIELD};
pub use policy::{CallFailure, Retry};
#[cfg(any(test, feature = "mock"))]
pub use test_mock::{Mock, Recorded};
pub use waiting::resolve_waiting;
