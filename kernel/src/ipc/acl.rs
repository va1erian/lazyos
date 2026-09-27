//! Kernel ACL hook (issue #68).
//!
//! Policy is a compact ordered list of rules keyed by `(actor authority,
//! interface_id, method)`. The fabric passes every call through this module
//! before dispatch; the compiled policy is loaded by `messengerd` through the
//! privileged `msg_acl_load` syscall (#69), never through a parcel.
//!
//! Security stance: `default deny`. Once a non-empty policy is installed, a call
//! that matches no rule is refused. The single documented exception is the
//! bootstrap window: while the policy is empty the kernel allows calls so `init`
//! can start `messengerd` and load the first rules. Bootstrap allows are audited
//! like any other decision, so the exception is visible, not silent.
//!
//! Rules are evaluated in order and the first match wins, so a policy compiler
//! can put exact denies ahead of broad allows.

use alloc::vec::Vec;
use spin::Mutex;

/// Wildcard actor: matches any authority.
pub const ANY_ACTOR: u32 = u32::MAX;
/// Wildcard interface: matches any interface id.
pub const ANY_INTERFACE: u64 = u64::MAX;
/// Wildcard method: matches any method id.
pub const ANY_METHOD: u32 = u32::MAX;

/// Machine-readable reasons copied into audit records, so `auditd` and
/// `messengerctl why <txn>` can explain a decision without parsing text.
pub mod reason {
    /// A matching allow rule permitted the call.
    pub const ALLOWED_BY_RULE: u32 = 1;
    /// No rule matched a non-empty policy (default deny).
    pub const DEFAULT_DENY: u32 = 2;
    /// A matching explicit deny rule refused the call.
    pub const EXPLICIT_DENY: u32 = 3;
    /// No policy is installed yet (the bootstrap exception).
    pub const BOOTSTRAP_ALLOW: u32 = 4;
}

/// One compiled policy rule. `actor` is the authority from [`super::credentials`]
/// (the uid today, or a label id), `ANY_*` wildcards are allowed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rule {
    /// Actor authority, or [`ANY_ACTOR`].
    pub actor: u32,
    /// Interface id, or [`ANY_INTERFACE`].
    pub interface_id: u64,
    /// Method id, or [`ANY_METHOD`].
    pub method: u32,
    /// `true` permits the call, `false` refuses it.
    pub allow: bool,
}

impl Rule {
    /// Whether this rule covers the call. Wildcards match anything.
    fn matches(&self, authority: u32, interface_id: u64, method: u32) -> bool {
        (self.actor == ANY_ACTOR || self.actor == authority)
            && (self.interface_id == ANY_INTERFACE || self.interface_id == interface_id)
            && (self.method == ANY_METHOD || self.method == method)
    }
}

/// The verdict of [`evaluate`]. `Deny` carries plain-language text for the
/// friendly-error path in `docs/messenger.md` section 12.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    /// The call may proceed.
    Allow,
    /// The call is refused; `reason` explains why to the user.
    Deny { reason: &'static str },
}

impl Decision {
    /// Whether this decision refuses the call.
    pub fn denied(&self) -> bool {
        matches!(self, Decision::Deny { .. })
    }

    /// The friendly explanation, when the call was denied.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Decision::Allow => None,
            Decision::Deny { reason } => Some(reason),
        }
    }
}

/// The installed policy. Empty means "not loaded yet": the bootstrap window.
static POLICY: Mutex<Vec<Rule>> = Mutex::new(Vec::new());

/// Install a compiled policy, replacing any previous one. `Kernel-only`: only
/// the privileged ACL loader path (#69) calls this; parcels are never policy.
pub fn load(rules: &[Rule]) {
    let mut policy = POLICY.lock();
    policy.clear();
    policy.extend_from_slice(rules);
}

/// Whether a non-empty policy is installed.
pub fn is_loaded() -> bool {
    !POLICY.lock().is_empty()
}

/// Number of rules in the installed policy.
pub fn rule_count() -> usize {
    POLICY.lock().len()
}

/// Evaluate a call and return the friendly decision. `authority` is the actor
/// identity the rules key on ([`super::credentials::Cred::authority`] today).
pub fn evaluate(authority: u32, interface_id: u64, method: u32) -> Decision {
    evaluate_verdict(authority, interface_id, method).0
}

/// Evaluate a call and also return the machine-readable reason code for the
/// audit ring. This is the hook [`super::authorize`] calls.
pub fn evaluate_verdict(authority: u32, interface_id: u64, method: u32) -> (Decision, u32) {
    let policy = POLICY.lock();
    if policy.is_empty() {
        return (Decision::Allow, reason::BOOTSTRAP_ALLOW);
    }
    for rule in policy.iter() {
        if rule.matches(authority, interface_id, method) {
            return if rule.allow {
                (Decision::Allow, reason::ALLOWED_BY_RULE)
            } else {
                (
                    Decision::Deny {
                        reason: "an explicit deny rule blocks this Messenger call",
                    },
                    reason::EXPLICIT_DENY,
                )
            };
        }
    }
    (
        Decision::Deny {
            reason: "this app was not granted access to this Messenger interface; \
                     ask an administrator to approve the permission",
        },
        reason::DEFAULT_DENY,
    )
}
