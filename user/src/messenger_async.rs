//! Async Messenger API: `Future`s over the split `OP_CALL_BEGIN` /
//! `OP_CALL_AWAIT` transaction ops, a minimal executor, a `select`-style
//! multiplexer, and the declarative [`service!`] macro (issue #91).
//!
//! # Why the futures look like this
//!
//! LazyOS syscalls are blocking and the native surface has no "probe this
//! transaction" op (`OP_CALL_AWAIT` parks until the transaction is terminal),
//! so a leaf future here parks the calling task inside the kernel rather than
//! returning [`Poll::Pending`]. What still makes the API asynchronous is the
//! split at the start: `Call` (and [`Selector::call`]) registers the request
//! with `OP_CALL_BEGIN` *before* anything waits, so any number of requests are
//! in flight at once. The first wait collapses all replies that have already
//! been queued, which is what a `select` loop needs: completion latency is the
//! slowest call, not the sum.
//!
//! Two shapes are provided:
//!
//! * [`Call`] / [`Recv`] are `async` leaf futures; [`call`] and [`recv`] are
//!   `async fn` wrappers, and [`block_on`] runs a single future to completion.
//! * [`Selector`] is the multiplexer: queue calls and one-way receives, then
//!   call [`Selector::step`]. Each step drains ready one-way messages without
//!   blocking, then waits for the oldest in-flight call; [`Selector::cancel`]
//!   cancels a pending call with `OP_CANCEL`.
//!
//! A begun call that is never awaited leaves the task parked in the kernel
//! until some Messenger event wakes it (any reply, send, or cancel notifies the
//! shared wait queue). Always await or cancel every `Call` you begin.
//!
//! # Executor and waker
//!
//! There is no external crate behind this: [`Executor`] is a `Vec` of pinned
//! `Future<Output = ()>` values polled in round-robin order, and [`block_on`]
//! is a one-future version of the same loop. The waker (built on
//! `alloc::task::Wake`) records that a poll asked to be rescheduled; because
//! kernel wakeups do not route through wakers, a future that returns `Pending`
//! without parking in the kernel is simply polled again on the next pass. Our
//! leaf futures always park, so the loop never spins for them; user futures
//! that yield cooperatively burn the task's quantum until the next timer tick,
//! which is the documented trade-off of a no_std single-task executor.
//!
//! # `service!`
//!
//! [`service!`] wires a manifest-like block into a single-threaded mailbox
//! service: method ids dispatch to handler functions, an optional heartbeat is
//! answered and (when an observer endpoint is configured) emitted every `every`
//! served messages, and a graceful shutdown control message ends the loop.
//! There is no wall clock in userspace yet, so the periodic heartbeat cadence
//! is counted in served messages rather than time.
//!
//! ```ignore
//! service! {
//!     /// A tiny echo service.
//!     EchoService {
//!         concurrency: mailbox,
//!         interface: 0xE5C0_0001,
//!         methods {
//!             1 => ping,
//!             2 => echo,
//!         },
//!         heartbeat { method: 40, every: 8 },
//!         shutdown { method: 41 },
//!     }
//! }
//!
//! fn ping(_message: &Message) -> Result<Parcel> { /* ... */ }
//! fn echo(message: &Message) -> Result<Parcel> { /* ... */ }
//!
//! let service = EchoService::new(endpoint);
//! // `service.run()` blocks in `recv` until method 41 arrives.
//! ```

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::task::Wake;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, Waker};

use libmessenger::{flags, Encoder, Header, VERSION};

/// Parcel type the generated handlers speak; re-exported so macro expansions
/// do not need to name `libmessenger` themselves.
pub use libmessenger::Parcel;

use crate::messenger::{errno, Endpoint, Error, Message, Result};

/// TLV field id of the served-message counter in a health parcel.
pub const FIELD_SERVED: u16 = 1;
/// TLV field id of the emitted-heartbeat counter in a health parcel.
pub const FIELD_HEARTBEATS: u16 = 2;
/// TLV field id of the "shutting down" flag in a health parcel.
pub const FIELD_SHUTTING_DOWN: u16 = 3;

/// Error code of a parcel answering an unexpected interface id.
pub const CODE_UNKNOWN_INTERFACE: u32 = 1;
/// Error code of a parcel answering an undeclared method id.
pub const CODE_UNKNOWN_METHOD: u32 = 2;
/// Error code of a parcel answering a handler that returned an error.
pub const CODE_HANDLER: u32 = 3;

/// How a generated service processes messages.
///
/// Only the single-threaded mailbox exists today: one request is received,
/// dispatched, and answered before the next. The manifest's `concurrency:`
/// clause selects the variant, so a future model can be added without changing
/// call sites.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Concurrency {
    /// Single-threaded mailbox: `recv` -> dispatch -> reply.
    Mailbox,
}

/// Build a parcel carrying one structured `Error` field. Used for replies to
/// unknown interfaces/methods and for handler failures, so a synchronous
/// caller never hangs on a request the service refused.
pub fn error_parcel(interface_id: u64, method: u32, code: u32, message: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.error(1, code, message).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id,
            method,
            ..Header::default()
        },
        body: body.finish(),
        ..Parcel::default()
    })
}

/// The reply a generated service sends for an undeclared method id.
#[doc(hidden)]
pub fn unknown_method(message: &Message) -> Result<Parcel> {
    error_parcel(
        message.interface_id(),
        message.method(),
        CODE_UNKNOWN_METHOD,
        "the service does not declare that method",
    )
}

// ---------------------------------------------------------------------------
// Minimal executor and waker
// ---------------------------------------------------------------------------

/// Waker state: a single "someone asked to be rescheduled" flag.
struct FlagWaker {
    woken: AtomicBool,
}

impl FlagWaker {
    fn new() -> Arc<FlagWaker> {
        Arc::new(FlagWaker {
            woken: AtomicBool::new(false),
        })
    }

    /// Whether a `wake` landed since the flag was last cleared.
    fn take(&self) -> bool {
        self.woken.swap(false, Ordering::AcqRel)
    }
}

impl Wake for FlagWaker {
    fn wake(self: Arc<Self>) {
        self.woken.store(true, Ordering::Release);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Release);
    }
}

/// Run one future to completion on the current task.
///
/// This is the whole executor: poll, and on `Pending` poll again. The leaf
/// futures in this module never return `Pending` (they park the task in the
/// kernel), so the loop makes progress without a scheduler; a composed future
/// that yields is re-polled immediately, which is the documented trade-off
/// described in the module docs.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let flag = FlagWaker::new();
    let waker = Waker::from(flag);
    let mut context = Context::from_waker(&waker);
    let mut future = core::pin::pin!(future);
    loop {
        match Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}

/// A single-task executor: spawn `Future<Output = ()>` jobs and poll them
/// round-robin until every job finishes.
///
/// Jobs are expected to block in the kernel when they have nothing to do; see
/// [`block_on`] for the motivation. [`Executor::poll_ready`] is a single pass,
/// [`Executor::run`] drains the queue.
#[derive(Default)]
pub struct Executor {
    tasks: Vec<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Executor {
    /// An executor with no jobs.
    pub fn new() -> Executor {
        Executor { tasks: Vec::new() }
    }

    /// Queue a job. `run` polls it until it resolves.
    pub fn spawn<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.tasks.push(Box::pin(future));
    }

    /// Number of unfinished jobs.
    pub fn pending(&self) -> usize {
        self.tasks.len()
    }

    /// Whether no jobs are queued.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Poll every job once, removing the ones that completed. Returns how many
    /// jobs resolved during this pass.
    pub fn poll_ready(&mut self) -> usize {
        let flag = FlagWaker::new();
        let waker = Waker::from(flag.clone());
        let mut context = Context::from_waker(&waker);
        let mut completed = 0;
        let mut index = 0;
        while index < self.tasks.len() {
            match self.tasks[index].as_mut().poll(&mut context) {
                Poll::Ready(()) => {
                    drop(self.tasks.swap_remove(index));
                    completed += 1;
                }
                Poll::Pending => index += 1,
            }
        }
        let _ = flag.take();
        completed
    }

    /// Poll the queue until every job has completed.
    pub fn run(&mut self) {
        while !self.tasks.is_empty() {
            self.poll_ready();
        }
    }
}

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
/// form used by [`Selector`].
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

// ---------------------------------------------------------------------------
// `service!` runtime
// ---------------------------------------------------------------------------

/// The runtime half of [`service!`]: a single-threaded mailbox that dispatches
/// declared methods, answers and emits heartbeats, and honours a graceful
/// shutdown method.
///
/// `Mailbox` is meant to be embedded by the macro, but it is public so a
/// hand-written dispatcher can reuse the same health/shutdown plumbing.
pub struct Mailbox {
    endpoint: Endpoint,
    interface_id: u64,
    heartbeat_method: u32,
    heartbeat_every: u64,
    heartbeat_out: Option<Endpoint>,
    shutdown_method: u32,
    served: u64,
    since_heartbeat: u64,
    heartbeats: u64,
    shutting_down: bool,
}

impl Mailbox {
    /// Wrap the endpoint the service receives on. Method id `0` disables the
    /// heartbeat or shutdown clause (`every: 0` disables periodic emission).
    pub const fn new(
        endpoint: Endpoint,
        interface_id: u64,
        heartbeat_method: u32,
        heartbeat_every: u64,
        shutdown_method: u32,
    ) -> Mailbox {
        Mailbox {
            endpoint,
            interface_id,
            heartbeat_method,
            heartbeat_every,
            heartbeat_out: None,
            shutdown_method,
            served: 0,
            since_heartbeat: 0,
            heartbeats: 0,
            shutting_down: false,
        }
    }

    /// The endpoint this mailbox receives on.
    pub const fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// The endpoint future heartbeats are sent on; its peer is the observer.
    pub fn set_heartbeat_endpoint(&mut self, endpoint: Option<Endpoint>) {
        self.heartbeat_out = endpoint;
    }

    /// Messages dispatched so far.
    pub const fn served(&self) -> u64 {
        self.served
    }

    /// Whether a graceful shutdown was requested.
    pub const fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    /// Dispatch one message through `handler` for ordinary methods.
    ///
    /// Returns `Ok(false)` after the shutdown method ran, so [`Mailbox::run`]
    /// can stop. Interface mismatches and unknown methods are answered with an
    /// error parcel instead of dropped, so a synchronous caller never hangs.
    pub fn dispatch(
        &mut self,
        message: &Message,
        handler: &mut dyn FnMut(&Message) -> Result<Parcel>,
    ) -> Result<bool> {
        self.served = self.served.saturating_add(1);
        self.since_heartbeat = self.since_heartbeat.saturating_add(1);

        if message.interface_id() != self.interface_id {
            let reply = error_parcel(
                self.interface_id,
                message.method(),
                CODE_UNKNOWN_INTERFACE,
                "unknown interface",
            );
            self.respond(message, reply)?;
            return Ok(true);
        }

        if self.heartbeat_method != 0 && message.method() == self.heartbeat_method {
            let reply = self.health_parcel(flags::SYNC, false)?;
            self.respond(message, Ok(reply))?;
        } else if self.shutdown_method != 0 && message.method() == self.shutdown_method {
            let reply = self.health_parcel(flags::SYNC, true)?;
            self.respond(message, Ok(reply))?;
            self.shutting_down = true;
            // Retained-ish: the last heartbeat an observer sees carries the
            // shutdown flag, so a controller does not need a separate goodbye.
            self.emit_heartbeat(true)?;
            return Ok(false);
        } else {
            let reply = handler(message);
            self.respond(message, reply)?;
        }

        if self.heartbeat_every != 0 && self.since_heartbeat >= self.heartbeat_every {
            self.emit_heartbeat(false)?;
        }
        Ok(true)
    }

    /// Receive one message and dispatch it. Blocks in `recv` when idle.
    pub fn serve_once(
        &mut self,
        handler: &mut dyn FnMut(&Message) -> Result<Parcel>,
    ) -> Result<bool> {
        let message = self.endpoint.recv(None)?;
        self.dispatch(&message, handler)
    }

    /// [`Mailbox::serve_once`] until the shutdown method runs.
    pub fn run(&mut self, handler: &mut dyn FnMut(&Message) -> Result<Parcel>) -> Result<()> {
        while self.serve_once(handler)? {}
        Ok(())
    }

    /// Send one heartbeat to the observer now; a no-op without an observer.
    pub fn heartbeat(&mut self) -> Result<()> {
        self.emit_heartbeat(false)
    }

    /// Build the health parcel: served count, emitted-heartbeat count, and the
    /// shutdown flag. Clients decode the [`FIELD_SERVED`] and
    /// [`FIELD_SHUTTING_DOWN`] fields (see the `async_service` example).
    pub fn health_parcel(&self, flags: u16, shutting_down: bool) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(FIELD_SERVED, self.served).map_err(Error::Parcel)?;
        body.u64(FIELD_HEARTBEATS, self.heartbeats)
            .map_err(Error::Parcel)?;
        body.bool(FIELD_SHUTTING_DOWN, shutting_down)
            .map_err(Error::Parcel)?;
        Ok(Parcel {
            header: Header {
                version: VERSION,
                flags,
                interface_id: self.interface_id,
                method: self.heartbeat_method,
                ..Header::default()
            },
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Send a heartbeat on the observer endpoint and bump the counters.
    fn emit_heartbeat(&mut self, shutting_down: bool) -> Result<()> {
        let Some(out) = self.heartbeat_out else {
            return Ok(());
        };
        let parcel = self.health_parcel(flags::ONE_WAY, shutting_down)?;
        out.send(&parcel)?;
        self.heartbeats = self.heartbeats.saturating_add(1);
        self.since_heartbeat = 0;
        Ok(())
    }

    /// Reply on `message`'s transaction when it was a call; handler errors
    /// become error parcels so the caller always gets an answer.
    fn respond(&self, message: &Message, reply: Result<Parcel>) -> Result<()> {
        let parcel = match reply {
            Ok(parcel) => parcel,
            Err(error) => error_parcel(
                self.interface_id,
                message.method(),
                CODE_HANDLER,
                error.message(),
            )?,
        };
        match message.txn {
            Some(txn) => self.endpoint.reply(txn, &parcel),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// `service!` macro
// ---------------------------------------------------------------------------

/// Declare a single-threaded mailbox service from a manifest-like block.
///
/// The macro generates a `pub struct` named by the manifest with a `new`
/// constructor, `dispatch`/`serve_once`/`run` loops, and `heartbeat`. Method
/// ids map to free handler functions in scope with the signature
/// `fn(&Message) -> Result<Parcel>`.
///
/// * `concurrency: mailbox` is the only model today (a second variant fails to
///   compile through the internal helper, by design).
/// * `heartbeat { method, every }` answers `method` with a health parcel and,
///   when [`with_heartbeat_endpoint`] is configured, emits a one-way heartbeat
///   every `every` served messages. `method: 0`/`every: 0` disable it; there is
///   no wall clock in userspace yet, so the cadence counts served messages.
/// * `shutdown { method }` replies to the control message, then stops the loop
///   (`run` returns `Ok(())`). `method: 0` disables it.
///
/// The heartbeat and shutdown clauses may be omitted entirely, which is the
/// same as `method: 0, every: 0`.
///
/// ```ignore
/// service! {
///     /// A tiny echo service.
///     EchoService {
///         concurrency: mailbox,
///         interface: 0xE5C0_0001,
///         methods {
///             1 => ping,
///             2 => echo,
///         },
///         heartbeat { method: 40, every: 8 },
///         shutdown { method: 41 },
///     }
/// }
///
/// fn ping(_message: &Message) -> Result<Parcel> {
///     // Build a reply parcel for method 1.
/// }
///
/// fn echo(message: &Message) -> Result<Parcel> {
///     // Echo the request body back.
/// }
///
/// let mut service = EchoService::new(endpoint);
/// service.run()?; // returns after method 41 arrives
/// ```
#[macro_export]
macro_rules! service {
    (
        $(#[$meta:meta])*
        $name:ident {
            concurrency: $concurrency:ident,
            interface: $interface:expr,
            methods { $($method:expr => $handler:ident),* $(,)? },
            heartbeat { method: $heartbeat:expr, every: $every:expr $(,)? },
            shutdown { method: $shutdown:expr $(,)? } $(,)?
        }
    ) => {
        $crate::__service_impl! {
            $(#[$meta])*
            $name {
                concurrency: $concurrency,
                interface: $interface,
                methods { $($method => $handler),* },
                heartbeat: $heartbeat,
                every: $every,
                shutdown: $shutdown,
            }
        }
    };
    (
        $(#[$meta:meta])*
        $name:ident {
            concurrency: $concurrency:ident,
            interface: $interface:expr,
            methods { $($method:expr => $handler:ident),* $(,)? } $(,)?
        }
    ) => {
        $crate::service! {
            $(#[$meta])*
            $name {
                concurrency: $concurrency,
                interface: $interface,
                methods { $($method => $handler),* },
                heartbeat { method: 0, every: 0 },
                shutdown { method: 0 },
            }
        }
    };
}

/// Concurrency-model lookup for [`service!`]; an unsupported ident is a
/// compile error with no matching arm. Internal.
#[macro_export]
#[doc(hidden)]
macro_rules! __service_concurrency {
    (mailbox) => {
        $crate::messenger_async::Concurrency::Mailbox
    };
}

/// Expansion body for [`service!`]. Internal; call `service!` instead.
#[macro_export]
#[doc(hidden)]
macro_rules! __service_impl {
    (
        $(#[$meta:meta])*
        $name:ident {
            concurrency: $concurrency:ident,
            interface: $interface:expr,
            methods { $($method:expr => $handler:ident),* $(,)? },
            heartbeat: $heartbeat:expr,
            every: $every:expr,
            shutdown: $shutdown:expr $(,)?
        }
    ) => {
        $(#[$meta])*
        pub struct $name {
            mailbox: $crate::messenger_async::Mailbox,
        }

        impl $name {
            /// Interface id every dispatched message must carry.
            pub const INTERFACE_ID: u64 = $interface;
            /// Declared concurrency model.
            pub const CONCURRENCY: $crate::messenger_async::Concurrency =
                $crate::__service_concurrency!($concurrency);
            /// Heartbeat/health method id; `0` disables the clause.
            pub const HEARTBEAT_METHOD: u32 = $heartbeat;
            /// Emit a heartbeat every this many served messages; `0` disables
            /// periodic emission while health requests stay answerable.
            pub const HEARTBEAT_EVERY: u64 = $every;
            /// Graceful-shutdown control method id; `0` disables it.
            pub const SHUTDOWN_METHOD: u32 = $shutdown;

            /// Wrap the endpoint the service receives on.
            pub const fn new(endpoint: $crate::messenger::Endpoint) -> Self {
                Self {
                    mailbox: $crate::messenger_async::Mailbox::new(
                        endpoint,
                        Self::INTERFACE_ID,
                        Self::HEARTBEAT_METHOD,
                        Self::HEARTBEAT_EVERY,
                        Self::SHUTDOWN_METHOD,
                    ),
                }
            }

            /// Set the endpoint future heartbeats are sent on (its peer is the
            /// observer). Without one, heartbeat requests are still answered.
            pub fn with_heartbeat_endpoint(
                mut self,
                endpoint: ::core::option::Option<$crate::messenger::Endpoint>,
            ) -> Self {
                self.mailbox.set_heartbeat_endpoint(endpoint);
                self
            }

            /// The endpoint this service receives on.
            pub const fn endpoint(&self) -> $crate::messenger::Endpoint {
                self.mailbox.endpoint()
            }

            /// Messages dispatched so far.
            pub const fn served(&self) -> u64 {
                self.mailbox.served()
            }

            /// Whether a graceful shutdown was requested.
            pub const fn is_shutting_down(&self) -> bool {
                self.mailbox.is_shutting_down()
            }

            /// Dispatch one message to the declared handler and reply.
            /// Returns `Ok(false)` once shutdown was requested.
            pub fn dispatch(
                &mut self,
                message: &$crate::messenger::Message,
            ) -> $crate::messenger::Result<bool> {
                self.mailbox.dispatch(message, &mut Self::handler())
            }

            /// Receive one message and dispatch it; blocks while idle.
            pub fn serve_once(&mut self) -> $crate::messenger::Result<bool> {
                self.mailbox.serve_once(&mut Self::handler())
            }

            /// Serve until the shutdown method runs.
            pub fn run(&mut self) -> $crate::messenger::Result<()> {
                self.mailbox.run(&mut Self::handler())
            }

            /// Send one heartbeat on the observer endpoint now; a no-op
            /// without an observer.
            pub fn heartbeat(&mut self) -> $crate::messenger::Result<()> {
                self.mailbox.heartbeat()
            }

            /// The declared method table as a handler closure.
            fn handler() -> impl FnMut(
                &$crate::messenger::Message,
            ) -> $crate::messenger::Result<$crate::messenger_async::Parcel>
            {
                |message: &$crate::messenger::Message| match message.method() {
                    $($method => $handler(message),)*
                    _ => $crate::messenger_async::unknown_method(message),
                }
            }
        }
    };
}
