use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::messenger::{errno, Endpoint, Error, Message, Result};

use super::executor::FlagWaker;
use super::future::{Call, Recv};
use super::Parcel;

// ---------------------------------------------------------------------------
// Select-style multiplexer
// ---------------------------------------------------------------------------

/// One completed operation reported by [`Selector::step`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// The call queued at `index` finished with this result.
    Call {
        /// Index returned by [`Selector::call`].
        index: usize,
        /// The reply parcel, or the error that ended the transaction.
        result: Result<Parcel>,
    },
    /// The receive queued at `index` delivered a message.
    Recv {
        /// Index returned by [`Selector::recv`].
        index: usize,
        /// The delivered message.
        message: Message,
    },
    /// Nothing is queued; there is nothing to wait for.
    Idle,
}

/// A `select`-style multiplexer over pending calls and one-way receives.
///
/// Calls are registered as they are queued, so all of them are in flight
/// before the first wait. [`Selector::step`] first drains already-queued
/// one-way messages without blocking, then waits for the oldest in-flight call
/// (a no-op when its reply is already queued). Because every reply wakes the
/// shared Messenger wait queue, once one call completes the next `step` finds
/// any other completed replies without waiting again.
///
/// There is no wall clock and no readiness op for a single transaction, so a
/// step can block on the oldest call even when a newer call already finished;
/// it does not lose that newer reply, it just reports it in a later step.
#[derive(Default)]
pub struct Selector {
    calls: Vec<Option<Call>>,
    recvs: Vec<Option<Recv>>,
    waker: Option<Waker>,
}

impl Selector {
    /// An empty selector.
    pub fn new() -> Selector {
        Selector {
            calls: Vec::new(),
            recvs: Vec::new(),
            waker: Some(Waker::from(FlagWaker::new())),
        }
    }

    /// Queue a call and register it immediately; returns its index for
    /// [`Event::Call`] and [`Selector::cancel`].
    pub fn call(
        &mut self,
        endpoint: Endpoint,
        request: Parcel,
        deadline: Option<u64>,
    ) -> Result<usize> {
        let call = Call::begin(endpoint, request, deadline)?;
        self.calls.push(Some(call));
        Ok(self.calls.len() - 1)
    }

    /// Queue a one-way receive; returns its index for [`Event::Recv`].
    pub fn recv(&mut self, endpoint: Endpoint) -> usize {
        self.recvs.push(Some(Recv::new(endpoint)));
        self.recvs.len() - 1
    }

    /// Cancel the pending call at `index`. Calls that already completed are
    /// gone, so cancelling them is `-EINVAL`.
    pub fn cancel(&mut self, index: usize) -> Result<()> {
        match self.calls.get_mut(index).and_then(Option::as_mut) {
            Some(call) => call.cancel(),
            None => Err(Error::Errno(-errno::EINVAL)),
        }
    }

    /// Number of unfinished operations.
    pub fn pending(&self) -> usize {
        self.calls.iter().filter(|call| call.is_some()).count()
            + self.recvs.iter().filter(|recv| recv.is_some()).count()
    }

    /// Whether no operations are queued.
    pub fn is_empty(&self) -> bool {
        self.pending() == 0
    }

    /// Run one multiplexing pass, reporting at most one event.
    ///
    /// 1. Any queued one-way message is delivered without blocking.
    /// 2. Otherwise the oldest unfinished call is polled to completion (its
    ///    poll parks the task only if the reply is not queued yet).
    /// 3. Otherwise the oldest queued receive blocks until a message arrives.
    /// 4. Otherwise [`Event::Idle`].
    pub fn step(&mut self) -> Result<Event> {
        // 1. One-way messages that already landed never block.
        for index in 0..self.recvs.len() {
            let Some(recv) = self.recvs[index].as_ref() else {
                continue;
            };
            if let Some(message) = recv.poll_ready()? {
                self.recvs[index] = None;
                return Ok(Event::Recv { index, message });
            }
        }
        // 2. Advance the oldest in-flight call. Its reply may already be
        //    queued; if not, this parks until it is.
        if let Some(index) = self.calls.iter().position(Option::is_some) {
            let result = {
                let call = self.calls[index]
                    .as_mut()
                    .expect("the position is an unfinished call");
                let waker = self
                    .waker
                    .clone()
                    .unwrap_or_else(|| Waker::from(FlagWaker::new()));
                let mut context = Context::from_waker(&waker);
                match Pin::new(call).poll(&mut context) {
                    Poll::Ready(result) => Some(result),
                    Poll::Pending => None,
                }
            };
            if let Some(result) = result {
                self.calls[index] = None;
                return Ok(Event::Call { index, result });
            }
            return Ok(Event::Idle);
        }
        // 3. No calls in flight: a queued receive may block, which is the
        //    task's only wait when the selector is idle.
        if let Some(index) = self.recvs.iter().position(Option::is_some) {
            let result = {
                let recv = self.recvs[index]
                    .as_mut()
                    .expect("the position is an unfinished receive");
                let waker = self
                    .waker
                    .clone()
                    .unwrap_or_else(|| Waker::from(FlagWaker::new()));
                let mut context = Context::from_waker(&waker);
                match Pin::new(recv).poll(&mut context) {
                    Poll::Ready(result) => Some(result),
                    Poll::Pending => None,
                }
            };
            if let Some(message) = result {
                self.recvs[index] = None;
                return Ok(Event::Recv {
                    index,
                    message: message?,
                });
            }
        }
        Ok(Event::Idle)
    }
}
