//! Who may use the Bluetooth radio (`docs/bluetooth-plan.md`, decisions 3
//! and 10), as data.
//!
//! `btd` runs as `_bt` with only `CAP_INPUT_SOURCE`. `usbd` serves the
//! controller's packets as `os.lazy.usb.bt` and answers `_bt` (and root) only:
//! the radio carries the keyboard and mouse of the machine, so authority over
//! it is authority over input.

#![no_std]

/// The `_bt` system user `btd` runs as: `CAP_INPUT_SOURCE`, nothing else.
/// (901 `_snd` .. 915 `_gpu` are taken; the next free uid is 917.)
pub const BT_UID: u32 = 916;

/// Whether `uid` may call `os.lazy.usb.bt.v1`: `_bt` and root (the kernel
/// stamps the uid, a request body cannot name a caller).
pub const fn may_use_transport(uid: u32) -> bool {
    uid == 0 || uid == BT_UID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bt_and_root_may_use_the_transport() {
        assert!(may_use_transport(BT_UID));
        assert!(may_use_transport(0));
        for uid in [1, 904, 915, 917, 1000, u32::MAX] {
            assert!(!may_use_transport(uid), "{uid}");
        }
    }

    #[test]
    fn the_uid_is_a_system_uid() {
        const { assert!(BT_UID > 915 && BT_UID < 1000) };
    }
}
