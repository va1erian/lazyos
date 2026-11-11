//! The std [`Transport`] over [`crate::msg`] (feature `client`): what the
//! static-musl programs (`xui-app`, LazyRAD) build their
//! [`libmessenger::client::Connection`]s on. The native `user` runtime has
//! its own over its `Endpoint`; both speak the same trait.

use std::time::Duration;
use std::vec;

use libmessenger::client::{Deadline, Errno, Handle, Transport};
use libmessenger::Parcel;

use crate::msg::parcel;
use crate::time::clock;

/// The reply buffer a call offers. `confd`'s `List` of a real tree or a 4 KiB
/// value is far above a small buffer (the kernel answers `E2BIG` when the
/// reply overflows it); the kernel itself allows parcels up to
/// `libmessenger::MAX_PARCEL_BYTES` (1 MiB).
const REPLY_BUF: usize = 64 * 1024;

/// How long a program waits for a service to register: the one connect bound
/// of the std runtime.
const CONNECT_BOUND: Deadline = Deadline::secs(10);

/// Pause between two attempts to resolve a name that is not registered yet.
const RESOLVE_PAUSE: Duration = Duration::from_millis(10);

/// The std transport. Stateless: the handle cache is the `Connection`'s.
#[derive(Clone, Copy, Debug, Default)]
pub struct Std;

impl Transport for Std {
    fn connect_bound(&self) -> Deadline {
        CONNECT_BOUND
    }

    /// The UI and LazyRAD loops serve and call on one thread, so their calls
    /// must not trip the kernel's cycle check.
    fn call_flags(&self) -> u16 {
        libmessenger::flags::ALLOW_NESTED
    }

    /// The runtime's one bounded wait for a name. The kernel-served registry
    /// has no "wait for name" operation, so this retries every
    /// [`RESOLVE_PAUSE`] until `until`; `docs/client-api-plan.md` 3.4 is the
    /// follow-up that replaces the sleep with a wake-up.
    fn resolve(&self, name: &str, until: Deadline) -> Result<Handle, Errno> {
        let stop = until.absolute_ticks(clock());
        loop {
            match parcel::resolve(name) {
                Ok(handle) => return Ok(handle),
                Err(code) => {
                    let expired = until == Deadline::Poll || (stop != 0 && clock() >= stop);
                    if expired {
                        return Err(Errno::from_syscall(code));
                    }
                    std::thread::sleep(RESOLVE_PAUSE);
                }
            }
        }
    }

    fn call(&self, handle: Handle, request: &Parcel, until: Deadline) -> Result<Parcel, Errno> {
        let mut buf = vec![0u8; REPLY_BUF];
        parcel::call(handle, request, &mut buf, until.absolute_ticks(clock()))
            .map_err(Errno::from_syscall)
    }

    fn send(&self, handle: Handle, request: &Parcel) -> Result<(), Errno> {
        parcel::send(handle, request).map_err(Errno::from_syscall)
    }

    fn release(&self, handle: Handle) {
        // A handle already gone has nothing left to release.
        let _ = crate::msg::release(handle);
    }
}
