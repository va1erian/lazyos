//! The kernel name registry: the `list` snapshot and the `resolve` op that
//! turns a well-known name into this task's handle-table endpoint.

use libmessenger::Parcel;
use messenger_generated::os_lazy_messenger_registry_v1 as wire;

use super::error::{error_code, EINVAL};

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
    let parcel = lazyos_sys::msg::parcel::list_parcel()?;
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
    lazyos_sys::msg::parcel::resolve(name)
}

/// Close an endpoint handle opened by [`resolve`].
pub(super) fn close(handle: u64) -> Result<(), i64> {
    lazyos_sys::msg::close(handle)
}
