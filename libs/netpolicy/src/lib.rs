//! The access-control rules of the networking stack, as data.
//!
//! Policy is a list of `(actor uid, interface, method, allow)` rules the kernel
//! evaluates first-match, default deny once a policy is installed
//! (`docs/architecture/ipc-security.md`). Today the fabric is still in its
//! bootstrap-allow window and nothing loads a policy, so these rules refuse
//! nothing yet; they are defined, named and tested now so that the moment a
//! loader exists (`messengerd`, `docs/security-model.md`) the network services
//! keep working and everything else is refused. The kernel test suite loads
//! exactly this table and checks the decisions it yields
//! (`dev_suite::sys_net_driver_policy_is_exactly_the_class_rules`).
//!
//! Interfaces and methods are spelled as names; the loader hashes them the way
//! every Messenger id is hashed (`tools/midlc`), and a host test here pins the
//! spelling to the `midlc`-generated ids.

#![no_std]

/// The `_net` system user the NIC driver runs as: only `CAP_DEV_CLAIM`.
pub const NET_UID: u32 = 902;

/// Matches any method of the interface.
pub const ANY_METHOD: &str = "*";

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

/// What `_net` may do to a device: claim a device *of the net class* and take
/// the MMIO/PIO and DMA rights a NIC driver needs (`docs/driver-plan.md`
/// section 3.5). Nothing names another class, so `_net` cannot claim storage,
/// audio or anything else, and nothing grants another uid the net class.
pub const NET_DRIVER_CLASS_RULES: &[RuleSpec] = &[
    allow(NET_UID, "os.kernel.dev.net", "claim"),
    allow(NET_UID, "os.kernel.dev.net", "map"),
    allow(NET_UID, "os.kernel.dev.net", "dma"),
];

#[cfg(test)]
mod tests {
    extern crate std;

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

    #[test]
    fn the_nic_interface_name_matches_the_generated_id() {
        assert_eq!(
            fnv1a64("os.lazy.net.nic.v1"),
            messenger_generated::os_lazy_net_nic_v1::INTERFACE_ID
        );
    }
}
