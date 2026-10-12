//! The NIC namespace: which identities may serve a card under
//! `os.lazy.net.nic/<ifname>`, and which kinds of card each may claim.
//! The registry enforces it at `Register`; `netd` checks it again on what it
//! lists.

use crate::{NETD_UID, NET_UID, ROOT_UID, WIFISIM_UID, WIFI_UID};

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

/// The interface name of the simulated radio's air card
/// (docs/wifi-prerequisites-plan.md section 3.6): a virtio-net card whose
/// frames are `wifisim`'s 802.11-in-Ethernet air, not traffic. `netdrv`
/// serves it under this name (in `LAZYOS_WIFI_SIM=1` images, for the card
/// with [`SIM_AIR_MAC`]) and `netd` never attaches it ([`stack_attaches`]).
pub const SIM_AIR_IFNAME: &str = "air0";

/// The MAC the harness (`tools/wifi/sim_run.py`, `run_demo.py --wifi-sim`)
/// gives the air card: locally administered, "WSIM".
pub const SIM_AIR_MAC: [u8; 6] = [0x02, 0x57, 0x53, 0x49, 0x4D, 0x00];

/// The interface-name prefix of carrier cards the stack must leave alone.
const CARRIER_PREFIX: &str = "air";

/// Whether the network stack attaches the card `ifname`: every card but an
/// air card (`air0`, ...), which belongs to the simulated radio.
pub fn stack_attaches(ifname: &str) -> bool {
    !ifname.starts_with(CARRIER_PREFIX)
}

/// Whether a caller with these kernel-stamped credentials may act as the
/// owner of the card `ifname` served by a driver running as `server_uid`:
/// every `os.lazy.net.nic.v1` method but `Info` and `Stats` (`AttachRing`,
/// `DetachRing`, `SetRxMode`, `Kick`). Whoever attaches the ring reads and
/// injects every frame of the card, so a driver must ask this before it
/// takes a first attachment, or a session task could attach before `netd`
/// (or while `netd` restarts) and own the wire.
///
/// * `_netd`, unlabelled and outside a session: any card the stack attaches
///   ([`stack_attaches`]), never an air card.
/// * `_wifisim`, likewise: air cards only (`air0`, the simulated radio's
///   carrier).
/// * The driver's own identity, if it is a NIC driver identity: the
///   evidence clients (`nicctl`) a `demo=1` driver spawns run as the driver.
/// * Root, unlabelled: the console image's boot identity and its diagnostic
///   tools (it holds every capability anyway; no desktop task runs as root).
///
/// Everyone else is refused, labelled apps and session tasks first. The
/// table [`crate::NIC_CLIENT_RULES`] says the same per interface, but it
/// has no instance names and no runtime loader yet: this is the check that
/// holds, in every server of a NIC name (`netdrv`, the Wi-Fi NIC servers).
pub fn may_own_nic(
    caller_uid: u32,
    label_id: u32,
    session: u64,
    server_uid: u32,
    ifname: &str,
) -> bool {
    if label_id != 0 {
        return false;
    }
    if caller_uid == ROOT_UID {
        return true;
    }
    if session != 0 {
        return false;
    }
    let carrier = !stack_attaches(ifname);
    match caller_uid {
        NETD_UID => !carrier,
        WIFISIM_UID if carrier => true,
        uid => uid == server_uid && NIC_DRIVERS.iter().any(|&(driver, _)| driver == uid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NETD_UID, WLAN_UID};

    #[test]
    fn netd_owns_every_card_but_the_air() {
        for ifname in ["eth0", "eth3", "wlan0"] {
            for server in [NET_UID, WIFI_UID, WIFISIM_UID, ROOT_UID] {
                assert!(may_own_nic(NETD_UID, 0, 0, server, ifname), "{ifname}");
            }
        }
        assert!(!may_own_nic(NETD_UID, 0, 0, NET_UID, SIM_AIR_IFNAME));
        assert!(!may_own_nic(NETD_UID, 0, 0, NET_UID, "air1"));
    }

    #[test]
    fn wifisim_owns_air_cards_only() {
        assert!(may_own_nic(WIFISIM_UID, 0, 0, NET_UID, SIM_AIR_IFNAME));
        for ifname in ["eth0", "eth1", "wlan0"] {
            assert!(!may_own_nic(WIFISIM_UID, 0, 0, NET_UID, ifname), "{ifname}");
            assert!(
                !may_own_nic(WIFISIM_UID, 0, 0, WIFI_UID, ifname),
                "{ifname}"
            );
        }
        // Its own simulated radio's card is its own.
        assert!(may_own_nic(WIFISIM_UID, 0, 0, WIFISIM_UID, "wlan0"));
    }

    #[test]
    fn a_driver_owns_its_own_card_for_its_evidence_clients() {
        assert!(may_own_nic(NET_UID, 0, 0, NET_UID, "eth0"));
        assert!(may_own_nic(WIFI_UID, 0, 0, WIFI_UID, "wlan0"));
        // Not another driver identity's card.
        assert!(!may_own_nic(NET_UID, 0, 0, WIFI_UID, "wlan0"));
        assert!(!may_own_nic(WIFI_UID, 0, 0, NET_UID, "eth0"));
        // "Its own" only counts for a driver identity.
        assert!(!may_own_nic(1000, 0, 0, 1000, "eth0"));
        assert!(!may_own_nic(WLAN_UID, 0, 0, WLAN_UID, "wlan0"));
    }

    #[test]
    fn sessions_apps_and_other_services_are_refused() {
        for ifname in ["eth0", "wlan0", SIM_AIR_IFNAME] {
            for uid in [
                1,
                901,
                904,
                905,
                906,
                909,
                910,
                WLAN_UID,
                999,
                1000,
                1001,
                u32::MAX,
            ] {
                assert!(!may_own_nic(uid, 0, 0, NET_UID, ifname), "{uid} {ifname}");
                assert!(!may_own_nic(uid, 0, 3, NET_UID, ifname), "{uid} {ifname}");
            }
            // A service uid inside a session or under a label is not that
            // service.
            for (label, session) in [(4, 0), (0, 1), (u32::MAX, u64::MAX)] {
                assert!(!may_own_nic(NETD_UID, label, session, NET_UID, ifname));
                assert!(!may_own_nic(WIFISIM_UID, label, session, NET_UID, ifname));
                assert!(!may_own_nic(NET_UID, label, session, NET_UID, ifname));
            }
        }
    }

    #[test]
    fn root_owns_any_card_unless_labelled() {
        for ifname in ["eth0", "wlan0", SIM_AIR_IFNAME] {
            assert!(may_own_nic(ROOT_UID, 0, 0, NET_UID, ifname));
            assert!(may_own_nic(ROOT_UID, 0, 2, ROOT_UID, ifname));
            assert!(!may_own_nic(ROOT_UID, 7, 0, NET_UID, ifname));
        }
    }
}
