//! The Bluetooth service names belong to their services, whatever the uid
//! rules or capabilities say (the kernel's `OP_REGISTER` gate asks here, as
//! it asks `netpolicy` for the NIC and Wi-Fi names).
//!
//! `os.lazy.usb.bt` is `usbd`'s (`_usb`): `btd` takes the radio's packets
//! from whoever holds it, and those packets become keystrokes in every
//! session. `os.lazy.bt` is `btd`'s (`_bt`): Settings and `btctl` show the
//! passkey it sends and confirm the pairings it asks about. Both are free while
//! their owner is away (no radio at boot, a restart), which is exactly when a
//! squatter would take them.

/// The name `usbd` registers while a Bluetooth controller is bound.
pub const TRANSPORT_NAME: &str = "os.lazy.usb.bt";
/// The name `btd` registers.
pub const API_NAME: &str = "os.lazy.bt";

/// `_usb`, the uid `usbd` runs as (`usbpolicy::USB_UID`, checked in the tests).
pub const USB_UID: u32 = 904;

/// `name` is `prefix` itself or a name under it (`prefix/...`).
fn in_namespace(name: &str, prefix: &str) -> bool {
    match name.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// A system service running as `uid`: unlabelled and outside any session.
const fn service(uid: u32, want: u32, label_id: u32, session: u64) -> bool {
    uid == want && label_id == 0 && session == 0
}

/// `None` when `name` is not a Bluetooth name; otherwise whether these
/// kernel-stamped credentials may register it. Root is not an owner: `init`
/// starts both services under their own uids.
pub fn register_verdict(name: &str, uid: u32, label_id: u32, session: u64) -> Option<bool> {
    if in_namespace(name, TRANSPORT_NAME) {
        return Some(service(uid, USB_UID, label_id, session));
    }
    if in_namespace(name, API_NAME) {
        return Some(service(uid, crate::BT_UID, label_id, session));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BT_UID;

    #[test]
    fn each_owner_gets_its_own_name_only() {
        assert_eq!(register_verdict(TRANSPORT_NAME, USB_UID, 0, 0), Some(true));
        assert_eq!(register_verdict(API_NAME, BT_UID, 0, 0), Some(true));
        assert_eq!(register_verdict(TRANSPORT_NAME, BT_UID, 0, 0), Some(false));
        assert_eq!(register_verdict(API_NAME, USB_UID, 0, 0), Some(false));
    }

    #[test]
    fn nobody_else_and_no_labelled_or_session_owner() {
        for name in [TRANSPORT_NAME, API_NAME, "os.lazy.bt/x", "os.lazy.usb.bt/0"] {
            for uid in [0, 1000, 902, 911, 59_999] {
                assert_eq!(
                    register_verdict(name, uid, 0, 0),
                    Some(false),
                    "{uid} {name}"
                );
            }
            assert_eq!(register_verdict(name, USB_UID, 3, 0), Some(false));
            assert_eq!(register_verdict(name, BT_UID, 0, 7), Some(false));
        }
    }

    #[test]
    fn neighbours_are_not_bluetooth_names() {
        for name in [
            "os.lazy.btx",
            "os.lazy.bt.v1",
            "os.lazy.usb.btx",
            "os.lazy.usb.audio",
            "os.lazy",
        ] {
            assert_eq!(register_verdict(name, 1000, 0, 0), None, "{name}");
        }
    }

    #[test]
    fn the_usb_uid_is_usbds() {
        assert_eq!(USB_UID, usbpolicy::USB_UID);
    }
}
