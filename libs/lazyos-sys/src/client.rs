//! The std [`Transport`] over [`crate::msg`] (feature `client`): what the
//! static-musl programs (`xui-app`, LazyRAD) build their
//! [`libmessenger::client::Connection`]s on. The native `user` runtime has
//! its own over its `Endpoint`; both speak the same trait.

use std::vec;

use libmessenger::client::{
    resolve_waiting, CallFailure, Deadline, Errno, Handle, Retry, Transport,
};
use libmessenger::Parcel;
use std::time::Duration;

use crate::msg::parcel;
use crate::time::clock;

/// The reply buffer a call offers. `confd`'s `List` of a real tree or a 4 KiB
/// value is far above a small buffer (the kernel answers `E2BIG` when the
/// reply overflows it); the kernel itself allows parcels up to
/// `libmessenger::MAX_PARCEL_BYTES` (1 MiB).
const REPLY_BUF: usize = 64 * 1024;

/// Pause between plain resolves while the kernel has no room for another
/// waiter (`EAGAIN`): the overload path only, never the normal wait.
const OVERLOAD_PAUSE: Duration = Duration::from_millis(10);

/// The std transport. Stateless: the handle cache is the `Connection`'s.
#[derive(Clone, Copy, Debug, Default)]
pub struct Std;

impl Transport for Std {
    /// The UI and LazyRAD loops serve and call on one thread, so their calls
    /// must not trip the kernel's cycle check.
    fn call_flags(&self) -> u16 {
        libmessenger::flags::ALLOW_NESTED
    }

    /// A name that is not registered yet is waited for in the kernel
    /// (`WaitFor`): the thread parks until it registers or `until` passes,
    /// and nothing polls. [`Deadline::Poll`] is one plain `Resolve`.
    fn resolve(&self, name: &str, until: Deadline) -> Result<Handle, Errno> {
        if until == Deadline::Poll {
            return parcel::resolve(name).map_err(Errno::from_syscall);
        }
        // Fixed once, so a re-issued wait keeps the caller's bound.
        let stop = until.absolute_ticks(clock());
        resolve_waiting(
            || parcel::wait_for(name, stop).map_err(Errno::from_syscall),
            || parcel::resolve(name).map_err(Errno::from_syscall),
            || stop != 0 && clock() >= stop,
            || std::thread::sleep(OVERLOAD_PAUSE),
            // `EINTR` leaves through `int 0x80`, which never runs a Linux
            // handler; a Linux syscall's return does (`deliver_linux`), and
            // `sched_yield` is the cheapest one. Without it a pending handler
            // runs only when the wait ends.
            std::thread::yield_now,
        )
    }

    /// A `retry=never` method issues the two halves of the kernel call, so a
    /// request the kernel refused before queuing (the cached handle's peer is
    /// gone) is told from one the peer had when it died. A `retry=safe` one
    /// is resent either way and takes the single `OP_CALL`.
    fn call(
        &self,
        handle: Handle,
        request: &Parcel,
        until: Deadline,
        retry: Retry,
    ) -> Result<Parcel, CallFailure> {
        let mut buf = vec![0u8; REPLY_BUF];
        let deadline = until.absolute_ticks(clock());
        if retry == Retry::Safe {
            return parcel::call(handle, request, &mut buf, deadline)
                .map_err(|code| CallFailure::delivered(Errno::from_syscall(code)));
        }
        parcel::call_tracked(handle, request, &mut buf, deadline).map_err(|(code, delivered)| {
            CallFailure {
                errno: Errno::from_syscall(code),
                delivered,
            }
        })
    }

    fn send(&self, handle: Handle, request: &Parcel) -> Result<(), Errno> {
        parcel::send(handle, request).map_err(Errno::from_syscall)
    }

    fn release(&self, handle: Handle) {
        // A handle already gone has nothing left to release.
        let _ = crate::msg::release(handle);
    }
}
