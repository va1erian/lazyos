//! The access-control rules of the USB HID driver (`docs/usb-hid-plan.md`
//! U5), as data.
//!
//! Policy is a list of `(actor uid, interface, method, allow)` rules the kernel
//! evaluates first-match, default deny once a policy is installed
//! (`docs/architecture/ipc-security.md`). The kernel installs this table at
//! boot with every other driver's class rules (`dev::policy`, issue #481), so
//! `usbd` keeps its controller and no other driver uid gets one. The kernel
//! test suite checks the decisions it yields
//! (`dev_suite::sys_usb_driver_policy_is_exactly_the_class_rules`,
//! `dev_suite::sys_boot_policy_confines_each_driver_to_its_class`).

#![no_std]

/// The `_usb` system user `usbd` runs as: `CAP_DEV_CLAIM` and
/// `CAP_INPUT_SOURCE`, nothing else.
pub const USB_UID: u32 = 904;

/// The ACL device class of every USB host controller (`dev::class::USB`,
/// PCI `0C/03/xx`).
pub const USB_CLASS: &str = "os.kernel.dev.usb";

/// The uids that may serve a block device to the kernel (syscall 33 ops 0-3,
/// together with `CAP_BLOCK_PROVIDER`): the USB driver only, for the mass
/// storage class (docs/architecture/usb-storage.md).
pub const BLOCK_PROVIDER_UIDS: &[u32] = &[USB_UID];

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

/// What `_usb` may do to a device: claim a USB host controller and take the
/// MMIO and DMA rights an xHCI driver needs. Nothing names another class, so
/// `_usb` cannot claim a NIC, a sound card or a disk, and nothing grants
/// another uid a USB controller.
pub const USB_DRIVER_CLASS_RULES: &[RuleSpec] = &[
    allow(USB_UID, USB_CLASS, "claim"),
    allow(USB_UID, USB_CLASS, "map"),
    allow(USB_UID, USB_CLASS, "dma"),
];

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn the_driver_gets_exactly_claim_map_and_dma_on_the_usb_class() {
        let methods: std::vec::Vec<_> = USB_DRIVER_CLASS_RULES.iter().map(|r| r.method).collect();
        assert_eq!(methods, ["claim", "map", "dma"]);
        for rule in USB_DRIVER_CLASS_RULES {
            assert!(rule.allow);
            assert_eq!(rule.actor, USB_UID);
            assert_eq!(rule.interface, USB_CLASS, "no other device class is named");
            assert_ne!(rule.method, "*", "no wildcard grants");
        }
    }

    #[test]
    fn only_the_usb_driver_may_provide_block_devices() {
        assert_eq!(BLOCK_PROVIDER_UIDS, &[USB_UID]);
    }

    #[test]
    fn the_usb_uid_is_its_own_system_uid() {
        const { assert!(USB_UID < 1000) };
        // `_snd` is 901, `_net` 902, `_netd` 903 (`init/state.rs`).
        const { assert!(USB_UID != 901 && USB_UID != 902 && USB_UID != 903) };
    }
}
