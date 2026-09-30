//! How an interface is configured, from `confd` (`sys/net/<if>/*`,
//! `docs/networking-plan.md` section 6).
//!
//! DHCP is the default. A static setup is taken only when *every* value it
//! needs parses and makes sense; anything absent, mistyped or hostile falls
//! back to DHCP instead of half-applying (the clamp-and-default rule of
//! `docs/driver-config-plan.md` section 4). `confd` is a soft dependency: with
//! no values at all the answer is DHCP.

/// A validated static configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaticConfig {
    pub addr: [u8; 4],
    pub prefix_len: u8,
    pub gateway: Option<[u8; 4]>,
    pub dns: Option<[u8; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dhcp,
    Static(StaticConfig),
}

/// Raw values as read from `confd`; `None` when the key is absent or not a string.
#[derive(Clone, Copy, Debug, Default)]
pub struct Raw<'a> {
    /// `dhcp` or `static`.
    pub mode: Option<&'a str>,
    /// `a.b.c.d/len`.
    pub address: Option<&'a str>,
    pub gateway: Option<&'a str>,
    pub dns: Option<&'a str>,
}

/// Parse dotted-quad IPv4 (exactly four decimal octets, no signs, no leading
/// zeros beyond a single `0`, no whitespace).
pub fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = text.split('.');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || (part.len() > 1 && part.starts_with('0')) {
            return None;
        }
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse::<u16>().ok().filter(|n| *n <= 255)? as u8;
    }
    parts.next().is_none().then_some(octets)
}

/// Whether `addr` can be a host's own unicast address.
pub fn is_usable_unicast(addr: [u8; 4]) -> bool {
    !(addr[0] == 0 || addr[0] == 127 || addr[0] >= 224)
}

/// Parse `a.b.c.d/len` with a usable unicast address and `len` in 1..=30.
pub fn parse_cidr(text: &str) -> Option<([u8; 4], u8)> {
    let (addr, len) = text.split_once('/')?;
    let addr = parse_ipv4(addr).filter(|a| is_usable_unicast(*a))?;
    if len.is_empty() || len.len() > 2 || !len.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let len: u8 = len.parse().ok().filter(|n| (1..=30).contains(n))?;
    Some((addr, len))
}

/// Whether `a` and `b` are in the same `/prefix_len` network.
pub fn same_subnet(a: [u8; 4], b: [u8; 4], prefix_len: u8) -> bool {
    let mask = u32::MAX
        .checked_shl(32 - u32::from(prefix_len))
        .unwrap_or(0);
    u32::from_be_bytes(a) & mask == u32::from_be_bytes(b) & mask
}

impl Mode {
    pub fn from_raw(raw: &Raw) -> Mode {
        if raw.mode != Some("static") {
            return Mode::Dhcp;
        }
        let Some((addr, prefix_len)) = raw.address.and_then(parse_cidr) else {
            return Mode::Dhcp;
        };
        // A gateway must be a usable unicast address inside the subnet and not
        // ours, or it is ignored (the interface is then on-link only).
        let gateway = raw.gateway.and_then(parse_ipv4).filter(|gw| {
            is_usable_unicast(*gw) && *gw != addr && same_subnet(*gw, addr, prefix_len)
        });
        let dns = raw
            .dns
            .and_then(parse_ipv4)
            .filter(|d| is_usable_unicast(*d));
        Mode::Static(StaticConfig {
            addr,
            prefix_len,
            gateway,
            dns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_quads() {
        assert_eq!(parse_ipv4("10.0.2.15"), Some([10, 0, 2, 15]));
        assert_eq!(parse_ipv4("0.0.0.0"), Some([0; 4]));
        assert_eq!(parse_ipv4("255.255.255.255"), Some([255; 4]));
        for bad in [
            "",
            "1.2.3",
            "1.2.3.4.5",
            "256.1.1.1",
            "1.2.3.04",
            "01.2.3.4",
            "1..3.4",
            "1.2.3.",
            ".1.2.3",
            " 1.2.3.4",
            "1.2.3.4 ",
            "+1.2.3.4",
            "-1.2.3.4",
            "1.2.3.4/24",
            "a.b.c.d",
            "1.2.3.٤",
            "1.2.3.1000",
            "0x1.2.3.4",
        ] {
            assert_eq!(parse_ipv4(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn cidrs() {
        assert_eq!(parse_cidr("10.0.2.15/24"), Some(([10, 0, 2, 15], 24)));
        assert_eq!(parse_cidr("192.168.1.2/30"), Some(([192, 168, 1, 2], 30)));
        assert_eq!(parse_cidr("10.0.0.1/1"), Some(([10, 0, 0, 1], 1)));
        for bad in [
            "10.0.2.15",
            "10.0.2.15/",
            "10.0.2.15/0",
            "10.0.2.15/31",
            "10.0.2.15/32",
            "10.0.2.15/33",
            "10.0.2.15/024",
            "10.0.2.15/-1",
            "10.0.2.15/ 4",
            "0.0.0.0/8",
            "127.0.0.1/8",
            "224.0.0.1/24",
            "255.255.255.255/24",
            "10.0.2.15/24/1",
        ] {
            assert_eq!(parse_cidr(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn subnets() {
        assert!(same_subnet([10, 0, 2, 2], [10, 0, 2, 15], 24));
        assert!(!same_subnet([10, 0, 3, 2], [10, 0, 2, 15], 24));
        assert!(same_subnet([10, 0, 3, 2], [10, 0, 2, 15], 16));
        assert!(!same_subnet([1, 2, 3, 4], [200, 2, 3, 4], 1));
        assert!(same_subnet([10, 0, 0, 1], [10, 0, 0, 1], 30));
    }

    #[test]
    fn dhcp_is_the_default_and_the_fallback() {
        assert_eq!(Mode::from_raw(&Raw::default()), Mode::Dhcp);
        let raw = |mode, address| Raw {
            mode,
            address,
            gateway: None,
            dns: None,
        };
        assert_eq!(
            Mode::from_raw(&raw(Some("dhcp"), Some("10.0.2.15/24"))),
            Mode::Dhcp
        );
        assert_eq!(
            Mode::from_raw(&raw(Some("STATIC"), Some("10.0.2.15/24"))),
            Mode::Dhcp
        );
        assert_eq!(
            Mode::from_raw(&raw(Some("static"), None)),
            Mode::Dhcp,
            "static needs an address"
        );
        assert_eq!(
            Mode::from_raw(&raw(Some("static"), Some("garbage"))),
            Mode::Dhcp
        );
        assert_eq!(
            Mode::from_raw(&raw(Some("static"), Some("127.0.0.1/8"))),
            Mode::Dhcp
        );
    }

    #[test]
    fn a_static_setup_keeps_only_sensible_parts() {
        let full = Raw {
            mode: Some("static"),
            address: Some("10.0.2.15/24"),
            gateway: Some("10.0.2.2"),
            dns: Some("10.0.2.3"),
        };
        assert_eq!(
            Mode::from_raw(&full),
            Mode::Static(StaticConfig {
                addr: [10, 0, 2, 15],
                prefix_len: 24,
                gateway: Some([10, 0, 2, 2]),
                dns: Some([10, 0, 2, 3])
            })
        );
        for gateway in [
            "10.0.3.2",
            "10.0.2.15",
            "0.0.0.0",
            "224.0.0.1",
            "nope",
            "10.0.2.2 ",
        ] {
            let mode = Mode::from_raw(&Raw {
                gateway: Some(gateway),
                ..full
            });
            let Mode::Static(config) = mode else {
                panic!("{gateway}")
            };
            assert_eq!(config.gateway, None, "{gateway:?} is not a usable gateway");
        }
        let Mode::Static(config) = Mode::from_raw(&Raw {
            dns: Some("0.0.0.0"),
            ..full
        }) else {
            panic!()
        };
        assert_eq!(config.dns, None);
    }
}
