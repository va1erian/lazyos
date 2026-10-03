//! What the network apps show and what the Network app writes, as plain data.
//!
//! [`NetStatus`] is one snapshot of `netd` (`os.lazy.net.stack.v1`) turned
//! into display lines. [`Form`] is the Network app's configuration form;
//! [`plan`] checks it with `netstack::config`, the parser `netd` reads
//! `sys/net/eth0/*` with, and turns it into `confd` writes. A static setup the
//! stack would not take (it silently falls back to DHCP) is refused here with
//! the reason, so what the user saves is what the stack applies.

use netstack::config::{is_usable_unicast, parse_cidr, parse_ipv4, same_subnet};

use crate::format;

/// The one interface `netd` drives today, and the `confd` namespace of its
/// configuration (`sys/net/<if>/{mode,address,gateway,dns}`).
pub const IFNAME: &str = "eth0";

/// How the interface gets its address (`os.lazy.net.stack.v1` `ConfigMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigMode {
    Dhcp,
    Static,
    Unknown,
}

/// The DHCP client's state (`DhcpState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DhcpState {
    Off,
    Discovering,
    Bound,
    Unknown,
}

/// The interface as `netd` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub mac: String,
    pub mtu: u32,
    pub link: bool,
    pub mode: ConfigMode,
    pub dhcp: DhcpState,
}

/// The address the interface holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub addr: [u8; 4],
    pub prefix_len: u32,
    pub from_dhcp: bool,
    /// Seconds of lease left (0 when static).
    pub lease_secs: u32,
}

/// Stack counters worth showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traffic {
    pub rx_frames: u64,
    pub tx_frames: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub pings_sent: u64,
    pub pings_answered: u64,
    pub lookups_sent: u64,
    pub lookups_answered: u64,
}

/// One snapshot of the stack.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetStatus {
    pub interface: Option<Interface>,
    pub address: Option<Address>,
    /// The default route's gateway.
    pub gateway: Option<[u8; 4]>,
    pub traffic: Traffic,
}

/// Four octets from a reply field; anything else is not an address.
pub fn ipv4(bytes: &[u8]) -> Option<[u8; 4]> {
    bytes.try_into().ok()
}

/// `a.b.c.d`.
pub fn dotted(addr: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3])
}

/// `52:54:00:12:34:56`, or `?` for a field of the wrong length.
pub fn mac(bytes: &[u8]) -> String {
    if bytes.len() != 6 {
        return String::from("?");
    }
    let parts: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    parts.join(":")
}

/// A lease length as `1h 05m` / `4m 10s`.
pub fn duration(secs: u32) -> String {
    let (hours, minutes, seconds) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else {
        format!("{minutes}m {seconds:02}s")
    }
}

impl NetStatus {
    /// The address as `10.0.2.15/24`, when there is one.
    pub fn cidr(&self) -> Option<String> {
        self.address
            .map(|a| format!("{}/{}", dotted(a.addr), a.prefix_len))
    }

    /// "eth0 · MAC 52:54:00:12:34:56 · link up · MTU 1500".
    pub fn interface_line(&self) -> String {
        match &self.interface {
            Some(i) => format!(
                "{} · MAC {} · link {} · MTU {}",
                format::clip(&i.name, 16),
                i.mac,
                if i.link { "up" } else { "down" },
                i.mtu
            ),
            None => String::from("no interface (is a network card attached?)"),
        }
    }

    /// How the address is obtained, in words.
    pub fn mode_line(&self) -> String {
        let Some(i) = &self.interface else {
            return String::from("-");
        };
        match (i.mode, i.dhcp) {
            (ConfigMode::Static, _) => String::from("Manual (static address)"),
            (ConfigMode::Dhcp, DhcpState::Bound) => String::from("Automatic (DHCP): lease held"),
            (ConfigMode::Dhcp, _) => String::from("Automatic (DHCP): looking for a server..."),
            (ConfigMode::Unknown, _) => String::from("unknown"),
        }
    }

    /// "10.0.2.15/24 (DHCP, lease 23h 59m left)" or "none yet".
    pub fn address_line(&self) -> String {
        match (self.address, self.cidr()) {
            (Some(a), Some(cidr)) if a.from_dhcp && a.lease_secs > 0 => {
                format!("{cidr} (DHCP, lease {} left)", duration(a.lease_secs))
            }
            (Some(a), Some(cidr)) if a.from_dhcp => format!("{cidr} (DHCP)"),
            (Some(_), Some(cidr)) => format!("{cidr} (static)"),
            _ => String::from("none yet"),
        }
    }

    /// The default gateway, or that there is none.
    pub fn gateway_line(&self) -> String {
        self.gateway
            .map(dotted)
            .unwrap_or_else(|| String::from("none (local network only)"))
    }

    /// "received 120 frames (30.1 KiB) · sent 98 frames (11.0 KiB)".
    pub fn traffic_line(&self) -> String {
        let t = &self.traffic;
        format!(
            "received {} frames ({}) · sent {} frames ({})",
            t.rx_frames,
            format::bytes(t.rx_bytes),
            t.tx_frames,
            format::bytes(t.tx_bytes)
        )
    }

    /// One line for a header: "eth0 10.0.2.15/24 via 10.0.2.2 · link up".
    pub fn headline(&self) -> String {
        let name = self.interface.as_ref().map_or("eth0", |i| i.name.as_str());
        let link = match &self.interface {
            Some(i) if i.link => "link up",
            Some(_) => "link down",
            None => "no card",
        };
        match (self.cidr(), self.gateway) {
            (Some(cidr), Some(gw)) => format!("{name} {cidr} via {} · {link}", dotted(gw)),
            (Some(cidr), None) => format!("{name} {cidr} · {link}"),
            (None, _) => format!("{name}: no address yet · {link}"),
        }
    }
}

/// The Network app's configuration form, as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    /// Manual (static) instead of DHCP.
    pub manual: bool,
    /// `a.b.c.d/len`.
    pub address: String,
    pub gateway: String,
    pub dns: String,
}

/// One `confd` change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Set(String, String),
    Delete(String),
}

/// The `confd` path of one configuration value.
pub fn key(name: &str) -> String {
    format!("sys/net/{IFNAME}/{name}")
}

/// The form filled from stored values (`None`: absent), falling back to what
/// the interface holds now so switching to Manual starts from a working setup.
pub fn form_from(stored: [Option<String>; 4], now: &NetStatus) -> Form {
    let [mode, address, gateway, dns] = stored;
    Form {
        manual: mode.as_deref() == Some("static"),
        address: address.or_else(|| now.cidr()).unwrap_or_default(),
        gateway: gateway
            .or_else(|| now.gateway.map(dotted))
            .unwrap_or_default(),
        dns: dns.unwrap_or_default(),
    }
}

/// The `confd` writes that store `form`, or why `netd` would not take it.
///
/// DHCP keeps the typed values (for the next switch to Manual) and only sets
/// `mode`. Manual writes the values first and `mode` last: `netd` re-reads
/// every few seconds, and must never see `static` with the old address.
pub fn plan(form: &Form) -> Result<Vec<Write>, String> {
    let set = |name: &str, value: &str| Write::Set(key(name), value.to_string());
    if !form.manual {
        return Ok(vec![set("mode", "dhcp")]);
    }
    let address = form.address.trim();
    let (addr, prefix_len) = parse_cidr(address).ok_or_else(|| {
        format!("Address {address:?} must look like 192.168.1.20/24 (prefix 1 to 30, not 0.x, 127.x or multicast).")
    })?;
    let mut writes = vec![set("address", address)];
    let gateway = form.gateway.trim();
    if gateway.is_empty() {
        writes.push(Write::Delete(key("gateway")));
    } else {
        let gw = parse_ipv4(gateway)
            .filter(|gw| is_usable_unicast(*gw))
            .ok_or_else(|| format!("Gateway {gateway:?} is not a usable IPv4 address."))?;
        if gw == addr {
            return Err(String::from(
                "The gateway cannot be this machine's own address.",
            ));
        }
        if !same_subnet(gw, addr, prefix_len as u8) {
            return Err(format!(
                "Gateway {gateway} is outside {address}: it must be on the local network."
            ));
        }
        writes.push(set("gateway", gateway));
    }
    let dns = form.dns.trim();
    if dns.is_empty() {
        writes.push(Write::Delete(key("dns")));
    } else {
        parse_ipv4(dns)
            .filter(|d| is_usable_unicast(*d))
            .ok_or_else(|| format!("DNS server {dns:?} is not a usable IPv4 address."))?;
        writes.push(set("dns", dns));
    }
    writes.push(set("mode", "static"));
    Ok(writes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use netstack::config::{Mode, Raw};

    fn manual(address: &str, gateway: &str, dns: &str) -> Form {
        Form {
            manual: true,
            address: address.into(),
            gateway: gateway.into(),
            dns: dns.into(),
        }
    }

    /// What `netd` would read after `writes`.
    fn applied(writes: &[Write]) -> Mode {
        let get = |name: &str| {
            writes.iter().rev().find_map(|w| match w {
                Write::Set(k, v) if *k == key(name) => Some(Some(v.as_str())),
                Write::Delete(k) if *k == key(name) => Some(None),
                _ => None,
            })?
        };
        Mode::from_raw(&Raw {
            mode: get("mode"),
            address: get("address"),
            gateway: get("gateway"),
            dns: get("dns"),
        })
    }

    #[test]
    fn a_saved_static_setup_is_exactly_what_netd_applies() {
        let writes = plan(&manual("10.0.2.20/24", "10.0.2.2", "10.0.2.3")).unwrap();
        assert_eq!(
            writes.last(),
            Some(&Write::Set(key("mode"), "static".into()))
        );
        match applied(&writes) {
            Mode::Static(cfg) => {
                assert_eq!(cfg.addr, [10, 0, 2, 20]);
                assert_eq!(cfg.prefix_len, 24);
                assert_eq!(cfg.gateway, Some([10, 0, 2, 2]));
                assert_eq!(cfg.dns, Some([10, 0, 2, 3]));
            }
            other => panic!("netd would apply {other:?}"),
        }
        // Optional values left empty are removed, not left stale.
        let writes = plan(&manual(" 192.168.7.9/16 ", "", "")).unwrap();
        assert!(writes.contains(&Write::Delete(key("gateway"))));
        assert!(writes.contains(&Write::Delete(key("dns"))));
        assert!(matches!(applied(&writes), Mode::Static(c) if c.gateway.is_none()));
    }

    #[test]
    fn dhcp_only_sets_the_mode() {
        let form = Form {
            manual: false,
            address: "garbage".into(),
            ..Form::default()
        };
        assert_eq!(
            plan(&form).unwrap(),
            vec![Write::Set(key("mode"), "dhcp".into())]
        );
    }

    #[test]
    fn setups_netd_would_replace_with_dhcp_are_refused() {
        for (address, gateway, dns) in [
            ("10.0.2.20", "", ""),
            ("10.0.2.20/31", "", ""),
            ("127.0.0.1/8", "", ""),
            ("10.0.2.20/24", "10.0.3.1", ""),
            ("10.0.2.20/24", "10.0.2.20", ""),
            ("10.0.2.20/24", "224.0.0.1", ""),
            ("10.0.2.20/24", "", "dns.example"),
            ("10.0.2.20/24", "", "0.0.0.0"),
        ] {
            assert!(
                plan(&manual(address, gateway, dns)).is_err(),
                "{address} {gateway} {dns}"
            );
        }
    }

    #[test]
    fn the_form_starts_from_stored_values_then_the_live_address() {
        let now = NetStatus {
            address: Some(Address {
                addr: [10, 0, 2, 15],
                prefix_len: 24,
                from_dhcp: true,
                lease_secs: 100,
            }),
            gateway: Some([10, 0, 2, 2]),
            ..NetStatus::default()
        };
        let form = form_from([None, None, None, None], &now);
        assert!(!form.manual);
        assert_eq!(
            (form.address.as_str(), form.gateway.as_str()),
            ("10.0.2.15/24", "10.0.2.2")
        );
        let stored = [
            Some("static".into()),
            Some("1.2.3.4/8".into()),
            None,
            Some("9.9.9.9".into()),
        ];
        let form = form_from(stored, &now);
        assert!(form.manual);
        assert_eq!(
            (form.address.as_str(), form.dns.as_str()),
            ("1.2.3.4/8", "9.9.9.9")
        );
    }

    #[test]
    fn lines_read_well_and_survive_odd_replies() {
        assert_eq!(mac(&[0x52, 0x54, 0, 0x12, 0x34, 0x56]), "52:54:00:12:34:56");
        assert_eq!(mac(&[1, 2]), "?");
        assert_eq!(ipv4(&[1, 2, 3]), None);
        assert_eq!(duration(3900), "1h 05m");
        assert_eq!(duration(250), "4m 10s");
        let empty = NetStatus::default();
        assert_eq!(empty.address_line(), "none yet");
        assert!(empty.headline().contains("no address"));
        let mut status = NetStatus {
            interface: Some(Interface {
                name: "eth0".into(),
                mac: "52:54:00:12:34:56".into(),
                mtu: 1500,
                link: true,
                mode: ConfigMode::Dhcp,
                dhcp: DhcpState::Bound,
            }),
            address: Some(Address {
                addr: [10, 0, 2, 15],
                prefix_len: 24,
                from_dhcp: true,
                lease_secs: 86_000,
            }),
            gateway: Some([10, 0, 2, 2]),
            traffic: Traffic::default(),
        };
        assert_eq!(
            status.headline(),
            "eth0 10.0.2.15/24 via 10.0.2.2 · link up"
        );
        assert_eq!(
            status.address_line(),
            "10.0.2.15/24 (DHCP, lease 23h 53m left)"
        );
        assert_eq!(status.mode_line(), "Automatic (DHCP): lease held");
        status.gateway = None;
        assert_eq!(status.gateway_line(), "none (local network only)");
    }
}
