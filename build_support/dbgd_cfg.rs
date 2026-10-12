//! `dbgd`'s switch and settings for `lazyos.cfg` (docs/dbgd-plan.md,
//! issue #701).
//!
//! `LAZYOS_DBGD=1` builds the remote inspection service into the image and
//! writes the `diag.dbg.*` lines it needs; without it nothing of `dbgd` is in
//! the image (no binary, no manifest row, no lines). Settings:
//!
//! * `LAZYOS_DBGD_KEY=<32..128 hex chars>`: the pre-shared key; when unset a
//!   random one is made once and kept in `target/dbgd.key`, which the host
//!   tools (`tools/dbg/dbgctl.py`, the MCP bridge) read by default;
//! * `LAZYOS_DBGD_PORT=<port>`: the TCP port (default 9701);
//! * `LAZYOS_DBGD_PEER=<a.b.c.d>`: the only address allowed to connect;
//! * `LAZYOS_DBGD_CONTROL=1`: the control tier (`diag.dbg.control=1`):
//!   restart and hot-reload services remotely (docs/dbgd-plan.md, v2);
//! * `LAZYOS_DBGD_MMIO=1`: `mmio.read` of allowlisted device registers
//!   (`diag.dbg.mmio=1`, docs/dbgd-plan.md "Hardware reads"); a register
//!   read can wedge hardware, so it is a switch of its own, off by default.
//!   `LAZYOS_DBGD_MMIO_CLAIMED=1` also allows reading a device a driver
//!   holds (`diag.dbg.mmio.claimed=1`), and
//!   `LAZYOS_DBGD_MMIO_ALLOW=VVVV:DDDD:BAR:START-END[,...]` adds readable
//!   ranges (`diag.dbg.mmio.allow=`, hex). Both need `LAZYOS_DBGD_MMIO=1`.
//!
//! The key lands in `/boot/lazyos.cfg` on the unencrypted boot volume: this
//! is a debug build, not a release image.

use std::path::Path;

/// Whether the build asked for `dbgd`.
pub fn enabled() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_DBGD");
    std::env::var_os("LAZYOS_DBGD").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// `key` as it must be written: 16..=64 bytes of hex.
pub fn validate_key(key: &str) -> Result<(), String> {
    let ok = (32..=128).contains(&key.len())
        && key.len().is_multiple_of(2)
        && key.bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(format!(
            "LAZYOS_DBGD_KEY must be 32 to 128 hex characters (16 to 64 bytes), got {} characters",
            key.len()
        ))
    }
}

/// `a.b.c.d`.
pub fn validate_peer(peer: &str) -> Result<(), String> {
    let parts: Vec<&str> = peer.split('.').collect();
    let ok = parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        });
    if ok {
        Ok(())
    } else {
        Err(format!("LAZYOS_DBGD_PEER={peer:?}: expected a.b.c.d"))
    }
}

/// The `diag.dbg*` lines for a key, an optional port, an optional peer and
/// the control tier.
pub fn lines(key: &str, port: Option<u16>, peer: Option<&str>, control: bool) -> String {
    let mut out = format!("diag.dbg=1\ndiag.dbg.key={key}\n");
    if let Some(port) = port {
        out.push_str(&format!("diag.dbg.port={port}\n"));
    }
    if let Some(peer) = peer {
        out.push_str(&format!("diag.dbg.peer={peer}\n"));
    }
    if control {
        out.push_str("diag.dbg.control=1\n");
    }
    out
}

/// `VVVV:DDDD:BAR:START-END` in hex, as `pciprobe::parse_range` takes it:
/// dword aligned, start below end, a BAR 0..=5, at most 64 MiB.
pub fn validate_range(range: &str) -> Result<(), String> {
    let fail = || {
        format!("LAZYOS_DBGD_MMIO_ALLOW entry {range:?}: expected VVVV:DDDD:BAR:START-END in hex")
    };
    let hex = |text: &str| {
        let digits = text.strip_prefix("0x").unwrap_or(text);
        (!digits.is_empty() && digits.len() <= 16)
            .then(|| u64::from_str_radix(digits, 16).ok())
            .flatten()
    };
    let parts: Vec<&str> = range.split(':').collect();
    let [vendor, device, bar, span] = parts[..] else {
        return Err(fail());
    };
    let (start, end) = span.split_once('-').ok_or_else(fail)?;
    let ok = hex(vendor).is_some_and(|v| v <= 0xFFFF)
        && hex(device).is_some_and(|d| d <= 0xFFFF)
        && hex(bar).is_some_and(|b| b <= 5);
    match (ok, hex(start), hex(end)) {
        (true, Some(start), Some(end))
            if start < end && start % 4 == 0 && end % 4 == 0 && end - start <= 64 << 20 =>
        {
            Ok(())
        }
        _ => Err(fail()),
    }
}

/// The `diag.dbg.mmio*` lines: nothing unless `mmio` (the sub-switches
/// mean nothing without it; asking for them alone is a build error where
/// the environment is read).
pub fn hardware_lines(mmio: bool, claimed: bool, allow: &[String]) -> String {
    if !mmio {
        return String::new();
    }
    let mut out = String::from("diag.dbg.mmio=1\n");
    if claimed {
        out.push_str("diag.dbg.mmio.claimed=1\n");
    }
    for range in allow {
        out.push_str(&format!("diag.dbg.mmio.allow={range}\n"));
    }
    out
}

fn flag(name: &str) -> bool {
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var_os(name).as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// The hardware-read lines the environment asks for.
fn hardware_from_env() -> String {
    let (mmio, claimed) = (flag("LAZYOS_DBGD_MMIO"), flag("LAZYOS_DBGD_MMIO_CLAIMED"));
    println!("cargo:rerun-if-env-changed=LAZYOS_DBGD_MMIO_ALLOW");
    let allow: Vec<String> = std::env::var("LAZYOS_DBGD_MMIO_ALLOW")
        .unwrap_or_default()
        .split(',')
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    if !mmio && (claimed || !allow.is_empty()) {
        panic!("LAZYOS_DBGD_MMIO_CLAIMED and LAZYOS_DBGD_MMIO_ALLOW need LAZYOS_DBGD_MMIO=1");
    }
    for range in &allow {
        validate_range(range).unwrap_or_else(|error| panic!("{error}"));
    }
    if allow.len() > 8 {
        panic!("LAZYOS_DBGD_MMIO_ALLOW: at most 8 ranges (lazyos.cfg is 4 KiB)");
    }
    hardware_lines(mmio, claimed, &allow)
}

/// Whether the build asked for the control tier (`LAZYOS_DBGD_CONTROL=1`).
fn control() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_DBGD_CONTROL");
    std::env::var_os("LAZYOS_DBGD_CONTROL").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// A new random key (32 bytes as hex) from the operating system's
/// cryptographic random source.
fn random_key() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .unwrap_or_else(|e| panic!("no OS random source for the dbgd key: {e}"));
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The key to use: `LAZYOS_DBGD_KEY`, else the one in `key_file`, else a new
/// one saved there.
fn key(key_file: &Path) -> String {
    println!("cargo:rerun-if-env-changed=LAZYOS_DBGD_KEY");
    if let Ok(given) = std::env::var("LAZYOS_DBGD_KEY") {
        let given = given.trim().to_string();
        if !given.is_empty() {
            validate_key(&given).unwrap_or_else(|error| panic!("{error}"));
            return given;
        }
    }
    if let Ok(saved) = std::fs::read_to_string(key_file) {
        let saved = saved.trim().to_string();
        if validate_key(&saved).is_ok() {
            return saved;
        }
    }
    let fresh = random_key();
    if let Some(dir) = key_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(key_file, format!("{fresh}\n"))
        .unwrap_or_else(|e| panic!("cannot save {}: {e}", key_file.display()));
    println!(
        "cargo:warning=LAZYOS_DBGD: new dbgd key saved to {}",
        key_file.display()
    );
    fresh
}

/// The `lazyos.cfg` lines for this build: empty unless `LAZYOS_DBGD=1`.
/// Panics (failing the build) on a malformed setting.
pub fn from_env(key_file: &Path) -> String {
    if !enabled() {
        return String::new();
    }
    println!("cargo:rerun-if-env-changed=LAZYOS_DBGD_PORT");
    println!("cargo:rerun-if-env-changed=LAZYOS_DBGD_PEER");
    let port = std::env::var("LAZYOS_DBGD_PORT")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .unwrap_or_else(|| panic!("LAZYOS_DBGD_PORT={v:?}: expected 1..65535"))
        });
    let peer = std::env::var("LAZYOS_DBGD_PEER")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if let Some(peer) = &peer {
        validate_peer(peer).unwrap_or_else(|error| panic!("{error}"));
    }
    lines(&key(key_file), port, peer.as_deref(), control()) + &hardware_from_env()
}
