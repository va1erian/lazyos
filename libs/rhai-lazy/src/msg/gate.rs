//! The real [`Bus`]: LazyOS's native Messenger gate (`int 0x80`, syscall 5),
//! issued by `lazyos-sys`, the crate every userspace program reaches the
//! native syscalls through. Bodies come from the `msg` codec, and the
//! registry's `Resolve`/`Register`/`List` from the compiled
//! `messenger-generated` stubs (inside `lazyos_sys::msg::parcel`), so no wire
//! field is written by hand here.
//!
//! Only meaningful inside LazyOS: on another kernel `int 0x80` is a different
//! ABI, so a program gets a [`Gate`] only from [`Gate::detect`].

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use lazyos_sys::msg::{self, parcel, EXPIRED_DEADLINE};
use lazyos_sys::time;
use libmessenger::{flags, Parcel};

use super::bus::{Bus, BusError, Incoming, Wait};

const ETIMEDOUT: i64 = lazyos_sys::errno::ETIMEDOUT;
const ENOENT: i64 = lazyos_sys::errno::ENOENT;
const E2BIG: i64 = lazyos_sys::errno::E2BIG;

/// Largest reply a call accepts (services cap inline payloads well below).
const REPLY_BUFFER: usize = 64 * 1024;

fn invalid(error: libmessenger::Error) -> BusError {
    BusError {
        code: -lazyos_sys::errno::EINVAL,
        message: String::from(error.message()),
    }
}

/// `body` as an encoded request for `method` of `interface`.
fn encode(interface: u64, method: u32, flag_bits: u16, body: &[u8]) -> Result<Vec<u8>, BusError> {
    let parcel = parcel::request(interface, method, flag_bits, body.to_vec());
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(invalid)?;
    Ok(bytes)
}

/// The reply parcel in `buf[..len]`.
fn decode(buf: &[u8], len: usize) -> Result<Parcel, BusError> {
    let bytes = buf.get(..len).ok_or_else(|| BusError::errno(-E2BIG))?;
    Parcel::decode(bytes).map_err(invalid)
}

/// The fabric as seen from this process.
#[derive(Debug, Default, Clone, Copy)]
pub struct Gate;

impl Gate {
    /// The gate, when this process runs on LazyOS
    /// ([`lazyos_sys::detect::on_lazyos`]); `None` elsewhere, so a host build
    /// of a LazyOS program never issues `int 0x80`.
    pub fn detect() -> Option<Gate> {
        lazyos_sys::detect::on_lazyos().then_some(Gate)
    }

    /// The kernel deadline for `wait`: `0` waits forever, [`EXPIRED_DEADLINE`]
    /// polls, anything else is an absolute PIT tick.
    fn deadline(wait: Wait) -> u64 {
        match wait {
            Wait::Forever => 0,
            Wait::Poll => EXPIRED_DEADLINE,
            Wait::Ms(ms) => time::deadline_after_ms(ms),
        }
    }
}

impl Bus for Gate {
    fn resolve(&self, name: &str) -> Result<u64, BusError> {
        parcel::resolve(name).map_err(BusError::errno)
    }

    fn call(
        &self,
        endpoint: u64,
        interface: u64,
        method: u32,
        body: &[u8],
        wait: Wait,
    ) -> Result<Vec<u8>, BusError> {
        // `ALLOW_NESTED`: a script may be parked on another transaction of
        // the same channel (a topic pull) when it calls again.
        let request = encode(interface, method, flags::SYNC | flags::ALLOW_NESTED, body)?;
        let mut buf = vec![0u8; REPLY_BUFFER];
        let len = msg::call(endpoint, &request, &mut buf, Self::deadline(wait))
            .map_err(BusError::errno)?;
        Ok(decode(&buf, len)?.body)
    }

    fn send(
        &self,
        endpoint: u64,
        interface: u64,
        method: u32,
        body: &[u8],
    ) -> Result<(), BusError> {
        let request = encode(interface, method, flags::ONE_WAY, body)?;
        msg::send(endpoint, &request).map_err(BusError::errno)
    }

    fn names(&self) -> Result<Vec<String>, BusError> {
        parcel::names().map_err(BusError::errno)
    }

    fn register(
        &self,
        name: &str,
        interfaces: &[u64],
        interface_names: &[&str],
    ) -> Result<u64, BusError> {
        let (published, server) = msg::create_pair().map_err(BusError::errno)?;
        parcel::register(name, published, interfaces, interface_names).map_err(BusError::errno)?;
        Ok(server)
    }

    fn unregister(&self, name: &str, endpoint: u64) -> Result<(), BusError> {
        let withdrawn = parcel::unregister(name).map_err(BusError::errno);
        // Close the receive side even when the name was already gone, so
        // callers holding the old endpoint fail with `EPIPE` at once.
        let closed = msg::close(endpoint).map_err(BusError::errno);
        withdrawn.and(closed)
    }

    fn recv(&self, endpoint: u64, wait: Wait) -> Result<Option<Incoming>, BusError> {
        let mut buf = vec![0u8; REPLY_BUFFER];
        let result = match msg::recv(endpoint, &mut buf, Self::deadline(wait)) {
            Ok(result) => result,
            Err(code) if code == -ETIMEDOUT => return Ok(None),
            Err(code) => return Err(BusError::errno(code)),
        };
        let parcel = decode(&buf, result.bytes as usize)?;
        Ok(Some(Incoming {
            interface: parcel.header.interface_id,
            method: parcel.header.method,
            txn: (result.value != 0).then_some(result.value),
            body: parcel.body,
        }))
    }

    fn reply(&self, txn: u64, interface: u64, method: u32, body: &[u8]) -> Result<(), BusError> {
        let reply = encode(interface, method, 0, body)?;
        match msg::reply(txn, &reply) {
            // The caller timed out, canceled or exited: an ordinary race.
            Err(code) if code == -ENOENT => Ok(()),
            other => other.map_err(BusError::errno),
        }
    }

    fn clock_ms(&self) -> u64 {
        time::clock().saturating_mul(time::TICK_MS)
    }
}
