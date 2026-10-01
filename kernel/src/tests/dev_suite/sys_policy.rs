//! The networking stack's access rules (`libs/netpolicy`, networking plan N1
//! and N2) loaded into the real ACL: the device-class rules exercised through
//! `claim`, and the call rules for the NIC driver and the stack service through
//! the kernel's own evaluation.
//!
//! The rules live in a host-tested data crate because nothing loads a policy at
//! boot yet; these tests prove the tables do what their docs say when something
//! does: `_net` may claim, map and DMA a net-class device and nothing else, no
//! other uid gets the net class, `_netd` is the NIC driver's client, and an
//! application can read the network and ping but not attach to the card.

use alloc::vec::Vec;

use super::fixture::*;
use super::*;
use crate::dev::class::{self, method};
use crate::dev::errno::*;
use crate::dev::syscall::*;
use crate::ipc::acl::{self as acl_rules, Rule, ANY_ACTOR, ANY_METHOD};
use crate::ipc::credentials::Cred;
use crate::ipc::handles::rights;
use crate::ipc::topics::{fnv1a32, fnv1a64};

/// A driver task with `cred`, entered.
fn driver_in(cred: Cred) -> Result<usize, String> {
    let slot = spawn_driver(cred)?;
    enter(slot)?;
    Ok(slot)
}

/// `netpolicy` rules, hashed the way the loader will hash them.
fn compile(specs: &[netpolicy::RuleSpec]) -> Vec<Rule> {
    specs
        .iter()
        .map(|spec| Rule {
            actor: if spec.actor == netpolicy::ANY_ACTOR {
                ANY_ACTOR
            } else {
                spec.actor
            },
            interface_id: fnv1a64(spec.interface),
            method: if spec.method == netpolicy::ANY_METHOD {
                ANY_METHOD
            } else {
                fnv1a32(spec.method)
            },
            allow: spec.allow,
        })
        .collect()
}

fn compiled() -> Vec<Rule> {
    compile(netpolicy::NET_DRIVER_CLASS_RULES)
}

/// `_net` gets the full rights of a NIC on a net-class device, is refused every
/// other class, and another uid holding the same capability is refused the
/// net class.
pub fn sys_net_driver_policy_is_exactly_the_class_rules() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let nic = add_device(Spec::nic(Some(LINE_A)))?;
    let storage = add_device(Spec::nic(None).with_class(0x01, 0))?;
    let audio = add_device(Spec::nic(None).with_class(0x04, 0x01))?;
    check!(
        netpolicy::NET_DRIVER_CLASS_RULES
            .iter()
            .all(|rule| fnv1a64(rule.interface) == class::NET.interface_id),
        "a rule names an interface other than os.kernel.dev.net"
    );
    check!(
        compiled().iter().map(|r| r.method).collect::<Vec<_>>()
            == [method::CLAIM, method::MAP, method::DMA],
        "the rules do not name claim, map and dma"
    );
    acl::load(&compiled());

    // `_net`: the net class with every right the device has.
    let net_cred = Cred::new(netpolicy::NET_UID, netpolicy::NET_UID, CAP_DEV_CLAIM, 0, 1);
    driver_in(net_cred)?;
    let handle = expect_ok(claim_plain(nic), "_net claims a net-class device")?;
    let granted = handles::get(handle)
        .map_err(|e| e.message().to_string())?
        .rights;
    check!(
        granted == rights::DEV_ALL,
        "_net was granted {granted:#x}, expected MMIO, PIO, DMA, IRQ and CONFIG"
    );
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;

    // ... and nothing else.
    expect_errno(
        claim_plain(storage),
        EACCES,
        "_net claiming a storage device",
    )?;
    expect_errno(claim_plain(audio), EACCES, "_net claiming an audio device")?;
    check!(
        table_state(storage).0.is_none() && table_state(audio).0.is_none(),
        "a refused claim left an owner"
    );

    // Another uid with the capability but without the rule.
    // Root is among them: it skips capability checks, not the class rules.
    for uid in [netpolicy::NET_UID + 1, DRIVER_UID, 0] {
        driver_in(Cred::new(uid, uid, CAP_DEV_CLAIM, 0, 1))?;
        expect_errno(
            claim_plain(nic),
            EACCES,
            "a uid without the net-class rule claiming a NIC",
        )?;
    }
    check!(
        table_state(nic).0.is_none(),
        "a refused claim left an owner"
    );
    Ok(())
}

/// `netpolicy`'s call rules for the NIC driver and the stack service, loaded
/// into the real ACL and asked about every method for each kind of caller.
pub fn net_call_rules_decide_who_may_call_what() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let mut rules = compile(netpolicy::NIC_CLIENT_RULES);
    rules.extend(compile(netpolicy::STACK_CLIENT_RULES));
    rules.extend(compile(netpolicy::SOCKET_CLIENT_RULES));
    acl::load(&rules);
    let allowed = |uid: u32, interface: &str, method: &str| {
        !acl_rules::evaluate(uid, fnv1a64(interface), fnv1a32(method)).denied()
    };
    let nic = netpolicy::NIC_INTERFACE;
    let stack = netpolicy::STACK_INTERFACE;
    let (netd, net, root, app, stranger) = (netpolicy::NETD_UID, netpolicy::NET_UID, 0, 1000, 4242);

    // The NIC driver: `_netd` is its client; everyone may read the card.
    for method in [
        "Info",
        "SetRxMode",
        "AttachRing",
        "DetachRing",
        "Stats",
        "Kick",
        "Notify",
    ] {
        check!(
            allowed(netd, nic, method),
            "_netd refused {method} on the NIC"
        );
        check!(
            allowed(root, nic, method),
            "root refused {method} on the NIC"
        );
    }
    for uid in [app, stranger, net] {
        for method in ["Info", "Stats"] {
            check!(
                allowed(uid, nic, method),
                "uid {uid} cannot read the card ({method})"
            );
        }
        for method in [
            "SetRxMode",
            "AttachRing",
            "DetachRing",
            "Kick",
            "NoSuchMethod",
        ] {
            check!(
                !allowed(uid, nic, method),
                "uid {uid} may call {method} on the NIC"
            );
        }
    }
    // `Notify` is the driver's wake-up to its client, sent as `_net`: the
    // driver may send it, nobody else may pose as the driver.
    check!(
        allowed(net, nic, "Notify"),
        "_net refused Notify on the NIC"
    );
    for uid in [app, stranger] {
        check!(
            !allowed(uid, nic, "Notify"),
            "uid {uid} may send the driver's Notify"
        );
    }

    // The stack service: anyone reads and pings; only `_netd` and root change it.
    for uid in [netd, root, app, stranger, net] {
        for method in ["Interfaces", "Addresses", "Routes", "Stats", "Ping", "Resolve"] {
            check!(
                allowed(uid, stack, method),
                "uid {uid} refused {method} on the stack"
            );
        }
    }
    for uid in [app, stranger, net] {
        for method in ["Renew", "Reattach", "NoSuchMethod"] {
            check!(
                !allowed(uid, stack, method),
                "uid {uid} may call {method} on the stack"
            );
        }
    }
    for uid in [netd, root] {
        for method in ["Renew", "Reattach"] {
            check!(
                allowed(uid, stack, method),
                "uid {uid} refused {method} on the stack"
            );
        }
    }

    // The socket service: every socket method is open to every caller (stage
    // N6 narrows it per profile); a method that is not one is refused.
    let socket = netpolicy::SOCKET_INTERFACE;
    for uid in [netd, root, app, stranger, net] {
        for method in netpolicy::SOCKET_METHODS {
            check!(
                allowed(uid, socket, method),
                "uid {uid} refused {method} on the socket service"
            );
        }
        check!(
            !allowed(uid, socket, "NoSuchMethod"),
            "uid {uid} may call a method the socket service does not have"
        );
    }

    // Default deny: an interface the rules do not name is refused to everyone.
    check!(
        !allowed(app, "os.lazy.confd.v1", "Get") && !allowed(root, "os.lazy.confd.v1", "Get"),
        "a policy of only network rules allowed an unrelated interface"
    );
    // The audit record of a denial names the rule outcome.
    let (decision, code) = acl_rules::evaluate_verdict(app, fnv1a64(nic), fnv1a32("AttachRing"));
    check!(
        decision.denied() && code == acl_rules::reason::EXPLICIT_DENY,
        "an application attaching to the NIC was denied for reason {code}"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_sys_net_driver_policy_is_exactly_the_class_rules",
        sys_net_driver_policy_is_exactly_the_class_rules,
    ),
    (
        "dev_net_call_rules_decide_who_may_call_what",
        net_call_rules_decide_who_may_call_what,
    ),
];
