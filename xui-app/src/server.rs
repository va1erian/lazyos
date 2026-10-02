//! A Messenger *service* from an xui app: publish an endpoint under a name and
//! answer calls on it without blocking the UI loop.
//!
//! Native services (`user/src/bin/*`) block in `recv`; an xui app cannot,
//! because its event loop must keep painting. [`Server::poll`] therefore asks
//! the channel counters whether anything is queued first (instant, never
//! parks) and only then receives, so the app polls it from a UI timer for free.
//!
//! The registration is the same kernel op native services use
//! (`registry::register`, `REGISTRY_TARGET_SELF`): this task owns the name
//! until it exits, when the kernel reaps it. Request and reply bodies come
//! from the generated stubs of the service's interface; this module only
//! moves parcels and builds the envelope and the structured error field.

use libmessenger::{Encoder, Header, Parcel, VERSION};

use crate::sys::{self, errno, msg_op, Cred, MsgArgs, MsgResult, EXPIRED_DEADLINE};

/// The structured error field id every LazyOS service replies with.
pub const ERROR_FIELD: u16 = 15;

/// One received call (or one-way message).
#[derive(Clone, Debug)]
pub struct Request {
    /// The sender's task slot, kernel-stamped.
    pub sender: u64,
    /// The transaction to answer, or `None` for a one-way message.
    pub txn: Option<u64>,
    pub parcel: Parcel,
}

impl Request {
    /// The sender's kernel-stamped credentials. Reading another task's block
    /// needs `CAP_SETUID`; without it this is `Err` and the caller must refuse.
    pub fn sender_cred(&self) -> Result<Cred, i64> {
        sys::cred_get(Some(self.sender))
    }
}

/// A published service endpoint.
pub struct Server {
    /// The end this task receives calls on.
    endpoint: u64,
}

impl Server {
    /// Publish a fresh endpoint as `name`, implementing `interfaces`. Fails
    /// with the registry's errno (`-EEXIST` while a previous owner's name is
    /// still registered); nothing stays open on failure.
    pub fn register(name: &str, interfaces: &[u64]) -> Result<Server, i64> {
        let (published, endpoint) = sys::msg_create_pair()?;
        if let Err(code) = sys::msg_register(name, published, interfaces) {
            let _ = close(published, 0);
            let _ = close(endpoint, 0);
            return Err(code);
        }
        Ok(Server { endpoint })
    }

    /// The next queued request, or `None` when nothing is waiting. Never
    /// parks when the queue is empty. A message too large for `buf` is
    /// dropped (`Err(-E2BIG)`); one that is not a parcel is skipped.
    pub fn poll(&self, buf: &mut [u8]) -> Result<Option<Request>, i64> {
        if sys::msg_queued(self.endpoint)? == 0 {
            return Ok(None);
        }
        let result = match sys::msg_recv(self.endpoint, buf, EXPIRED_DEADLINE) {
            Ok(result) => result,
            Err(code) if code == -errno::ETIMEDOUT => return Ok(None),
            Err(code) => return Err(code),
        };
        release_transfers(&result);
        let txn = (result.value != 0).then_some(result.value);
        let Ok(parcel) = Parcel::decode(&buf[..result.bytes as usize]) else {
            // Answer a malformed call so its sender does not wait forever.
            if let Some(txn) = txn {
                let _ = self.reply(txn, &error_parcel(0, 0, errno::EINVAL, "bad parcel"));
            }
            return Ok(None);
        };
        Ok(Some(Request {
            sender: result.aux,
            txn,
            parcel,
        }))
    }

    /// Answer `txn`. A caller that gave up (deadline, cancel, exit) leaves no
    /// transaction and the kernel answers `-ENOENT`: an ordinary race, not an
    /// error of the service, so it is swallowed.
    pub fn reply(&self, txn: u64, reply: &Parcel) -> Result<(), i64> {
        match sys::msg_reply(txn, reply) {
            Err(code) if code == -errno::ENOENT => Ok(()),
            other => other,
        }
    }
}

/// A reply parcel for `interface`/`method` carrying `body`.
pub fn reply_parcel(interface: u64, method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: interface,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        handles: Vec::new(),
        buffers: Vec::new(),
    }
}

/// A failure reply: the structured error field with positive errno `code`
/// and a short friendly `message`.
pub fn error_parcel(interface: u64, method: u32, code: i64, message: &str) -> Parcel {
    let mut body = Encoder::new();
    // One bounded field into a fresh encoder cannot overflow.
    let _ = body.error(ERROR_FIELD, code.unsigned_abs() as u32, message);
    reply_parcel(interface, method, body.finish())
}

/// Close what a sender transferred with its message: no shell protocol takes
/// a handle or a buffer, and keeping them would let any caller fill this
/// task's tables. Only the first of each is reported by the kernel, which is
/// all a well-formed request could carry anyway.
fn release_transfers(result: &MsgResult) {
    if result.reserved[1] > 0 {
        let _ = close(result.reserved[0], msg_op::CLOSE_RELEASE);
    }
    if result.reserved[3] > 0 {
        let _ = sys::display_close_buffer(result.reserved[2]);
    }
}

/// `CLOSE_ENDPOINT` on `handle` with `flags`.
fn close(handle: u64, flags: u64) -> Result<(), i64> {
    let args = MsgArgs {
        handle,
        flags,
        ..MsgArgs::default()
    };
    let code = sys::messenger(
        msg_op::CLOSE_ENDPOINT,
        &args as *const MsgArgs as u64,
        &mut MsgResult::default() as *mut MsgResult as u64,
    );
    if code < 0 {
        Err(code)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libmessenger::{Decoder, Kind};

    #[test]
    fn an_error_reply_carries_the_positive_code_and_text() {
        let parcel = error_parcel(7, 3, -13, "denied");
        assert_eq!((parcel.header.interface_id, parcel.header.method), (7, 3));
        let mut decoder = Decoder::new(&parcel.body);
        let field = decoder.next().unwrap().unwrap();
        assert_eq!((field.kind, field.id), (Kind::Error, ERROR_FIELD));
        assert_eq!(field.error_parts().unwrap(), (13, "denied"));
    }
}
