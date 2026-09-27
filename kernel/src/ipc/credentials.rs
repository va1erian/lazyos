//! Kernel credential stamping (issue #68).
//!
//! Every Messenger call carries an identity the sender cannot forge: the kernel
//! stamps `uid/gid/caps/label/session` from this registry, and no syscall or
//! parcel field can write them back (`docs/security-model.md` section 2 and
//! `docs/messenger.md` section 14). The stamp is what the ACL hook and the audit
//! ring read, so there is exactly one source of truth.
//!
//! Like the handle table, credentials live here keyed by task slot rather than
//! in `Task`, so this lands without churning the scheduler or the Linux ABI.
//! Moving them into `Task` later is a pure refactor: [`of`] and [`set`] are the
//! only accessors callers use.
//!
//! `userspace can never set a credential`: the setters below are kernel-side
//! APIs only. Profile loading (init/messengerd) and the elevation service are
//! the intended callers; #69 must not expose [`set`]/[`set_current`] through a
//! syscall.

use spin::Mutex;

use crate::task::MAX_TASKS;

/// Bind ports below 1024 (see `docs/security-model.md` section 4.2).
pub const CAP_NET_BIND: u32 = 1 << 0;
/// Open raw sockets.
pub const CAP_NET_RAW: u32 = 1 << 1;
/// Mounts and driver grants.
pub const CAP_SYS_ADMIN: u32 = 1 << 2;
/// Set the system clock.
pub const CAP_SYS_TIME: u32 = 1 << 3;
/// Read the audit stream (`os.lazy.audit.v1`).
pub const CAP_AUDIT_READ: u32 = 1 << 4;
/// Manage other services' endpoints (revoke, reconfigure).
pub const CAP_IPC_CONTROL: u32 = 1 << 5;
/// Every capability bit defined today.
pub const CAP_ALL: u32 =
    CAP_NET_BIND | CAP_NET_RAW | CAP_SYS_ADMIN | CAP_SYS_TIME | CAP_AUDIT_READ | CAP_IPC_CONTROL;

/// The kernel-stamped identity attached to a task and copied into every call
/// credential block and audit record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cred {
    /// User id; `0` is the system/root user.
    pub uid: u32,
    /// Primary group id.
    pub gid: u32,
    /// Capability bits (`CAP_*`).
    pub caps: u32,
    /// Sandbox/profile label id used by policy.
    pub label_id: u32,
    /// Session the task belongs to (`logind`); `0` before login.
    pub session: u64,
}

impl Cred {
    /// The credentials every task starts with: root, every capability, no
    /// session. The default exists so bring-up and the kernel task work before
    /// profiles are loaded; a real session replaces it via [`set`].
    pub const ROOT: Cred = Cred {
        uid: 0,
        gid: 0,
        caps: CAP_ALL,
        label_id: 0,
        session: 0,
    };

    /// Build a credential. Kernel-side constructors use this; userspace has no
    /// path to it.
    pub const fn new(uid: u32, gid: u32, caps: u32, label_id: u32, session: u64) -> Self {
        Cred {
            uid,
            gid,
            caps,
            label_id,
            session,
        }
    }

    /// The identity the ACL keys on today: the uid. Label-keyed policy can call
    /// [`super::acl::evaluate`] with [`Cred::label_id`] directly.
    pub const fn authority(&self) -> u32 {
        self.uid
    }

    /// Whether every bit in `cap` is held.
    pub const fn has_cap(&self, cap: u32) -> bool {
        self.caps & cap == cap
    }
}

/// One credential per task slot. Slots start as root; reuse a slot only after
/// [`reset_for_task`] so a new process never inherits a dead one's identity.
static CREDS: Mutex<[Cred; MAX_TASKS]> = Mutex::new([Cred::ROOT; MAX_TASKS]);

/// The credentials of the task in `slot`. Unknown slots read as root, matching
/// the bring-up default.
pub fn of(slot: usize) -> Cred {
    CREDS.lock().get(slot).copied().unwrap_or(Cred::ROOT)
}

/// Replace `slot`'s credentials. `Kernel-only`: profiles (init/messengerd) and
/// the elevation service call this; nothing reachable from a syscall or parcel
/// does. Out-of-range slots are ignored.
pub fn set(slot: usize, cred: Cred) {
    if let Some(entry) = CREDS.lock().get_mut(slot) {
        *entry = cred;
    }
}

/// Replace the current task's credentials. `Kernel-only`; see [`set`].
pub fn set_current(cred: Cred) {
    set(crate::task::current(), cred);
}

/// Restore the root default for `slot`, e.g. when a slot is reused or a process
/// exits.
pub fn reset_for_task(slot: usize) {
    set(slot, Cred::ROOT);
}
