//! The NIC namespace: which identities may serve a card under
//! `os.lazy.net.nic/<ifname>`, and which kinds of card each may claim.
//! The registry enforces it at `Register`; `netd` checks it again on what it
//! lists.

use crate::{NET_UID, ROOT_UID, WIFISIM_UID, WIFI_UID};

/// The registry namespace of the cards: a driver serves its card as
/// `os.lazy.net.nic/<ifname>`.
pub const NIC_NAME_PREFIX: &str = "os.lazy.net.nic";

/// `NicInfo.kind` of an Ethernet-class card (`idl/net.midl`).
pub const NIC_KIND_WIRED: u32 = 0;
/// `NicInfo.kind` of a Wi-Fi card.
pub const NIC_KIND_WIRELESS: u32 = 1;

/// Whether `name` lies in the NIC namespace the registry reserves for
/// drivers: the bare prefix and everything under `os.lazy.net.nic/`, an empty
/// or invalid interface name included (a reserved name is refused first and
/// judged for validity later). `os.lazy.net.nicX` is not in it.
pub fn is_nic_name(name: &str) -> bool {
    match name.strip_prefix(NIC_NAME_PREFIX) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// The bit of `kind` in a set of card kinds (`NicInfo.kind`).
const fn kind_bit(kind: u32) -> u32 {
    if kind < 32 {
        1 << kind
    } else {
        0
    }
}

/// The identities that may register a NIC name, and the card kinds each may
/// claim. `_net` drives Ethernet only; the Wi-Fi uids drive wireless cards
/// only. Root is the boot identity of a console image, where the kernel
/// starts `netdrv` itself with no supervisor (`LAZYOS_NET=1` without
/// services): it holds every capability anyway and no desktop task runs as
/// it ("nobody is root").
const NIC_DRIVERS: &[(u32, u32)] = &[
    (NET_UID, kind_bit(NIC_KIND_WIRED)),
    (WIFI_UID, kind_bit(NIC_KIND_WIRELESS)),
    (WIFISIM_UID, kind_bit(NIC_KIND_WIRELESS)),
    (
        ROOT_UID,
        kind_bit(NIC_KIND_WIRED) | kind_bit(NIC_KIND_WIRELESS),
    ),
];

/// Whether a task with these kernel-stamped credentials may hold a name in
/// the NIC namespace: a driver identity ([`NIC_DRIVERS`]) with no label (an
/// app or a development run has one) and no login session. The registry
/// enforces it at `Register`; `netd` checks it again on what it lists.
pub fn may_register_nic_name(uid: u32, label_id: u32, session: u64) -> bool {
    label_id == 0 && session == 0 && NIC_DRIVERS.iter().any(|&(driver, _)| driver == uid)
}

/// Whether the driver identity `uid` may claim a card of `kind`
/// (`NicInfo.kind`). An unknown kind or uid is refused.
pub fn nic_kind_allowed(uid: u32, kind: u32) -> bool {
    NIC_DRIVERS
        .iter()
        .any(|&(driver, kinds)| driver == uid && kinds & kind_bit(kind) != 0)
}
