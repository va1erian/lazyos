//! Device-class policy (issue #481): which driver uid may claim, map and DMA
//! which device class.
//!
//! Each driver's rules are data in its own crate (`libs/netpolicy`,
//! `libs/usbpolicy`, `libs/sndpolicy`). The kernel compiles them in and
//! [`install_boot_policy`] installs them before `init` can start any driver, so
//! enforcement does not depend on a userspace loader being up, honest or fast.
//!
//! This rule set is separate from the Messenger uid policy (`ipc::acl`): that
//! one is still in its bootstrap-allow window, and loading driver rules into it
//! would default-deny every other Messenger call on the system. A `claim` must
//! pass both: the Messenger decision (`ipc::authorize`, which also judges
//! labelled tasks) and this one.
//!
//! Once installed, the class policy is default deny for every non-root uid:
//! a uid may use a class method only if a rule allows it, first match wins.
//! Root keeps its ambient authority (it can become any driver uid anyway), so
//! drivers the kernel boots directly as root in the harness images keep
//! working. In-kernel drivers (ATA, virtio-blk) and the PS/2 and display paths
//! never go through `claim`, so they are unaffected. Before installation (the
//! kernel test suite, which installs it explicitly where it tests it) nothing
//! is refused here.

use alloc::vec::Vec;
use spin::Mutex;

use crate::ipc::acl::{Rule, ANY_INTERFACE, ANY_METHOD};
use crate::ipc::credentials::Cred;
use crate::ipc::topics::{fnv1a32, fnv1a64};

use super::class::Class;

/// The installed class rules, or `None` before [`install`].
static CLASS_POLICY: Mutex<Option<Vec<Rule>>> = Mutex::new(None);

/// One driver crate's rule, spelled with names. Every policy crate shares the
/// `(actor, interface, method, allow)` shape and the `"*"` / `u32::MAX`
/// wildcards.
struct Spec {
    actor: u32,
    interface: &'static str,
    method: &'static str,
    allow: bool,
}

macro_rules! specs {
    ($table:expr) => {
        $table.iter().map(|rule| Spec {
            actor: rule.actor,
            interface: rule.interface,
            method: rule.method,
            allow: rule.allow,
        })
    };
}

/// Hash a named rule the way every Messenger id is hashed (`tools/midlc`).
fn compile(spec: Spec) -> Rule {
    Rule {
        actor: spec.actor,
        interface_id: match spec.interface {
            "*" => ANY_INTERFACE,
            name => fnv1a64(name),
        },
        method: match spec.method {
            "*" => ANY_METHOD,
            name => fnv1a32(name),
        },
        allow: spec.allow,
    }
}

/// The device-class rules of every driver `init` can start.
pub fn boot_rules() -> Vec<Rule> {
    specs!(netpolicy::NET_DRIVER_CLASS_RULES)
        .chain(specs!(usbpolicy::USB_DRIVER_CLASS_RULES))
        .chain(specs!(sndpolicy::SND_DRIVER_CLASS_RULES))
        .map(compile)
        .collect()
}

/// Install [`boot_rules`]. `main` calls this right after device enumeration,
/// before the filesystem is mounted and long before `init` runs.
pub fn install_boot_policy() {
    let rules = boot_rules();
    crate::serial_println!("DEV:POLICY rules={}", rules.len());
    install(rules);
}

/// Replace the class policy with `rules`.
pub fn install(rules: Vec<Rule>) {
    *CLASS_POLICY.lock() = Some(rules);
}

/// Forget the class policy (back to "not installed"). Test isolation only.
#[cfg(lazyos_tests)]
pub fn clear_for_tests() {
    *CLASS_POLICY.lock() = None;
}

/// A copy of the installed rules, or `None` before [`install`] (for the
/// read-only `policy` op, `super::inspect`).
pub fn rules() -> Option<Vec<Rule>> {
    CLASS_POLICY.lock().clone()
}

/// Whether a class policy is installed.
pub fn is_installed() -> bool {
    CLASS_POLICY.lock().is_some()
}

/// Whether `cred` may use `method` on `class`.
pub fn allows(cred: &Cred, class: &Class, method: u32) -> bool {
    let policy = CLASS_POLICY.lock();
    let Some(rules) = policy.as_ref() else {
        return true;
    };
    if cred.uid == 0 {
        return true;
    }
    rules
        .iter()
        .find(|rule| rule.matches(cred.uid, class.interface_id, method))
        .is_some_and(|rule| rule.allow)
}
