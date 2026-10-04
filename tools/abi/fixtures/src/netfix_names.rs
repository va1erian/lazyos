//! `netfix`'s name checks (docs/tls-plan.md stage T1): names resolved by
//! musl's own `getaddrinfo` through the `/etc` files the Linux personality
//! serves, and the trust anchors a TLS client would load.
//!
//! * `resolv_conf`: `/etc/resolv.conf` exists (`netd` wrote it from its lease)
//!   and holds nothing but `nameserver` lines and comments;
//! * `hosts`: `lazyos` and `localhost`, which only `/etc/hosts` knows, resolve
//!   to the loopback address without DNS;
//! * `trust`: `/etc/ssl/certs/ca-certificates.crt` is a bundle of over a
//!   hundred PEM certificates and nothing else;
//! * `dns`: a real name (`github.com`) resolves through the resolver in
//!   `resolv.conf`, IPv4 first, and a name under `.invalid` fails promptly.
//!   With no answer at all (a host without a network) this prints
//!   `NETFIX:dns:OFFLINE` instead of failing; the capture shows the queries.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::time::Instant;

const RESOLV_CONF: &str = "/etc/resolv.conf";
const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
const REAL_NAME: &str = "github.com";
const MISSING_NAME: &str = "lazyos-netfix.invalid";

fn lookup(name: &str) -> std::io::Result<Vec<SocketAddr>> {
    (name, 443).to_socket_addrs().map(Iterator::collect)
}

pub fn resolv_conf() -> Result<String, String> {
    let text = std::fs::read_to_string(RESOLV_CONF).map_err(|e| format!("{RESOLV_CONF}: {e}"))?;
    let mut servers = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let server = line
            .strip_prefix("nameserver ")
            .ok_or(format!("unexpected line {line:?}"))?;
        server
            .parse::<Ipv4Addr>()
            .map_err(|_| format!("nameserver {server:?} is not an IPv4 address"))?;
        servers.push(server);
    }
    if servers.is_empty() {
        return Err(String::from("no nameserver"));
    }
    Ok(format!("nameservers={}", servers.join(",")))
}

pub fn hosts() -> Result<String, String> {
    for name in ["lazyos", "localhost"] {
        let addrs = lookup(name).map_err(|e| format!("{name}: {e}"))?;
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        if addrs.first().map(SocketAddr::ip) != Some(loopback) {
            return Err(format!("{name} resolved to {addrs:?}"));
        }
    }
    Ok(String::from("lazyos and localhost from /etc/hosts"))
}

pub fn trust() -> Result<String, String> {
    let text = std::fs::read_to_string(CA_BUNDLE).map_err(|e| format!("{CA_BUNDLE}: {e}"))?;
    let begins = text.matches("-----BEGIN CERTIFICATE-----").count();
    let ends = text.matches("-----END CERTIFICATE-----").count();
    if begins < 100 || begins != ends {
        return Err(format!("{begins} certificates begin, {ends} end"));
    }
    if text.lines().any(|l| l.starts_with("-----") && !l.ends_with(" CERTIFICATE-----")) {
        return Err(String::from("a block that is not a certificate"));
    }
    Ok(format!("certificates={begins} bytes={}", text.len()))
}

/// `Ok(Some(detail))` resolved, `Ok(None)` no answer (offline), `Err` wrong.
pub fn dns() -> Result<Option<String>, String> {
    let started = Instant::now();
    let real = lookup(REAL_NAME);
    let real_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let missing = lookup(MISSING_NAME);
    let missing_ms = started.elapsed().as_millis();
    if let Ok(addrs) = missing {
        return Err(format!("{MISSING_NAME} resolved to {addrs:?}"));
    }
    if missing_ms > 30_000 {
        return Err(format!("{MISSING_NAME} took {missing_ms} ms to fail"));
    }
    let addrs = match real {
        Ok(addrs) => addrs,
        Err(e) => {
            println!("NETFIX:dns:OFFLINE {REAL_NAME}: {e} after {real_ms} ms");
            return Ok(None);
        }
    };
    match addrs.first() {
        Some(first) if first.is_ipv4() => Ok(Some(format!(
            "{REAL_NAME} -> {} ({} addresses, {real_ms} ms); {MISSING_NAME} refused in {missing_ms} ms",
            first.ip(),
            addrs.len()
        ))),
        _ => Err(format!("{REAL_NAME} resolved to {addrs:?} (IPv4 must come first)")),
    }
}
