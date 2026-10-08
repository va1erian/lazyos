//! The device syscall's read-only inspection ops (issue #481): the inventory,
//! the driver class rules the kernel installed at boot, and the refused claims
//! (`kernel/src/dev/inspect.rs`; row layouts and names in `libs/devinspect`).
//! `devctl` prints them; [`cross_class_probe`] is the boot evidence that a
//! driver uid cannot claim another driver's class.

use alloc::format;
use alloc::vec::Vec;

use devinspect::{Denial, Device, Rule};

use super::errno;
use crate::sys;

/// Every device, with its owner and rights; the error is the positive errno.
pub fn inventory() -> Result<Vec<Device>, i64> {
    lazyos_sys::dev::inventory().map_err(|code| -code)
}

/// The installed class rules; `Ok(None)` before the kernel installed them.
pub fn policy() -> Result<Option<Vec<Rule>>, i64> {
    lazyos_sys::dev::policy().map_err(|code| -code)
}

/// Refused claims still in the audit ring, newest first (`CAP_AUDIT_READ`).
pub fn denials() -> Result<Vec<Denial>, i64> {
    lazyos_sys::dev::denials().map_err(|code| -code)
}

/// Try to claim every device whose class this task does not already own, and
/// print `DEV:CROSSCLAIM:<tag>:PASS|FAIL|SKIP`. Each attempt must be refused
/// with `EACCES` by the class rules; a claim that succeeds is released at
/// once and reported. Root keeps its ambient authority, so it skips. Drivers
/// call this once, after claiming their own device, under their harness flag.
pub fn cross_class_probe(tag: &str) {
    let cred = sys::cred_get(None).unwrap_or_default();
    if cred.uid == 0 {
        sys::write_str(&format!(
            "DEV:CROSSCLAIM:{tag}:SKIP uid=0 (root is not confined)\n"
        ));
        return;
    }
    let devices = match inventory() {
        Ok(devices) => devices,
        Err(errno) => {
            sys::write_str(&format!(
                "DEV:CROSSCLAIM:{tag}:FAIL inventory errno={errno}\n"
            ));
            return;
        }
    };
    let own: Vec<u64> = devices
        .iter()
        .filter(|device| device.owner == Some(cred.uid))
        .map(|device| device.class_id)
        .collect();
    let mut refused = 0;
    for device in devices
        .iter()
        .filter(|device| !own.contains(&device.class_id))
    {
        match super::claim(u64::from(device.id)) {
            Err(errno::EACCES) => refused += 1,
            Ok(handle) => {
                let _ = super::release(handle);
                sys::write_str(&format!(
                    "DEV:CROSSCLAIM:{tag}:FAIL uid={} claimed device {} ({})\n",
                    cred.uid,
                    device.id,
                    device.class_name()
                ));
                return;
            }
            Err(errno) => {
                sys::write_str(&format!(
                    "DEV:CROSSCLAIM:{tag}:FAIL uid={} device {} ({}) errno={errno}, want EACCES\n",
                    cred.uid,
                    device.id,
                    device.class_name()
                ));
                return;
            }
        }
    }
    sys::write_str(&format!(
        "DEV:CROSSCLAIM:{tag}:PASS uid={} own={} refused={refused}\n",
        cred.uid,
        own.len()
    ));
}
