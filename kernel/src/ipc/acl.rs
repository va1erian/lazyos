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
//!
//! # Label-keyed rules
//!
//! A task stamped with a label (`super::labels`) is judged by a second,
//! separate rule set whose actor is the *label id*, never its uid. That set is
//! default-deny from the first call: there is no bootstrap window for a
//! labelled task, and an unmatched call is refused even while the uid policy
//! is empty. [`load_label`] replaces all of one label's rules at once, so
//! revoking an application is loading an empty list. Storage is bounded per
//! label and in total, so a policy loader cannot exhaust kernel memory.

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
    /// A labelled task matched no rule for its label (default deny).
    pub const LABEL_DEFAULT_DENY: u32 = 5;
    /// A labelled task used something its own namespace grants implicitly
    /// (the registry calls; its `app.<id>.*` names and `app/<id>/` topics).
    pub const ALLOWED_BY_NAMESPACE: u32 = 6;
    /// A name or topic outside the namespace the actor's label owns.
    pub const OUTSIDE_NAMESPACE: u32 = 7;
    /// A reserved namespace (`os.lazy.*`, or another app's `app.<id>.*`)
    /// claimed by an actor that does not own it.
    pub const RESERVED_NAMESPACE: u32 = 8;
    /// The caller lacks the capability the policy loader requires.
    pub const LOADER_NOT_PRIVILEGED: u32 = 9;
    /// A registration's interface names do not spell out its interface ids
    /// (missing, a different count, or a name that hashes to another id); a
    /// labelled app must name every interface it advertises (issue #495).
    pub const UNNAMED_INTERFACE: u32 = 10;
    /// A labelled app advertised an interface outside its own domain
    /// (`<id>.<name>.v<N>`, issue #495).
    pub const FOREIGN_INTERFACE: u32 = 11;
    /// A topic or filter that reaches into another uid's private
    /// `user/<uid>/` namespace (`ipc::topics::private`, issue #407).
    pub const PRIVATE_NAMESPACE: u32 = 12;
}

/// Most rules one label may hold.
pub const MAX_RULES_PER_LABEL: usize = 256;
/// Most label-keyed rules the kernel stores in total.
pub const MAX_LABEL_RULES: usize = 4096;

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
    pub(crate) fn matches(&self, authority: u32, interface_id: u64, method: u32) -> bool {
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

/// Rules of every label, grouped so each label's rules keep their load order.
/// The `actor` of each rule is a label id.
static LABEL_POLICY: Mutex<Vec<Rule>> = Mutex::new(Vec::new());

/// Why [`load_label`] refused a rule set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadError {
    /// `0` names no label.
    Unlabelled,
    /// More than [`MAX_RULES_PER_LABEL`] rules for one label.
    TooManyRules,
    /// The kernel-wide [`MAX_LABEL_RULES`] budget would be exceeded.
    PolicyFull,
}

/// Replace every rule of `label_id` with `rules` (an empty slice revokes the
/// label's grants). Rule actors are ignored and rewritten to `label_id`, so a
/// loader cannot smuggle a rule for another label into this batch. Nothing
/// changes when the call fails. `Kernel-only`: `OP_ACL_LOAD` is the caller.
pub fn load_label(label_id: u32, rules: &[Rule]) -> Result<(), LoadError> {
    if label_id == 0 {
        return Err(LoadError::Unlabelled);
    }
    if rules.len() > MAX_RULES_PER_LABEL {
        return Err(LoadError::TooManyRules);
    }
    let mut policy = LABEL_POLICY.lock();
    let others = policy.iter().filter(|rule| rule.actor != label_id).count();
    if others + rules.len() > MAX_LABEL_RULES {
        return Err(LoadError::PolicyFull);
    }
    policy.retain(|rule| rule.actor != label_id);
    policy.extend(rules.iter().map(|rule| Rule {
        actor: label_id,
        ..*rule
    }));
    Ok(())
}

/// Drop every label-keyed rule. Test isolation only.
#[cfg(lazyos_tests)]
pub fn reset_labels_for_tests() {
    LABEL_POLICY.lock().clear();
}

/// Number of rules loaded for `label_id`.
pub fn label_rule_count(label_id: u32) -> usize {
    LABEL_POLICY
        .lock()
        .iter()
        .filter(|rule| rule.actor == label_id)
        .count()
}

/// Number of label-keyed rules across every label.
pub fn label_rules_total() -> usize {
    LABEL_POLICY.lock().len()
}

/// Evaluate a call by a task labelled `label_id`: first matching rule of its
/// label wins, and no match is a default deny. There is no bootstrap allow.
pub fn evaluate_label(label_id: u32, interface_id: u64, method: u32) -> (Decision, u32) {
    let policy = LABEL_POLICY.lock();
    for rule in policy.iter() {
        if rule.actor == label_id && rule.matches(label_id, interface_id, method) {
            return if rule.allow {
                (Decision::Allow, reason::ALLOWED_BY_RULE)
            } else {
                (
                    Decision::Deny {
                        reason: "an explicit deny rule blocks this app's Messenger call",
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
        reason::LABEL_DEFAULT_DENY,
    )
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
