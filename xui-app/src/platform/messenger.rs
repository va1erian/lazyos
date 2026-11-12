//! How an xui program holds a Messenger client: one generated client per
//! interface per thread, over the std transport with the UI-thread stall trace.
//!
//! The registry name, header ids, deadlines and error mapping all come from the
//! interface's `.midl` (`messenger_generated::<interface>::Client`); nothing
//! here frames a request. [`client`] connects a generated client on first use
//! and keeps it for the life of the thread: a resolved handle is released when
//! its [`Connection`] drops (the kernel opens every resolver onto the
//! service's single endpoint, and `release` closes a side only when no other
//! handle names it), so the client is kept, not rebuilt per call. A call that
//! finds the peer gone drops the handle, resolves a restarted service once and
//! retries once ([`Connection`]).

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lazyos_sys::client::Std;
use libmessenger::client::{is_peer_gone, Connection, Errno, Handle, Transport};
pub use libmessenger::client::{CallError, Deadline};
use libmessenger::Parcel;

use crate::sys;

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

/// What one thread keeps: a generated client by its type (and, for a member
/// of a service family, the instance name).
type Held = (TypeId, Option<String>, Rc<dyn Any>);

thread_local! {
    /// The clients of this thread.
    static CLIENTS: RefCell<Vec<Held>> = const { RefCell::new(Vec::new()) };
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

fn held<C: 'static>(instance: Option<&str>) -> Option<Rc<C>> {
    CLIENTS.with(|clients| {
        let clients = clients.borrow();
        let (_, _, any) = clients
            .iter()
            .find(|(id, name, _)| *id == TypeId::of::<C>() && name.as_deref() == instance)?;
        any.clone().downcast::<C>().ok()
    })
}

fn keep<C: 'static>(instance: Option<&str>, made: C) -> Rc<C> {
    let made = Rc::new(made);
    CLIENTS.with(|clients| {
        clients.borrow_mut().push((
            TypeId::of::<C>(),
            instance.map(str::to_owned),
            made.clone() as Rc<dyn Any>,
        ));
    });
    made
}

/// This thread's generated client `C`, made on first use with `connect` and
/// kept after: `wire::Client::connect` waits for the service to register (the
/// std runtime's connect bound), `wire::Client::try_connect` makes one attempt,
/// for a caller whose service being absent has a cheap fallback (the
/// clipboard, the notifications) and which must never wait for it. A failed
/// connect keeps nothing, so the next call tries again. The client is shared
/// by everything on the thread, so a name resolves once.
pub fn client<C: 'static, E>(
    connect: fn(Observed) -> Result<C, CallError<E>>,
) -> Result<Rc<C>, CallError<E>> {
    if let Some(known) = held::<C>(None) {
        return Ok(known);
    }
    Ok(keep(None, connect(Observed::default())?))
}

/// This thread's client `C` of the family member `instance` (the interface's
/// `SERVICE_INSTANCE_PREFIX` and `instance`, such as `os.lazy.net.nic/eth0`),
/// made with `wire::Client::from_connection` on first use. Nothing resolves
/// until its first call.
pub fn instance_client<C: 'static>(
    prefix: &str,
    instance: &str,
    from_connection: fn(Conn) -> C,
) -> Rc<C> {
    let name = format!("{prefix}{instance}");
    if let Some(known) = held::<C>(Some(&name)) {
        return known;
    }
    let conn = Connection::named(Observed::default(), name.clone());
    keep(Some(&name), from_connection(conn))
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
/// The generated clients have no asynchronous form yet (`docs/client-api-plan.md`
/// 3.2: `_begin` waits for an asynchronous transport call), so this is the one
/// place an xui program frames a request: `body` is what the interface's
/// generated `encode_*_args` returned, and `connection` names the generated
/// `client`'s [`Connection`] (`wire::Client::connection`).
///
/// The begin's own errors are transport failures; they arrive in `on_done`'s
/// result, and a dead peer evicts the cached handle, as for a synchronous call.
pub fn call_async<C: 'static>(
    client: &Rc<C>,
    connection: fn(&C) -> &Conn,
    interface: u64,
    method: u32,
    body: Vec<u8>,
    until: Deadline,
    on_done: Box<dyn FnOnce(Result<Parcel, CallError>)>,
) -> Result<(), Errno> {
    let request = Conn::request(
        interface,
        method,
        libmessenger::flags::ALLOW_NESTED,
        body,
        Vec::new(),
    );
    let mut bytes = Vec::new();
    request.encode(&mut bytes).map_err(|_| Errno::INVAL)?;
    let handle = connection(client).connect()?;
    let owner = client.clone();
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
                        connection(&owner).evict();
                    }
                    Err(CallError::Transport(errno))
                }
            })
        }),
    )
    .map_err(Errno::from_syscall)
    .inspect_err(|&errno| {
        if is_peer_gone(errno) {
            connection(client).evict();
        }
    })
}
