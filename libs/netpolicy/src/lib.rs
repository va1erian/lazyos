//! The access-control rules of the networking stack, as data.
//!
//! Policy is a list of `(actor uid, interface, method, allow)` rules the kernel
//! evaluates first-match, default deny once a policy is installed
//! (`docs/architecture/ipc-security.md`). The device-class rules
//! ([`NET_DRIVER_CLASS_RULES`]) are installed by the kernel at boot
//! (`dev::policy`, issue #481), so `_net` gets the net class and nothing else
//! on every boot. The call rules wait for a Messenger policy loader
//! (`messengerd`, `docs/security-model.md`): the fabric is still in its
//! bootstrap-allow window, so they refuse nothing yet. The kernel test suite
//! loads these tables and checks the decisions they yield
//! (`dev_suite::sys_net_driver_policy_is_exactly_the_class_rules`,
//! `dev_suite::sys_boot_policy_confines_each_driver_to_its_class`).
//!
//! Interfaces and methods are spelled as names; the loader hashes them the way
//! every Messenger id is hashed (`tools/midlc`), and a host test here pins the
//! spelling to the `midlc`-generated ids.

#![no_std]

/// The `_net` system user the NIC driver runs as: only `CAP_DEV_CLAIM`.
pub const NET_UID: u32 = 902;

/// The `_netd` system user the network stack runs as: **no** capabilities.
pub const NETD_UID: u32 = 903;

/// Matches any actor (the kernel's `ANY_ACTOR`).
pub const ANY_ACTOR: u32 = u32::MAX;

/// Root. It has ambient authority anyway; the NIC rules name it so that the
/// diagnostic tools (`nicctl`, run from a root shell) keep working.
pub const ROOT_UID: u32 = 0;

/// Matches any method of the interface.
pub const ANY_METHOD: &str = "*";

/// The NIC driver's interface (`idl/net.midl`).
pub const NIC_INTERFACE: &str = "os.lazy.net.nic.v1";
/// The stack service's interface (`idl/net.midl`).
pub const STACK_INTERFACE: &str = "os.lazy.net.stack.v1";
/// The socket interface `netd` serves next to it (`idl/net.midl`, stage N3).
pub const SOCKET_INTERFACE: &str = "os.lazy.net.socket.v1";

/// One rule. `actor` is a uid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleSpec {
    pub actor: u32,
    pub interface: &'static str,
    pub method: &'static str,
    pub allow: bool,
}

const fn allow(actor: u32, interface: &'static str, method: &'static str) -> RuleSpec {
    RuleSpec {
        actor,
        interface,
        method,
        allow: true,
    }
}

const fn deny(actor: u32, interface: &'static str, method: &'static str) -> RuleSpec {
    RuleSpec {
        actor,
        interface,
        method,
        allow: false,
    }
}

/// What `_net` may do to a device: claim a device *of the net class* and take
/// the MMIO/PIO and DMA rights a NIC driver needs (`docs/driver-plan.md`
/// section 3.5). Nothing names another class, so `_net` cannot claim storage,
/// audio or anything else, and nothing grants another uid the net class.
pub const NET_DRIVER_CLASS_RULES: &[RuleSpec] = &[
    allow(NET_UID, "os.kernel.dev.net", "claim"),
    allow(NET_UID, "os.kernel.dev.net", "map"),
    allow(NET_UID, "os.kernel.dev.net", "dma"),
];

/// Who may call the NIC driver. `_netd` is the stack and the only real client;
/// anyone may read the card (`Info`, `Stats`); root is named for the diagnostic
/// tools (and cannot displace an attached `_netd`: the driver accepts one
/// client and refuses a second). Everything else, `Kick` and `Notify` included,
/// is refused, which is what stops an application from attaching to the card or
/// feeding the driver frames.
pub const NIC_CLIENT_RULES: &[RuleSpec] = &[
    allow(NETD_UID, NIC_INTERFACE, ANY_METHOD),
    allow(ROOT_UID, NIC_INTERFACE, ANY_METHOD),
    allow(ANY_ACTOR, NIC_INTERFACE, "Info"),
    allow(ANY_ACTOR, NIC_INTERFACE, "Stats"),
    // The driver's own wake-up to the client: `Notify` is sent *by* `_net`, to
    // the notify endpoint the client handed it, and is judged like any call.
    allow(NET_UID, NIC_INTERFACE, "Notify"),
    deny(ANY_ACTOR, NIC_INTERFACE, ANY_METHOD),
];

/// Who may call the stack service: anyone reads it and pings; changing it
/// (`Renew`, `Reattach`) is for `_netd` itself and root.
pub const STACK_CLIENT_RULES: &[RuleSpec] = &[
    allow(ANY_ACTOR, STACK_INTERFACE, "Interfaces"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Addresses"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Routes"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Stats"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Ping"),
    allow(ANY_ACTOR, STACK_INTERFACE, "Resolve"),
    allow(NETD_UID, STACK_INTERFACE, ANY_METHOD),
    allow(ROOT_UID, STACK_INTERFACE, ANY_METHOD),
    deny(ANY_ACTOR, STACK_INTERFACE, ANY_METHOD),
];

/// The socket methods, in the order of the interface. Each is its own rule so
/// that a profile can later grant `Connect` without `Listen` (stage N6 narrows
/// who gets which; until then any caller may use sockets, and `netd` enforces
/// ownership of each socket and its own quotas whatever this table says).
pub const SOCKET_METHODS: &[&str] = &[
    "Open",
    "Bind",
    "Connect",
    "Listen",
    "Accept",
    "Send",
    "Recv",
    "SendTo",
    "RecvFrom",
    "Poll",
    "Shutdown",
    "LocalAddr",
    "PeerAddr",
    "Close",
    "Stats",
];

const fn socket_allow(method: &'static str) -> RuleSpec {
    allow(ANY_ACTOR, SOCKET_INTERFACE, method)
}

/// Who may call the socket service: every method is open to every caller for
/// now, and anything that is not a socket method is refused.
pub const SOCKET_CLIENT_RULES: &[RuleSpec] = &[
    socket_allow("Open"),
    socket_allow("Bind"),
    socket_allow("Connect"),
    socket_allow("Listen"),
    socket_allow("Accept"),
    socket_allow("Send"),
    socket_allow("Recv"),
    socket_allow("SendTo"),
    socket_allow("RecvFrom"),
    socket_allow("Poll"),
    socket_allow("Shutdown"),
    socket_allow("LocalAddr"),
    socket_allow("PeerAddr"),
    socket_allow("Close"),
    socket_allow("Stats"),
    deny(ANY_ACTOR, SOCKET_INTERFACE, ANY_METHOD),
];

/// Whether `topic` is one the stack (`netd`, [`NETD_UID`]) publishes under
/// the reserved `system/` root: an interface's retained address
/// (`system/net/<if>/addr`), the list of interfaces (`system/net/interfaces`)
/// and the network-up event (`system/events/network/up`), all declared in
/// `idl/net.midl`. Nothing
/// else under `system/` is the stack's (a NIC's link is its driver's).
pub fn is_stack_topic(topic: &str) -> bool {
    let addr = topic
        .strip_prefix("system/net/")
        .and_then(|rest| rest.strip_suffix("/addr"))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'));
    addr || topic == "system/net/interfaces" || topic == "system/events/network/up"
}

#[cfg(test)]
mod tests {
    extern crate std;

    #[test]
    fn the_stack_publishes_its_address_and_the_up_event_only() {
        use super::is_stack_topic;
        assert!(is_stack_topic("system/net/eth0/addr"));
        assert!(is_stack_topic("system/events/network/up"));
        assert!(is_stack_topic("system/net/interfaces"));
        assert!(!is_stack_topic("system/net/interfaces/x"));
        assert!(!is_stack_topic("system/net/eth0/link"));
        assert!(!is_stack_topic("system/net//addr"));
        assert!(!is_stack_topic("system/net/a/b/addr"));
        assert!(!is_stack_topic("system/events/network/down"));
        assert!(!is_stack_topic("system/power/state"));
    }

    use super::*;

    /// FNV-1a, the hash of every Messenger interface and method id.
    fn fnv1a64(text: &str) -> u64 {
        text.bytes().fold(0xCBF2_9CE4_8422_2325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3)
        })
    }

    #[test]
    fn the_driver_gets_exactly_claim_map_and_dma_on_the_net_class() {
        let methods: std::vec::Vec<_> = NET_DRIVER_CLASS_RULES.iter().map(|r| r.method).collect();
        assert_eq!(methods, ["claim", "map", "dma"]);
        for rule in NET_DRIVER_CLASS_RULES {
            assert!(rule.allow);
            assert_eq!(rule.actor, NET_UID);
            assert_eq!(
                rule.interface, "os.kernel.dev.net",
                "no other device class is named"
            );
            assert_ne!(rule.method, ANY_METHOD, "no wildcard grants");
        }
    }

    #[test]
    fn the_net_uid_is_a_system_uid_below_the_user_range() {
        const { assert!(NET_UID < 1000) };
        const { assert!(NET_UID != 901, "not the sound driver's uid") };
    }

    /// FNV-1a 32, the hash of every Messenger method id.
    fn fnv1a32(text: &str) -> u32 {
        let hash = text.bytes().fold(0x811C_9DC5u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        });
        hash & 0x7FFF_FFFF
    }

    #[test]
    fn the_nic_interface_name_matches_the_generated_id() {
        assert_eq!(
            fnv1a64("os.lazy.net.nic.v1"),
            messenger_generated::os_lazy_net_nic_v1::INTERFACE_ID
        );
        assert_eq!(
            fnv1a64(NIC_INTERFACE),
            messenger_generated::os_lazy_net_nic_v1::INTERFACE_ID
        );
        assert_eq!(
            fnv1a64(STACK_INTERFACE),
            messenger_generated::os_lazy_net_stack_v1::INTERFACE_ID
        );
        assert_eq!(
            fnv1a64(SOCKET_INTERFACE),
            messenger_generated::os_lazy_net_socket_v1::INTERFACE_ID
        );
    }

    #[test]
    fn the_socket_rules_name_every_method_of_the_interface_and_only_those() {
        use messenger_generated::os_lazy_net_socket_v1 as socket;
        let ids = [
            ("Open", socket::METHOD_OPEN),
            ("Bind", socket::METHOD_BIND),
            ("Connect", socket::METHOD_CONNECT),
            ("Listen", socket::METHOD_LISTEN),
            ("Accept", socket::METHOD_ACCEPT),
            ("Send", socket::METHOD_SEND),
            ("Recv", socket::METHOD_RECV),
            ("SendTo", socket::METHOD_SENDTO),
            ("RecvFrom", socket::METHOD_RECVFROM),
            ("Poll", socket::METHOD_POLL),
            ("Shutdown", socket::METHOD_SHUTDOWN),
            ("LocalAddr", socket::METHOD_LOCALADDR),
            ("PeerAddr", socket::METHOD_PEERADDR),
            ("Close", socket::METHOD_CLOSE),
            ("Stats", socket::METHOD_STATS),
        ];
        assert_eq!(ids.len(), SOCKET_METHODS.len());
        for ((name, id), listed) in ids.iter().zip(SOCKET_METHODS) {
            assert_eq!(name, listed);
            assert_eq!(fnv1a32(name), *id, "{name}");
        }
        let allowed: std::vec::Vec<_> = SOCKET_CLIENT_RULES
            .iter()
            .filter(|r| r.allow)
            .map(|r| r.method)
            .collect();
        assert_eq!(allowed, SOCKET_METHODS);
        assert!(SOCKET_CLIENT_RULES
            .iter()
            .all(|r| r.interface == SOCKET_INTERFACE));
    }

    #[test]
    fn every_method_name_in_the_rules_is_a_real_method() {
        use messenger_generated::os_lazy_net_nic_v1 as nic;
        use messenger_generated::os_lazy_net_stack_v1 as stack;
        let nic_methods = [
            ("Info", nic::METHOD_INFO),
            ("SetRxMode", nic::METHOD_SETRXMODE),
            ("AttachRing", nic::METHOD_ATTACHRING),
            ("DetachRing", nic::METHOD_DETACHRING),
            ("Stats", nic::METHOD_STATS),
            ("Kick", nic::METHOD_KICK),
            ("Notify", nic::METHOD_NOTIFY),
        ];
        let stack_methods = [
            ("Interfaces", stack::METHOD_INTERFACES),
            ("Addresses", stack::METHOD_ADDRESSES),
            ("Routes", stack::METHOD_ROUTES),
            ("Stats", stack::METHOD_STATS),
            ("Ping", stack::METHOD_PING),
            ("Renew", stack::METHOD_RENEW),
            ("Reattach", stack::METHOD_REATTACH),
            ("Resolve", stack::METHOD_RESOLVE),
        ];
        for (name, id) in nic_methods.iter().chain(stack_methods.iter()) {
            assert_eq!(fnv1a32(name), *id, "{name}");
        }
        for rule in NIC_CLIENT_RULES.iter().chain(STACK_CLIENT_RULES) {
            if rule.method != ANY_METHOD {
                assert!(
                    nic_methods
                        .iter()
                        .chain(stack_methods.iter())
                        .any(|(name, _)| *name == rule.method),
                    "{} is not a method",
                    rule.method
                );
            }
        }
    }

    #[test]
    fn every_ruleset_ends_in_an_explicit_deny() {
        for rules in [NIC_CLIENT_RULES, STACK_CLIENT_RULES, SOCKET_CLIENT_RULES] {
            let last = rules.last().unwrap();
            assert!(!last.allow && last.actor == ANY_ACTOR && last.method == ANY_METHOD);
            // Nothing after an allow can be reached by the wildcard deny's actor
            // class except by earlier, more specific allows: no allow follows it.
            assert!(rules.iter().rev().skip(1).all(|r| r.allow));
        }
    }

    #[test]
    fn the_driver_may_send_its_wake_up_and_nothing_else_of_the_clients() {
        let allowed: std::vec::Vec<_> = NIC_CLIENT_RULES
            .iter()
            .filter(|r| r.allow && r.actor == NET_UID)
            .map(|r| r.method)
            .collect();
        assert_eq!(allowed, ["Notify"]);
    }

    #[test]
    fn netd_is_a_system_user_distinct_from_the_driver() {
        assert_ne!(NETD_UID, NET_UID);
        const { assert!(NETD_UID < 1000) };
    }
}
