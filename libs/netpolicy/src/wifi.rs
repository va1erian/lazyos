//! The Wi-Fi names (docs/wifi-prerequisites-plan.md WP4,
//! docs/architecture/wifi.md "Names and identities").
//!
//! * A radio serves `os.lazy.net.wifi.hw.v1` as **`os.lazy.net.wifi.hw/<ifname>`**
//!   (`wlan0`), beside its `os.lazy.net.nic/<ifname>`, the same interface
//!   name in both. Only the Wi-Fi driver identities hold names there: the
//!   station manager hands the holder of one its frames and its temporal
//!   keys.
//! * The station manager `wlanmd` serves `os.lazy.net.wifi.v1` as
//!   **`os.lazy.net.wifi`**, the one name `AddNetwork` passphrases are sent
//!   to, so only `_wlan` holds it.
//! * A radio accepts `Attach` from `_wlan` alone ([`may_attach_wifi_hw`]):
//!   the first attacher owns the radio, so an impostor must not win that race.
//!
//! The registry enforces the names at `Register` (`kernel/src/ipc/policy.rs`).

use crate::{WIFISIM_UID, WIFI_UID, WLAN_UID};

/// The registry namespace of the radios: `os.lazy.net.wifi.hw/<ifname>`.
pub const WIFI_HW_NAME_PREFIX: &str = "os.lazy.net.wifi.hw";
/// The station manager's registry name.
pub const WIFI_SERVICE_NAME: &str = "os.lazy.net.wifi";

/// `name` is `prefix` or lies under `prefix/`.
fn in_namespace(name: &str, prefix: &str) -> bool {
    match name.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// Whether `name` lies in the radios' namespace (the bare prefix included).
pub fn is_wifi_hw_name(name: &str) -> bool {
    in_namespace(name, WIFI_HW_NAME_PREFIX)
}

/// Whether `name` is the station manager's (or under it).
/// `os.lazy.net.wifi.hw/...` is not: that is [`is_wifi_hw_name`].
pub fn is_wifi_service_name(name: &str) -> bool {
    in_namespace(name, WIFI_SERVICE_NAME)
}

/// A system service running as `uid`: unlabelled and outside any session.
const fn service(uid: u32, want: u32, label_id: u32, session: u64) -> bool {
    uid == want && label_id == 0 && session == 0
}

/// Whether these kernel-stamped credentials may hold a radio's name: the
/// chip driver `_wifi` or the simulator `_wifisim`.
pub fn may_register_wifi_hw_name(uid: u32, label_id: u32, session: u64) -> bool {
    service(uid, WIFI_UID, label_id, session) || service(uid, WIFISIM_UID, label_id, session)
}

/// Whether these kernel-stamped credentials are the station manager's:
/// `_wlan`, unlabelled, outside any session. `confd` lets it keep the Wi-Fi
/// profiles; `keyd` gives it PMKs and takes secrets from it.
pub fn is_wlanmd(uid: u32, label_id: u32, session: u64) -> bool {
    service(uid, WLAN_UID, label_id, session)
}

/// Whether these credentials may hold the station manager's name: `_wlan`.
pub fn may_register_wifi_service_name(uid: u32, label_id: u32, session: u64) -> bool {
    is_wlanmd(uid, label_id, session)
}

/// Whether a radio accepts `Attach` (and so any owner-only call) from these
/// credentials: `_wlan` alone. Root is not named: no session runs as it, and
/// a radio has nothing to offer a diagnostic tool that `Info` and `Stats`
/// (open to every caller) do not.
pub fn may_attach_wifi_hw(uid: u32, label_id: u32, session: u64) -> bool {
    is_wlanmd(uid, label_id, session)
}

/// Whether `topic` is one `wlanmd` publishes: an interface's retained
/// `system/net/<if>/wifi/state` or its `system/net/<if>/wifi/scan`.
pub fn is_wifi_topic(topic: &str) -> bool {
    let Some(rest) = topic.strip_prefix("system/net/") else {
        return false;
    };
    let ifname = rest
        .strip_suffix("/wifi/state")
        .or_else(|| rest.strip_suffix("/wifi/scan"));
    ifname.is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NETD_UID, NET_UID, ROOT_UID};

    #[test]
    fn the_namespaces_are_exactly_their_subtrees() {
        assert!(is_wifi_hw_name("os.lazy.net.wifi.hw"));
        assert!(is_wifi_hw_name("os.lazy.net.wifi.hw/wlan0"));
        assert!(is_wifi_hw_name("os.lazy.net.wifi.hw/"));
        assert!(!is_wifi_hw_name("os.lazy.net.wifi.hwX/wlan0"));
        assert!(!is_wifi_hw_name("os.lazy.net.wifi.hw.v1"));
        assert!(!is_wifi_hw_name("os.lazy.net.wifi"));
        assert!(is_wifi_service_name("os.lazy.net.wifi"));
        assert!(is_wifi_service_name("os.lazy.net.wifi/x"));
        assert!(!is_wifi_service_name("os.lazy.net.wifi.hw/wlan0"));
        assert!(!is_wifi_service_name("os.lazy.net.wifi.v1"));
        assert!(!is_wifi_service_name("os.lazy.net.wifiX"));
        assert!(!is_wifi_service_name("os.lazy.net"));
        assert!(!is_wifi_service_name(""));
    }

    #[test]
    fn only_the_radio_drivers_hold_radio_names() {
        assert!(may_register_wifi_hw_name(WIFI_UID, 0, 0));
        assert!(may_register_wifi_hw_name(WIFISIM_UID, 0, 0));
        for uid in [ROOT_UID, NET_UID, NETD_UID, WLAN_UID, 906, 909, 914, 1000, u32::MAX] {
            assert!(!may_register_wifi_hw_name(uid, 0, 0), "{uid}");
        }
        assert!(!may_register_wifi_hw_name(WIFI_UID, 1, 0));
        assert!(!may_register_wifi_hw_name(WIFISIM_UID, 0, 2));
    }

    #[test]
    fn only_wlan_serves_wifi_and_attaches_to_radios() {
        assert!(may_register_wifi_service_name(WLAN_UID, 0, 0));
        assert!(may_attach_wifi_hw(WLAN_UID, 0, 0));
        for uid in [ROOT_UID, WIFI_UID, WIFISIM_UID, NETD_UID, 909, 1000] {
            assert!(!may_register_wifi_service_name(uid, 0, 0), "{uid}");
            assert!(!may_attach_wifi_hw(uid, 0, 0), "{uid}");
        }
        for (label, session) in [(1, 0), (0, 1), (u32::MAX, u64::MAX)] {
            assert!(!may_register_wifi_service_name(WLAN_UID, label, session));
            assert!(!may_attach_wifi_hw(WLAN_UID, label, session));
        }
    }

    #[test]
    fn wlanmd_publishes_the_wifi_state_and_scan_topics_only() {
        assert!(is_wifi_topic("system/net/wlan0/wifi/state"));
        assert!(is_wifi_topic("system/net/wlan1/wifi/scan"));
        for not in [
            "system/net/wlan0/addr",
            "system/net/wlan0/link",
            "system/net//wifi/state",
            "system/net/a/b/wifi/state",
            "system/net/wlan0/wifi/state/x",
            "system/net/interfaces",
            "system/net/wlan0/wifi",
            "user/net/wlan0/wifi/state",
        ] {
            assert!(!is_wifi_topic(not), "{not}");
        }
    }

    #[test]
    fn the_uids_are_the_reserved_ones() {
        assert_eq!((WIFI_UID, WLAN_UID, WIFISIM_UID), (911, 912, 913));
    }
}
