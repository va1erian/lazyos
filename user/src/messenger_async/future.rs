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
/// with `OP_CALL_BEGIN`; the poll that finds the transaction still open parks
/// the task in `OP_CALL_AWAIT` until the reply is queued. [`Call::cancel`]
/// answers any later poll with `-ECANCELED` through `OP_CANCEL`, or marks an
/// unstarted call canceled so it is never sent.
pub struct Call {
    endpoint: Endpoint,
    request: Option<Parcel>,
    deadline: Option<u64>,
    txn: Option<u64>,
    cancelled: bool,
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
        let txn = this.txn.expect("a registered call owns a transaction id");
        Poll::Ready(this.endpoint.await_reply(txn))
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
