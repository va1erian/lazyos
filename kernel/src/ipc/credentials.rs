//! Kernel credential stamping (issue #68) and the audited transition gate
//! (issue #101).
//!
//! Every Messenger call carries an identity the sender cannot forge: the kernel
//! stamps `uid/gid/caps/label/session` from this registry, and no syscall or
//! parcel field can write them back (`docs/security-model.md` section 2 and
//! `docs/messenger.md` section 9). The stamp is what the ACL hook and the audit
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
//! # Labels (application package system, phase 1)
//!
//! `label_id` names an interned string ([`super::labels`]) such as
//! `app:com.example.notes`. A label only ever goes from `0` to a value, once,
//! on a *child being created* by a [`CAP_SETUID`] holder (the labelled spawn,
//! [`LabelStamp::Assign`]); every other stamp must keep the label it lands on
//! ([`LabelStamp::Keep`]), so a task cannot relabel itself, strip its label
//! or hand a different one to a peer. The one other assignment is a labelled
//! IDE's spawn into a `dev:` label ([`LabelStamp::Develop`],
//! [`super::devspawn`], issue #529): no `CAP_SETUID`, but the caller's own
//! label rules must allow that exact label, and the child keeps the caller's
//! uid, gid, session and (at most) capabilities. Children inherit their
//! creator's label like the rest of the credential, which keeps an app's
//! helpers inside its sandbox.
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
/// Send signals to tasks running under a different uid (`kill`/`tkill`/
/// `tgkill`); without it a sender may only signal its own uid's tasks.
pub const CAP_KILL: u32 = 1 << 7;
/// Attempt to claim or list devices at all (`dev_*`, issue #240). Coarse on
/// purpose: it is only the gate; authority over a specific device is the
/// `Device` handle `claim` returns, whose rights the class ACL rule
/// (`os.kernel.dev.<class>`) bounds.
pub const CAP_DEV_CLAIM: u32 = 1 << 8;
/// Drain the raw input event bus (syscall 25, `docs/input-plan.md`). Every
/// keystroke on the machine passes through it, so `init` stamps it onto
/// `inputd` alone and strips it from every other service it starts.
pub const CAP_INPUT_RAW: u32 = 1 << 9;
/// Publish onto the raw input bus as a registered source (syscall 25 ops
/// 4-6, `docs/usb-hid-plan.md` U1): what an input driver (`usbd`) needs. It
/// grants no reading; the kernel stamps each source's device id, so a holder
/// cannot pose as another device.
pub const CAP_INPUT_SOURCE: u32 = 1 << 10;
/// Serve a block device to the kernel from user space (syscall 33 ops 0-3,
/// `block::provider`, docs/architecture/usb-storage.md): what the USB
/// storage driver (`usbd`) needs. The kernel also requires the caller's uid
/// to be a block-provider uid (`usbpolicy::BLOCK_PROVIDER_UIDS`).
pub const CAP_BLOCK_PROVIDER: u32 = 1 << 11;
/// Serve a directory tree to the VFS from user space and mount it under
/// `/mnt` (syscall 35, `fs::fuse`, docs/smb-plan.md F1): what a
/// user-space filesystem daemon (`memfuse`, later `smbfuse`) needs.
pub const CAP_FS_PROVIDER: u32 = 1 << 12;
/// Claim the login console's keyboard (syscall 25 ops 7-8,
/// `input::console`, issue #396): while the holder's claim stands, typed
/// keys reach it through `inputd`'s sessionless input session and are kept
/// off the kernel terminal queue. `init` stamps it onto `logind` alone.
pub const CAP_INPUT_CONSOLE: u32 = 1 << 13;
/// Every capability bit defined today.
pub const CAP_ALL: u32 = CAP_NET_BIND
    | CAP_NET_RAW
    | CAP_SYS_ADMIN
    | CAP_SYS_TIME
    | CAP_AUDIT_READ
    | CAP_IPC_CONTROL
    | CAP_SETUID
    | CAP_KILL
    | CAP_DEV_CLAIM
    | CAP_INPUT_RAW
    | CAP_INPUT_SOURCE
    | CAP_BLOCK_PROVIDER
    | CAP_FS_PROVIDER
    | CAP_INPUT_CONSOLE;

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
    /// The request would change a label a task already carries, or set one
    /// anywhere but on a child being created.
    pub const TRANSITION_LABEL_LOCKED: u32 = 5;
    /// A labelled task's spawn into a `dev:` label its rules do not allow, or
    /// one that holds no approved rule set ([`super::super::devspawn`]).
    pub const TRANSITION_DEV_NOT_ALLOWED: u32 = 6;
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

    /// The uid identity the legacy ACL keys on. A labelled task is keyed by
    /// [`Cred::label_id`] instead ([`super::acl::evaluate_cred`]).
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
    /// The request would change a label (labels go from `0` to a value once,
    /// on a child being created, and never change afterwards).
    LabelLocked,
    /// A spawn into a `dev:` label the caller's rules do not allow, or that
    /// holds no approved rule set.
    DevNotAllowed,
}

impl TransitionError {
    /// The audit reason code for this refusal.
    pub const fn reason_code(self) -> u32 {
        match self {
            TransitionError::NotPrivileged => reason::TRANSITION_NOT_PRIVILEGED,
            TransitionError::Widening => reason::TRANSITION_WIDENING,
            TransitionError::BadTarget => reason::TRANSITION_BAD_TARGET,
            TransitionError::LabelLocked => reason::TRANSITION_LABEL_LOCKED,
            TransitionError::DevNotAllowed => reason::TRANSITION_DEV_NOT_ALLOWED,
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

/// [`of`] without waiting: `None` while another context holds the table.
/// The scheduler's tick reads identities through this, since it may
/// interrupt a kernel thread that is in the middle of [`set`].
pub fn try_of(slot: usize) -> Option<Cred> {
    Some(CREDS.try_lock()?.get(slot).copied().unwrap_or(Cred::ROOT))
}

/// Replace `slot`'s credentials. `Kernel-only`: profiles (init/messengerd) and
/// the elevation service call this; nothing reachable from a syscall or parcel
/// does. Out-of-range slots are ignored.
pub fn set(slot: usize, cred: Cred) {
    if let Some(entry) = CREDS.lock().get_mut(slot) {
        *entry = cred;
    }
    // A new identity starts in good standing with the CPU quota until its
    // first booked tick (issue #483).
    crate::quota::cpu::forget_slot(slot);
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

/// Remove capability bits from `slot`, keeping everything else. `Kernel-only`,
/// like [`set`]: the boot path uses it to withhold `CAP_INPUT_RAW` from every
/// program it starts except `init`, which delegates it to `inputd` alone.
pub fn drop_caps(slot: usize, caps: u32) {
    if let Some(entry) = CREDS.lock().get_mut(slot) {
        entry.caps &= !caps;
    }
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

/// How a stamp treats the label of the task it lands on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LabelStamp {
    /// The label must stay exactly `current` (the target's label, or the label
    /// a new child inherits from its creator). Every ordinary stamp.
    Keep { current: u32 },
    /// A labelled spawn: the child gets `requested.label_id`. Only a creator
    /// that is unlabelled (or already in that very label) may assign it, so a
    /// labelled task can never hop to another label.
    Assign,
    /// A labelled task's spawn into a `dev:` label ([`super::devspawn`]): no
    /// `CAP_SETUID`, but the caller's rules must allow the label and the child
    /// keeps the caller's identity, capabilities at most narrowed.
    Develop,
}

/// Validate a transition without applying or auditing it.
///
/// A spawn-time stamp validates up front (so the spawn can be refused before a
/// task exists) and again in [`transition`] to record the decision. The label
/// is held fixed at the actor's own (what a plain spawn inherits); see
/// [`check_stamp`] for the other shapes. Kernel-side only.
pub fn check(actor_slot: usize, requested: Cred) -> Result<(), TransitionError> {
    let current = of(actor_slot).label_id;
    check_stamp(actor_slot, LabelStamp::Keep { current }, requested)
}

/// [`check`] with an explicit label rule.
pub fn check_stamp(
    actor_slot: usize,
    label: LabelStamp,
    requested: Cred,
) -> Result<(), TransitionError> {
    let actor = of(actor_slot);
    if label == LabelStamp::Develop {
        return super::devspawn::recheck(&actor, &requested).map_err(TransitionError::from);
    }
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
    let label_ok = match label {
        LabelStamp::Keep { current } => requested.label_id == current,
        LabelStamp::Assign => {
            requested.label_id != 0 && (actor.label_id == 0 || actor.label_id == requested.label_id)
        }
        // Decided above.
        LabelStamp::Develop => false,
    };
    if !label_ok {
        return Err(TransitionError::LabelLocked);
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
    let label = LabelStamp::Keep {
        current: of(target_slot).label_id,
    };
    transition_with(actor_slot, target_slot, requested, label)
}

/// [`transition`] with an explicit label rule: the labelled-spawn path stamps
/// the new child with [`LabelStamp::Assign`].
pub fn transition_with(
    actor_slot: usize,
    target_slot: usize,
    requested: Cred,
    label: LabelStamp,
) -> Result<Cred, TransitionError> {
    let verdict =
        check_stamp(actor_slot, label, requested).and_then(|()| target_ok(actor_slot, target_slot));
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
