//! The driver's tunables, clamped to the rules of
//! `docs/driver-config-plan.md` section 2 (`net/virtio-net/*`).
//!
//! This is the pure half: the driver reads raw values from `confd` (soft
//! dependency, defaults when it is absent) and hands them here. A key that is
//! absent, of the wrong type, out of range or hostile falls back to its default
//! or is clamped; nothing is ever allocated from an unclamped value and nothing
//! panics.
//!
//! **Departures from the config plan, on purpose:**
//!
//! * ring entries clamp to 16..=[`MAX_ENTRIES`] (256), not 16..=4096: the
//!   virtio queue implementation caps a queue at 256 entries
//!   (`virtio::queue::MAX_QUEUE`), which is also the plan's default, so the
//!   default is reachable and larger requests are honoured as far as they can be.
//! * `mtu` clamps to 576..=[`crate::MAX_MTU`] (1500), not 9000: a frame slot is
//!   2048 bytes, so jumbo frames need bigger slots first.

use crate::{MAX_MTU, MIN_MTU};

/// Largest receive or transmit queue the driver asks for.
pub const MAX_ENTRIES: u16 = 256;
/// Smallest.
pub const MIN_ENTRIES: u16 = 16;
pub const DEFAULT_ENTRIES: u16 = 256;
pub const DEFAULT_POLL_MS: u32 = 10;

/// How the driver learns of device events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqMode {
    /// Follow the device: interrupts where the line routes, polling where not.
    Auto,
    /// Never arm the interrupt line.
    Poll,
}

/// Raw values as read from `confd`; `None` when the key is absent or not of
/// the expected type.
#[derive(Clone, Copy, Debug, Default)]
pub struct Raw<'a> {
    pub irq_mode: Option<&'a str>,
    pub poll_interval_ms: Option<u64>,
    pub rx_ring_entries: Option<u64>,
    pub tx_ring_entries: Option<u64>,
    pub mtu: Option<u64>,
    pub mac_override: Option<&'a str>,
}

/// The effective, clamped settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub irq_mode: IrqMode,
    pub poll_interval_ms: u32,
    pub rx_entries: u16,
    pub tx_entries: u16,
    pub mtu: u16,
    pub mac_override: Option<[u8; 6]>,
}

impl Settings {
    /// What applies when `confd` is unreachable or empty.
    pub const DEFAULT: Settings = Settings {
        irq_mode: IrqMode::Auto,
        poll_interval_ms: DEFAULT_POLL_MS,
        rx_entries: DEFAULT_ENTRIES,
        tx_entries: DEFAULT_ENTRIES,
        mtu: MAX_MTU,
        mac_override: None,
    };

    pub fn from_raw(raw: &Raw) -> Settings {
        Settings {
            irq_mode: match raw.irq_mode {
                Some("poll") => IrqMode::Poll,
                _ => IrqMode::Auto,
            },
            poll_interval_ms: raw
                .poll_interval_ms
                .map_or(DEFAULT_POLL_MS, |v| v.clamp(1, 1000) as u32),
            rx_entries: raw.rx_ring_entries.map_or(DEFAULT_ENTRIES, clamp_entries),
            tx_entries: raw.tx_ring_entries.map_or(DEFAULT_ENTRIES, clamp_entries),
            mtu: raw.mtu.map_or(MAX_MTU, |v| {
                v.clamp(u64::from(MIN_MTU), u64::from(MAX_MTU)) as u16
            }),
            mac_override: raw.mac_override.and_then(parse_mac_override),
        }
    }

    /// Whether moving from `self` to `new` needs a driver restart. Only the
    /// keys marked *restart* count, and only when their *effective* value
    /// differs, so a garbage value that clamps to the current setting can
    /// never start a crash loop (`docs/driver-config-plan.md` section 4).
    pub fn needs_restart(&self, new: &Settings) -> bool {
        self.irq_mode != new.irq_mode
            || self.rx_entries != new.rx_entries
            || self.tx_entries != new.tx_entries
            || self.mac_override != new.mac_override
    }
}

/// Round down to a power of two in `MIN_ENTRIES..=MAX_ENTRIES`.
fn clamp_entries(value: u64) -> u16 {
    let value = value.clamp(u64::from(MIN_ENTRIES), u64::from(MAX_ENTRIES));
    // `value` is in 16..=256, so the shift stays small.
    (1u64 << value.ilog2()) as u16
}

/// Parse `xx:xx:xx:xx:xx:xx`. The address must be unicast with the
/// locally-administered bit set, otherwise the override is ignored (a
/// universally administered or multicast address must not be forged).
pub fn parse_mac_override(text: &str) -> Option<[u8; 6]> {
    let bytes = text.as_bytes();
    if bytes.len() != 17 {
        return None;
    }
    let hex = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut mac = [0u8; 6];
    for (i, octet) in mac.iter_mut().enumerate() {
        let at = i * 3;
        if i > 0 && bytes[at - 1] != b':' {
            return None;
        }
        *octet = hex(bytes[at])? << 4 | hex(bytes[at + 1])?;
    }
    (mac[0] & 1 == 0 && mac[0] & 2 != 0).then_some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_nothing_is_set() {
        assert_eq!(Settings::from_raw(&Raw::default()), Settings::DEFAULT);
        assert_eq!(Settings::DEFAULT.rx_entries, 256);
        assert_eq!(Settings::DEFAULT.mtu, 1500);
    }

    #[test]
    fn entries_round_down_to_a_power_of_two_in_range() {
        let entries = |v| {
            Settings::from_raw(&Raw {
                rx_ring_entries: Some(v),
                ..Raw::default()
            })
            .rx_entries
        };
        assert_eq!(entries(0), 16);
        assert_eq!(entries(1), 16);
        assert_eq!(entries(16), 16);
        assert_eq!(entries(17), 16);
        assert_eq!(entries(100), 64);
        assert_eq!(entries(255), 128);
        assert_eq!(entries(256), 256);
        assert_eq!(entries(300), 256);
        assert_eq!(
            entries(4096),
            256,
            "the config plan's upper bound is clamped to the queue cap"
        );
        assert_eq!(entries(1 << 60), 256);
        assert_eq!(entries(u64::MAX), 256);
    }

    #[test]
    fn mtu_and_poll_interval_clamp() {
        let s = |mtu, poll| {
            Settings::from_raw(&Raw {
                mtu: Some(mtu),
                poll_interval_ms: Some(poll),
                ..Raw::default()
            })
        };
        assert_eq!((s(0, 0).mtu, s(0, 0).poll_interval_ms), (576, 1));
        assert_eq!((s(575, 1).mtu, s(575, 1).poll_interval_ms), (576, 1));
        assert_eq!(
            (s(1400, 500).mtu, s(1400, 500).poll_interval_ms),
            (1400, 500)
        );
        assert_eq!(
            (s(9000, 1000).mtu, s(9000, 1001).poll_interval_ms),
            (1500, 1000)
        );
        assert_eq!(s(u64::MAX, u64::MAX).mtu, 1500);
        assert_eq!(s(u64::MAX, u64::MAX).poll_interval_ms, 1000);
    }

    #[test]
    fn irq_mode_accepts_two_words_only() {
        let mode = |text| {
            Settings::from_raw(&Raw {
                irq_mode: Some(text),
                ..Raw::default()
            })
            .irq_mode
        };
        assert_eq!(mode("poll"), IrqMode::Poll);
        assert_eq!(mode("auto"), IrqMode::Auto);
        for junk in ["", "POLL", "Poll ", "msi", "poll\0", "1"] {
            assert_eq!(mode(junk), IrqMode::Auto, "{junk:?}");
        }
    }

    #[test]
    fn mac_override_rules() {
        assert_eq!(
            parse_mac_override("02:00:00:00:00:01"),
            Some([2, 0, 0, 0, 0, 1])
        );
        assert_eq!(
            parse_mac_override("FE:ab:CD:00:00:01"),
            Some([0xFE, 0xAB, 0xCD, 0, 0, 1])
        );
        for bad in [
            "",
            "02:00:00:00:00",
            "02:00:00:00:00:01:",
            "02-00-00-00-00-01",
            "02:00:00:00:00:0g",
            "00:00:00:00:00:01", // universally administered
            "03:00:00:00:00:01", // multicast (and locally administered)
            "01:00:5e:00:00:01", // multicast
            "ff:ff:ff:ff:ff:ff", // broadcast
            "02:00:00:00:00:001",
            " 2:00:00:00:00:01",
            "0é:00:00:00:00:01",
        ] {
            assert_eq!(parse_mac_override(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn restart_only_when_the_effective_value_changes() {
        let base = Settings::DEFAULT;
        // Live keys never restart.
        let live = Settings {
            mtu: 1400,
            poll_interval_ms: 50,
            ..base
        };
        assert!(!base.needs_restart(&live));
        // A restart key that clamps back to the current value does not restart.
        let clamped = Settings::from_raw(&Raw {
            rx_ring_entries: Some(1 << 40),
            ..Raw::default()
        });
        assert!(!base.needs_restart(&clamped));
        let garbage = Settings::from_raw(&Raw {
            mac_override: Some("garbage"),
            irq_mode: Some("???"),
            ..Raw::default()
        });
        assert!(!base.needs_restart(&garbage));
        // A real change does.
        let poll = Settings {
            irq_mode: IrqMode::Poll,
            ..base
        };
        assert!(base.needs_restart(&poll));
        assert!(base.needs_restart(&Settings {
            rx_entries: 64,
            ..base
        }));
        assert!(base.needs_restart(&Settings {
            mac_override: Some([2, 0, 0, 0, 0, 9]),
            ..base
        }));
    }
}
