//! The kernel policy loader (`acl_load`, application package system phase 1).
//!
//! The wire types are the `midlc`-generated `os.lazy.messenger.policy.v1`
//! stubs (`idl/policy.midl`). Only a task holding `CAP_IPC_CONTROL` may load
//! rules; the kernel refuses everyone else with `-EPERM`.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Header, Parcel, VERSION};

pub use messenger_generated::os_lazy_messenger_names_resolve_v1 as resolve_scope;
pub use messenger_generated::os_lazy_messenger_policy_v1 as wire;

use super::endpoint::{encode, syscall};
use super::{op, Error, MsgArgs, MsgResult, Result};

/// Wildcard for [`wire::LabelRule::interface_id`].
pub const ANY_INTERFACE: u64 = u64::MAX;
/// Wildcard for [`wire::LabelRule::method`].
pub const ANY_METHOD: u32 = u32::MAX;

/// Replace every rule of `label` with `rules` (first match wins, unmatched
/// calls are denied). An empty list revokes the label. Returns how many rules
/// the label now holds.
pub fn load_label(label: &str, rules: &[wire::LabelRule]) -> Result<u64> {
    let body = wire::encode_load_label_args(&wire::LoadLabelArgs {
        label: String::from(label),
        rules: rules.to_vec(),
    })
    .map_err(Error::Parcel)?;
    // The kernel op ignores the header; the body carries the request.
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: wire::INTERFACE_ID,
            method: wire::METHOD_LOADLABEL,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let bytes = encode(&parcel)?;
    let args = MsgArgs {
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::ACL_LOAD, &args, &mut result)?;
    Ok(result.value)
}

/// A rule allowing exactly `(interface_id, method)`.
pub const fn allow(interface_id: u64, method: u32) -> wire::LabelRule {
    wire::LabelRule {
        interface_id,
        method,
        allow: true,
    }
}

/// A rule allowing resolution of the service `name`.
pub fn allow_resolve(name: &str) -> wire::LabelRule {
    allow(resolve_scope::INTERFACE_ID, fnv1a32(name))
}

/// FNV-1a 32 (31-bit positive): the method-id hash of `tools/midlc`, which the
/// kernel uses as the method of a name or topic segment.
pub const fn fnv1a32(text: &str) -> u32 {
    let bytes = text.as_bytes();
    let mut hash = 0x811C_9DC5u32;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u32).wrapping_mul(0x0100_0193);
        index += 1;
    }
    hash & 0x7FFF_FFFF
}
