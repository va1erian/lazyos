//! A tiny synchronous Messenger client for the platform services (mimed,
//! clipboardd) the migrated apps call.
//!
//! The display client ([`crate::display`]) has its own endpoint because it also
//! receives one-way events; the platform services are request/reply only, so
//! this helper keeps the resolve/call boilerplate in one place. It never
//! writes wire fields by hand: bodies are built with the generated
//! `messenger-generated` stubs and only the envelope is assembled here.
//!
//! A resolved handle must stay open for the life of the task: the kernel opens
//! every resolver onto the service's single endpoint, so closing one resolved
//! handle is peer death for the service (`kernel/src/ipc/registry.rs`,
//! `resolve`). Endpoints are therefore cached per name and never closed; a
//! call that finds the peer gone evicts the entry so the next call re-resolves
//! a restarted service.

use std::cell::RefCell;

use libmessenger::{Decoder, Header, Kind, Parcel, VERSION};

use crate::sys::{self, errno};

/// PIT ticks to wait for a service's name to appear (10 s at 100 Hz).
const CONNECT_TICKS: u64 = 1000;

/// The reply buffer a platform call offers. `confd`'s `List` of a real tree or
/// a 4 KiB value does not fit the old 4 KiB buffer (the kernel answers `E2BIG`
/// when the reply overflows it), so this is sized well above any reply the
/// platform services send today; the kernel itself allows parcels up to
/// `libmessenger::MAX_PARCEL_BYTES` (1 MiB).
const REPLY_BUF: usize = 64 * 1024;

/// One resolved endpoint, kept open for the process lifetime.
struct Cached {
    name: &'static str,
    endpoint: u64,
}

thread_local! {
    /// Endpoints resolved by this task, one per service name. Never closed, so
    /// the service's endpoint outlives every individual call.
    static SERVICES: RefCell<Vec<Cached>> = const { RefCell::new(Vec::new()) };
}

fn cached(name: &str) -> Option<u64> {
    SERVICES.with(|services| {
        services
            .borrow()
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.endpoint)
    })
}

fn remember(name: &'static str, endpoint: u64) {
    SERVICES.with(|services| services.borrow_mut().push(Cached { name, endpoint }));
}

fn forget(name: &str) {
    SERVICES.with(|services| services.borrow_mut().retain(|entry| entry.name != name));
}

/// A resolved service endpoint.
pub struct Service {
    name: &'static str,
    endpoint: u64,
}

impl Service {
    /// Resolve `name` into this task, retrying until the deadline (services
    /// start alongside the app, so a cold boot can race). A previously
    /// resolved endpoint is reused.
    pub fn connect(name: &'static str) -> Result<Service, i64> {
        if let Some(endpoint) = cached(name) {
            return Ok(Service { name, endpoint });
        }
        let deadline = sys::clock_ticks().saturating_add(CONNECT_TICKS);
        loop {
            match sys::msg_resolve(name) {
                Ok(endpoint) => {
                    remember(name, endpoint);
                    return Ok(Service { name, endpoint });
                }
                Err(code) => {
                    if sys::clock_ticks() >= deadline {
                        return Err(code);
                    }
                    sys::sleep_millis(10);
                }
            }
        }
    }

    /// Resolve `name` once, without an endpoint already cached. Returns `None`
    /// immediately when the service is not registered, so a caller whose
    /// absence has a cheap fallback (the clipboard) never blocks.
    pub fn try_connect(name: &'static str) -> Option<Service> {
        if let Some(endpoint) = cached(name) {
            return Some(Service { name, endpoint });
        }
        let endpoint = sys::msg_resolve(name).ok()?;
        remember(name, endpoint);
        Some(Service { name, endpoint })
    }

    /// One synchronous call on the service; a structured error field `error_id`
    /// in the reply becomes its negative errno.
    ///
    /// A dead peer (`-EPIPE`, or `-ENOENT` from a reaped registration) evicts
    /// the cached endpoint so a later call resolves the service again.
    pub fn call(
        &self,
        interface: u64,
        method: u32,
        error_id: u16,
        body: Vec<u8>,
    ) -> Result<Parcel, i64> {
        self.call_detailed(interface, method, error_id, body)
            .map_err(|error| error.code)
    }

    /// [`Service::call`] keeping the service's friendly error text, for a
    /// caller that shows the refusal to the user (the package installer shows
    /// why `pkgd` declined).
    pub fn call_detailed(
        &self,
        interface: u64,
        method: u32,
        error_id: u16,
        body: Vec<u8>,
    ) -> Result<Parcel, CallError> {
        self.call_at(interface, method, error_id, body, 0)
    }

    /// [`Service::call`] that gives up with `-ETIMEDOUT` after `ticks` PIT
    /// ticks, for a caller on the UI thread that must not wait forever on a
    /// peer that accepted the request but never answers (the Task Manager's
    /// once-a-second snapshots).
    pub fn call_within(
        &self,
        interface: u64,
        method: u32,
        error_id: u16,
        body: Vec<u8>,
        ticks: u64,
    ) -> Result<Parcel, i64> {
        // An absolute deadline, never 0 ("forever") or `EXPIRED_DEADLINE` (a
        // poll the callee may answer only during its own service turn).
        let deadline = sys::clock_ticks()
            .saturating_add(ticks.max(1))
            .max(sys::EXPIRED_DEADLINE + 1);
        self.call_at(interface, method, error_id, body, deadline)
            .map_err(|error| error.code)
    }

    /// One call bounded by `deadline` (an absolute PIT tick; `0` waits
    /// forever).
    fn call_at(
        &self,
        interface: u64,
        method: u32,
        error_id: u16,
        body: Vec<u8>,
        deadline: u64,
    ) -> Result<Parcel, CallError> {
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                // The app's own event receive must not trip the kernel's
                // per-channel cycle check while a call is in flight.
                flags: libmessenger::flags::ALLOW_NESTED,
                interface_id: interface,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = vec![0u8; REPLY_BUF];
        let reply = match sys::msg_call(self.endpoint, &parcel, &mut buf, deadline) {
            Ok(reply) => reply,
            Err(code) => {
                if code == -errno::EPIPE || code == -errno::ENOENT {
                    forget(self.name);
                }
                return Err(CallError {
                    code,
                    message: String::new(),
                });
            }
        };
        match error_field(&reply, error_id) {
            Some(error) => Err(error),
            None => Ok(reply),
        }
    }
}

/// A refused or failed call: the negative errno and, when the service sent
/// one, its friendly text (empty for a transport failure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallError {
    pub code: i64,
    pub message: String,
}

/// The structured error in a reply, when a service refused the call.
fn error_field(parcel: &Parcel, error_id: u16) -> Option<CallError> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == error_id {
            let (code, message) = field.error_parts().ok()?;
            return Some(CallError {
                code: -(code as i64),
                message: message.to_owned(),
            });
        }
    }
    None
}
