//! `devd`'s static driver manifest (issue #497, docs/driver-plan.md 3.6):
//! which driver row of `init`'s manifest each enumerated PCI function
//! matches, and which device each row is started for.
//!
//! A row names a program `init` knows (`netdrv`, `sndd`); the program's path,
//! uid, arguments and restart policy are `init`'s, never `devd`'s. Matching is
//! by exact vendor and device ids, or by PCI class for a register interface
//! that is the same on every vendor's part (HDA).
//!
//! A driver whose card has a name of its own runs once per card: every
//! network card gets an interface name (`eth0`, `eth1`, ... in the kernel's
//! enumeration order) and serves `os.lazy.net.nic/<name>`. A driver row whose
//! Messenger name is unique (`os.lazy.audio.card`) serves one device: the
//! first matching device in enumeration order wins and later ones are
//! reported `busy`.
//!
//! Pure data and logic, `no_std`, host tested.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
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
    /// The interface-name prefix of a driver that runs once per card
    /// (`eth` gives `eth0`, `eth1`, ...); `None` for a driver with one
    /// device only.
    pub ifname_prefix: Option<&'static str>,
}

/// The device ids `netdrv`'s 8254x back end accepts (`libs/e1000`); a test
/// keeps the two in step.
pub const E1000_DEVICES: &[u16] = &[0x100E, 0x100F, 0x1008, 0x1010, 0x1011, 0x1026];

/// The device ids `netdrv`'s Realtek back end accepts (`libs/rtl8168`); a
/// test keeps the two in step. Whether the revision behind the id is
/// supported is the driver's answer (it parks on the others).
pub const RTL8168_DEVICES: &[u16] = &[0x8168];

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
        ifname_prefix: Some("eth"),
    },
    Entry {
        driver: "netdrv",
        model: "Intel 8254x",
        matches: Match::Ids {
            vendor: 0x8086,
            devices: E1000_DEVICES,
        },
        ifname_prefix: Some("eth"),
    },
    Entry {
        driver: "netdrv",
        model: "Realtek RTL8168",
        matches: Match::Ids {
            vendor: 0x10EC,
            devices: RTL8168_DEVICES,
        },
        ifname_prefix: Some("eth"),
    },
    Entry {
        driver: "sndd",
        model: "virtio-sound",
        matches: Match::Ids {
            vendor: 0x1AF4,
            devices: &[0x1059],
        },
        ifname_prefix: None,
    },
    Entry {
        driver: "sndd",
        model: "Intel HDA",
        matches: Match::Class {
            class: 0x04,
            subclass: 0x03,
        },
        ifname_prefix: None,
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

/// One driver start the manifest asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub driver: &'static str,
    /// The enumerated function's id.
    pub id: u16,
    pub entry: &'static Entry,
    /// The interface name for a driver that runs once per card; empty
    /// otherwise.
    pub ifname: String,
}

/// The driver starts to ask for, in enumeration order. A driver with an
/// interface prefix starts for every matching function and names them
/// `<prefix>0`, `<prefix>1`, ... (one counter per prefix, so a virtio-net and
/// an 8254x count together); any other driver row starts for the first
/// matching function only.
pub fn plan(functions: &[Function]) -> Vec<Planned> {
    let mut chosen: Vec<Planned> = Vec::new();
    let mut named: Vec<(&'static str, usize)> = Vec::new();
    for function in functions {
        let Some(entry) = entry_for(function) else {
            continue;
        };
        let ifname = match entry.ifname_prefix {
            Some(prefix) => {
                let at = match named.iter().position(|(p, _)| *p == prefix) {
                    Some(at) => at,
                    None => {
                        named.push((prefix, 0));
                        named.len() - 1
                    }
                };
                let number = named[at].1;
                named[at].1 += 1;
                format!("{prefix}{number}")
            }
            None if chosen.iter().any(|p| p.driver == entry.driver) => continue,
            None => String::new(),
        };
        chosen.push(Planned {
            driver: entry.driver,
            id: function.id,
            entry,
            ifname,
        });
    }
    chosen
}

/// Whether `name` is an interface name `init` accepts for a driver: lower-case
/// letters then at least one digit, at most 15 bytes (the length Linux's
/// `IFNAMSIZ` allows), so it is safe inside a registry name, a topic and an
/// argument.
pub fn valid_ifname(name: &str) -> bool {
    let letters = name.bytes().take_while(u8::is_ascii_lowercase).count();
    let digits = &name[letters..];
    letters > 0
        && !digits.is_empty()
        && name.len() <= 15
        && digits.bytes().all(|b| b.is_ascii_digit())
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
            (
                0x10EC,
                0x8168,
                0x02,
                0x00,
                Some(("netdrv", "Realtek RTL8168")),
            ),
            (0x1AF4, 0x1059, 0x04, 0x01, Some(("sndd", "virtio-sound"))),
            (0x8086, 0x2668, 0x04, 0x03, Some(("sndd", "Intel HDA"))),
            (0x8086, 0x54C8, 0x04, 0x03, Some(("sndd", "Intel HDA"))),
            (0x1002, 0x1640, 0x04, 0x03, Some(("sndd", "Intel HDA"))),
            // Not this manifest's: the boot disk, a bridge, an e1000e, xHCI.
            (0x1AF4, 0x1001, 0x01, 0x00, None),
            (0x8086, 0x1237, 0x06, 0x00, None),
            (0x8086, 0x10D3, 0x02, 0x00, None),
            // The Wi-Fi next to the box's Ethernet.
            (0x10EC, 0xC822, 0x02, 0x80, None),
            (0x1B36, 0x000D, 0x0C, 0x03, None),
        ];
        for (vendor, device, class, subclass, want) in cases {
            let got = entry_for(&function(0, vendor, device, class, subclass))
                .map(|entry| (entry.driver, entry.model));
            assert_eq!(got, want, "{vendor:04x}:{device:04x}");
        }
    }

    #[test]
    fn every_network_card_is_named_and_a_sound_card_is_unique() {
        let functions = [
            function(3, 0x8086, 0x1237, 0x06, 0x00),
            function(5, 0x8086, 0x100E, 0x02, 0x00),
            function(6, 0x1AF4, 0x1000, 0x02, 0x00),
            function(7, 0x8086, 0x2668, 0x04, 0x03),
            function(8, 0x1AF4, 0x1059, 0x04, 0x01),
        ];
        let planned = plan(&functions);
        let plan: Vec<(&str, u16, &str)> = planned
            .iter()
            .map(|p| (p.driver, p.id, p.ifname.as_str()))
            .collect();
        assert_eq!(
            plan,
            [("netdrv", 5, "eth0"), ("netdrv", 6, "eth1"), ("sndd", 7, "")]
        );
        assert!(super::plan(&[]).is_empty());
    }

    #[test]
    fn interface_names_are_checked() {
        for good in ["eth0", "eth12", "wlan0", "en1"] {
            assert!(valid_ifname(good), "{good}");
        }
        for bad in [
            "",
            "eth",
            "0",
            "Eth0",
            "eth0/x",
            "eth0 ",
            "e-th0",
            "eth0a",
            "abcdefghijklmnop1",
        ] {
            assert!(!valid_ifname(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_manifest_agrees_with_the_drivers() {
        // netdrv's 8254x back end takes exactly these ids.
        let mut e1000: Vec<u16> = e1000::DEVICES.iter().map(|(id, _)| *id).collect();
        let mut ours = E1000_DEVICES.to_vec();
        e1000.sort_unstable();
        ours.sort_unstable();
        assert_eq!(e1000, ours);
        // netdrv's Realtek back end takes exactly these ids.
        let mut rtl: Vec<u16> = rtl8168::DEVICES.iter().map(|(id, _)| *id).collect();
        let mut ours = RTL8168_DEVICES.to_vec();
        rtl.sort_unstable();
        ours.sort_unstable();
        assert_eq!(rtl, ours);
        // sndd's HDA back end takes exactly this class.
        assert!(hda::is_controller(0x04, 0x03));
        for entry in MANIFEST {
            assert!(matches!(entry.driver, "netdrv" | "sndd"), "{entry:?}");
        }
    }

    #[test]
    fn the_kabylake_box_gets_its_ethernet_driver_and_nothing_for_its_wifi() {
        let functions = [
            function(1, 0x8086, 0x5916, 0x03, 0x00),
            function(2, 0x8086, 0x9d2f, 0x0c, 0x03),
            function(3, 0x10EC, 0xC822, 0x02, 0x80),
            function(4, 0x10EC, 0x8168, 0x02, 0x00),
        ];
        let planned = plan(&functions);
        let plan: Vec<(&str, u16, &str)> = planned
            .iter()
            .map(|p| (p.driver, p.id, p.ifname.as_str()))
            .collect();
        assert_eq!(plan, [("netdrv", 4, "eth0")]);
    }

    #[test]
    fn devd_is_an_unprivileged_system_user() {
        const { assert!(DEVD_UID > 905 && DEVD_UID < 1000) };
    }
}
