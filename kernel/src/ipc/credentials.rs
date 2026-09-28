//! Kernel credential stamping (issue #68) and the audited transition gate
//! (issue #101).
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
//! # The transition gate (issue #101)
//!
//! `userspace can never write a credential directly`: [`set`]/[`set_current`]
//! stay kernel-only. Accounts and login need one controlled exception, so
//! [`transition`] is the single audited kernel API a privileged service reaches
//! through the native `creds` syscall (`process::SYS_CREDS`):
//!
//! * the actor must hold [`CAP_SETUID`],
//! * the request may never widen privilege: only the root uid may stamp uid 0,
//!   and the requested capability bits must be a subset of the actor's,
//! * the target must be the actor itself or a live task, and the kernel task
//!   may only restamp itself,
//! * every attempt -- allowed or refused -- extends the audit ring with
//!   [`AUDIT_INTERFACE`] and a [`reason`] code, so `auditd` sees logins and
//!   elevation attempts even when they fail.
//!
//! A task created by another task (`fork`, `clone`, the native `spawn`) starts
//! with a copy of its creator's credentials ([`inherit`]); only a program the
//! kernel itself starts (no creator) begins as [`Cred::ROOT`]. Either way the
//! slot is stamped at creation, so a re-used slot can never inherit a dead
//! process's identity, and a child can never be more privileged than its parent.

use spin::Mutex;

use super::audit::{self, AuditEvent};
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
/// Change another task's `uid/gid/caps/label/session` (issue #101).
///
/// Held by `init` and the login/elevation services; kernel-side credentials
/// alone never confer it. The transition gate additionally refuses to grant
/// privilege the actor does not hold, so a compromised login service cannot
/// mint root or capabilities it lacks.
pub const CAP_SETUID: u32 = 1 << 6;
/// Every capability bit defined today.
pub const CAP_ALL: u32 = CAP_NET_BIND
    | CAP_NET_RAW
    | CAP_SYS_ADMIN
    | CAP_SYS_TIME
    | CAP_AUDIT_READ
    | CAP_IPC_CONTROL
    | CAP_SETUID;

/// Audit interface id for credential transitions (issue #101). The ring keys on
/// this so `auditd` can separate login/elevation records from Messenger policy
/// decisions; the method field is always [`AUDIT_METHOD_SET`].
pub const AUDIT_INTERFACE: u64 = u64::from_le_bytes(*b"os.cred.");
/// Audit method for a credential stamp (direct or spawn-time).
pub const AUDIT_METHOD_SET: u32 = 1;

/// Machine-readable reasons copied into credential-transition audit records.
pub mod reason {
    /// The transition passed every check and was applied.
    pub const TRANSITION_ALLOWED: u32 = 1;
    /// The actor does not hold [`super::credentials::CAP_SETUID`].
    pub const TRANSITION_NOT_PRIVILEGED: u32 = 2;
    /// The request would grant uid 0 or capability bits the actor lacks.
    pub const TRANSITION_WIDENING: u32 = 3;
    /// The target slot does not name the actor or a live task.
    pub const TRANSITION_BAD_TARGET: u32 = 4;
}

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

    /// The user ABI's 40-byte block: `uid, gid, caps, label_id, session`, each
    /// little-endian. `process::SYS_CREDS` and `user::sys` mirror this order.
    pub const fn to_words(self) -> [u64; 5] {
        [
            self.uid as u64,
            self.gid as u64,
            self.caps as u64,
            self.label_id as u64,
            self.session,
        ]
    }

    /// Decode the user ABI's block; extra high bits are truncated exactly as
    /// the register ABI would (the transition checks still apply).
    pub const fn from_words(words: [u64; 5]) -> Cred {
        Cred {
            uid: words[0] as u32,
            gid: words[1] as u32,
            caps: words[2] as u32,
            label_id: words[3] as u32,
            session: words[4],
        }
    }
}

/// Why a credential transition was refused. Every variant is audited with its
/// [`reason`] code before the caller sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransitionError {
    /// The actor does not hold [`CAP_SETUID`].
    NotPrivileged,
    /// The request would gain privilege the actor does not hold.
    Widening,
    /// The target is not the actor and not a live task.
    BadTarget,
}

impl TransitionError {
    /// The audit reason code for this refusal.
    pub const fn reason_code(self) -> u32 {
        match self {
            TransitionError::NotPrivileged => reason::TRANSITION_NOT_PRIVILEGED,
            TransitionError::Widening => reason::TRANSITION_WIDENING,
            TransitionError::BadTarget => reason::TRANSITION_BAD_TARGET,
        }
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

/// Give `child_slot` a copy of `parent_slot`'s credentials.
///
/// Every task-creation path calls this (or [`reset_for_task`] for a program
/// the kernel itself starts), so a child is never more privileged than the task
/// that made it and never inherits the stale identity of the dead task that
/// used to own the slot. `fork`, `clone` and the native `spawn` all inherit;
/// only the credential gate ([`transition`]) can then change the copy, and only
/// downward.
pub fn inherit(parent_slot: usize, child_slot: usize) {
    set(child_slot, of(parent_slot));
}

/// Validate a transition without applying or auditing it.
///
/// A spawn-time stamp validates up front (so the spawn can be refused before a
/// task exists) and again in [`transition`] to record the decision. Kernel-side
/// only.
pub fn check(actor_slot: usize, requested: Cred) -> Result<(), TransitionError> {
    let actor = of(actor_slot);
    if !actor.has_cap(CAP_SETUID) {
        return Err(TransitionError::NotPrivileged);
    }
    // Never mint privilege the actor does not hold: uid 0 is the system
    // identity, and capability bits only flow downward.
    if requested.uid == 0 && actor.uid != 0 {
        return Err(TransitionError::Widening);
    }
    if requested.caps & !actor.caps != 0 {
        return Err(TransitionError::Widening);
    }
    Ok(())
}

/// Whether `target` names the actor or a live task.
///
/// The kernel task (slot 0) is the multiplexer: only it may restamp itself, so
/// a privileged service cannot rewrite the kernel's own identity.
fn target_ok(actor_slot: usize, target_slot: usize) -> Result<(), TransitionError> {
    if target_slot >= MAX_TASKS
        || (target_slot == crate::task::KERNEL_TASK && actor_slot != crate::task::KERNEL_TASK)
    {
        return Err(TransitionError::BadTarget);
    }
    if target_slot != actor_slot && crate::task::snapshot(target_slot).is_none() {
        return Err(TransitionError::BadTarget);
    }
    Ok(())
}

/// Stamp `target_slot` with `requested`, validated against `actor_slot`'s
/// credentials and audited.
///
/// This is the one userspace-reachable credential write: login sets a freshly
/// spawned shell before it can run, and the elevation service stamps children.
/// Returns the applied credential. Out-of-range or dead targets, missing
/// [`CAP_SETUID`], and any widening request are refused with the matching
/// [`TransitionError`]; all three are recorded in the audit ring.
pub fn transition(
    actor_slot: usize,
    target_slot: usize,
    requested: Cred,
) -> Result<Cred, TransitionError> {
    let verdict = check(actor_slot, requested).and_then(|()| target_ok(actor_slot, target_slot));
    let actor = of(actor_slot);
    audit::record(AuditEvent {
        ticks: crate::task::ticks(),
        actor_slot,
        uid: actor.uid,
        label_id: actor.label_id,
        interface_id: AUDIT_INTERFACE,
        method: AUDIT_METHOD_SET,
        allow: verdict.is_ok(),
        reason_code: match verdict {
            Ok(()) => reason::TRANSITION_ALLOWED,
            Err(error) => error.reason_code(),
        },
        // The target slot is the correlation id: `messengerctl why` style tools
        // can trace "who stamped this process".
        txn_id: target_slot as u64,
    });
    verdict?;
    set(target_slot, requested);
    Ok(requested)
}

/// Read `target_slot`'s credentials on behalf of `actor_slot`.
///
/// A task may always read its own identity. Reading another task's is a
/// privileged operation (`CAP_SETUID`), reserved for the login/accounts
/// services that must check a client's stamped uid; it is not audited because
/// it changes nothing (transitions are).
pub fn read(actor_slot: usize, target_slot: usize) -> Result<Cred, TransitionError> {
    target_ok(actor_slot, target_slot)?;
    if target_slot == actor_slot {
        return Ok(of(actor_slot));
    }
    if !of(actor_slot).has_cap(CAP_SETUID) {
        return Err(TransitionError::NotPrivileged);
    }
    Ok(of(target_slot))
}
