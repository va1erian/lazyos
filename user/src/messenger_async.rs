//! Async Messenger API: `Future`s over the split `OP_CALL_BEGIN` /
//! `OP_CALL_AWAIT` transaction ops, a minimal executor, a `select`-style
//! multiplexer, and the declarative [`service!`] macro (issue #91).
//!
//! # Why the futures look like this
//!
//! LazyOS syscalls are blocking and the native surface has no "probe this
//! transaction" op (`OP_CALL_AWAIT` parks until the transaction is terminal),
//! so a leaf future here parks the calling task inside the kernel rather than
//! returning [`Poll::Pending`](core::task::Poll::Pending). What still makes the API asynchronous is the
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
//!   blocking, then waits for the oldest in-flight call, or, with no calls in
//!   flight, parks on every queued receive at once (`wait_any`);
//!   [`Selector::cancel`] cancels a pending call with `OP_CANCEL`.
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
//! without parking in the kernel is polled again on the next pass. Our leaf
//! futures always park, so the loop never spins for them; a future that wakes
//! itself and yields is polled again at once, and a pass that leaves work
//! pending with no wake parks the task for a millisecond before the next
//! (P7.4) instead of burning its quantum.
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

use libmessenger::{Encoder, Header, VERSION};

/// Parcel type the generated handlers speak; re-exported so macro expansions
/// do not need to name `libmessenger` themselves.
pub use libmessenger::Parcel;

use crate::messenger::{Error, Message, Result};

mod executor;
mod future;
mod mailbox;
mod selector;

pub use executor::{block_on, Executor};
pub use future::{call, recv, Call, Recv};
pub use mailbox::Mailbox;
pub use selector::{Event, Selector};

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
