//! Decoders and names for the device syscall's read-only inspection ops
//! (`kernel/src/dev/inspect.rs`, issue #481): the device inventory with each
//! owner and its rights, the driver class rules the kernel installed at boot,
//! and the refused claims in the audit ring.
//!
//! Shared by `devctl` (native) and the Devices app (static musl), so the row
//! layouts and the names live in one place. Pure data: the callers issue the
//! syscall themselves (`int 0x80`, syscall 23, `op` / buffer / capacity).

#![no_std]

use core::fmt;

/// The device syscall number.
pub const SYS_DEV: u64 = 23;

/// Inspection op codes (`dev::syscall::OP_*`).
pub mod op {
    pub const INVENTORY: u64 = 10;
    pub const POLICY: u64 = 11;
    pub const DENIALS: u64 = 12;
}

/// `u64` words per row of each op.
pub const INVENTORY_WORDS: usize = 4;
pub const RULE_WORDS: usize = 3;
pub const DENIAL_WORDS: usize = 4;

/// Errno values the inspection ops return.
pub mod errno {
    pub const EPERM: i64 = 1;
    /// `policy`: no class policy is installed.
    pub const ENOENT: i64 = 2;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
}

/// `Device` handle rights (`ipc::handles::rights::DEV_*`).
pub mod rights {
    pub const MMIO: u32 = 1 << 8;
    pub const PIO: u32 = 1 << 9;
    pub const IRQ: u32 = 1 << 10;
    pub const DMA: u32 = 1 << 11;
    pub const CONFIG: u32 = 1 << 12;
}

/// Wildcard actor and method in a rule.
pub const ANY_ACTOR: u32 = u32::MAX;
pub const ANY_METHOD: u32 = u32::MAX;

/// The FNV-1a hashes every Messenger id uses (`tools/midlc`): 64-bit for
/// interfaces, 32-bit with the top bit cleared for methods (`ipc::topics`).
pub const fn fnv1a64(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

pub const fn fnv1a32(text: &str) -> u32 {
    let bytes = text.as_bytes();
    let mut hash = 0x811c_9dc5u32;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash & 0x7FFF_FFFF
}

/// The device classes (`dev::class`), as `(short name, interface id)`.
pub const CLASSES: &[(&str, u64)] = &[
    ("storage", fnv1a64("os.kernel.dev.storage")),
    ("net", fnv1a64("os.kernel.dev.net")),
    ("display", fnv1a64("os.kernel.dev.display")),
    ("audio", fnv1a64("os.kernel.dev.audio")),
    ("multimedia", fnv1a64("os.kernel.dev.multimedia")),
    ("bridge", fnv1a64("os.kernel.dev.bridge")),
    ("serial", fnv1a64("os.kernel.dev.serial")),
    ("usb", fnv1a64("os.kernel.dev.usb")),
    ("system", fnv1a64("os.kernel.dev.system")),
    ("other", fnv1a64("os.kernel.dev.other")),
];

/// The short name of a class interface id, `"?"` if unknown.
pub fn class_name(interface_id: u64) -> &'static str {
    CLASSES
        .iter()
        .find(|(_, id)| *id == interface_id)
        .map_or("?", |(name, _)| name)
}

/// The name of a class-rule method id.
pub fn method_name(method: u32) -> &'static str {
    const METHODS: &[(&str, u32)] = &[
        ("claim", fnv1a32("claim")),
        ("map", fnv1a32("map")),
        ("dma", fnv1a32("dma")),
    ];
    if method == ANY_METHOD {
        return "*";
    }
    METHODS
        .iter()
        .find(|(_, id)| *id == method)
        .map_or("?", |(name, _)| name)
}

/// The system user a uid names, if it is one of the well-known ones.
pub fn uid_name(uid: u32) -> Option<&'static str> {
    match uid {
        0 => Some("root"),
        sndpolicy::SND_UID => Some("_snd"),
        netpolicy::NET_UID => Some("_net"),
        netpolicy::NETD_UID => Some("_netd"),
        usbpolicy::USB_UID => Some("_usb"),
        ANY_ACTOR => Some("*"),
        _ => None,
    }
}

/// Why a claim was refused, from an audit reason code (`ipc::acl::reason`
/// and `dev::report::reason`).
pub fn reason_name(code: u32) -> &'static str {
    match code {
        2 => "no Messenger rule",
        3 => "Messenger deny rule",
        5 => "app not granted",
        0x13 => "no CAP_DEV_CLAIM",
        0x14 => "no rights",
        0x15 => "busy",
        0x17 => "quota",
        0x18 => "bad endpoint",
        0x19 => "line busy",
        0x22 => "class rule",
        _ => "other",
    }
}

/// Rights bits, displayed as `MMIO PIO IRQ DMA CFG` (`-` for none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(pub u32);

impl fmt::Display for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = [
            (rights::MMIO, "MMIO"),
            (rights::PIO, "PIO"),
            (rights::IRQ, "IRQ"),
            (rights::DMA, "DMA"),
            (rights::CONFIG, "CFG"),
        ];
        let mut first = true;
        for (bit, name) in names {
            if self.0 & bit != 0 {
                if !first {
                    f.write_str(" ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("-")?;
        }
        Ok(())
    }
}

/// A uid shown as its system name when it has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uid(pub u32);

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match uid_name(self.0) {
            Some(name) if self.0 != ANY_ACTOR => write!(f, "{name}({})", self.0),
            Some(name) => f.write_str(name),
            None => write!(f, "{}", self.0),
        }
    }
}

/// One inventory row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub vendor: u16,
    pub device: u16,
    pub class_id: u64,
    /// The owner's uid, `None` when the device is free.
    pub owner: Option<u32>,
    pub rights: Rights,
}

impl Device {
    pub fn from_words(w: &[u64; INVENTORY_WORDS]) -> Device {
        let owner = (w[3] & 0xFFFF_FFFF) as u32;
        Device {
            id: w[0] as u16,
            class: (w[0] >> 16) as u8,
            subclass: (w[0] >> 24) as u8,
            prog_if: (w[0] >> 32) as u8,
            vendor: w[1] as u16,
            device: (w[1] >> 16) as u16,
            class_id: w[2],
            owner: (owner != u32::MAX).then_some(owner),
            rights: Rights((w[3] >> 32) as u32),
        }
    }

    pub fn class_name(&self) -> &'static str {
        class_name(self.class_id)
    }
}

/// One installed class rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub actor: u32,
    pub interface_id: u64,
    pub method: u32,
    pub allow: bool,
}

impl Rule {
    pub fn from_words(w: &[u64; RULE_WORDS]) -> Rule {
        Rule {
            actor: w[0] as u32,
            interface_id: w[1],
            method: w[2] as u32,
            allow: (w[2] >> 32) & 1 == 1,
        }
    }
}

/// One refused claim from the audit ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Denial {
    /// PIT ticks (100 Hz) since boot.
    pub ticks: u64,
    pub uid: u32,
    pub reason: u32,
    pub class_id: u64,
    pub device: u16,
}

impl Denial {
    pub fn from_words(w: &[u64; DENIAL_WORDS]) -> Denial {
        Denial {
            ticks: w[0],
            uid: w[1] as u32,
            reason: (w[1] >> 32) as u32,
            class_id: w[2],
            device: w[3] as u16,
        }
    }
}

/// Decode a flat word buffer of `count` rows (as the ops fill it).
pub fn rows<const N: usize>(words: &[u64], count: usize) -> impl Iterator<Item = [u64; N]> + '_ {
    words.as_chunks::<N>().0.iter().take(count).copied()
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::string::ToString;

    #[test]
    fn hashes_match_the_kernel_spelling() {
        // FNV-1a test vectors.
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
        // The 31-bit method id: FNV-1a-32 of "a" is 0xe40c292c.
        assert_eq!(fnv1a32("a"), 0x640c_292c);
        assert_eq!(class_name(fnv1a64("os.kernel.dev.usb")), "usb");
        assert_eq!(class_name(1), "?");
        assert_eq!(method_name(fnv1a32("dma")), "dma");
        assert_eq!(method_name(ANY_METHOD), "*");
    }

    #[test]
    fn rows_decode_like_the_kernel_encodes_them() {
        let device = Device::from_words(&[
            7 | 0x02 << 16,
            0x1AF4 | 0x1000 << 16,
            fnv1a64("os.kernel.dev.net"),
            u64::from(netpolicy::NET_UID) | u64::from(rights::MMIO | rights::DMA) << 32,
        ]);
        assert_eq!(device.id, 7);
        assert_eq!(device.class_name(), "net");
        assert_eq!(device.owner, Some(902));
        assert_eq!(device.rights.to_string(), "MMIO DMA");
        let free = Device::from_words(&[0, 0, 0, u64::from(u32::MAX)]);
        assert_eq!(free.owner, None);
        assert_eq!(free.rights.to_string(), "-");

        let rule = Rule::from_words(&[
            904,
            fnv1a64("os.kernel.dev.usb"),
            u64::from(fnv1a32("claim")) | 1 << 32,
        ]);
        assert!(rule.allow);
        assert_eq!(Uid(rule.actor).to_string(), "_usb(904)");
        assert_eq!(method_name(rule.method), "claim");

        let denial = Denial::from_words(&[500, 904 | 0x22 << 32, fnv1a64("os.kernel.dev.net"), 3]);
        assert_eq!((denial.uid, denial.device), (904, 3));
        assert_eq!(reason_name(denial.reason), "class rule");
        assert_eq!(class_name(denial.class_id), "net");
    }

    #[test]
    fn rows_stop_at_the_count() {
        let words = [1u64, 2, 3, 4, 5, 6, 7];
        let decoded: std::vec::Vec<[u64; 3]> = rows::<3>(&words, 5).collect();
        assert_eq!(decoded, [[1, 2, 3], [4, 5, 6]]);
    }

    #[test]
    fn well_known_uids_have_names() {
        assert_eq!(Uid(0).to_string(), "root(0)");
        assert_eq!(Uid(1000).to_string(), "1000");
        assert_eq!(Uid(ANY_ACTOR).to_string(), "*");
    }
}
