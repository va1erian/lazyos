//! Label-keyed Messenger policy (application package system, phase 1).
//!
//! The kernel stamps a task with a label (`super::labels`) when `init` spawns
//! it; this module turns the label into the namespace rules of issue #308:
//!
//! * Service names. `os.lazy.*` belongs to the platform: a `system:*` task or
//!   an unlabelled task holding root, `CAP_IPC_CONTROL` or `CAP_DEV_CLAIM` (a
//!   driver `init` provisioned) may register there, and any other unlabelled
//!   task falls back to the uid rules (bootstrap-allow until a uid policy is
//!   loaded, so `init`'s capability-less services such as `netd` still come
//!   up). `app.<id>.<name>` belongs
//!   to the task labelled `app:<id>`. A labelled task may register nothing
//!   else. The `<name>` part is a single segment (no dots), so a name maps to
//!   exactly one `<id>`: `app:com.foo` cannot claim `app.com.foo.bar.svc`,
//!   which is `app:com.foo.bar`'s.
//! * Topics. A task labelled `app:<id>` may publish and subscribe anything at
//!   or under `app/<id>/`.
//! * A `dev:<id>` label (an app run from an IDE, issue #529) owns exactly the
//!   names and topics of `app:<id>`; its other rules are the ones `pkgd`
//!   loaded for it after the user approved them.
//! * Everything else a labelled task does (resolve a name, call an interface,
//!   touch another topic) is default-deny unless an allow rule for its label
//!   was loaded ([`super::acl::load_label`]). Resolving a name is checked as a
//!   call to the [`RESOLVE_SCOPE`] pseudo-interface with `fnv1a32(name)` as
//!   the method, so a rule grants exactly one name.
//!
//! Unlabelled tasks keep the legacy behaviour (uid rules, bootstrap-allow
//! while empty) except for the reserved namespaces above, which nobody may
//! squat. Every refusal is written to the audit ring with the actor's label
//! id and a machine-readable reason; [`explain`] turns the pair into the
//! friendly sentence naming what the app *may* use.

use alloc::format;
use alloc::string::String;

use super::acl::{self, reason, Decision};
use super::audit::{self, AuditEvent};
use super::credentials::{self, Cred, CAP_DEV_CLAIM, CAP_IPC_CONTROL};
use super::labels::{self, Kind};
use super::registry;
use super::topics::{fnv1a32, fnv1a64};

/// The generated policy loader interface (`idl/policy.midl`).
pub use messenger_generated::os_lazy_messenger_names_resolve_v1 as resolve_scope;
pub use messenger_generated::os_lazy_messenger_policy_v1 as wire;

/// Policy scope a labelled task's `Resolve` of a name is evaluated against.
pub const RESOLVE_SCOPE: u64 = resolve_scope::INTERFACE_ID;
/// Interface id of the policy loader, for audit records of `acl_load`.
pub const LOADER_INTERFACE: u64 = wire::INTERFACE_ID;
/// Method id of `LoadLabel`.
pub const LOAD_METHOD: u32 = wire::METHOD_LOADLABEL;

const _: () = {
    assert!(fnv1a64("os.lazy.messenger.names.resolve.v1") == RESOLVE_SCOPE);
    assert!(fnv1a64("os.lazy.messenger.policy.v1") == LOADER_INTERFACE);
};

/// The platform's reserved service-name prefix.
pub const SYSTEM_NAMES: &str = "os.lazy.";
/// Prefix of the per-application service namespace, `app.<id>.<name>`.
pub const APP_NAMES: &str = "app.";
/// First topic segment of the per-application namespace, `app/<id>/...`.
pub const APP_TOPICS: &str = "app";

/// What a task is trying to do to a service name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NameOp {
    /// Publish the name in the registry.
    Register,
    /// Open the endpoint behind the name.
    Resolve,
}

impl NameOp {
    fn method(self) -> u32 {
        match self {
            NameOp::Register => registry::method::REGISTER,
            NameOp::Resolve => registry::method::RESOLVE,
        }
    }
}

/// A refused name operation: the audit reason code, for the caller to map to
/// `EACCES` (the denial is already audited).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NameDenied {
    /// One of the [`acl::reason`] codes.
    pub reason_code: u32,
}

/// The app id of `app.<id>.<tail>`: everything between `app.` and the last
/// dot, with a non-empty, dot-free tail.
pub fn app_name_id(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(APP_NAMES)?;
    let (id, tail) = rest.rsplit_once('.')?;
    (!id.is_empty() && !tail.is_empty()).then_some(id)
}

/// The app id of a label, for `app:<id>` and `dev:<id>` labels: a development
/// run of an app (issue #529) owns the installed app's names and topics, so a
/// project behaves under Play exactly as it will once installed.
fn app_id_of(label_id: u32) -> Option<alloc::string::String> {
    labels::with(label_id, |label| match labels::parse(label) {
        Ok((Kind::App | Kind::Dev, id)) => Some(String::from(id)),
        _ => None,
    })
    .flatten()
}

/// Whether `cred` is a platform actor by credentials alone: unlabelled with
/// root, `CAP_IPC_CONTROL` or `CAP_DEV_CLAIM`.
fn unlabelled_admin(cred: &Cred) -> bool {
    cred.label_id == 0
        && (cred.uid == 0 || cred.has_cap(CAP_IPC_CONTROL) || cred.has_cap(CAP_DEV_CLAIM))
}

/// The legacy uid rules for an unlabelled task's registry call.
fn uid_verdict(cred: &Cred, method: u32) -> Result<u32, u32> {
    let (decision, code) = acl::evaluate_verdict(cred.authority(), registry::INTERFACE, method);
    if decision.denied() {
        Err(code)
    } else {
        Ok(code)
    }
}

/// Decide a name operation without auditing it: `Ok(reason)` or `Err(reason)`.
fn name_verdict(cred: &Cred, op: NameOp, name: &str) -> Result<u32, u32> {
    let own_app = app_id_of(cred.label_id);
    let namespaced = app_name_id(name)
        .zip(own_app.as_deref())
        .is_some_and(|(named, own)| named == own);
    match op {
        NameOp::Register => {
            if name.starts_with(SYSTEM_NAMES) {
                let system = labels::kind_of(cred.label_id) == Some(Kind::System);
                if system || unlabelled_admin(cred) {
                    return Ok(reason::ALLOWED_BY_NAMESPACE);
                }
                if cred.label_id != 0 {
                    return Err(reason::RESERVED_NAMESPACE);
                }
                // An unlabelled, unprivileged task (a platform service `init`
                // runs under its own uid with no capabilities, like `netd`)
                // keeps the legacy uid rules: bootstrap-allow until
                // `messengerd` loads a uid policy, then whatever it says.
                return uid_verdict(cred, registry::method::REGISTER);
            }
            if name.starts_with(APP_NAMES) {
                return if namespaced || unlabelled_admin(cred) {
                    Ok(reason::ALLOWED_BY_NAMESPACE)
                } else {
                    Err(reason::RESERVED_NAMESPACE)
                };
            }
            if cred.label_id == 0 {
                Ok(reason::BOOTSTRAP_ALLOW)
            } else {
                Err(reason::OUTSIDE_NAMESPACE)
            }
        }
        NameOp::Resolve => {
            if cred.label_id == 0 || namespaced {
                return Ok(reason::ALLOWED_BY_NAMESPACE);
            }
            let (decision, code) = acl::evaluate_label(cred.label_id, RESOLVE_SCOPE, fnv1a32(name));
            if decision.denied() {
                Err(code)
            } else {
                Ok(code)
            }
        }
    }
}

/// Authorize `op` on `name` for the task in `slot` (the *client's* slot, even
/// when `messengerd` proxies the request), auditing every refusal and, when
/// tracing is on, every allow. The audit record's method is the registry
/// method and its `txn_id` the FNV-1a hash of the name, so a denial can be
/// matched to the request without the ring storing the name.
pub fn check_name(slot: usize, op: NameOp, name: &str) -> Result<(), NameDenied> {
    let cred = credentials::of(slot);
    let verdict = name_verdict(&cred, op, name);
    #[cfg(lazyos_label_trace)]
    if verdict.is_err() {
        super::label_trace::denied(
            cred.label_id,
            format_args!("resolve={name} op={}", op.method()),
        );
    }
    if verdict.is_err() || audit::trace() {
        audit::record(AuditEvent {
            ticks: crate::task::ticks(),
            actor_slot: slot,
            uid: cred.uid,
            label_id: cred.label_id,
            interface_id: registry::INTERFACE,
            method: op.method(),
            allow: verdict.is_ok(),
            reason_code: match verdict {
                Ok(code) | Err(code) => code,
            },
            txn_id: fnv1a64(name),
        });
    }
    verdict
        .map(|_| ())
        .map_err(|reason_code| NameDenied { reason_code })
}

/// Whether `name` (a validated topic or filter) lies in the `app/<id>/`
/// namespace of the actor's own `app:<id>` label.
pub fn owns_topic(cred: &Cred, name: &str) -> bool {
    let Some(id) = app_id_of(cred.label_id) else {
        return false;
    };
    let mut segments = name.split('/');
    segments.next() == Some(APP_TOPICS) && segments.next() == Some(id.as_str())
}

/// The registry calls every labelled task makes implicitly: registering,
/// unregistering and resolving are always *attempted*, and [`check_name`]
/// decides the outcome per name. Everything else on the registry interface
/// (listing every service) needs an explicit rule.
fn implicit_registry_call(interface_id: u64, method: u32) -> bool {
    interface_id == registry::INTERFACE
        && matches!(
            method,
            registry::method::REGISTER | registry::method::UNREGISTER | registry::method::RESOLVE
        )
}

/// Evaluate an interface/method call for a labelled task. The one hook
/// [`super::authorize`] calls for any non-zero label.
pub fn evaluate_labelled(cred: &Cred, interface_id: u64, method: u32) -> (Decision, u32) {
    if implicit_registry_call(interface_id, method) {
        return (Decision::Allow, reason::ALLOWED_BY_NAMESPACE);
    }
    acl::evaluate_label(cred.label_id, interface_id, method)
}

/// The friendly sentence for a refusal, naming the namespace the app may use.
/// `label_id` and `reason_code` are exactly what the audit record holds.
pub fn explain(label_id: u32, reason_code: u32) -> String {
    let own = app_id_of(label_id);
    let may = match own {
        Some(id) => {
            format!("it may publish services named app.{id}.<name> and topics under app/{id}/")
        }
        None => String::from("ask an administrator which names it may use"),
    };
    let why = match reason_code {
        reason::RESERVED_NAMESPACE => "that name belongs to the system or to another app",
        reason::OUTSIDE_NAMESPACE => "apps may only publish names in their own namespace",
        _ => "it was not granted access to that Messenger interface",
    };
    format!("denied: {why}; {may}")
}
