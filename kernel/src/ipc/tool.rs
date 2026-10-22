//! Developer-tool labels, the kernel's part (`tool:<id>`,
//! `docs/devtool-packages-plan.md` phase T2).
//!
//! A tool is an app whose calls are chosen by the user (a scripting window, an
//! inspector), so its Messenger rules cannot be a manifest's exact list: `pkgd`
//! loads *default allow minus a short deny list* for it, and the rule engine
//! ([`super::acl`]) needs nothing new for that. What the rules cannot say is
//! which *names* and *topics* a tool may use, because those are checked before
//! (and apart from) the rules. The kernel treats a `tool:` task as a user task
//! that happens to carry a name:
//!
//! * **Services** ([`super::policy`]). It owns `app.<id>.<name>` for its own id
//!   like an `app:` label, and may also register any name that is not
//!   reserved: not `os.lazy.*`, not another app's `app.<id>.*`, not the NIC
//!   and Wi-Fi namespaces (their special cases come first and are unchanged).
//!   An unlabelled user task gets `BOOTSTRAP_ALLOW` there; a tool gets
//!   `ALLOWED_BY_NAMESPACE`. The interface-domain rule of issue #495 does not
//!   apply, so a tool may serve `os.lazy.echo.v1` under `demo.pirate`; a
//!   registration whose names do not hash to its ids is still refused.
//!   Both of these exist **only while the tool's rules are loaded**
//!   ([`holds_rules`]): before `pkgd` loads them, and again after it revokes
//!   (an empty list), the label is an `app:` label of the same id, no more: it
//!   keeps the implicit grants every `app:` label has (its own `app.<id>.*`
//!   names with the domain rule, its own `app/<id>/` topics, resolving its own
//!   names), and a free name is `OUTSIDE_NAMESPACE`. What a revoke does not do
//!   is unregister a name the process already holds; `pkgd` ends the process.
//! * **Topics** ([`topic_reserved`]). `app/<id>/` is the tool's own (as for an
//!   app); `user/<uid>/` stays the uid's alone (`topics::private`, checked
//!   first); every other topic is open to the label's rules, except what a
//!   tool must not forge or read through a wildcard: the `app/` tree of other
//!   apps (any mode), and publishing under `system/` (the platform's events)
//!   and `time/` (`timed`'s retained `time/tick`, which every clock reader
//!   trusts). What a tool can still publish is its own user's and session's
//!   topics (`user/<own uid>/...`, `session/<n>/...`, as the unlabelled
//!   Terminal could) and names nobody reserved; none of them carries the
//!   platform's word. An unlabelled user task is limited by nothing here, so
//!   this only narrows.
//! * **Children.** A spawned or forked child inherits the label like any
//!   label. A tool never has child labels (`tool:<id>/<name>` is malformed,
//!   [`super::labels::parent_of`] is `None`), so the `execve` entry of
//!   [`super::children`] never applies to it.
//! * **Spawning into `dev:`** ([`super::devspawn`]) is the same rule as for
//!   an app: the tool's rules must allow the spawn scope, which the deny list
//!   refuses unless the package declared `develop`.

use super::acl::reason;
use super::audit::{self, AuditEvent};
use super::credentials::Cred;
use super::labels::{self, Kind};

/// First topic segment of the per-application namespace (`app/<id>/...`).
const APP_ROOT: &str = "app";
/// First topic segment of the platform's events (`system/events/...`).
const SYSTEM_ROOT: &str = "system";
/// First topic segment of the clock's retained tick (`time/tick`, `timed`).
const TIME_ROOT: &str = "time";

/// Whether `label_id` is a developer-tool label.
pub fn is_tool(label_id: u32) -> bool {
    labels::kind_of(label_id) == Some(Kind::Tool)
}

/// Whether `label_id` is a developer tool whose rules are loaded right now.
/// Everything a tool holds beyond what an `app:` label of the same id has
/// implicitly (the free service names, the waiver of the interface-domain
/// rule) is gated on this, so `pkgd`'s revoke (an empty rule list: "denied
/// everything from this moment on") ends it for a process that is still
/// running, and a tool whose rules were never loaded has it neither.
pub fn holds_rules(label_id: u32) -> bool {
    is_tool(label_id) && super::acl::label_rule_count(label_id) > 0
}

/// Whether `cred` is a tool that may not use the (validated) topic or filter
/// `name`, beyond what its rules and the per-uid namespace say. Called after
/// the tool's own `app/<id>/` subtree has been granted, so any other name that
/// starts at `app` is another app's. A filter that starts with a wildcard
/// could reach `app/` (and everything else), so it is reserved for a tool;
/// publishing has no wildcards. Anything that is not a tool is never reserved
/// here.
pub fn topic_reserved(cred: &Cred, publish: bool, name: &str) -> bool {
    // Most callers are unlabelled: no label-table lookup (and its lock) for
    // them.
    if cred.label_id == 0 || !is_tool(cred.label_id) {
        return false;
    }
    match name.split('/').next().unwrap_or("") {
        APP_ROOT => true,
        "+" | "#" => !publish,
        SYSTEM_ROOT | TIME_ROOT => publish,
        _ => false,
    }
}

/// Audit a refusal of [`topic_reserved`] (it never reaches
/// [`super::authorize`], like the per-uid rule).
pub(super) fn deny_topic(
    actor_slot: usize,
    cred: &Cred,
    interface_id: u64,
    name: &str,
    txn_id: u64,
) {
    audit::record(AuditEvent {
        ticks: crate::task::ticks(),
        actor_slot,
        uid: cred.uid,
        label_id: cred.label_id,
        interface_id,
        method: super::topics::segment_method(name.split('/').next().unwrap_or("")),
        allow: false,
        reason_code: reason::RESERVED_NAMESPACE,
        txn_id,
    });
}
