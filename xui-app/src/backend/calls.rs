//! The UI thread's async call lane (issue #725, its UI integration): calls
//! a client app begins without waiting for the reply of.
//!
//! The reply returns to whoever parked the loop: `park_client` names the
//! kernel's `WAIT_CALLS` doorbell beside its window endpoints and the
//! `WAIT_FD` set, so one park covers every in-flight call at once, and
//! [`drain`] — called when the doorbell bit is up and once per tick —
//! completes the calls and invokes their callbacks. The lane is
//! thread-local: it exists on the UI thread, where the loop runs, and a
//! callback may capture a `Ui::proxy` and send its own message (all of it
//! happens before the same tick's `Wake` deliveries).
//!
//! The callbacks are app-supplied and must be cheap: they run between the
//! input and update stages of a tick, so a long one makes exactly the
//! difference the bounded calls measure. Work that can outlive a couple of
//! frames does not belong here (see `Service::call_unbounded` and the
//! worker threads).

use std::cell::RefCell;

use crate::sys;

/// The reply buffer one check offers the kernel (the platform's own shape:
/// a decodable reply, `E2BIG` beyond it).
const REPLY_BUF: usize = 64 * 1024;
/// Completions drained per pass: the doorbell woke the loop for them all,
/// and the rest ride the next tick (a pass takes work, it does not do a
/// busy loop on a burst).
const DRAIN_BATCH: usize = 16;

struct Pending {
    txn: u64,
    callback: Box<dyn FnOnce(Result<libmessenger::Parcel, i64>)>,
}

thread_local! {
    static PENDING: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
}

/// Whether the loop's next park must include `WAIT_CALLS`.
pub(super) fn in_flight() -> bool {
    PENDING.with(|pending| !pending.borrow().is_empty())
}

/// Register a call: hand the kernel the encoded `request_bytes` `CALL_BEGIN`
/// makes, record `on_done` against the transaction, and return. The errors
/// the caller has to handle are throwaway — a dead peer is the caller's
/// cached-endpoint question, and the lane never leaves entries behind for
/// refused calls.
pub fn begin(
    handle: u64,
    request_bytes: &[u8],
    deadline: u64,
    on_done: Box<dyn FnOnce(Result<libmessenger::Parcel, i64>)>,
) -> Result<(), i64> {
    let txn = sys::msg_call_begin(handle, request_bytes, deadline)?;
    PENDING.with(|pending| {
        pending.borrow_mut().push(Pending {
            txn,
            callback: on_done,
        })
    });
    Ok(())
}

/// Hand every finished call to its callback (the reply, or the errno that
/// ended it). Runs on the UI thread: the tick that follows the wake the
/// doorbell rang for the loop.
pub(super) fn drain() {
    let mut txns = [0u64; DRAIN_BATCH];
    // One scratch per drain: the checks below write at most the reply's
    // own bytes (the buffer is allocated once, never zeroed).
    let mut buf = vec![0u8; REPLY_BUF];
    loop {
        let Ok(count) = sys::msg_drain_calls(&mut txns) else {
            // The queue refused the read outright — a thread the kernel
            // never saw. Nothing to do; the next wait would fail the same.
            return;
        };
        if count == 0 {
            return;
        }
        for &txn in &txns[..count] {
            let Some(pending) = take(txn) else {
                continue;
            };
            let result = match sys::msg_call_check(txn, &mut buf) {
                // The kernel's own failures keep their reasons (the dead
                // peer's `EPIPE`, the deadline's `ETIMEDOUT`); only a reply
                // beyond this drain's scratch is shaped into the decode
                // codes (`E2BIG` for the overflow, `EINVAL` for bytes that
                // are not a parcel).
                Err(code) => Err(code),
                Ok(len) if len <= buf.len() => {
                    libmessenger::Parcel::decode(&buf[..len]).map_err(|_| -sys::errno::EINVAL)
                }
                Ok(_) => Err(-sys::errno::E2BIG),
            };
            (pending.callback)(result);
        }
    }
}

/// The entry `txn` named, oldest first (`CALL_DRAIN` is per-transaction
/// and may return an id the registry has never tracked after a task
/// died): paired up and cancelled.
fn take(txn: u64) -> Option<Pending> {
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        let index = pending.iter().position(|call| call.txn == txn)?;
        Some(pending.swap_remove(index))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lane is empty per thread: an in-flight call on it never speaks
    /// for another thread's loop.
    #[test]
    fn the_lane_starts_and_stays_empty_per_thread() {
        assert!(!in_flight());
        let joined = std::thread::spawn(|| in_flight()).join().unwrap();
        assert!(!joined, "another thread saw the calls");
        assert!(!in_flight());
    }

    /// `take` hands over and removes the entry a completion names; an id
    /// the lane never tracked (a stale one) is nothing, so a second pass
    /// on the same id is inert.
    #[test]
    fn take_removes_its_own_and_skips_strays() {
        assert!(take(0x1234).is_none(), "the lane began with entries");
        let gone = std::rc::Rc::new(std::cell::Cell::new(0));
        PENDING.with(|pending| {
            pending.borrow_mut().push(Pending {
                txn: u64::from(u16::MAX) + 7,
                callback: Box::new({
                    let gone = std::rc::Rc::clone(&gone);
                    move |reply| {
                        gone.set(match reply {
                            Ok(_) => 1,
                            Err(_) => 2,
                        });
                    }
                }),
            });
        });
        assert!(take(u64::from(u16::MAX) + 7).is_some(), "its own was lost");
        assert!(take(u64::from(u16::MAX) + 7).is_none(), "a take repeats");
    }
}
