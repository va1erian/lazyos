//! The `diag.dbg.*` lines of `/boot/lazyos.cfg`.
//!
//! ```text
//! diag.dbg=1                  # off unless this line says so
//! diag.dbg.port=9701          # TCP port (default 9701)
//! diag.dbg.key=<32..128 hex>  # pre-shared key; without one dbgd refuses to run
//! diag.dbg.peer=192.168.1.20  # only this IPv4 address may connect (optional)
//! diag.dbg.control=1          # allow the control tier: restart, hot reload (v2)
//! ```
//!
//! The file sits on the unencrypted boot volume: anyone who can read the
//! stick has the key, which is the threat model of a debug interface that
//! the image build puts there on purpose (`LAZYOS_DBGD`).

use alloc::vec::Vec;

use crate::auth;

/// The system uid `init` runs `dbgd` as: no capabilities, nothing but what
/// any task may read.
pub const DBGD_UID: u32 = 911;

/// The default TCP port.
pub const DEFAULT_PORT: u16 = 9701;

/// A parsed configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub port: u16,
    pub key: Vec<u8>,
    /// Only this IPv4 peer may connect; `None` allows any.
    pub peer: Option<[u8; 4]>,
    /// Whether the control methods (restart, hot reload) may be used at all
    /// (`diag.dbg.control=1`); off, they answer `DENIED`.
    pub control: bool,
}

/// Why `dbgd` will not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No `diag.dbg=1`: the normal case, not an error.
    Disabled,
    NoKey,
    BadKey,
    BadPort,
    BadPeer,
}

impl Refusal {
    pub fn text(&self) -> &'static str {
        match self {
            Refusal::Disabled => "diag.dbg is not 1",
            Refusal::NoKey => "diag.dbg.key is missing",
            Refusal::BadKey => "diag.dbg.key is not 16 to 64 bytes of hex",
            Refusal::BadPort => "diag.dbg.port is not 1..65535",
            Refusal::BadPeer => "diag.dbg.peer is not an IPv4 address",
        }
    }
}

/// `a.b.c.d` as four bytes.
pub fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = text.trim().split('.');
    for byte in out.iter_mut() {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *byte = part.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Read the `diag.dbg*` lines of `cfg`. Other lines are ignored; a later
/// duplicate of a key wins.
pub fn parse(cfg: &str) -> Result<Config, Refusal> {
    let mut enabled = false;
    let mut port = None;
    let mut key = None;
    let mut peer = None;
    let control = control_enabled(cfg);
    for line in cfg.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("diag.dbg=") {
            enabled = value.trim() == "1";
        } else if let Some(value) = line.strip_prefix("diag.dbg.port=") {
            port = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("diag.dbg.key=") {
            key = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("diag.dbg.peer=") {
            peer = Some(value.trim());
        }
    }
    if !enabled {
        return Err(Refusal::Disabled);
    }
    let port = match port {
        None => DEFAULT_PORT,
        Some(text) => text
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(Refusal::BadPort)?,
    };
    let key = auth::parse_key(key.ok_or(Refusal::NoKey)?).ok_or(Refusal::BadKey)?;
    let peer = match peer {
        None | Some("") => None,
        Some(text) => Some(parse_ipv4(text).ok_or(Refusal::BadPeer)?),
    };
    Ok(Config {
        port,
        key,
        peer,
        control,
    })
}

/// Whether `cfg` turns the control tier on: `diag.dbg=1` and
/// `diag.dbg.control=1` (a later duplicate wins). `init` reads this too
/// before it accepts a reload from `dbgd`: the switch is the box's, not the
/// requester's.
pub fn control_enabled(cfg: &str) -> bool {
    let mut enabled = false;
    let mut control = false;
    for line in cfg.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("diag.dbg=") {
            enabled = value.trim() == "1";
        } else if let Some(value) = line.strip_prefix("diag.dbg.control=") {
            control = value.trim() == "1";
        }
    }
    enabled && control
}

/// The cfg lines the image build writes for `LAZYOS_DBGD=<key>[,port[,peer]]`
/// style settings: `diag.dbg=1` plus the given values.
pub fn cfg_lines(key_hex: &str, port: Option<u16>, peer: Option<&str>) -> alloc::string::String {
    let mut out = alloc::string::String::from("diag.dbg=1\n");
    out.push_str("diag.dbg.key=");
    out.push_str(key_hex);
    out.push('\n');
    if let Some(port) = port {
        out.push_str(&alloc::format!("diag.dbg.port={port}\n"));
    }
    if let Some(peer) = peer {
        out.push_str("diag.dbg.peer=");
        out.push_str(peer);
        out.push('\n');
    }
    out
}
