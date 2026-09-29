//! A tiny synchronous Messenger client for the platform services (mimed,
//! clipboardd) the migrated apps call.
//!
//! The display client ([`crate::display`]) has its own endpoint because it also
//! receives one-way events; the platform services are request/reply only, so
//! this helper keeps the resolve/call/close boilerplate in one place. It never
//! writes wire fields by hand: bodies are built with the generated
//! `messenger-generated` stubs and only the envelope is assembled here.

use libmessenger::{Decoder, Header, Kind, Parcel, VERSION};

use crate::sys::{self, msg_op, MsgArgs, MsgResult};

/// PIT ticks to wait for a service's name to appear (10 s at 100 Hz).
const CONNECT_TICKS: u64 = 1000;

/// A resolved service endpoint.
pub struct Service {
    endpoint: u64,
}

impl Service {
    /// Resolve `name` into this task, retrying until the deadline (services
    /// start alongside the app, so a cold boot can race).
    pub fn connect(name: &str) -> Result<Service, i64> {
        let deadline = sys::clock_ticks().saturating_add(CONNECT_TICKS);
        loop {
            match sys::msg_resolve(name) {
                Ok(endpoint) => return Ok(Service { endpoint }),
                Err(code) => {
                    if sys::clock_ticks() >= deadline {
                        return Err(code);
                    }
                    sys::sleep_millis(10);
                }
            }
        }
    }

    /// Resolve `name` once, without waiting. Returns `None` immediately when
    /// the service is not registered, so a caller whose absence has a cheap
    /// fallback (the clipboard) never blocks.
    pub fn try_connect(name: &str) -> Option<Service> {
        sys::msg_resolve(name)
            .ok()
            .map(|endpoint| Service { endpoint })
    }

    /// One synchronous call on the service; a structured error field `error_id`
    /// in the reply becomes its negative errno.
    pub fn call(
        &self,
        interface: u64,
        method: u32,
        error_id: u16,
        body: Vec<u8>,
    ) -> Result<Parcel, i64> {
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
        let mut buf = [0u8; 4096];
        let reply = sys::msg_call(self.endpoint, &parcel, &mut buf, 0)?;
        match error_field(&reply, error_id) {
            Some(code) => Err(code),
            None => Ok(reply),
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let args = MsgArgs {
            handle: self.endpoint,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        let _ = sys::messenger(
            msg_op::CLOSE_ENDPOINT,
            &args as *const MsgArgs as u64,
            &mut result as *mut MsgResult as u64,
        );
    }
}

/// The structured error code in a reply, when a service refused the call.
fn error_field(parcel: &Parcel, error_id: u16) -> Option<i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == error_id {
            let (code, _message) = field.error_parts().ok()?;
            return Some(-(code as i64));
        }
    }
    None
}
