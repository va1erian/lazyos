//! The device configuration structure (virtio 1.2, 5.1.4), read through the
//! transport's device-config window.
//!
//! ```text
//! 0x00 mac[6]   (MAC)     0x06 status u16 (STATUS)   0x08 max_virtqueue_pairs u16 (MQ)
//! 0x0A mtu u16  (MTU)     0x0C speed u32, 0x10 duplex u8 (SPEED_DUPLEX)
//! ```
//!
//! A field exists only when its feature was negotiated; a driver must not read
//! one that does not. Every value is device supplied and untrusted.

use crate::features;

/// `status` bit: the link is up.
pub const S_LINK_UP: u16 = 1;
/// `status` bit: the driver must announce itself (gratuitous ARP); not used.
pub const S_ANNOUNCE: u16 = 2;

pub const OFF_MAC: u32 = 0x00;
pub const OFF_STATUS: u32 = 0x06;
pub const OFF_MAX_PAIRS: u32 = 0x08;
pub const OFF_MTU: u32 = 0x0A;

/// What the device configuration says, for the features that were negotiated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetConfig {
    /// The device's MAC address; `None` when `MAC` was not negotiated.
    pub mac: Option<[u8; 6]>,
    /// The raw `status` word; without `STATUS` the spec says the link is
    /// always up, and this is [`S_LINK_UP`].
    pub status: u16,
    /// `max_virtqueue_pairs`; 1 without `MQ`.
    pub max_pairs: u16,
    /// The device's MTU hint; `None` without `MTU`.
    pub mtu: Option<u16>,
}

impl NetConfig {
    /// Read the fields `features` says exist, through `read(offset, width)`
    /// (a device-config read of 1, 2 or 4 bytes). The first error from `read`
    /// is returned.
    pub fn read<E>(
        features: u64,
        mut read: impl FnMut(u32, u32) -> Result<u32, E>,
    ) -> Result<NetConfig, E> {
        let mac = if features & features::MAC != 0 {
            let mut mac = [0u8; 6];
            for (i, byte) in mac.iter_mut().enumerate() {
                *byte = read(OFF_MAC + i as u32, 1)? as u8;
            }
            Some(mac)
        } else {
            None
        };
        let status = if features & features::STATUS != 0 {
            read(OFF_STATUS, 2)? as u16
        } else {
            S_LINK_UP
        };
        let max_pairs = if features & features::MQ != 0 {
            read(OFF_MAX_PAIRS, 2)? as u16
        } else {
            1
        };
        let mtu = if features & features::MTU != 0 {
            Some(read(OFF_MTU, 2)? as u16)
        } else {
            None
        };
        Ok(NetConfig {
            mac,
            status,
            max_pairs,
            mtu,
        })
    }

    /// Parse from a byte image of the configuration structure (the shape a
    /// fuzzer and the tests supply). Fields past the end of `bytes` read as
    /// zero, exactly as an absent device window would.
    pub fn from_bytes(features: u64, bytes: &[u8]) -> NetConfig {
        let byte = |i: u32| bytes.get(i as usize).copied().unwrap_or(0);
        let result: Result<NetConfig, core::convert::Infallible> =
            NetConfig::read(features, |offset, width| {
                let mut value = 0u32;
                for i in 0..width {
                    value |= u32::from(byte(offset + i)) << (8 * i);
                }
                Ok(value)
            });
        match result {
            Ok(config) => config,
            Err(never) => match never {},
        }
    }

    pub fn link_up(&self) -> bool {
        self.status & S_LINK_UP != 0
    }

    /// A MAC the driver can use: present, unicast and not all zeros. A
    /// multicast or zero address would put every frame the stack sends on the
    /// wire from a nonsensical source, so the driver refuses to start on one
    /// and says why instead.
    pub fn usable_mac(&self) -> Option<[u8; 6]> {
        self.mac.filter(|mac| mac[0] & 1 == 0 && *mac != [0; 6])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: [u8; 12] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 1, 0, 4, 0, 0xDC, 5];

    #[test]
    fn fields_appear_only_with_their_features() {
        let none = NetConfig::from_bytes(0, &IMAGE);
        assert_eq!(
            none,
            NetConfig {
                mac: None,
                status: S_LINK_UP,
                max_pairs: 1,
                mtu: None
            }
        );
        let mac = NetConfig::from_bytes(features::MAC, &IMAGE);
        assert_eq!(mac.mac, Some([0x52, 0x54, 0, 0x12, 0x34, 0x56]));
        assert_eq!(mac.status, S_LINK_UP, "no STATUS: the link is always up");
        let all = NetConfig::from_bytes(
            features::MAC | features::STATUS | features::MQ | features::MTU,
            &IMAGE,
        );
        assert_eq!((all.status, all.max_pairs, all.mtu), (1, 4, Some(0x05DC)));
        assert!(all.link_up());
    }

    #[test]
    fn link_state_follows_the_status_word() {
        let mut image = IMAGE;
        image[6] = 0;
        let down = NetConfig::from_bytes(features::MAC | features::STATUS, &image);
        assert!(!down.link_up());
        image[6] = 3; // link up + announce
        assert!(NetConfig::from_bytes(features::MAC | features::STATUS, &image).link_up());
    }

    #[test]
    fn a_short_window_reads_as_zeros() {
        for len in 0..12 {
            let config = NetConfig::from_bytes(u64::MAX, &IMAGE[..len]);
            assert!(config.mac.is_some());
            let _ = config.link_up();
        }
        let empty = NetConfig::from_bytes(features::MAC | features::STATUS, &[]);
        assert_eq!(empty.mac, Some([0; 6]));
        assert!(!empty.link_up());
        assert_eq!(empty.usable_mac(), None);
    }

    #[test]
    fn read_reports_the_first_device_error() {
        let mut reads = 0;
        let result = NetConfig::read(features::MAC | features::STATUS, |_, _| {
            reads += 1;
            if reads == 3 {
                Err("boom")
            } else {
                Ok(0)
            }
        });
        assert_eq!(result, Err("boom"));
        assert_eq!(reads, 3, "no read after the failure");
    }

    #[test]
    fn unusable_macs_are_refused() {
        let with = |mac: Option<[u8; 6]>| NetConfig {
            mac,
            status: 1,
            max_pairs: 1,
            mtu: None,
        };
        assert_eq!(with(None).usable_mac(), None);
        assert_eq!(with(Some([0; 6])).usable_mac(), None);
        assert_eq!(
            with(Some([0x01, 0, 0x5E, 0, 0, 1])).usable_mac(),
            None,
            "multicast"
        );
        assert_eq!(with(Some([0xFF; 6])).usable_mac(), None, "broadcast");
        assert_eq!(
            with(Some([0x52, 0x54, 0, 0x12, 0x34, 0x56])).usable_mac(),
            Some([0x52, 0x54, 0, 0x12, 0x34, 0x56])
        );
    }
}
