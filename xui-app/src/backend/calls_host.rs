//! The async call lane on a host that is not Linux: no calls in flight.
//!
//! The real lane (`calls.rs`) rides the kernel's `WAIT_CALLS` doorbell, which
//! only a LazyOS process has. A Windows or macOS host builds this crate for
//! tools that never run a backend (`lzpack` packages a LazyRAD project), so
//! nothing is ever in flight and `begin` refuses, as a kernel without the
//! call would.

use crate::sys;

pub(super) fn in_flight() -> bool {
    false
}

/// Refused: there is no kernel to begin a call on.
pub fn begin(
    _handle: u64,
    _request_bytes: &[u8],
    _deadline: u64,
    _on_done: Box<dyn FnOnce(Result<libmessenger::Parcel, i64>)>,
) -> Result<(), i64> {
    Err(-sys::errno::ENOSYS)
}

pub(super) fn drain() {}
