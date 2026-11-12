//! [`resolve_waiting`]: the one way both runtimes wait for a name, over the
//! kernel's `WaitFor`, with the two answers that are not an outcome.

use super::{Errno, Handle};

/// Wait for a name through `wait` (one `WaitFor` with the caller's absolute
/// deadline, fixed before the first call).
///
/// * `EINTR`: a signal ended the kernel wait; `on_interrupt` runs, then the
///   wait is issued again with the same deadline. Returning `EINTR` is not
///   enough to run a Linux handler: Messenger ops enter through `int 0x80`,
///   whose return only ends a native task (`deliver_native`) and never lays out
///   a handler frame. So the std runtime's hook is a Linux syscall
///   (`sched_yield`) whose return goes through `deliver_linux`; without it a
///   deferred frame would be re-issued in a busy loop until the kernel killed
///   the task, and an ordinary handled signal would wait out the deadline.
///   The native runtime has no handlers (a fatal signal ends it on this very
///   return), so its hook is empty.
/// * `EAGAIN`: the kernel's waiter table is full (or this process holds its
///   share). That is overload, not a missing service, so the connect does not
///   fail: it degrades to `poll` (a plain `Resolve`) every `pause`, until
///   `expired`. This is the only sleep left in the runtime, taken only
///   when the kernel has no room to park another waiter.
pub fn resolve_waiting(
    mut wait: impl FnMut() -> Result<Handle, Errno>,
    mut poll: impl FnMut() -> Result<Handle, Errno>,
    mut expired: impl FnMut() -> bool,
    mut pause: impl FnMut(),
    mut on_interrupt: impl FnMut(),
) -> Result<Handle, Errno> {
    loop {
        match wait() {
            Err(Errno::INTR) => on_interrupt(),
            Err(Errno::AGAIN) => break,
            outcome => return outcome,
        }
    }
    loop {
        match poll() {
            Ok(handle) => return Ok(handle),
            Err(_) if expired() => return Err(Errno::TIMEDOUT),
            Err(_) => pause(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_signal_reissues_the_wait() {
        let calls = Cell::new(0);
        let order = std::cell::RefCell::new(Vec::new());
        let found = resolve_waiting(
            || {
                order.borrow_mut().push("wait");
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(Errno::INTR)
                } else {
                    Ok(7)
                }
            },
            || panic!("no fallback for a signal"),
            || false,
            || panic!("no pause"),
            || order.borrow_mut().push("hook"),
        );
        assert_eq!(found, Ok(7));
        assert_eq!(calls.get(), 3);
        // The hook runs after each interrupted wait and before the next one.
        assert_eq!(*order.borrow(), ["wait", "hook", "wait", "hook", "wait"]);
    }

    #[test]
    fn a_timeout_or_refusal_is_returned_as_it_is() {
        for errno in [Errno::TIMEDOUT, Errno::ACCES, Errno::CANCELED] {
            let found = resolve_waiting(
                || Err(errno),
                || panic!(),
                || false,
                || panic!(),
                || panic!("no hook"),
            );
            assert_eq!(found, Err(errno));
        }
    }

    #[test]
    fn a_full_table_falls_back_to_polling_until_the_deadline() {
        let polls = Cell::new(0);
        let pauses = Cell::new(0);
        let found = resolve_waiting(
            || Err(Errno::AGAIN),
            || {
                polls.set(polls.get() + 1);
                if polls.get() == 3 {
                    Ok(9)
                } else {
                    Err(Errno::NOENT)
                }
            },
            || false,
            || pauses.set(pauses.get() + 1),
            || panic!("no hook"),
        );
        assert_eq!(found, Ok(9));
        assert_eq!(pauses.get(), 2);
        // A name that never appears ends at the bound with the timeout.
        let gone = resolve_waiting(
            || Err(Errno::AGAIN),
            || Err(Errno::NOENT),
            || true,
            || panic!("no pause after the bound"),
            || panic!("no hook"),
        );
        assert_eq!(gone, Err(Errno::TIMEDOUT));
    }
}
