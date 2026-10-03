//! Spawning into a development label (issue #529,
//! `docs/lazyrad-package-plan.md` section 3).
//!
//! An IDE that is itself a package (`app:os.lazy.lazyrad`) runs the project
//! being edited as a child it keeps the pipes of. A child inherits its
//! creator's label, so without this the project would run under the IDE's
//! permissions. Instead the IDE asks `spawnv` (`AS_LABELLED`) for the label
//! `dev:<system_name>`: the project then owns the installed app's names and
//! topics ([`super::policy`]) and is judged by the rules `pkgd` loaded for that
//! label after the user approved them in the Installer.
//!
//! Only the credential gate's unlabelled `CAP_SETUID` holders could ever assign
//! a label before. This is the one other way, and it is narrow:
//!
//! * the target must be a `dev:` label (never `app:` or `system:`);
//! * the caller's own label rules must allow [`SPAWN_SCOPE`] with
//!   `fnv1a32(target label)` as the method (a manifest's `develop = true`),
//!   the pattern of `os.lazy.messenger.names.resolve.v1`;
//! * the target label must already exist and hold rules: `pkgd` loads an
//!   approved set (always at least one rule) and revokes it by loading none,
//!   so an unapproved or revoked label cannot be entered, and this path never
//!   interns a label;
//! * the child keeps the caller's uid, gid and session, and its capabilities
//!   are a subset of the caller's ([`check_identity`]).
//!
//! Every refusal is audited (the scope's interface id, the hashed label as the
//! method) and, with `LAZYOS_LABEL_TRACE=1`, printed as a `LABEL:DENY` line, so
//! an IDE's missing `develop = true` shows up in a trace run like any other
//! missing permission.

use super::acl::{self, reason};
use super::audit::{self, AuditEvent};
use super::credentials::{Cred, TransitionError};
use super::labels::{self, Kind};
use super::topics::fnv1a32;

/// The generated scope interface (`idl/policy.midl`).
pub use messenger_generated::os_lazy_process_label_spawn_v1 as scope;

/// Policy scope a labelled task's spawn into a `dev:` label is evaluated
/// against.
pub const SPAWN_SCOPE: u64 = scope::INTERFACE_ID;

const _: () = assert!(super::topics::fnv1a64("os.lazy.process.label.spawn.v1") == SPAWN_SCOPE);

/// Why a spawn into a `dev:` label was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The caller's rules do not allow that label, or the label has no
    /// approved rule set (never approved, or revoked).
    NotAllowed,
    /// The request changes uid, gid or session, or adds a capability.
    Widening,
}

impl From<Refusal> for TransitionError {
    fn from(refusal: Refusal) -> TransitionError {
        match refusal {
            Refusal::NotAllowed => TransitionError::DevNotAllowed,
            Refusal::Widening => TransitionError::Widening,
        }
    }
}

/// Whether a spawn of `label` by a task with `actor`'s credentials takes the
/// development path: a labelled caller naming a `dev:` label. Everything else
/// (unlabelled callers, `app:`/`system:` targets) keeps the credential gate's
/// rules unchanged.
pub fn applies(actor: &Cred, label: &str) -> bool {
    actor.label_id != 0 && matches!(labels::parse(label), Ok((Kind::Dev, _)))
}

/// The child's identity may only narrow the caller's: same uid, gid and
/// session, capabilities a subset.
fn check_identity(actor: &Cred, requested: &Cred) -> Result<(), Refusal> {
    let same = requested.uid == actor.uid
        && requested.gid == actor.gid
        && requested.session == actor.session;
    if same && requested.caps & !actor.caps == 0 {
        Ok(())
    } else {
        Err(Refusal::Widening)
    }
}

/// Whether `actor`'s rules allow `label` and the label holds an approved rule
/// set. Returns the label's id. Pure: nothing is interned or audited.
fn check_policy(actor: &Cred, label: &str) -> Result<u32, Refusal> {
    let (decision, _) = acl::evaluate_label(actor.label_id, SPAWN_SCOPE, fnv1a32(label));
    if decision.denied() {
        return Err(Refusal::NotAllowed);
    }
    let id = labels::lookup(label).ok_or(Refusal::NotAllowed)?;
    if acl::label_rule_count(id) == 0 {
        return Err(Refusal::NotAllowed);
    }
    Ok(id)
}

/// Decide a spawn of `label` with `requested` credentials (its `label_id` is
/// ignored) by the task in `actor_slot`, auditing a refusal. Returns the
/// credential to stamp on the child, its `label_id` the target's.
///
/// Callers check [`applies`] first.
pub fn approve(actor_slot: usize, requested: Cred, label: &str) -> Result<Cred, Refusal> {
    let actor = super::credentials::of(actor_slot);
    let verdict = check_policy(&actor, label).and_then(|id| {
        check_identity(&actor, &requested)?;
        Ok(Cred {
            label_id: id,
            ..requested
        })
    });
    if let Err(refusal) = verdict {
        record_refusal(actor_slot, &actor, label, refusal);
    }
    verdict
}

/// Re-check a stamp at the moment it is applied (the child exists by then):
/// the same rules as [`approve`], without the audit record.
pub fn recheck(actor: &Cred, requested: &Cred) -> Result<(), Refusal> {
    let label = labels::name_of(requested.label_id).ok_or(Refusal::NotAllowed)?;
    if !applies(actor, &label) {
        return Err(Refusal::NotAllowed);
    }
    check_identity(actor, requested)?;
    check_policy(actor, &label).map(|_| ())
}

/// Audit one refusal (and trace it when the label trace is on).
fn record_refusal(actor_slot: usize, actor: &Cred, label: &str, refusal: Refusal) {
    let method = fnv1a32(label);
    #[cfg(lazyos_label_trace)]
    super::label_trace::denied(
        actor.label_id,
        format_args!("iface={SPAWN_SCOPE:#018x} method={method} spawn={label}"),
    );
    audit::record(AuditEvent {
        ticks: crate::task::ticks(),
        actor_slot,
        uid: actor.uid,
        label_id: actor.label_id,
        interface_id: SPAWN_SCOPE,
        method,
        allow: false,
        reason_code: match refusal {
            Refusal::NotAllowed => reason::LABEL_DEFAULT_DENY,
            Refusal::Widening => super::credentials::reason::TRANSITION_WIDENING,
        },
        txn_id: 0,
    });
}
