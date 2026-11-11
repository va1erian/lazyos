//! A tiny synchronous Messenger client for the platform services (mimed,
//! clipboardd) the migrated apps call.
//!
//! The display client ([`crate::display`]) has its own endpoint because it also
//! receives one-way events; the platform services are request/reply only. The
//! resolve, handle cache, reconnect-once and error-field decoding are
//! [`libmessenger::client::Connection`] over the std transport
//! ([`lazyos_sys::client::Std`]), shared by every runtime; this module keeps
//! one `Connection` per service name per thread and adds the UI-thread stall
//! trace. It never writes wire fields by hand: bodies are built with the
//! generated `messenger-generated` stubs.
//!
//! A resolved handle is released when its connection is dropped (the kernel
//! opens every resolver onto the service's single endpoint, and `release`
//! closes a side only when no other handle names it), so a connection is
//! kept for the life of the thread: it is shared by every [`Service`] of
//! that name. A call that finds the peer gone drops the handle, resolves a
//! restarted service once and retries once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lazyos_sys::client::Std;
use libmessenger::client::{is_peer_gone, Connection, Errno, Handle, Transport};
pub use libmessenger::client::{CallError, Deadline};
use libmessenger::{Object, Parcel};

use crate::sys::{self, errno};

/// The std transport with the UI-thread stall trace: a resolve or a call
/// that holds the thread longer than [`crate::stall::SLOW_NS`] leaves a
/// `UI:STALL` line naming what it waited on.
#[derive(Clone, Copy, Debug, Default)]
pub struct Observed(Std);

impl Transport for Observed {
    fn connect_bound(&self) -> Deadline {
        self.0.connect_bound()
    }

    fn call_flags(&self) -> u16 {
        self.0.call_flags()
    }

    fn resolve(&self, name: &str, until: Deadline) -> Result<Handle, Errno> {
        let began = crate::stall::start();
        let result = self.0.resolve(name, until);
        crate::stall::finish(began, crate::stall::SLOW_NS, "connect", || {
            format!("name={name} ok={}", result.is_ok())
        });
        result
    }

    fn call(&self, handle: Handle, request: &Parcel, until: Deadline) -> Result<Parcel, Errno> {
        let began = crate::stall::start();
        let result = self.0.call(handle, request, until);
        crate::stall::finish(began, crate::stall::SLOW_NS, "call", || {
            format!(
                "iface={:#x} method={} deadline={until:?} result={}",
                request.header.interface_id,
                request.header.method,
                match &result {
                    Ok(_) => String::from("ok"),
                    Err(errno) => errno.to_syscall().to_string(),
                }
            )
        });
        result
    }

    fn send(&self, handle: Handle, request: &Parcel) -> Result<(), Errno> {
        self.0.send(handle, request)
    }

    fn release(&self, handle: Handle) {
        self.0.release(handle);
    }
}

/// A connection over the transport every xui program uses.
pub type Conn = Connection<Observed>;

thread_local! {
    /// The connections of this thread, one per service name.
    static SERVICES: RefCell<Vec<Rc<Conn>>> = const { RefCell::new(Vec::new()) };
    /// Threads the tree's worker closures run on (`mark_worker`): a
    /// [`Deadline::Never`] call is only legitimate on them, and the UI-thread
    /// paths they share code with consult [`worker_thread`] to keep the
    /// bounded shape.
    static WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Mark the current thread as one of the tree's worker threads (no UI
/// loop runs here): a [`Deadline::Never`] call is only legitimate on them.
pub fn mark_worker() {
    WORKER.with(|worker| worker.set(true));
}

/// Whether the calling thread was marked with [`mark_worker`]: false on
/// every UI thread, which is what keeps the platform client's per-call
/// budgets the default.
pub fn worker_thread() -> bool {
    WORKER.with(Cell::get)
}

/// This thread's connection to `name`, made on first use: what every
/// client of a service in this process (the display, the input grab, the
/// audio mixer, [`Service`]) shares, so a name resolves once.
pub fn shared(name: &'static str) -> Rc<Conn> {
    SERVICES.with(|services| {
        let mut services = services.borrow_mut();
        if let Some(known) = services.iter().find(|conn| conn.name() == name) {
            return known.clone();
        }
        let conn = Rc::new(Connection::new(Observed::default(), name));
        services.push(conn.clone());
        conn
    })
}

/// A Messenger service by registry name.
pub struct Service {
    conn: Rc<Conn>,
}

impl Service {
    /// The service `name`, waiting for it to register (services start
    /// alongside the app, so a cold boot can race) for the std runtime's
    /// connect bound; the negative errno when it never does.
    pub fn connect(name: &'static str) -> Result<Service, i64> {
        let conn = shared(name);
        conn.connect().map_err(Errno::to_syscall)?;
        Ok(Service { conn })
    }

    /// The service `name` if it is registered now, without waiting, so a
    /// caller whose absence has a cheap fallback (the clipboard) never blocks.
    pub fn try_connect(name: &'static str) -> Option<Service> {
        let conn = shared(name);
        conn.try_connect().ok()?;
        Some(Service { conn })
    }

    /// One synchronous call, answered within `until`: a call whose UI must
    /// not freeze behind a stalled or missing service. [`Deadline::Never`]
    /// is for worker threads only (`tools/xui/check_call_bounds.py` keeps it
    /// off the UI thread). A structured error field in the reply is
    /// [`CallError::Remote`], with the service's friendly text.
    pub fn call(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        until: Deadline,
    ) -> Result<Parcel, CallError> {
        self.call_moving(interface, method, body, Vec::new(), until)
    }

    /// [`Service::call`] whose request carries `objects` (what the generated
    /// `encode_*_args` returned beside the body: the tray's `Set` moves its
    /// event channel, `init`'s `Watch` likewise).
    pub fn call_moving(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
        until: Deadline,
    ) -> Result<Parcel, CallError> {
        self.conn.call(interface, method, body, objects, until)
    }

    /// Begin a call and keep the window paintable while the service works
    /// (issue #725's UI integration): the request goes out now and `on_done`
    /// carries the reply, on the UI thread, at the tick the kernel's
    /// `WAIT_CALLS` doorbell gave the loop. `until` [`Deadline::Never`] waits
    /// for ever: the caller who began it is not parked, so only the app's own
    /// patience for a still-open call is a bound. The lane runs on the thread
    /// the backend's loop runs on (the only one whose `tick` drains it); a
    /// call begun elsewhere waits for a drain that thread never makes, so do
    /// not.
    ///
    /// The begin's own errors are transport failures; they arrive in
    /// `on_done`'s result, and a dead peer evicts the cached handle, as for
    /// [`Service::call`].
    pub fn call_async(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        until: Deadline,
        on_done: Box<dyn FnOnce(Result<Parcel, CallError>)>,
    ) -> Result<(), i64> {
        let request = Conn::request(
            interface,
            method,
            libmessenger::flags::ALLOW_NESTED,
            body,
            Vec::new(),
        );
        let mut bytes = Vec::new();
        if request.encode(&mut bytes).is_err() {
            return Err(-errno::EINVAL);
        }
        let handle = self.conn.connect().map_err(Errno::to_syscall)?;
        let conn = self.conn.clone();
        let deadline = until.absolute_ticks(sys::clock_ticks());
        crate::backend::calls::begin(
            handle,
            &bytes,
            deadline,
            Box::new(move |result| {
                on_done(match result {
                    Ok(reply) => Conn::check(reply),
                    Err(code) => {
                        let errno = Errno::from_syscall(code);
                        if is_peer_gone(errno) {
                            conn.evict();
                        }
                        Err(CallError::Transport(errno))
                    }
                })
            }),
        )
        .inspect_err(|&code| {
            if is_peer_gone(Errno::from_syscall(code)) {
                self.conn.evict();
            }
        })
    }

    /// A oneway message (`flags::ONE_WAY`): input and slot returns that
    /// have no answer. Never parks. A dead peer evicts the cached handle, as
    /// for a call.
    pub fn send_oneway(&self, interface: u64, method: u32, body: Vec<u8>) -> Result<(), i64> {
        self.conn
            .send(interface, method, body, Vec::new())
            .map_err(|error| error.code())
    }
}
