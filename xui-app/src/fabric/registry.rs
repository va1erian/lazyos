//! The kernel name registry: the `list` snapshot and the `resolve` op that
//! turns a well-known name into this task's handle-table endpoint.

use libmessenger::{Header, Parcel, VERSION};
use messenger_generated::os_lazy_messenger_registry_v1 as wire;

use crate::sys::{self, msg_op, MsgArgs, MsgResult, REGISTRY_TARGET_SELF};

use super::error::{error_code, E2BIG, EINVAL};

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

/// Decode the generated `List` reply into display entries.
fn decode_registry(parcel: &Parcel) -> Option<Vec<RegistryEntry>> {
    let reply = wire::decode_list_reply(&parcel.body).ok()?;
    Some(
        reply
            .entries
            .into_iter()
            .map(|entry| RegistryEntry {
                name: entry.name,
                object_id: entry.object,
                owner_slot: entry.owner,
                interfaces: entry.interfaces,
                lease_remaining: entry.lease_remaining,
            })
            .collect(),
    )
}

/// Resolve `name` into this task's handle table through syscall 5 `resolve`.
pub(super) fn resolve(name: &str) -> Result<u64, i64> {
    let body =
        wire::encode_resolve_args(&wire::ResolveArgs { name: name.into() }).map_err(|_| -EINVAL)?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: wire::INTERFACE_ID,
            method: wire::METHOD_RESOLVE,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
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
