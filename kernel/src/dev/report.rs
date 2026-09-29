//! Audit records for device claims (issue #240, driver-plan section 3.5).
//!
//! Every claim, release, denial and interrupt-ack timeout is one record in the
//! hash-chained ring of [`crate::ipc::audit`]: the actor is the claiming task,
//! the interface id is the *class-specific* `os.kernel.dev.<class>` id, the
//! method says what happened, and `txn_id` carries the device id
//! ([`correlation`]) so `auditd` can group a device's history. The reason code
//! says why ([`reason`]); a granted claim also encodes the rights it received.

use crate::ipc::audit::{self, AuditEvent};
use crate::ipc::credentials;

use super::class::class_of;
use super::{DeviceId, DeviceInfo};

/// Machine-readable reason codes for device audit records. They start above
/// the ACL's (`ipc::acl::reason`) so the two never collide in one ring.
pub mod reason {
    /// A claim was granted; the rights are in bits 8 and up of the code.
    pub const CLAIMED: u32 = 0x10;
    /// The owner released the claim.
    pub const RELEASED: u32 = 0x11;
    /// The owner died and teardown released the claim.
    pub const TEARDOWN: u32 = 0x12;
    /// The caller lacks `CAP_DEV_CLAIM`.
    pub const NO_CAP: u32 = 0x13;
    /// Device resources and class policy intersect in nothing.
    pub const NO_RIGHTS: u32 = 0x14;
    /// The device already has an owner.
    pub const BUSY: u32 = 0x15;
    /// A claimant did not acknowledge an interrupt before its deadline.
    pub const IRQ_TIMEOUT: u32 = 0x16;
    /// The caller's device-claim quota is exhausted.
    pub const QUOTA: u32 = 0x17;
    /// The named interrupt endpoint is not a usable channel handle.
    pub const BAD_ENDPOINT: u32 = 0x18;
    /// The interrupt line is held exclusively by another claim.
    pub const LINE_BUSY: u32 = 0x19;
    /// A write tried to enable bus mastering without the `DMA` right.
    pub const DMA_DENIED: u32 = 0x1A;
    /// A BAR overlapping system RAM was refused.
    pub const BAR_IN_RAM: u32 = 0x1B;
}

/// Tag in the correlation id marking it as a device id, not a transaction.
const DEVICE_TAG: u64 = 1 << 40;

/// The `txn_id` value audit records carry for device `id`.
pub fn correlation(id: DeviceId) -> u64 {
    DEVICE_TAG | u64::from(id.0)
}

/// The device id a [`correlation`] value names, if it is one.
pub fn device_of(txn_id: u64) -> Option<DeviceId> {
    (txn_id & !0xFFFF == DEVICE_TAG).then_some(DeviceId((txn_id & 0xFFFF) as u16))
}

/// Append one device record on behalf of `actor`. `method` is one of
/// [`super::class::method`]; `allow` says whether the action went ahead.
pub fn record(actor: usize, info: &DeviceInfo, method: u32, allow: bool, reason_code: u32) {
    let cred = credentials::of(actor);
    audit::record(AuditEvent {
        ticks: crate::task::ticks(),
        actor_slot: actor,
        uid: cred.uid,
        label_id: cred.label_id,
        interface_id: class_of(info).interface_id,
        method,
        allow,
        reason_code,
        txn_id: correlation(info.id),
    });
}
