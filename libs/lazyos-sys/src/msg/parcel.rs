//! Parcel-level Messenger helpers (feature `parcel`): encode with
//! `libmessenger`, move the bytes with [`super`], decode the reply. The
//! kernel registry ops build their bodies from the generated
//! `os.lazy.messenger.registry.v1` stubs, so no wire field is written by hand.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_messenger_registry_v1 as registry;

use super::{op, REGISTRY_TARGET_SELF};
use crate::errno::{E2BIG, EINVAL};

/// Largest name-table snapshot [`list`] reads.
pub const LIST_BUFFER: usize = 32 * 1024;

/// A parcel for `method` of `interface` with `flag_bits` and `body`, no
/// objects.
pub fn request(interface: u64, method: u32, flag_bits: u16, body: Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: flag_bits,
            interface_id: interface,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    }
}

/// `parcel`'s wire bytes; a parcel the codec refuses is `-EINVAL`.
pub fn encode(parcel: &Parcel) -> Result<Vec<u8>, i64> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| -EINVAL)?;
    Ok(bytes)
}

/// The parcel in `bytes`; a malformed one is `-EINVAL`.
pub fn decode(bytes: &[u8]) -> Result<Parcel, i64> {
    Parcel::decode(bytes).map_err(|_| -EINVAL)
}

/// One synchronous call: send `parcel` on `handle`, wait for the reply into
/// `buf` (until `deadline`, an absolute PIT tick; `0` waits forever), and
/// decode it.
pub fn call(handle: u64, parcel: &Parcel, buf: &mut [u8], deadline: u64) -> Result<Parcel, i64> {
    let len = super::call(handle, &encode(parcel)?, buf, deadline)?;
    decode(buf.get(..len).ok_or(-E2BIG)?)
}

/// Send `parcel` one way on `handle` (it should carry `flags::ONE_WAY`).
pub fn send(handle: u64, parcel: &Parcel) -> Result<(), i64> {
    super::send(handle, &encode(parcel)?)
}

/// Answer the call `txn` with `reply`.
pub fn reply(txn: u64, reply: &Parcel) -> Result<(), i64> {
    super::reply(txn, &encode(reply)?)
}

/// One kernel registry op for this task with a generated `body`.
fn registry_op(op: u64, method: u32, body: Vec<u8>) -> Result<u64, i64> {
    // `ALLOW_NESTED`, like every registry client: the op may run while this
    // task is parked on another transaction.
    let parcel = request(registry::INTERFACE_ID, method, flags::ALLOW_NESTED, body);
    Ok(super::registry(op, REGISTRY_TARGET_SELF, &encode(&parcel)?)?.value)
}

/// Resolve `name` through the kernel registry: the service's published
/// endpoint, opened in this task's table (release it, don't close it).
pub fn resolve(name: &str) -> Result<u64, i64> {
    let body = registry::encode_resolve_args(&registry::ResolveArgs { name: name.into() })
        .map_err(|_| -EINVAL)?;
    registry_op(op::RESOLVE, registry::METHOD_RESOLVE, body)
}

/// Open a private connection to `name` (issue #483): an endpoint this task
/// alone holds; the service receives the other end.
pub fn connect(name: &str) -> Result<u64, i64> {
    let body = registry::encode_connect_args(&registry::ConnectArgs { name: name.into() })
        .map_err(|_| -EINVAL)?;
    registry_op(op::CONNECT, registry::METHOD_CONNECT, body)
}

/// Publish `endpoint` (a handle in this task) under `name` as a permanent
/// registration implementing `interfaces`, spelled out in `interface_names`
/// (an app must name what it serves, issue #495); this task is the owner.
pub fn register(
    name: &str,
    endpoint: u64,
    interfaces: &[u64],
    interface_names: &[&str],
) -> Result<(), i64> {
    let body = registry::encode_register_args(&registry::RegisterArgs {
        name: name.into(),
        endpoint: Some(endpoint),
        interfaces: interfaces.to_vec(),
        lease_ticks: 0,
        interface_names: interface_names.iter().map(|&n| n.into()).collect(),
    })
    .map_err(|_| -EINVAL)?;
    registry_op(op::REGISTER, registry::METHOD_REGISTER, body).map(drop)
}

/// Withdraw `name`, which this task registered.
pub fn unregister(name: &str) -> Result<(), i64> {
    let body = registry::encode_unregister_args(&registry::UnregisterArgs { name: name.into() })
        .map_err(|_| -EINVAL)?;
    registry_op(op::UNREGISTER, registry::METHOD_UNREGISTER, body).map(drop)
}

/// The kernel name table's reply parcel (a `List` reply, or a structured
/// error the caller may inspect).
pub fn list_parcel() -> Result<Parcel, i64> {
    let mut buf = vec![0u8; LIST_BUFFER];
    let len = super::list(&mut buf)?;
    decode(&buf[..len])
}

/// The kernel name table.
pub fn list() -> Result<Vec<registry::Entry>, i64> {
    let reply = list_parcel()?;
    Ok(registry::decode_list_reply(&reply.body)
        .map_err(|_| -EINVAL)?
        .entries)
}

/// Every registered name.
pub fn names() -> Result<Vec<String>, i64> {
    Ok(list()?.into_iter().map(|entry| entry.name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_through_the_codec() {
        let parcel = request(0x1234, 7, flags::ONE_WAY, vec![1, 2, 3]);
        let back = decode(&encode(&parcel).unwrap()).unwrap();
        assert_eq!(back.header.interface_id, 0x1234);
        assert_eq!(back.header.method, 7);
        assert_eq!(back.header.flags, flags::ONE_WAY);
        assert_eq!(back.body, [1, 2, 3]);
    }

    #[test]
    fn garbage_is_einval() {
        assert_eq!(decode(&[0xff; 3]).err(), Some(-EINVAL));
    }
}
