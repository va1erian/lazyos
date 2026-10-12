//! Host tests of the rule tables and the name and topic rules.

extern crate std;

#[test]
fn only_the_prefix_and_its_subtree_are_nic_names() {
    assert!(is_nic_name("os.lazy.net.nic"));
    assert!(is_nic_name("os.lazy.net.nic/eth0"));
    assert!(is_nic_name("os.lazy.net.nic/"));
    assert!(is_nic_name("os.lazy.net.nic//x/../y"));
    let long = std::format!("os.lazy.net.nic/{}", "a".repeat(100_000));
    assert!(is_nic_name(&long));
    assert!(!is_nic_name("os.lazy.net.nicX"));
    assert!(!is_nic_name("os.lazy.net.nicX/eth0"));
    assert!(!is_nic_name("os.lazy.net.nic.v1"));
    assert!(!is_nic_name("os.lazy.net.ni"));
    assert!(!is_nic_name("os.lazy.net"));
    assert!(!is_nic_name("os.lazy.net.stack"));
    assert!(!is_nic_name("OS.LAZY.NET.NIC/eth0"));
    assert!(!is_nic_name(" os.lazy.net.nic/eth0"));
    assert!(!is_nic_name("app.x.os.lazy.net.nic/eth0"));
    assert!(!is_nic_name(""));
}

#[test]
fn only_the_driver_identities_register_nic_names() {
    for uid in [NET_UID, WIFI_UID, WIFISIM_UID, ROOT_UID] {
        assert!(may_register_nic_name(uid, 0, 0), "{uid}");
    }
    // The stack itself, every other service uid, the session users.
    for uid in [
        1,
        901,
        903,
        904,
        905,
        906,
        907,
        908,
        909,
        910,
        912,
        914,
        999,
        1000,
        1001,
        65_534,
        u32::MAX,
    ] {
        assert!(!may_register_nic_name(uid, 0, 0), "{uid}");
    }
}

#[test]
fn a_driver_uid_with_a_label_or_a_session_is_refused() {
    assert!(!may_register_nic_name(NET_UID, 1, 0));
    assert!(!may_register_nic_name(NET_UID, u32::MAX, 0));
    assert!(!may_register_nic_name(NET_UID, 0, 1));
    assert!(!may_register_nic_name(ROOT_UID, 0, u64::MAX));
    assert!(!may_register_nic_name(WIFI_UID, 7, 3));
}

#[test]
fn a_driver_may_claim_only_the_kind_of_card_it_drives() {
    assert!(nic_kind_allowed(NET_UID, NIC_KIND_WIRED));
    assert!(!nic_kind_allowed(NET_UID, NIC_KIND_WIRELESS));
    for uid in [WIFI_UID, WIFISIM_UID] {
        assert!(nic_kind_allowed(uid, NIC_KIND_WIRELESS));
        assert!(!nic_kind_allowed(uid, NIC_KIND_WIRED));
    }
    assert!(nic_kind_allowed(ROOT_UID, NIC_KIND_WIRED));
    assert!(nic_kind_allowed(ROOT_UID, NIC_KIND_WIRELESS));
    for kind in [2, 31, 32, 33, u32::MAX] {
        for uid in [NET_UID, WIFI_UID, WIFISIM_UID, ROOT_UID] {
            assert!(!nic_kind_allowed(uid, kind), "{uid} kind {kind}");
        }
    }
    assert!(!nic_kind_allowed(1000, NIC_KIND_WIRED));
    assert!(!nic_kind_allowed(903, NIC_KIND_WIRED));
}

#[test]
fn the_stack_leaves_air_cards_to_the_simulator() {
    for name in ["eth0", "eth12", "wlan0", "wl0"] {
        assert!(stack_attaches(name), "{name}");
    }
    for name in [SIM_AIR_IFNAME, "air1", "air"] {
        assert!(!stack_attaches(name), "{name}");
    }
    assert_eq!(SIM_AIR_MAC[0] & 0x03, 0x02, "locally administered unicast");
}

#[test]
fn the_kind_constants_match_the_interface() {
    use messenger_generated::os_lazy_net_nic_v1 as nic;
    assert_eq!(NIC_KIND_WIRED, nic::NIC_KIND_WIRED);
    assert_eq!(NIC_KIND_WIRELESS, nic::NIC_KIND_WIRELESS);
}

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
fn the_wifi_driver_gets_the_same_rights_on_the_wireless_class_only() {
    let methods: std::vec::Vec<_> = WIFI_DRIVER_CLASS_RULES.iter().map(|r| r.method).collect();
    assert_eq!(methods, ["claim", "map", "dma"]);
    for rule in WIFI_DRIVER_CLASS_RULES {
        assert!(rule.allow && rule.actor == WIFI_UID);
        assert_eq!(rule.interface, "os.kernel.dev.wireless");
    }
    const { assert!(WIFI_UID < 1000 && WIFI_UID != NET_UID && WIFI_UID != NETD_UID) };
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

#[test]
fn the_simulator_gets_the_ring_methods_and_its_wake_up_not_a_wildcard() {
    let allowed: std::vec::Vec<_> = NIC_CLIENT_RULES
        .iter()
        .filter(|r| r.allow && r.actor == WIFISIM_UID)
        .map(|r| r.method)
        .collect();
    assert_eq!(
        allowed,
        ["AttachRing", "DetachRing", "SetRxMode", "Kick", "Notify"]
    );
}
