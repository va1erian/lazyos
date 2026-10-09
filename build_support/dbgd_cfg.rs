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
//! * `LAZYOS_DBGD_PEER=<a.b.c.d>`: the only address allowed to connect.
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
        && key.len() % 2 == 0
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

/// The `diag.dbg*` lines for a key, an optional port and an optional peer.
pub fn lines(key: &str, port: Option<u16>, peer: Option<&str>) -> String {
    let mut out = format!("diag.dbg=1\ndiag.dbg.key={key}\n");
    if let Some(port) = port {
        out.push_str(&format!("diag.dbg.port={port}\n"));
    }
    if let Some(peer) = peer {
        out.push_str(&format!("diag.dbg.peer={peer}\n"));
    }
    out
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
    lines(&key(key_file), port, peer.as_deref())
}
