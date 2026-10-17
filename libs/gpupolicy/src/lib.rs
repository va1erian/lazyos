//! The access-control rules of the Intel GPU service (`gpud`,
//! `docs/kabylake-gpu-plan.md` section 3), as data.
//!
//! Like `libs/netpolicy`, `libs/usbpolicy` and `libs/sndpolicy`, the kernel
//! installs these device-class rules at boot (`dev::policy`), so `_gpu` may
//! claim a display-class function and nothing else, and no other driver uid
//! gets the display class.

#![no_std]

/// The `_gpu` system user `gpud` runs as: only `CAP_DEV_CLAIM`. (The next
/// free uid after `_dbgd` 914: `devmatch::DEVD_UID`'s comment lists the rest.)
pub const GPU_UID: u32 = 915;

/// The ACL device class of a display controller (`dev::class::DISPLAY`, PCI
/// base class `03`).
pub const DISPLAY_CLASS: &str = "os.kernel.dev.display";

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

/// What `_gpu` may do to a device: claim a display function and take the
/// MMIO and DMA rights a graphics driver needs (`map` for the register BAR
/// and the aperture, `dma` for the command-buffer memory of the later
/// stages and, already in G1, for restoring the function's bus-master bit
/// that every claim clears). Nothing names another class.
pub const GPU_DRIVER_CLASS_RULES: &[RuleSpec] = &[
    allow(GPU_UID, DISPLAY_CLASS, "claim"),
    allow(GPU_UID, DISPLAY_CLASS, "map"),
    allow(GPU_UID, DISPLAY_CLASS, "dma"),
];

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn the_driver_gets_exactly_claim_map_and_dma_on_the_display_class() {
        let methods: std::vec::Vec<_> = GPU_DRIVER_CLASS_RULES.iter().map(|r| r.method).collect();
        assert_eq!(methods, ["claim", "map", "dma"]);
        for rule in GPU_DRIVER_CLASS_RULES {
            assert!(rule.allow);
            assert_eq!(rule.actor, GPU_UID);
            assert_eq!(
                rule.interface, DISPLAY_CLASS,
                "no other device class is named"
            );
            assert_ne!(rule.method, "*", "no wildcard grants");
        }
    }

    #[test]
    fn the_gpu_uid_is_its_own_system_uid() {
        const { assert!(GPU_UID < 1000) };
        // 901 `_snd` .. 904 `_usb`, 905 `_audio`, 906 `_devd`, 907
        // `_greeter`, 908 `_accounts`, 909 `_elev`, 910 `_mountd`, 911
        // `_wifi`, 912 `_wlan`, 913 `_wifisim`, 914 `_dbgd`.
        const { assert!(GPU_UID > 914) };
    }
}
