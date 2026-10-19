//! `devd`'s static driver manifest (issue #497, docs/driver-plan.md 3.6):
//! which driver row of `init`'s manifest each enumerated PCI function
//! matches, and which device each row is started for.
//!
//! A row names a program `init` knows (`netdrv`, `sndd`, `wifid`); the program's path,
//! uid, arguments and restart policy are `init`'s, never `devd`'s. Matching is
//! by exact vendor and device ids, or by PCI class for a register interface
//! that is the same on every vendor's part (HDA).
//!
//! A driver whose card has a name of its own runs once per card: every
//! network card gets an interface name in the kernel's enumeration order, one
//! counter per prefix (`eth0`, `eth1`, ... for wired cards, `wlan0`, `wlan1`,
//! ... for wireless ones), and serves `os.lazy.net.nic/<name>`. A driver row whose
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
/// 907 `_greeter`, 908 `_accounts`, 909 `_elev`, 910 `_mountd`, 911 `_wifi`
/// (`wifid`, the Wi-Fi chip driver, `netpolicy::WIFI_UID`), 914 `_dbgd`
/// (`dbgwire::config::DBGD_UID`). Reserved for the Wi-Fi plan, defined with
/// their programs: 912 `_wlan` (`wlanmd`, the station manager), 913
/// `_wifisim` (`wifisim`, the CI simulator); docs/wifi-prerequisites-plan.md
/// section 2; 915 `_gpu` (`gpud`, `gpupolicy::GPU_UID`). The next free uid
/// is 916.)
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

/// The device ids `wifid` drives (`libs/rtw88`'s `DEVICES`, Realtek's vendor
/// id); a test keeps the two in step.
pub const RTW88_DEVICES: &[u16] = &[0xC822];

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
        driver: "wifid",
        model: "Realtek RTL8822CE",
        matches: Match::Ids {
            vendor: 0x10EC,
            devices: RTW88_DEVICES,
        },
        ifname_prefix: Some("wlan"),
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

/// The device ids `gpud` drives (`libs/intelgfx`'s targets, Intel's vendor
/// id): the Kaby Lake GT2 HD Graphics 620 of the i3-7100U box (Gen9) and the
/// Alder Lake-UP3 GT1 UHD Graphics of the i3-1220P box (display 13).
pub const INTELGFX_DEVICES: &[u16] = &[0x5916, 0x46B3];

/// The entries only an image built with `LAZYOS_INTELGFX=1` matches
/// (docs/kabylake-gpu-plan.md section 7): `devd` passes them to
/// [`plan_with`] and [`entry_for_with`] when it is built with that switch,
/// so an ordinary image does not even name the GPU. After [`MANIFEST`] in
/// priority; nothing in it overlaps a [`MANIFEST`] entry.
pub const INTELGFX_MANIFEST: &[Entry] = &[Entry {
    driver: "gpud",
    model: "Intel graphics",
    matches: Match::Ids {
        vendor: 0x8086,
        devices: INTELGFX_DEVICES,
    },
    ifname_prefix: None,
}];

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
    entry_for_with(function, &[])
}

/// [`entry_for`] with `extra` entries after [`MANIFEST`]'s.
pub fn entry_for_with(function: &Function, extra: &'static [Entry]) -> Option<&'static Entry> {
    MANIFEST
        .iter()
        .chain(extra)
        .find(|entry| match entry.matches {
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
    plan_with(functions, &[])
}

/// [`plan`] with `extra` entries after [`MANIFEST`]'s (see
/// [`INTELGFX_MANIFEST`]).
pub fn plan_with(functions: &[Function], extra: &'static [Entry]) -> Vec<Planned> {
    let mut chosen: Vec<Planned> = Vec::new();
    let mut named: Vec<(&'static str, usize)> = Vec::new();
    for function in functions {
        let Some(entry) = entry_for_with(function, extra) else {
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
            (
                0x10EC,
                0xC822,
                0x02,
                0x80,
                Some(("wifid", "Realtek RTL8822CE")),
            ),
            // Another Realtek Wi-Fi chip (RTL8821CE): not driven yet.
            (0x10EC, 0xC821, 0x02, 0x80, None),
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
            [
                ("netdrv", 5, "eth0"),
                ("netdrv", 6, "eth1"),
                ("sndd", 7, "")
            ]
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
        // wifid takes exactly the rtw88 ids, of Realtek's vendor id.
        let mut rtw: Vec<u16> = rtw88::DEVICES.iter().map(|(id, _)| *id).collect();
        let mut ours = RTW88_DEVICES.to_vec();
        rtw.sort_unstable();
        ours.sort_unstable();
        assert_eq!(rtw, ours);
        for &device in RTW88_DEVICES {
            assert!(rtw88::model(rtw88::VENDOR, device).is_some());
        }
        // sndd's HDA back end takes exactly this class.
        assert!(hda::is_controller(0x04, 0x03));
        for entry in MANIFEST {
            assert!(
                matches!(entry.driver, "netdrv" | "sndd" | "wifid"),
                "{entry:?}"
            );
        }
    }

    #[test]
    fn the_kaby_lake_box_gets_its_ethernet_and_its_wifi_each_with_its_own_name() {
        // The Kaby Lake box (docs/compat/kabylake): the Wi-Fi enumerates
        // before the Ethernet and still gets `wlan0`, the Ethernet `eth0`.
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
        assert_eq!(plan, [("wifid", 3, "wlan0"), ("netdrv", 4, "eth0")]);
    }

    #[test]
    fn wireless_cards_count_apart_from_wired_ones() {
        let functions = [
            function(1, 0x10EC, 0xC822, 0x02, 0x80),
            function(2, 0x1AF4, 0x1000, 0x02, 0x00),
            function(3, 0x10EC, 0xC822, 0x02, 0x80),
            function(4, 0x8086, 0x100E, 0x02, 0x00),
        ];
        let named: Vec<(u16, String)> = plan(&functions)
            .into_iter()
            .map(|p| (p.id, p.ifname))
            .collect();
        let want = [(1, "wlan0"), (2, "eth0"), (3, "wlan1"), (4, "eth1")];
        assert_eq!(named.len(), want.len());
        for ((id, name), (want_id, want_name)) in named.iter().zip(want) {
            assert_eq!((*id, name.as_str()), (want_id, want_name));
            assert!(valid_ifname(name), "{name}");
        }
    }

    #[test]
    fn the_gpu_matches_only_with_the_intelgfx_manifest() {
        let gpu = function(2, 0x8086, 0x5916, 0x03, 0x00);
        let alder_lake = function(2, 0x8086, 0x46B3, 0x03, 0x00);
        // An ordinary image never names the GPU, whatever else it matches.
        assert!(entry_for(&gpu).is_none() && entry_for(&alder_lake).is_none());
        assert!(entry_for_with(&alder_lake, INTELGFX_MANIFEST).is_some_and(|e| e.driver == "gpud"));
        assert!(plan(&[gpu]).is_empty());
        let entry = entry_for_with(&gpu, INTELGFX_MANIFEST).expect("matched with the switch");
        assert_eq!((entry.driver, entry.ifname_prefix), ("gpud", None));
        let planned = plan_with(&[gpu], INTELGFX_MANIFEST);
        assert_eq!(planned.len(), 1);
        assert_eq!((planned[0].driver, planned[0].id), ("gpud", 2));
        assert!(planned[0].ifname.is_empty());
        // Exactly the listed ids; another Intel display function is not taken,
        // nor is any other class of the same id.
        for other in [
            function(2, 0x8086, 0x5917, 0x03, 0x00),
            function(2, 0x8086, 0x5912, 0x03, 0x00),
            function(2, 0x8086, 0x46A3, 0x03, 0x00),
            function(2, 0x1AF4, 0x5916, 0x03, 0x00),
        ] {
            assert!(entry_for_with(&other, INTELGFX_MANIFEST).is_none());
        }
        // The extra rows never shadow the main manifest's.
        for entry in MANIFEST {
            for extra in INTELGFX_MANIFEST {
                assert_ne!(entry.driver, extra.driver);
            }
        }
        // Two display functions: one `gpud` only, as for every one-device row.
        let two = [gpu, function(3, 0x8086, 0x5916, 0x03, 0x00)];
        assert_eq!(plan_with(&two, INTELGFX_MANIFEST).len(), 1);
    }

    #[test]
    fn devd_is_an_unprivileged_system_user() {
        const { assert!(DEVD_UID > 905 && DEVD_UID < 1000) };
    }
}
