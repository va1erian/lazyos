//! The printer address a user types, checked and completed into the URI a
//! request names and the host and path HTTP needs.

use alloc::format;
use alloc::string::String;

/// IPP's registered port.
pub const IPP_PORT: u16 = 631;
/// The path IPP Everywhere printers serve (`ipp/print`).
pub const DEFAULT_PATH: &str = "/ipp/print";
/// Longest host or path accepted.
const MAX_PART: usize = 255;

/// A printer's address: `ipp://host:port/path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterUri {
    /// A DNS name, an IPv4 address, or an IPv6 address in brackets.
    pub host: String,
    pub port: u16,
    /// Starts with `/`.
    pub path: String,
}

impl PrinterUri {
    /// Reads what a user typed: `192.168.1.89`, `printer.lan:8631`,
    /// `ipp://host/ipp/print` or `http://host:631/ipp/print`. The port
    /// defaults to 631 (80 for `http://`) and the path to `/ipp/print`.
    /// `ipps://` is refused until LazyOS has TLS for it, as is anything that
    /// could smuggle a second header line or another host into a request.
    pub fn parse(input: &str) -> Result<PrinterUri, &'static str> {
        let input = input.trim();
        let (rest, default_port) = if let Some(rest) = strip(input, "ipp://") {
            (rest, IPP_PORT)
        } else if let Some(rest) = strip(input, "http://") {
            (rest, 80)
        } else if strip(input, "ipps://").is_some() || strip(input, "https://").is_some() {
            return Err("secure printing (ipps) is not supported yet: use ipp://");
        } else if input.contains("://") {
            return Err("the address must start with ipp:// or be a host name");
        } else {
            (input, IPP_PORT)
        };
        let (authority, path) = match rest.find('/') {
            Some(slash) => (&rest[..slash], &rest[slash..]),
            None => (rest, DEFAULT_PATH),
        };
        let path = if path == "/" { DEFAULT_PATH } else { path };
        if path.len() > MAX_PART || !path.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("the path may hold only printable characters");
        }
        let (host, port) = split_port(authority, default_port)?;
        if host.is_empty() || host.len() > MAX_PART {
            return Err("enter the printer's address, e.g. 192.168.1.89");
        }
        Ok(PrinterUri {
            host: String::from(host),
            port,
            path: String::from(path),
        })
    }

    /// The URI requests name in `printer-uri`.
    pub fn to_ipp(&self) -> String {
        format!("ipp://{}:{}{}", self.host, self.port, self.path)
    }

    /// The HTTP `Host` header value.
    pub fn host_header(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// The host without the brackets of an IPv6 address, for connecting.
    pub fn connect_host(&self) -> &str {
        self.host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&self.host)
    }
}

fn strip<'a>(input: &'a str, scheme: &str) -> Option<&'a str> {
    let head = input.get(..scheme.len())?;
    head.eq_ignore_ascii_case(scheme)
        .then(|| &input[scheme.len()..])
}

/// `host[:port]` or `[v6][:port]`, with the host's characters checked.
fn split_port(authority: &str, default_port: u16) -> Result<(&str, u16), &'static str> {
    const BAD_HOST: &str = "the address may hold only letters, digits, '.', '-' and ':'";
    let (host, port) = if authority.starts_with('[') {
        let close = authority.find(']').ok_or(BAD_HOST)?;
        let (host, rest) = authority.split_at(close + 1);
        let inner = &host[1..host.len() - 1];
        if inner.is_empty()
            || !inner
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        {
            return Err(BAD_HOST);
        }
        match rest {
            "" => (host, None),
            _ => (host, Some(rest.strip_prefix(':').ok_or(BAD_HOST)?)),
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if !host.starts_with('[')
        && !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(BAD_HOST);
    }
    let port = match port {
        None => default_port,
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or("the port must be a number from 1 to 65535")?,
    };
    Ok((host, port))
}
