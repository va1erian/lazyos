//! Child labels: helper programs of an installed app that run with less
//! (issue #832, `docs/security-model.md` section 2, `docs/packages.md`
//! "Process tables").
//!
//! An app that starts a helper it does not trust as much as itself (a browser's
//! page process, which runs whatever a website sends) declares it in its
//! manifest: `[processes.<name>]` with the helper's `program` and the subset
//! of the app's permissions it needs. `pkgd` loads that as the label
//! `app:<system_name>/<name>` ([`super::labels::Kind::Child`]) with its rules
//! and the program (`LoadLabel`'s `program`), after the app's own label.
//!
//! The kernel rule, in full:
//!
//! * **Entering.** When a task labelled `app:<id>` `execve`s a program whose
//!   path equals the `program` bound to one of that label's children, the new
//!   image takes the child's label (same uid, gid, capabilities and session).
//!   Nothing else gives a task a child label: no other path, no other app's
//!   child (the lookup is keyed by the caller's own label), and a task already
//!   in a child label has no children to enter. A child of a labelled task that
//!   is not exec'd as that program keeps the parent label, as before.
//! * **Holding.** A child is allowed a call only when its own rules *and* its
//!   app's rules allow it (`acl::evaluate_label`), and the rules are refused
//!   at load unless the app's allow them (`acl::covered_by`): a child never
//!   holds more than the app, whatever `pkgd` loaded, and revoking the app
//!   revokes every child at once. A child owns no service name and no topic
//!   (the implicit `app.<id>.*` and `app/<id>/` namespaces belong to the
//!   `app:<id>` label alone).
//! * **Starting.** A task in a child label may not start a program: `execve`,
//!   `fork`, a `vfork`-style `clone` and `spawnv` are refused (`EPERM`,
//!   audited, a `LABEL:DENY ... start=<call>` line under `LAZYOS_LABEL_TRACE`).
//!   Threads are not programs and stay allowed.
//! * **Leaving.** Loading a child with no rules and no program unbinds it, and
//!   so does loading an empty rule list for the app: a package's children go
//!   with it. A task that still holds the label then has no rules and is denied
//!   everything.
//!
//! The bindings are a bounded table; the child-to-app link is the label's own
//! (`labels::parent_of`), so there is nothing here to keep consistent with it.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use super::acl::reason;
use super::audit::{self, AuditEvent};
use super::credentials::{self, Cred, LabelStamp, TransitionError};
use super::devspawn::SPAWN_SCOPE;
use super::labels::{self, Kind};
use super::topics::fnv1a32;

/// Most program bindings the kernel keeps (an app may declare at most four
/// children; the label table's capacity bounds everything else).
pub const MAX_BINDINGS: usize = 64;
/// Longest bound program path, in bytes (`spawnv`'s `PATH_MAX`).
pub const MAX_PROGRAM_BYTES: usize = 255;

/// One bound program: the child label that takes it.
struct Binding {
    child: u32,
    program: String,
}

static BINDINGS: Mutex<Vec<Binding>> = Mutex::new(Vec::new());

/// Why a program could not be bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindError {
    /// Not an absolute, canonical path of at most [`MAX_PROGRAM_BYTES`].
    BadProgram,
    /// [`MAX_BINDINGS`] programs are bound already.
    Full,
}

/// Whether `program` is an absolute path of non-empty segments, none of them
/// `.` or `..` and none containing a NUL, at most [`MAX_PROGRAM_BYTES`] long.
/// `execve` hands the kernel a canonical path, so a bound path must already be
/// one, or it could never match.
pub fn valid_program(program: &str) -> bool {
    let Some(rest) = program.strip_prefix('/') else {
        return false;
    };
    program.len() <= MAX_PROGRAM_BYTES
        && rest.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && !segment.bytes().any(|b| b == 0)
        })
}

/// Whether a binding for `child` (an existing label id, or `None` for one not
/// interned yet) fits the table: it replaces its own earlier binding, or there
/// is a free entry. [`bind`] checks again under the lock.
pub fn has_room_for(child: Option<u32>) -> bool {
    let bindings = BINDINGS.lock();
    bindings.len() < MAX_BINDINGS || child.is_some_and(|c| bindings.iter().any(|b| b.child == c))
}

/// Bind `program` to the child label `child`, replacing its earlier binding.
///
/// A program enters one child of an app, so a binding of the same program to
/// another child of the same app is taken over: the loader loads a new
/// version's tables after the old version's, and a table renamed in an upgrade
/// must not leave the program bound to a label the new version no longer
/// declares. Returns the displaced children, whose rules the caller clears.
pub fn bind(child: u32, program: &str) -> Result<Vec<u32>, BindError> {
    if !valid_program(program) {
        return Err(BindError::BadProgram);
    }
    // Candidates under the lock; the label table is asked after it is released.
    let same: Vec<u32> = BINDINGS
        .lock()
        .iter()
        .filter(|binding| binding.program == program && binding.child != child)
        .map(|binding| binding.child)
        .collect();
    let parent = labels::parent_of(child);
    let displaced: Vec<u32> = same
        .into_iter()
        .filter(|other| parent.is_some() && labels::parent_of(*other) == parent)
        .collect();
    let mut bindings = BINDINGS.lock();
    let replacing = bindings.iter().any(|binding| binding.child == child);
    let freed = bindings
        .iter()
        .filter(|binding| displaced.contains(&binding.child))
        .count();
    if !replacing && bindings.len() - freed >= MAX_BINDINGS {
        return Err(BindError::Full);
    }
    bindings.retain(|binding| !displaced.contains(&binding.child));
    match bindings.iter_mut().find(|b| b.child == child) {
        Some(existing) => existing.program = String::from(program),
        None => bindings.push(Binding {
            child,
            program: String::from(program),
        }),
    }
    Ok(displaced)
}

/// Forget `child`'s program, if it has one.
pub fn unbind(child: u32) {
    BINDINGS.lock().retain(|binding| binding.child != child);
}

/// Forget every program bound to a child of the app label `parent` and return
/// those child labels, so their rules can be revoked with them.
pub fn unbind_children_of(parent: u32) -> Vec<u32> {
    let all: Vec<u32> = BINDINGS.lock().iter().map(|b| b.child).collect();
    let ours: Vec<u32> = all
        .into_iter()
        .filter(|child| labels::parent_of(*child) == Some(parent))
        .collect();
    BINDINGS.lock().retain(|b| !ours.contains(&b.child));
    ours
}

/// The program bound to `child`.
pub fn program_of(child: u32) -> Option<String> {
    BINDINGS
        .lock()
        .iter()
        .find(|binding| binding.child == child)
        .map(|binding| binding.program.clone())
}

/// Number of bound programs.
pub fn count() -> usize {
    BINDINGS.lock().len()
}

/// The child label a task with `actor`'s credentials takes by `execve`ing
/// `path`: bound to `path`, and a child of the actor's own label. `None` for
/// an unlabelled actor, a path nothing is bound to, and another app's child.
pub fn label_for_exec(actor: &Cred, path: &str) -> Option<u32> {
    if actor.label_id == 0 {
        return None;
    }
    let candidates: Vec<u32> = BINDINGS
        .lock()
        .iter()
        .filter(|binding| binding.program == path)
        .map(|binding| binding.child)
        .collect();
    candidates
        .into_iter()
        .find(|child| labels::parent_of(*child) == Some(actor.label_id))
}

/// Re-check a child stamp at the moment it is applied: `requested` is the
/// actor's own identity with a label that is a bound child of the actor's.
pub fn recheck(actor: &Cred, requested: &Cred) -> Result<(), TransitionError> {
    let same_identity = requested.uid == actor.uid
        && requested.gid == actor.gid
        && requested.caps == actor.caps
        && requested.session == actor.session;
    let child = requested.label_id;
    let entered = actor.label_id != 0
        && labels::parent_of(child) == Some(actor.label_id)
        && program_of(child).is_some();
    if same_identity && entered {
        Ok(())
    } else {
        Err(TransitionError::LabelLocked)
    }
}

/// What `execve`ing `path` does to the label of the task in `slot`: it takes
/// the child label bound to the path, if any. `Ok(None)` when the path is not
/// a child's program; an `Err` must fail the `execve` (the process would
/// otherwise run with the wider label its app holds). The stamp goes through
/// the credential gate, so it is audited like every other transition.
pub fn enter_on_exec(slot: usize, path: &str) -> Result<Option<u32>, TransitionError> {
    let actor = credentials::of(slot);
    let Some(child) = label_for_exec(&actor, path) else {
        return Ok(None);
    };
    let requested = Cred {
        label_id: child,
        ..actor
    };
    credentials::transition_with(slot, slot, requested, LabelStamp::Child)?;
    super::label_trace::entered(slot, child, path);
    Ok(Some(child))
}

/// Whether the task in `slot` is in a child label.
pub fn is_child(slot: usize) -> bool {
    let label = credentials::of(slot).label_id;
    label != 0 && labels::kind_of(label) == Some(Kind::Child)
}

/// Whether the task in `slot` may start a program (`what` names the call:
/// `execve`, `fork`, `vfork`, `spawnv`). A task in a child label may not; the
/// refusal is audited and traced, and the caller returns `EPERM`.
pub fn may_start(slot: usize, what: &str) -> bool {
    if !is_child(slot) {
        return true;
    }
    let cred = credentials::of(slot);
    let method = fnv1a32(what);
    super::label_trace::denied(cred.label_id, format_args!("start={what}"));
    audit::record(AuditEvent {
        ticks: crate::task::ticks(),
        actor_slot: slot,
        uid: cred.uid,
        label_id: cred.label_id,
        interface_id: SPAWN_SCOPE,
        method,
        allow: false,
        reason_code: reason::CHILD_MAY_NOT_START,
        txn_id: 0,
    });
    false
}

/// Drop every binding. Test isolation only.
#[cfg(lazyos_tests)]
pub fn reset_for_tests() {
    BINDINGS.lock().clear();
}
