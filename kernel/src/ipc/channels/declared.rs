//! The declared-object gate (issue #516, `docs/messenger-core-plan.md` 2.3).
//!
//! A request carries exactly the objects its method declares in `.midl` (its
//! `Channel<I>`, `Buffer` and `Ring<...>` parameters, nested structs
//! included): `midlc` compiles every declaration into
//! [`messenger_generated::DECLARED_OBJECTS`], keyed by the parcel header's
//! `(interface_id, method)`, as one static kind list per method. The send and
//! call paths compare the parcel's object list with it, entry for entry,
//! before any handle is resolved or moved, so a refused request leaves the
//! sender's table exactly as it was and nothing reaches the receiver's. A
//! request of an unknown interface, or of a method that declares nothing,
//! may carry no objects at all: explicit by default.
//!
//! The header is the receiver's own view: servers dispatch on the header's
//! interface and method (`Message::method`), so the gate checks the same pair
//! the receiver acts on. The generated decoder repeats the check as defence
//! in depth (each object field claims its declared slot). Replies are refused
//! any object separately (`call::reply_owned`).

use core::sync::atomic::{AtomicU64, Ordering};

use libmessenger::{ObjectKind, ParcelView};

use super::Error;

/// Requests refused by the gate since boot (diagnostics and tests).
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// The interface id the kernel test suite's ad-hoc object fixtures use
/// (`0x0bad_cafe`). Test builds let every method of it carry any kind list
/// up to the per-message limit, so suites that exercise the object
/// machinery itself keep working; normal builds have no such exemption.
#[cfg(lazyos_tests)]
pub const TEST_INTERFACE: u64 = 0x0bad_cafe;

/// The kinds `(interface, method)` declares, in object-list order; empty
/// when it declares none (including every method of an unknown interface).
pub fn declared(interface: u64, method: u32) -> &'static [ObjectKind] {
    messenger_generated::declared_objects(interface, method)
}

/// Refuse a request whose object list is not exactly what its header's
/// interface and method declare: same length, same kinds, same order.
pub(super) fn check_declared(parcel: &ParcelView<'_>) -> Result<(), Error> {
    let header = &parcel.header;
    #[cfg(lazyos_tests)]
    if header.interface_id == TEST_INTERFACE {
        return Ok(());
    }
    let kinds = declared(header.interface_id, header.method);
    if parcel.object_count() == kinds.len()
        && parcel
            .objects()
            .map(|object| object.kind())
            .eq(kinds.iter().copied())
    {
        return Ok(());
    }
    REFUSED.fetch_add(1, Ordering::Relaxed);
    Err(Error::UndeclaredObject)
}

/// Requests the gate has refused since boot.
pub fn refused() -> u64 {
    REFUSED.load(Ordering::Relaxed)
}
