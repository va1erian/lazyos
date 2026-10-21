//! Confined labels: the file, network, spawn and signal gates of a sandboxed
//! process (issue #832, `docs/architecture/process-sandbox.md`).
//!
//! A label's Messenger rules never covered the rest of what a process can do:
//! open a file, open a socket, start a program, signal another process. For an
//! ordinary `app:<id>` that is still the user's own authority (its `files` and
//! `network` entries are consent text). A **confined** label is a process
//! whose whole point is that it has less, such as a web engine's WebProcess,
//! and for it the kernel enforces what the manifest says:
//!
//! * **Files.** Every path the VFS is asked about ([`crate::fs::vfs::Id`]
//!   carries the label) is judged by [`filepolicy::allows`] against the rule
//!   ids the label holds; a refusal is `EPERM`, audited, and a `LABEL:DENY
//!   file=...` trace line. This covers the Linux ABI and the native
//!   filesystem calls alike, because both reach the VFS with the task's
//!   [`Id`](crate::fs::vfs::Id); the fabricated entries (`/etc`, `/proc`,
//!   devices) are named by the same paths and judged first by the same call.
//! * **Network.** `socket(AF_INET)` needs the label's `os.lazy.net.socket.v1`
//!   rule, which `network = ["outbound"]` compiles to, and what it gets is a
//!   client (no `listen`, no named local port); named `AF_UNIX` sockets are
//!   file-like names and need a `write` rule on the path. A *developer tool*
//!   is not confined (its files, programs and signals are the user's), but
//!   its `AF_INET` follows its rules too: [`may_open_inet`] refuses when the
//!   label's rules deny the socket interface, which is what a tool without
//!   `network` has, and allows when they do not, so a tool's own native code
//!   cannot reach the network its manifest did not declare. (A tool with the
//!   declaration keeps a full socket: `may_serve` is not asked of it.)
//! * **Spawning.** A confined task creates no process and starts no program:
//!   `fork`, `vfork`, `clone` without `CLONE_THREAD`, a native `spawnv`, the
//!   native programs `execve` starts as children, and `execve` itself are
//!   `EPERM`. Its image was loaded before its label took effect.
//! * **Signals.** A confined task signals only itself and its own threads.
//! * **Descriptors.** An image that starts confined keeps descriptors 0 to 2
//!   and connected sockets ([`scrub_inherited_descriptors`]).
//!
//! A label is confined when its rule set carries the marker
//! [`filepolicy::confine_id`] (what `pkgstore` compiles `files` to for
//! sandboxed tables), or, failing closed, when it is a *process label*: its
//! name has a `/` (`app:os.lazy.webkit/webprocess`), however its rules loaded.
//! A label that is confined holds *nothing* the manifest did not list, so a
//! process label whose rules never loaded can do nothing.
//!
//! Unconfined labels, unlabelled tasks (every service) and the kernel pay one
//! cached lookup and are otherwise untouched.
//!
//! # Cache
//!
//! The ids of a label are copied out of the rule table once per rule
//! generation ([`acl::label_generation`]) into a sorted list, so a path check
//! is a few binary searches instead of a scan of every label's rules.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use super::acl::{self, reason};
use super::audit::{self, AuditEvent};
use super::credentials;
use super::label_trace;
use super::labels;
use super::topics::fnv1a32;

pub use filepolicy::Access;

/// The interface id file refusals are audited under (`os.file.`), with
/// `fnv1a32(path)` as the method.
pub const FILE_SCOPE: u64 = u64::from_le_bytes(*b"os.file.");
/// The interface id spawn and signal refusals are audited under (`os.proc.`).
pub const PROCESS_SCOPE: u64 = u64::from_le_bytes(*b"os.proc.");

/// The socket interface a label's `network = ["outbound"]` allows
/// (`pkgstore::rules`): the permission `AF_INET` is checked against.
const SOCKET_INTERFACE: u64 = messenger_generated::os_lazy_net_socket_v1::INTERFACE_ID;

/// Methods of the [`PROCESS_SCOPE`] audit records.
mod what {
    pub const SPAWN: u32 = 1;
    pub const SIGNAL: u32 = 2;
    pub const INET: u32 = 3;
    pub const UNIX: u32 = 4;
    pub const EXEC: u32 = 5;
    pub const SERVE: u32 = 6;
}

/// What a label is, as the gates need it.
struct Snapshot {
    /// The rule generation this was built at.
    generation: u64,
    confined: bool,
    /// The rule ids the label is allowed, sorted.
    ids: Vec<u64>,
}

impl Snapshot {
    fn holds(&self, id: u64) -> bool {
        self.ids.binary_search(&id).is_ok()
    }
}

/// One snapshot per label that was asked about; bounded by the label table.
static CACHE: Mutex<BTreeMap<u32, Arc<Snapshot>>> = Mutex::new(BTreeMap::new());

/// Labels that held the confinement marker at any time since boot. A label
/// stays confined when its rules are replaced or revoked (`pkgd` loads an
/// empty list when an app is removed, and a task still running under the
/// label must not become free by that): label ids are never reused, so
/// "once confined" is a property of the label for the whole boot.
static EVER_CONFINED: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// Called by the rule loader with the rules it is about to store for
/// `label_id`: a set that carries the confinement marker makes the label
/// confined for the rest of the boot, whether or not anything asks about it
/// before the rules are replaced.
pub(super) fn note_rules(label_id: u32, rules: &[acl::Rule]) {
    let marker = filepolicy::confine_id();
    if rules
        .iter()
        .any(|rule| rule.allow && rule.method == acl::ANY_METHOD && rule.interface_id == marker)
    {
        EVER_CONFINED.lock().insert(label_id);
    }
}

/// Whether `label_id` names a process label: `app:<id>/<name>`.
fn is_process_label(label_id: u32) -> bool {
    labels::with(label_id, |label| label.contains('/')).unwrap_or(false)
}

fn snapshot(label_id: u32) -> Arc<Snapshot> {
    // Read first: a load that races with the rebuild below leaves the stored
    // generation behind the table's, so the next call rebuilds again.
    let generation = acl::label_generation();
    if let Some(cached) = CACHE.lock().get(&label_id) {
        if cached.generation == generation {
            return Arc::clone(cached);
        }
    }
    let ids = acl::label_allowed_ids(label_id);
    if ids.binary_search(&filepolicy::confine_id()).is_ok() {
        EVER_CONFINED.lock().insert(label_id);
    }
    let confined = EVER_CONFINED.lock().contains(&label_id) || is_process_label(label_id);
    let built = Arc::new(Snapshot {
        generation,
        confined,
        ids,
    });
    CACHE.lock().insert(label_id, Arc::clone(&built));
    built
}

/// Whether `label_id` is confined (see the module docs). `0` never is.
pub fn confined(label_id: u32) -> bool {
    label_id != 0 && snapshot(label_id).confined
}

/// Whether the task in `slot` runs under a confined label.
pub fn slot_confined(slot: usize) -> bool {
    confined(credentials::of(slot).label_id)
}

/// A refused access. The refusal is already audited and traced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Denied;

/// Judge `access` to the absolute, normalised `path` for a task labelled
/// `label_id`. `Ok` for every label that is not confined.
pub fn check_path(label_id: u32, path: &str, access: Access) -> Result<(), Denied> {
    if label_id == 0 {
        return Ok(());
    }
    let snap = snapshot(label_id);
    if !snap.confined {
        return Ok(());
    }
    if filepolicy::allows(path, access, &|id| snap.holds(id)) {
        return Ok(());
    }
    let mode = match access {
        Access::Stat => "stat",
        Access::Read => "read",
        Access::Write => "write",
    };
    refuse(
        crate::task::current(),
        label_id,
        FILE_SCOPE,
        fnv1a32(path),
        reason::LABEL_DEFAULT_DENY,
        format_args!("file={} access={mode}", Printable(path)),
    );
    Err(Denied)
}

/// A path as one line of a trace: a sandboxed task chose the name, and a newline in it must not
/// start a line of its own (a forged `LABEL:DENY`), so control characters print as `?`.
struct Printable<'a>(&'a str);

impl core::fmt::Display for Printable<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use core::fmt::Write;
        for ch in self.0.chars() {
            f.write_char(if ch.is_control() { '?' } else { ch })?;
        }
        Ok(())
    }
}

/// The access a VFS permission mask asks for (`vfs::READ`/`WRITE`/`EXECUTE`,
/// `0` for a lookup).
pub fn access_of_mask(mask: u8) -> Access {
    const WRITE: u8 = 2;
    if mask == 0 {
        Access::Stat
    } else if mask & WRITE != 0 {
        Access::Write
    } else {
        Access::Read
    }
}

/// Audit and trace one refusal of the task in `slot`, as `reason_code`: a
/// confined label is refused by default (nothing allowed it,
/// `reason::LABEL_DEFAULT_DENY`); a tool's socket is refused by the rule
/// engine, whose verdict says whether a deny rule matched (`EXPLICIT_DENY`) or
/// none did (a revoked tool: default deny).
fn refuse(
    slot: usize,
    label_id: u32,
    interface_id: u64,
    method: u32,
    reason_code: u32,
    what: core::fmt::Arguments<'_>,
) {
    let cred = credentials::of(slot);
    label_trace::denied(label_id, what);
    audit::record(AuditEvent {
        ticks: crate::task::ticks(),
        actor_slot: slot,
        uid: cred.uid,
        label_id,
        interface_id,
        method,
        allow: false,
        reason_code,
        txn_id: 0,
    });
}

/// Whether the current task may create a process. A confined one may not;
/// the refusal is audited and traced as `spawn`.
pub fn may_spawn() -> bool {
    let slot = crate::task::current();
    let label_id = credentials::of(slot).label_id;
    if !confined(label_id) {
        return true;
    }
    refuse(
        slot,
        label_id,
        PROCESS_SCOPE,
        what::SPAWN,
        reason::LABEL_DEFAULT_DENY,
        format_args!("spawn=process"),
    );
    false
}

/// Whether the current task may `execve`. A confined label starts no
/// program at all: its image was loaded before the label took effect, and a
/// sandbox that could replace itself with a file it wrote would only have
/// found a way around the executable bit of what it may read.
pub fn may_exec() -> bool {
    let slot = crate::task::current();
    let label_id = credentials::of(slot).label_id;
    if !confined(label_id) {
        return true;
    }
    refuse(
        slot,
        label_id,
        PROCESS_SCOPE,
        what::EXEC,
        reason::LABEL_DEFAULT_DENY,
        format_args!("spawn=exec"),
    );
    false
}

/// The descriptor step of an image that starts confined: when the current
/// task's label is confined, everything above the three standard streams that
/// is not a connected socket is closed (a file the launcher left open without
/// `FD_CLOEXEC` would otherwise be a way to read it, because a descriptor's
/// access was decided when its owner opened it). Returns how many were
/// closed; `0` for an unconfined task.
pub fn scrub_inherited_descriptors() -> usize {
    if !slot_confined(crate::task::current()) {
        return 0;
    }
    let closed = crate::task::fd_close_all_but_sockets();
    // Standard input too: it is whatever the launcher's terminal or pipe delivers.
    crate::process::linux::null_stdin();
    closed
}

/// Whether the current task may bind or connect the named `AF_UNIX` socket
/// `name` (`sun_path`): the namespace of those names is global, not the
/// file system's, so a confined task needs a write rule for the path it
/// names, and an abstract name (leading NUL) is never allowed.
pub fn check_socket_name(name: &[u8]) -> Result<(), Denied> {
    let slot = crate::task::current();
    let label_id = credentials::of(slot).label_id;
    if !confined(label_id) {
        return Ok(());
    }
    match core::str::from_utf8(name) {
        Ok(path) if path.starts_with('/') => check_path(label_id, path, Access::Write),
        _ => {
            refuse(
                slot,
                label_id,
                PROCESS_SCOPE,
                what::UNIX,
                reason::LABEL_DEFAULT_DENY,
                format_args!("unix=unnamed-or-abstract"),
            );
            Err(Denied)
        }
    }
}

/// Whether the current task may open an `AF_INET` socket: an unconfined task
/// always, a confined one only with the socket interface in its rules (the
/// `network = ["outbound"]` permission), and a developer tool as its rules
/// judge the socket interface.
///
/// A tool (`tool:<id>`, `super::tool`) is not confined, but its manifest's
/// `network` entry is the only thing that lifts the explicit deny of the
/// socket interface from its default-allow rules (`devtool::DENY`), and the
/// consent text promises "no network unless listed". So the same rule that
/// refuses its Messenger calls to `os.lazy.net.socket.v1` refuses a raw
/// `socket(AF_INET)` of its own native code (and of every child, which
/// inherits the label): the verdict is the label's rules for the socket
/// interface, evaluated as any Messenger call would be. A revoked tool (an
/// empty rule list) is default-denied, so it loses sockets with everything
/// else; the Terminal, a shell tool that declares `network`, keeps them.
pub fn may_open_inet() -> bool {
    let slot = crate::task::current();
    let label_id = credentials::of(slot).label_id;
    if label_id == 0 {
        return true;
    }
    let mut why = reason::LABEL_DEFAULT_DENY;
    if super::tool::is_tool(label_id) {
        // The verdict's own reason: the deny rule a plain tool compiles to
        // (`EXPLICIT_DENY`), or default deny for a revoked one.
        let (decision, code) = acl::evaluate_label(label_id, SOCKET_INTERFACE, acl::ANY_METHOD);
        if !decision.denied() {
            return true;
        }
        why = code;
    } else {
        let snap = snapshot(label_id);
        if !snap.confined || snap.holds(SOCKET_INTERFACE) {
            return true;
        }
    }
    refuse(
        slot,
        label_id,
        PROCESS_SCOPE,
        what::INET,
        why,
        format_args!("net=inet"),
    );
    false
}

/// Whether the current task may serve on the network: bind a named port or listen. The
/// `network` permission is `outbound` (v1), so a confined task is a client only: it may bind
/// port `0` (the ephemeral local port a resolver or a connect picks) and never listen.
pub fn may_serve() -> bool {
    let slot = crate::task::current();
    let label_id = credentials::of(slot).label_id;
    if !confined(label_id) {
        return true;
    }
    refuse(
        slot,
        label_id,
        PROCESS_SCOPE,
        what::SERVE,
        reason::LABEL_DEFAULT_DENY,
        format_args!("net=serve"),
    );
    false
}

/// Whether the task in `caller` may signal `target`, beyond the uid rule: a
/// confined task signals only a task of its own process (itself, its threads).
/// `same_process` says whether `target` shares the caller's address space.
pub fn may_signal(caller: usize, same_process: bool) -> bool {
    let label_id = credentials::of(caller).label_id;
    if same_process || !confined(label_id) {
        return true;
    }
    refuse(
        caller,
        label_id,
        PROCESS_SCOPE,
        what::SIGNAL,
        reason::LABEL_DEFAULT_DENY,
        format_args!("signal=other"),
    );
    false
}

/// Forget every snapshot and every label's history. Test isolation only.
#[cfg(lazyos_tests)]
pub fn reset_for_tests() {
    CACHE.lock().clear();
    EVER_CONFINED.lock().clear();
}
