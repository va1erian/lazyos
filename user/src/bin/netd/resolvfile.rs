//! Keep `fhs::state::RESOLV_CONF` (Linux programs' `/etc/resolv.conf`) in step
//! with the resolvers the stack holds (docs/tls-plan.md §5.1): written when a
//! lease, a renewal or a static configuration brings resolvers, rewritten when
//! they change, removed when they go away.
//!
//! The text comes from `netstack::resolvconf`, which writes only validated
//! addresses. The file is replaced atomically (a temporary file renamed over
//! it) so a reader never sees half of it. `/transient` is world-writable, so
//! the kernel serves the file only while it and `/transient/net` belong to
//! `_netd` (or root) and nobody else can write them: the directory is made
//! `0755` and the file `0644`. A directory someone else made first is
//! reported, not used.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::files::{self, Kind};
use user::sys;

/// Ticks between attempts after a failed write.
const RETRY_TICKS: u64 = 500;

/// The file's directory (`/transient/net`).
fn directory() -> &'static str {
    fhs::state::RESOLV_CONF
        .rsplit_once('/')
        .map_or("/", |(dir, _)| dir)
}

pub(super) struct ResolvFile {
    /// The resolvers the file currently says, `None` before the first sync.
    written: Option<Vec<[u8; 4]>>,
    /// Earliest tick for another attempt after a failure.
    retry_at: u64,
}

impl ResolvFile {
    pub(super) fn new() -> ResolvFile {
        ResolvFile {
            written: None,
            retry_at: 0,
        }
    }

    /// Bring the file in line with `dns`, if it is not already. Cheap when
    /// nothing changed; called once per loop.
    pub(super) fn sync(&mut self, dns: &[[u8; 4]], tick: u64) {
        if self.written.as_deref() == Some(dns) || tick < self.retry_at {
            return;
        }
        let result = match netstack::resolvconf::render(dns) {
            Some(text) => write(&text),
            None => remove(),
        };
        match result {
            Ok(what) => {
                sys::write_str(&format!("NETD:RESOLV {what}\n"));
                self.written = Some(Vec::from(dns));
            }
            Err(message) => {
                sys::write_str(&format!("NETD:RESOLV:FAIL {message}\n"));
                self.retry_at = tick + RETRY_TICKS;
            }
        }
    }
}

/// Create `/transient/net` (or accept the one this service made earlier).
fn ensure_directory() -> Result<(), String> {
    let dir = directory();
    match files::stat(dir) {
        Ok((_, Kind::Dir)) => {}
        Ok((_, Kind::File)) => return Err(format!("{dir} is not a directory")),
        Err(_) => files::mkdir(dir).map_err(|errno| format!("mkdir {dir}: errno {errno}"))?,
    }
    // Only the owner may change the mode: a directory another user planted
    // fails here, and the kernel would not trust it anyway.
    files::chmod(dir, 0o755).map_err(|errno| format!("chmod {dir}: errno {errno} (not ours?)"))
}

fn write(text: &str) -> Result<String, String> {
    ensure_directory()?;
    let path = fhs::state::RESOLV_CONF;
    let temp = format!("{path}.new");
    let fail = |what: &str, errno: i64| format!("{what} {temp}: errno {errno}");
    files::write_file(&temp, text.as_bytes()).map_err(|errno| fail("write", errno))?;
    files::chmod(&temp, 0o644).map_err(|errno| fail("chmod", errno))?;
    files::rename(&temp, path).map_err(|errno| {
        let _ = files::remove(&temp);
        fail("rename", errno)
    })?;
    let servers: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("nameserver "))
        .collect();
    Ok(format!("wrote {path} nameservers={}", servers.join(",")))
}

fn remove() -> Result<String, String> {
    let path = fhs::state::RESOLV_CONF;
    match files::stat(path) {
        Err(_) => Ok(format!("no resolvers; {path} absent")),
        Ok(_) => files::remove(path)
            .map(|()| format!("no resolvers; removed {path}"))
            .map_err(|errno| format!("remove {path}: errno {errno}")),
    }
}
