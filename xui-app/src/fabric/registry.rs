//! The kernel name registry: the `list` snapshot and the `resolve` op that
//! turns a well-known name into this task's handle-table endpoint.

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use crate::sys::{self, msg_op, MsgArgs, MsgResult, REGISTRY_TARGET_SELF};

use super::error::{error_code, E2BIG, EINVAL};

/// Registry TLV field ids, mirroring `kernel/src/ipc/registry.rs`.
mod registry_field {
    pub const NAME: u16 = 1;
    pub const INTERFACES: u16 = 2;
    pub const OBJECT: u16 = 5;
    pub const OWNER: u16 = 6;
    pub const LEASE_REMAINING: u16 = 7;
    pub const ENTRY: u16 = 8;
}

/// The registry interface id: the first eight bytes of `os.lazy.messenger.registry.v1`.
const REGISTRY_INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");
/// Registry method `resolve`.
const REGISTRY_RESOLVE: u32 = 2;
/// Largest `list` reply the client offers the kernel (64 names of 128 bytes).
const REGISTRY_LIST_BUFFER: usize = 32 * 1024;

/// One registered name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegistryEntry {
    /// Service name.
    pub name: String,
    /// Kernel object the name refers to (diagnostic).
    pub object_id: u64,
    /// Task slot that owns the name.
    pub owner_slot: u64,
    /// Interface ids the service implements.
    pub interfaces: Vec<u64>,
    /// Remaining lease ticks; `0` when permanent.
    pub lease_remaining: u64,
}

/// Snapshot the kernel name table through syscall 5 `list`.
pub fn registry() -> Result<Vec<RegistryEntry>, i64> {
    let mut buf = vec![0u8; REGISTRY_LIST_BUFFER];
    let args = MsgArgs {
        txn_id: REGISTRY_TARGET_SELF,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    let code = sys::messenger(
        msg_op::LIST,
        &args as *const MsgArgs as u64,
        &mut result as *mut MsgResult as u64,
    );
    if code < 0 {
        return Err(code);
    }
    let len = result.bytes as usize;
    if len > buf.len() {
        return Err(-E2BIG);
    }
    let parcel = Parcel::decode(&buf[..len]).map_err(|_| -EINVAL)?;
    if let Some(error) = error_code(&parcel) {
        return Err(error);
    }
    decode_registry(&parcel).ok_or(-EINVAL)
}

/// Decode the registry `ENTRY` records of a `list` reply.
fn decode_registry(parcel: &Parcel) -> Option<Vec<RegistryEntry>> {
    let mut entries = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(record)) = decoder.next() {
        if record.kind != Kind::Struct || record.id != registry_field::ENTRY {
            continue;
        }
        let mut nested = record.nested(0).ok()?;
        let mut entry = RegistryEntry {
            name: String::new(),
            object_id: 0,
            owner_slot: 0,
            interfaces: Vec::new(),
            lease_remaining: 0,
        };
        while let Ok(Some(item)) = nested.next() {
            match (item.kind, item.id) {
                (Kind::String, registry_field::NAME) => {
                    entry.name = item.as_str().ok()?.to_string()
                }
                (Kind::U64, registry_field::OBJECT) => entry.object_id = item.as_u64().ok()?,
                (Kind::U64, registry_field::OWNER) => entry.owner_slot = item.as_u64().ok()?,
                (Kind::U64, registry_field::LEASE_REMAINING) => {
                    entry.lease_remaining = item.as_u64().ok()?
                }
                (Kind::Array, registry_field::INTERFACES) => {
                    let mut array = item.nested(0).ok()?;
                    while let Ok(Some(id)) = array.next() {
                        if id.kind == Kind::U64 {
                            entry.interfaces.push(id.as_u64().ok()?);
                        }
                    }
                }
                _ => {}
            }
        }
        entries.push(entry);
    }
    Some(entries)
}

/// Resolve `name` into this task's handle table through syscall 5 `resolve`.
pub(super) fn resolve(name: &str) -> Result<u64, i64> {
    let mut body = Encoder::new();
    body.string(registry_field::NAME, name)
        .map_err(|_| -EINVAL)?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: REGISTRY_INTERFACE,
            method: REGISTRY_RESOLVE,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| -EINVAL)?;
    let args = MsgArgs {
        txn_id: REGISTRY_TARGET_SELF,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    let code = sys::messenger(
        msg_op::RESOLVE,
        &args as *const MsgArgs as u64,
        &mut result as *mut MsgResult as u64,
    );
    if code < 0 {
        return Err(code);
    }
    Ok(result.value)
}

/// Close an endpoint handle opened by [`resolve`].
pub(super) fn close(handle: u64) -> Result<(), i64> {
    let args = MsgArgs {
        handle,
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
