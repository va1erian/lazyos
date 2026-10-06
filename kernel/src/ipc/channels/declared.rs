//! The declared-transfer gate (issue #516).
//!
//! A request may carry only the handles and shared buffers its method
//! declares in `.midl` (`transfers (...)`): `midlc` compiles every declaration
//! into [`messenger_generated::DECLARED_TRANSFERS`], keyed by the parcel
//! header's `(interface_id, method)`. The send and call paths check the parcel
//! against it before any handle is resolved or moved, so a refused request
//! leaves the sender's table exactly as it was and nothing reaches the
//! receiver's. A request of an unknown interface, or of a method that declares
//! nothing, may carry no transfers at all: explicit by default.
//!
//! The header is the receiver's own view: servers dispatch on the header's
//! interface and method (`Message::method`), so the gate checks the same pair
//! the receiver acts on. Carrying *fewer* objects than declared passes here;
//! servers still demand an exact match (`Message::carries`), which stays as
//! defence in depth. Replies are refused any transfer separately
//! (`call::reply_owned`).

use core::sync::atomic::{AtomicU64, Ordering};

use libmessenger::ParcelView;
use messenger_generated::transfers::Transfers;

use super::Error;

/// Requests refused by the gate since boot (diagnostics and tests).
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// The interface id the kernel test suite's ad-hoc transfer fixtures use
/// (`0x0bad_cafe`). Test builds declare every method of it as carrying up to
/// the per-message limits, so suites that exercise the transfer machinery
/// itself keep working; normal builds have no such exemption.
#[cfg(lazyos_tests)]
pub const TEST_INTERFACE: u64 = 0x0bad_cafe;

/// What `(interface, method)` may carry.
pub fn declared(interface: u64, method: u32) -> Transfers {
    #[cfg(lazyos_tests)]
    if interface == TEST_INTERFACE {
        return Transfers {
            handles: libmessenger::MAX_HANDLES as u8,
            buffers: libmessenger::MAX_BUFFERS as u8,
        };
    }
    messenger_generated::declared_transfers(interface, method)
}

/// Refuse a request that carries more handles or buffers than its header's
/// interface and method declare.
pub(super) fn check_declared(parcel: &ParcelView<'_>) -> Result<(), Error> {
    let header = &parcel.header;
    let allowed = declared(header.interface_id, header.method);
    if allowed.allows(parcel.handle_count(), parcel.buffer_count()) {
        return Ok(());
    }
    REFUSED.fetch_add(1, Ordering::Relaxed);
    Err(Error::UndeclaredTransfer)
}

/// Requests the gate has refused since boot.
pub fn refused() -> u64 {
    REFUSED.load(Ordering::Relaxed)
}
