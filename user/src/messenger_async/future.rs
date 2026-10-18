use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::messenger::{errno, Endpoint, Error, Message, Result};

use super::Parcel;

// ---------------------------------------------------------------------------
// Leaf futures
// ---------------------------------------------------------------------------

/// A request/response transaction as a future.
///
/// The first poll (or [`Call::begin`]) encodes the request and registers it
/// with `OP_CALL_BEGIN`; the polls after it check the transaction without
/// parking (`OP_CALL_CHECK`, issue #725) and return
/// [`Poll::Pending`](core::task::Poll::Pending) while it is open, so a
/// composed future stays in charge of the task. The wakeup is the kernel's
/// completion queue ([`wait::WAIT_CALLS`](crate::messenger::wait), the
/// executor's park): the poll that changed nothing does not set a waker, so
/// the *executor* is what re-polls after the wait reports the call's
/// completion. [`Call::cancel`] answers any later poll with `-ECANCELED`
/// through `OP_CANCEL`, or marks an unstarted call canceled so it is never
/// sent.
pub struct Call {
    endpoint: Endpoint,
    request: Option<Parcel>,
    deadline: Option<u64>,
    txn: Option<u64>,
    cancelled: bool,
    /// The outcome was taken: nothing is left in the kernel to cancel.
    finished: bool,
}

impl Call {
    /// A future that registers and awaits a call when it is first polled.
    pub fn new(endpoint: Endpoint, request: Parcel, deadline: Option<u64>) -> Call {
        Call {
            endpoint,
            request: Some(request),
            deadline,
            txn: None,
            cancelled: false,
            finished: false,
        }
    }

    /// Register the call immediately (`OP_CALL_BEGIN`), before any wait.
    ///
    /// This is what gives the multiplexer its concurrency: requests are in
    /// flight while later calls are still being queued. The kernel parks the
    /// task on `begin_call` even though the syscall returns immediately, so a
    /// begun call must be awaited or cancelled before the process sleeps on
    /// something else.
    pub fn begin(endpoint: Endpoint, request: Parcel, deadline: Option<u64>) -> Result<Call> {
        let mut call = Call::new(endpoint, request, deadline);
        call.register()?;
        Ok(call)
    }

    /// The endpoint the request travels on.
    pub const fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// The kernel transaction id, once the call has been registered.
    pub const fn txn_id(&self) -> Option<u64> {
        self.txn
    }

    /// Cancel the call: `OP_CANCEL` when it is in flight, otherwise a marker
    /// that the request is never sent. The next poll resolves with
    /// `-ECANCELED` unless the reply was already queued, in which case the
    /// reply wins (the kernel refuses to cancel a terminal transaction, and
    /// `await_reply` returns whatever ended it).
    pub fn cancel(&mut self) -> Result<()> {
        match self.txn {
            Some(txn) => self.endpoint.cancel(txn),
            None => {
                self.cancelled = true;
                Ok(())
            }
        }
    }

    /// Encode and register the pending request, if any.
    fn register(&mut self) -> Result<()> {
        if self.cancelled {
            return Err(Error::Errno(-errno::ECANCELED));
        }
        let Some(request) = self.request.take() else {
            return Ok(());
        };
        let txn = self.endpoint.begin_call(&request, self.deadline)?;
        pending::track(txn);
        self.txn = Some(txn);
        Ok(())
    }
}

impl Future for Call {
    type Output = Result<Parcel>;

    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Err(error) = this.register() {
            return Poll::Ready(Err(error));
        }
        let Some(txn) = this.txn else {
            // Nothing left to send and nothing sent: cancelled before it
            // could register.
            return Poll::Ready(Err(Error::Errno(-errno::ECANCELED)));
        };
        // The non-parking check (OP_CALL_CHECK): a terminal transaction's
        // outcome, or `Pending` with the wakeup on the completion queue.
        // `EAGAIN` is WouldBlock's errno; every other failure is the round's
        // end (the call leaves the registry either way).
        match this.endpoint.await_reply_now(txn) {
            Ok(reply) => {
                pending::untrack(txn);
                this.finished = true;
                Poll::Ready(Ok(reply))
            }
            Err(error) if error.errno() == Some(-errno::EAGAIN) => Poll::Pending,
            Err(error) => {
                pending::untrack(txn);
                this.finished = true;
                Poll::Ready(Err(error))
            }
        }
    }
}

impl Drop for Call {
    /// A future dropped mid-flight can never deliver its reply, so the
    /// transaction is cancelled and its outcome consumed: left in the
    /// kernel's table it would hold its slot, and a reply of up to a parcel
    /// long, until the task died (audit H9, M-R3). The cancel is refused for
    /// a call that already ended (its reply wins), and consuming the outcome
    /// then frees that reply; a call that never registered owns nothing.
    fn drop(&mut self) {
        if let Some(txn) = self.txn.take() {
            pending::untrack(txn);
            if !self.finished {
                let _ = self.endpoint.cancel(txn);
                let _ = self.endpoint.await_reply_now(txn);
            }
        }
    }
}

/// The transactions of this task's leaf `Call` futures, tracked so the
/// executor knows what its park may wait on (the kernel's `WAIT_CALLS`
/// doorbell covers them all; the ids are only what could still arrive). One
/// global table: a leaf that resolves untracks itself, and a transaction
/// only its own caller can await makes a stale entry harmless (the
/// executor's wait checks and prunes the vanished ones).
pub(super) mod pending {
    use alloc::vec::Vec;
    use spin::Mutex;

    use crate::messenger::wait::{WaitItem, MAX_ENDPOINTS};

    static TRACKED: Mutex<Vec<u64>> = Mutex::new(Vec::new());

    /// Add `txn`, once (a poll that re-registers is one call).
    pub(in crate::messenger_async) fn track(txn: u64) {
        let mut tracked = TRACKED.lock();
        if !tracked.contains(&txn) {
            tracked.push(txn);
        }
    }

    /// The tracked ids, for the executor's stale repair.
    pub(in crate::messenger_async) fn ids() -> Vec<u64> {
        TRACKED.lock().clone()
    }

    /// A poll resolved `txn`: it can wake nobody any more.
    pub(in crate::messenger_async) fn untrack(txn: u64) {
        TRACKED.lock().retain(|tracked| *tracked != txn);
    }

    /// The tracked transactions, oldest first, at most [`MAX_ENDPOINTS`] per
    /// wait: the wait arms the kernel's `WAIT_CALLS` doorbell beside them, so
    /// a completion outside the window still returns the park (the ids only
    /// carry the rescan; the doorbell is what covers everything).
    pub(in crate::messenger_async) fn items() -> Vec<WaitItem> {
        let tracked = TRACKED.lock();
        tracked
            .iter()
            .take(MAX_ENDPOINTS)
            .map(|txn| WaitItem::Call(*txn))
            .collect()
    }
}

/// A one-way receive as a future.
///
/// The poll parks the task in `OP_RECV` until a message is queued, the
/// deadline passes, or the peer dies; [`Recv::poll_ready`] is the non-blocking
/// form used by [`Selector`](super::Selector).
pub struct Recv {
    endpoint: Endpoint,
    deadline: Option<u64>,
}

impl Recv {
    /// A receive that waits forever for the next message.
    pub const fn new(endpoint: Endpoint) -> Recv {
        Recv {
            endpoint,
            deadline: None,
        }
    }

    /// A receive that ends with `-ETIMEDOUT` at the absolute PIT deadline.
    pub const fn with_deadline(endpoint: Endpoint, deadline: u64) -> Recv {
        Recv {
            endpoint,
            deadline: Some(deadline),
        }
    }

    /// The endpoint this receive drains.
    pub const fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Check for a queued message without blocking; `Ok(None)` means "try
    /// again later". See [`Endpoint::poll_recv`].
    pub fn poll_ready(&self) -> Result<Option<Message>> {
        self.endpoint.poll_recv()
    }
}

impl Future for Recv {
    type Output = Result<Message>;

    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(self.endpoint.recv(self.deadline))
    }
}

/// `async` spelling of a call: `call(endpoint, request, deadline).await`.
pub async fn call(endpoint: Endpoint, request: Parcel, deadline: Option<u64>) -> Result<Parcel> {
    Call::new(endpoint, request, deadline).await
}

/// `async` spelling of a one-way receive.
pub async fn recv(endpoint: Endpoint, deadline: Option<u64>) -> Result<Message> {
    Recv { endpoint, deadline }.await
}
