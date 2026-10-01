//! Read-only views of the device layer for `devctl` and the Devices app
//! (issue #481): who owns which device with what rights, the driver class
//! rules the kernel installed at boot, and the recent refused claims.
//!
//! Nothing here changes state, and nothing here installs or edits a rule: the
//! class policy is compiled into the kernel on purpose (`super::policy`). The
//! inventory and the rules are visible to any task the Messenger policy lets
//! call `os.kernel.dev` (labelled apps are refused by default); the denial log
//! reads the audit ring, so it also needs `CAP_AUDIT_READ`. Row layouts are
//! mirrored in `user/src/dev/inspect.rs`.

use alloc::vec::Vec;

use crate::ipc::audit;
use crate::ipc::credentials::{self, CAP_AUDIT_READ};
use crate::ipc::topics::fnv1a32;
use crate::user_ptr;

use super::claims::CLAIMS;
use super::class::{class_of, method, DEV_INTERFACE};
use super::errno::*;
use super::report;
use super::table;

/// `u64` words per inventory row.
pub const INVENTORY_WORDS: usize = 4;
/// `u64` words per class-rule row.
pub const RULE_WORDS: usize = 3;
/// `u64` words per denial row.
pub const DENIAL_WORDS: usize = 4;

/// Inventory owner value for a free device.
pub const NO_OWNER: u64 = u32::MAX as u64;

/// Method ids the inspection ops are authorized as, on `os.kernel.dev`.
pub const INVENTORY: u32 = fnv1a32("inventory");
pub const POLICY: u32 = fnv1a32("policy");
pub const DENIALS: u32 = fnv1a32("denials");

/// Ask the Messenger policy whether `slot` may use inspection `method`.
fn authorized(slot: usize, method: u32) -> Result<(), Errno> {
    if crate::ipc::authorize(slot, DEV_INTERFACE, method, 0).denied() {
        return Err(EACCES);
    }
    Ok(())
}

/// Copy at most `capacity` rows to `buf` and return `total`, so a caller with
/// a short buffer learns how large to retry.
fn copy_rows<const N: usize>(rows: &[[u64; N]], buf: u64, capacity: u64) -> Result<u64, Errno> {
    let count = rows
        .len()
        .min(usize::try_from(capacity).unwrap_or(usize::MAX));
    if count > 0 {
        let words: Vec<u64> = rows[..count].iter().flatten().copied().collect();
        user_ptr::try_copy_words(buf, &words).map_err(|_| EFAULT)?;
    }
    Ok(rows.len() as u64)
}

/// `inventory(buf, capacity)`: one row per device.
///
/// ```text
///   w0 = id | class << 16 | subclass << 24 | prog_if << 32
///   w1 = vendor | device << 16
///   w2 = class interface id (os.kernel.dev.<class>)
///   w3 = owner uid (NO_OWNER when free) | claim rights << 32
/// ```
pub fn inventory(slot: usize, buf: u64, capacity: u64) -> Result<u64, Errno> {
    authorized(slot, INVENTORY)?;
    let devices: Vec<_> = table().lock().iter().collect();
    let claims = CLAIMS.lock();
    let rows: Vec<[u64; INVENTORY_WORDS]> = devices
        .iter()
        .map(|info| {
            let (owner, rights) = claims
                .get(info.id)
                .map_or((NO_OWNER, 0), |claim| (u64::from(claim.uid), claim.rights));
            [
                u64::from(info.id.0)
                    | u64::from(info.class) << 16
                    | u64::from(info.subclass) << 24
                    | u64::from(info.prog_if) << 32,
                u64::from(info.vendor) | u64::from(info.device) << 16,
                class_of(info).interface_id,
                owner | u64::from(rights) << 32,
            ]
        })
        .collect();
    drop(claims);
    copy_rows(&rows, buf, capacity)
}

/// `policy(buf, capacity)`: the installed class rules, in evaluation order;
/// `ENOENT` before installation.
///
/// ```text
///   w0 = actor uid   w1 = interface id   w2 = method | allow << 32
/// ```
pub fn policy(slot: usize, buf: u64, capacity: u64) -> Result<u64, Errno> {
    authorized(slot, POLICY)?;
    let rules = super::policy::rules().ok_or(ENOENT)?;
    let rows: Vec<[u64; RULE_WORDS]> = rules
        .iter()
        .map(|rule| {
            [
                u64::from(rule.actor),
                rule.interface_id,
                u64::from(rule.method) | u64::from(rule.allow) << 32,
            ]
        })
        .collect();
    copy_rows(&rows, buf, capacity)
}

/// `denials(buf, capacity)`: refused device claims still in the audit ring,
/// newest first. Requires `CAP_AUDIT_READ`.
///
/// ```text
///   w0 = ticks   w1 = uid | reason << 32   w2 = class interface id   w3 = device id
/// ```
pub fn denials(slot: usize, buf: u64, capacity: u64) -> Result<u64, Errno> {
    if !credentials::of(slot).has_cap(CAP_AUDIT_READ) {
        return Err(EPERM);
    }
    authorized(slot, DENIALS)?;
    let rows: Vec<[u64; DENIAL_WORDS]> = audit::recent(audit::AUDIT_CAPACITY)
        .into_iter()
        .filter(|event| event.method == method::CLAIM && !event.allow)
        .filter_map(|event| {
            let device = report::device_of(event.txn_id)?;
            Some([
                event.ticks,
                u64::from(event.uid) | u64::from(event.reason_code) << 32,
                event.interface_id,
                u64::from(device.0),
            ])
        })
        .collect();
    copy_rows(&rows, buf, capacity)
}
