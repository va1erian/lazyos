//! `devd`'s static driver manifest (issue #497, docs/driver-plan.md 3.6):
//! which driver row of `init`'s manifest each enumerated PCI function
//! matches, and which device each row is started for.
//!
//! A row names a program `init` knows (`netdrv`, `sndd`); the program's path,
//! uid, arguments and restart policy are `init`'s, never `devd`'s. Matching is
//! by exact vendor and device ids, or by PCI class for a register interface
//! that is the same on every vendor's part (HDA). A driver row serves one
//! device: its Messenger name (`os.lazy.net.nic`, `os.lazy.audio.card`) is
//! unique, so the first matching device in the kernel's enumeration order
//! wins and later ones are reported `busy`.
//!
//! Pure data and logic, `no_std`, host tested.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// The `_devd` system user `devd` runs as: **no** capabilities. It reads the
/// kernel's read-only device inventory and talks to `init`; it never claims a
/// device. (901 `_snd`, 902 `_net`, 903 `_netd`, 904 `_usb`, 905 `_audio`,
/// 907 `_greeter`, 908 `_accounts`, 909 `_elev`, 910 `_mountd`. Reserved for
/// the Wi-Fi plan, defined with their programs: 911 `_wifi` (`wifid`, the
/// chip driver), 912 `_wlan` (`wlanmd`, the station manager), 913 `_wifisim`
/// (`wifisim`, the CI simulator); docs/wifi-prerequisites-plan.md section 2.
/// The next free uid after those is 914.)
pub const DEVD_UID: u32 = 906;

/// How a manifest entry recognises a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// Exactly these device ids of this vendor.
    Ids {
        vendor: u16,
        devices: &'static [u16],
    },
    /// Any function of this PCI class and subclass.
    Class { class: u8, subclass: u8 },
}

/// One manifest entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The driver row in `init`'s manifest.
    pub driver: &'static str,
    /// What the match is, for logs and the `DeviceState.model` field.
    pub model: &'static str,
    pub matches: Match,
}

/// The device ids `netdrv`'s 8254x back end accepts (`libs/e1000`); a test
/// keeps the two in step.
pub const E1000_DEVICES: &[u16] = &[0x100E, 0x100F, 0x1008, 0x1010, 0x1011, 0x1026];

/// The manifest, in priority order: an entry earlier in the list wins when
/// two match the same function.
pub const MANIFEST: &[Entry] = &[
    Entry {
        driver: "netdrv",
        model: "virtio-net",
        // Transitional and modern-only virtio-net.
        matches: Match::Ids {
            vendor: 0x1AF4,
            devices: &[0x1000, 0x1041],
        },
    },
    Entry {
        driver: "netdrv",
        model: "Intel 8254x",
        matches: Match::Ids {
            vendor: 0x8086,
            devices: E1000_DEVICES,
        },
    },
    Entry {
        driver: "sndd",
        model: "virtio-sound",
        matches: Match::Ids {
            vendor: 0x1AF4,
            devices: &[0x1059],
        },
    },
    Entry {
        driver: "sndd",
        model: "Intel HDA",
        matches: Match::Class {
            class: 0x04,
            subclass: 0x03,
        },
    },
];

/// What `devd` knows of one enumerated function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Function {
    pub id: u16,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
}

/// The manifest entry `function` matches, if any.
pub fn entry_for(function: &Function) -> Option<&'static Entry> {
    MANIFEST.iter().find(|entry| match entry.matches {
        Match::Ids { vendor, devices } => {
            function.vendor == vendor && devices.contains(&function.device)
        }
        Match::Class { class, subclass } => {
            function.class == class && function.subclass == subclass
        }
    })
}

/// The driver rows to start: `(driver, device id, entry)`, one per driver
/// row, for the first matching function in `functions`' order.
pub fn plan(functions: &[Function]) -> Vec<(&'static str, u16, &'static Entry)> {
    let mut chosen: Vec<(&'static str, u16, &'static Entry)> = Vec::new();
    for function in functions {
        let Some(entry) = entry_for(function) else {
            continue;
        };
        if chosen.iter().all(|(driver, ..)| *driver != entry.driver) {
            chosen.push((entry.driver, function.id, entry));
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(id: u16, vendor: u16, device: u16, class: u8, subclass: u8) -> Function {
        Function {
            id,
            vendor,
            device,
            class,
            subclass,
        }
    }

    #[test]
    fn each_card_matches_its_driver() {
        let cases = [
            (0x1AF4, 0x1000, 0x02, 0x00, Some(("netdrv", "virtio-net"))),
            (0x1AF4, 0x1041, 0x02, 0x00, Some(("netdrv", "virtio-net"))),
            (0x8086, 0x100E, 0x02, 0x00, Some(("netdrv", "Intel 8254x"))),
            (0x1AF4, 0x1059, 0x04, 0x01, Some(("sndd", "virtio-sound"))),
            (0x8086, 0x2668, 0x04, 0x03, Some(("sndd", "Intel HDA"))),
            (0x8086, 0x54C8, 0x04, 0x03, Some(("sndd", "Intel HDA"))),
            (0x1002, 0x1640, 0x04, 0x03, Some(("sndd", "Intel HDA"))),
            // Not this manifest's: the boot disk, a bridge, an e1000e, xHCI.
            (0x1AF4, 0x1001, 0x01, 0x00, None),
            (0x8086, 0x1237, 0x06, 0x00, None),
            (0x8086, 0x10D3, 0x02, 0x00, None),
            (0x1B36, 0x000D, 0x0C, 0x03, None),
        ];
        for (vendor, device, class, subclass, want) in cases {
            let got = entry_for(&function(0, vendor, device, class, subclass))
                .map(|entry| (entry.driver, entry.model));
            assert_eq!(got, want, "{vendor:04x}:{device:04x}");
        }
    }

    #[test]
    fn one_device_per_driver_row_first_in_enumeration_order() {
        let functions = [
            function(3, 0x8086, 0x1237, 0x06, 0x00),
            function(5, 0x8086, 0x100E, 0x02, 0x00),
            function(6, 0x1AF4, 0x1000, 0x02, 0x00),
            function(7, 0x8086, 0x2668, 0x04, 0x03),
            function(8, 0x1AF4, 0x1059, 0x04, 0x01),
        ];
        let plan: Vec<(&str, u16)> = plan(&functions)
            .into_iter()
            .map(|(driver, id, _)| (driver, id))
            .collect();
        assert_eq!(plan, [("netdrv", 5), ("sndd", 7)]);
        assert!(super::plan(&[]).is_empty());
    }

    #[test]
    fn the_manifest_agrees_with_the_drivers() {
        // netdrv's 8254x back end takes exactly these ids.
        let mut e1000: Vec<u16> = e1000::DEVICES.iter().map(|(id, _)| *id).collect();
        let mut ours = E1000_DEVICES.to_vec();
        e1000.sort_unstable();
        ours.sort_unstable();
        assert_eq!(e1000, ours);
        // sndd's HDA back end takes exactly this class.
        assert!(hda::is_controller(0x04, 0x03));
        for entry in MANIFEST {
            assert!(matches!(entry.driver, "netdrv" | "sndd"), "{entry:?}");
        }
    }

    #[test]
    fn devd_is_an_unprivileged_system_user() {
        const { assert!(DEVD_UID > 905 && DEVD_UID < 1000) };
    }
}
