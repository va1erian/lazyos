//! Operation arguments: confd values as `(kind, text)` and the validators
//! every operation's arguments go through ([`crate::Operation::parse`]).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{Value, MAX_SECRET};

/// A confd value as `(kind, text)`: `bool` (`true`/`false`), `i64`, `u64`,
/// `str`, `bytes` (lowercase hex).
pub fn value_args(value: &Value) -> (&'static str, String) {
    match value {
        Value::Bool(flag) => ("bool", format!("{flag}")),
        Value::I64(number) => ("i64", format!("{number}")),
        Value::U64(number) => ("u64", format!("{number}")),
        Value::Str(text) => ("str", text.clone()),
        Value::Bytes(bytes) => ("bytes", hex(bytes)),
    }
}

/// The inverse of [`value_args`].
pub fn parse_value(kind: &str, text: &str) -> Result<Value, &'static str> {
    let bad = "the value does not match its kind";
    Ok(match kind {
        "bool" => match text {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => return Err(bad),
        },
        "i64" => Value::I64(text.parse().map_err(|_| bad)?),
        "u64" => Value::U64(text.parse().map_err(|_| bad)?),
        "str" => Value::Str(text.to_string()),
        "bytes" => Value::Bytes(unhex(text).ok_or(bad)?),
        _ => return Err("the kind is bool, i64, u64, str or bytes"),
    })
}

/// `unix` as `YYYY-MM-DD HH:MM UTC`.
pub(crate) fn civil(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (year, month, day) = timezone::civil::civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        secs / 3600,
        secs % 3600 / 60
    )
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2)
        || !text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// A confd path (`confd::validate_path`).
pub(crate) fn conf_path(path: &str) -> Result<String, &'static str> {
    confd::validate_path(path).map_err(|_| "not a confd path")?;
    Ok(path.to_string())
}

/// An absolute package path of sane shape, with no character that could
/// make it read other than it is (`pkgd` normalises and checks it again
/// before reading).
pub(crate) fn package_path(path: &str) -> Result<String, &'static str> {
    let ok = path.starts_with('/')
        && path.len() > 1
        && crate::text::plain(path)
        && path[1..]
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."));
    ok.then(|| path.to_string())
        .ok_or("not an absolute package path")
}

/// A package's reverse-DNS system name: dot-separated lowercase words.
pub(crate) fn system_name(name: &str) -> bool {
    name.len() <= 128
        && name.contains('.')
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        })
}

/// An account name (`accountdb`'s rule, system accounts excluded).
pub(crate) fn account(name: &str) -> Result<String, &'static str> {
    if accountdb::valid_name(name) && !name.starts_with('_') {
        Ok(name.to_string())
    } else {
        Err("not an account name")
    }
}

/// A password `keyd` takes.
pub(crate) fn secret(text: &str) -> Result<String, &'static str> {
    if text.is_empty() || text.len() > MAX_SECRET || text.chars().any(char::is_control) {
        return Err("a password is 1 to 64 characters, without control characters");
    }
    Ok(text.to_string())
}

/// The address setup `net.config` takes: `("", "", "")` is DHCP; otherwise a
/// CIDR address (prefix 1 to 30) with an optional gateway and DNS server,
/// each a dotted IPv4 address. `netd` judges the rest (a gateway off the
/// subnet makes it fall back to DHCP).
pub(crate) fn net_setup(
    address: &str,
    gateway: &str,
    dns: &str,
) -> Result<(String, String, String), &'static str> {
    use core::net::Ipv4Addr;
    let ip = |text: &str| text.parse::<Ipv4Addr>().is_ok();
    if address.is_empty() {
        if !gateway.is_empty() || !dns.is_empty() {
            return Err("DHCP takes no gateway or DNS server");
        }
        return Ok((String::new(), String::new(), String::new()));
    }
    let (host, prefix) = address.split_once('/').ok_or("the address is a.b.c.d/n")?;
    let prefix: u8 = prefix.parse().map_err(|_| "the address is a.b.c.d/n")?;
    if !ip(host) || !(1..=30).contains(&prefix) {
        return Err("the address is a.b.c.d/n, with n from 1 to 30");
    }
    for server in [gateway, dns] {
        if !server.is_empty() && !ip(server) {
            return Err("the gateway and DNS server are a.b.c.d");
        }
    }
    Ok((address.to_string(), gateway.to_string(), dns.to_string()))
}

/// `[a-z0-9_-]{1,32}`: a service name or a power policy key.
pub(crate) fn word(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}
