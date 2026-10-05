use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::messenger::wait::{wait_items, WaitItem, MAX_ENDPOINTS};
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

/// What bit `i` of a wait mask stands for.
#[derive(Clone, Copy)]
enum Slot {
    Call(usize),
    Recv(usize),
}

/// A `select`-style multiplexer over pending calls and one-way receives.
///
/// Calls are registered as they are queued, so all of them are in flight
/// before the first wait. [`Selector::step`] first drains already-queued
/// one-way messages without blocking, then parks once on every in-flight
/// call's transaction and every queued receive's endpoint together (the
/// kernel's `wait` op, issue #309) and reports whichever is ready first: a
/// newer call that finishes before an older one is reported first, and a
/// message on any endpoint wakes the task while calls are still in flight.
///
/// One wait names at most [`MAX_ENDPOINTS`] items. With more queued, every
/// item still wakes the step: the wait watches a window of that many, parks
/// for at most one tick, and the window rotates over the rest on each timeout,
/// so anything outside the current window is seen within a few ticks (and a
/// queued message on any receive is taken before every wait). With
/// [`MAX_ENDPOINTS`] items or fewer the wait has no deadline.
#[derive(Default)]
pub struct Selector {
    calls: Vec<Option<Call>>,
    recvs: Vec<Option<Recv>>,
    waker: Option<Waker>,
    /// First item of the next window when more than [`MAX_ENDPOINTS`] are
    /// queued.
    rotation: usize,
}

impl Selector {
    /// An empty selector.
    pub fn new() -> Selector {
        Selector {
            calls: Vec::new(),
            recvs: Vec::new(),
            waker: Some(Waker::from(FlagWaker::new())),
            rotation: 0,
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

    /// Run one multiplexing pass, reporting exactly one event.
    ///
    /// 1. Any queued one-way message is delivered without blocking.
    /// 2. Otherwise the task parks on every in-flight call and queued receive
    ///    at once and reports the first that is ready: a finished call (its
    ///    await returns at once), or a delivered message.
    /// 3. With nothing queued, [`Event::Idle`].
    pub fn step(&mut self) -> Result<Event> {
        loop {
            if let Some(event) = self.take_queued_message()? {
                return Ok(event);
            }
            let (items, slots, deadline) = self.wait_set();
            if items.is_empty() {
                return Ok(Event::Idle);
            }
            let ready = match wait_items(&items, 0, deadline) {
                Ok(ready) => ready,
                // Only a windowed wait has a deadline: watch the next window.
                Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {
                    self.rotation = self.rotation.wrapping_add(MAX_ENDPOINTS);
                    continue;
                }
                Err(error) => return Err(error),
            };
            for (bit, slot) in slots.iter().enumerate() {
                if ready & (1 << bit) == 0 {
                    continue;
                }
                match *slot {
                    Slot::Call(index) => return Ok(self.finish_call(index)),
                    Slot::Recv(index) => {
                        if let Some(event) = self.take_message(index)? {
                            return Ok(event);
                        }
                    }
                }
            }
            // A ready endpoint whose message is already gone (another holder
            // took it): wait again.
        }
    }

    /// Deliver the first queued one-way message, without blocking.
    fn take_queued_message(&mut self) -> Result<Option<Event>> {
        for index in 0..self.recvs.len() {
            if let Some(event) = self.take_message(index)? {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    /// Take receive `index`'s message if one is queued.
    fn take_message(&mut self, index: usize) -> Result<Option<Event>> {
        let Some(recv) = self.recvs[index].as_ref() else {
            return Ok(None);
        };
        let Some(message) = recv.poll_ready()? else {
            return Ok(None);
        };
        self.recvs[index] = None;
        Ok(Some(Event::Recv { index, message }))
    }

    /// The items of one wait (calls first, then receives), what each bit of
    /// its mask stands for, and its deadline: none when everything fits, else
    /// the next tick, for a window of [`MAX_ENDPOINTS`] starting at the
    /// rotation.
    fn wait_set(&self) -> (Vec<WaitItem>, Vec<Slot>, Option<u64>) {
        let calls = self.calls.iter().enumerate().filter_map(|(index, call)| {
            let txn = call.as_ref()?.txn_id()?;
            Some((WaitItem::Call(txn), Slot::Call(index)))
        });
        let recvs = self.recvs.iter().enumerate().filter_map(|(index, recv)| {
            let endpoint = recv.as_ref()?.endpoint();
            Some((WaitItem::Endpoint(endpoint), Slot::Recv(index)))
        });
        let all: Vec<(WaitItem, Slot)> = calls.chain(recvs).collect();
        if all.len() <= MAX_ENDPOINTS {
            let (items, slots) = all.into_iter().unzip();
            return (items, slots, None);
        }
        let start = self.rotation % all.len();
        let window = all.iter().cycle().skip(start).take(MAX_ENDPOINTS);
        let (items, slots) = window.copied().unzip();
        (items, slots, Some(crate::sys::clock() + 1))
    }

    /// Finish call `index`, which the wait reported ready: its await returns
    /// at once with the reply or the error that ended it.
    fn finish_call(&mut self, index: usize) -> Event {
        let mut call = self.calls[index]
            .take()
            .expect("a ready slot is an unfinished call");
        let waker = self
            .waker
            .clone()
            .unwrap_or_else(|| Waker::from(FlagWaker::new()));
        let mut context = Context::from_waker(&waker);
        let result = match Pin::new(&mut call).poll(&mut context) {
            Poll::Ready(result) => result,
            // A registered call's poll always resolves (it awaits in the
            // kernel); keep the shape total anyway.
            Poll::Pending => Err(Error::Errno(-errno::EAGAIN)),
        };
        Event::Call { index, result }
    }
}
