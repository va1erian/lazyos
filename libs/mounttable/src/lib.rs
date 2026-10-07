//! `mountd`'s decisions as data (docs/smb-plan.md §3.4), so they are tested
//! on the host: which requests are well formed ([`validate`]), the table of
//! mounts and their states ([`Table`]), the `ftpfuse` command line that
//! serves one ([`daemon_args`]) and what a daemon's exit status means
//! ([`exit_reason`]). The service itself only adds the syscalls.
//!
//! Every field of a request is hostile: it reaches a child's `argv`, so a
//! value that `ftpfuse` would read as another option (a host starting with
//! `-` or containing `=`) or that carries a control character is refused
//! here, before anything is started.

#![no_std]

extern crate alloc;

mod table;
#[cfg(test)]
mod tests;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub use table::{Entry, Error, State, Table, TIMED_OUT};

/// The `_mountd` system user the service runs as: only `CAP_FS_PROVIDER`,
/// which every `ftpfuse` it starts inherits.
pub const MOUNTD_UID: u32 = 907;

/// Most mounts the service keeps, failed ones included.
pub const MAX_MOUNTS: usize = 8;
/// The FTP control port a request with port 0 gets.
pub const DEFAULT_PORT: u16 = 21;
/// Longest mount name: it becomes a directory name under `/mnt`.
pub const NAME_MAX: usize = 32;
/// Longest host: a DNS name.
pub const HOST_MAX: usize = 253;
/// Longest user name or password.
pub const CREDENTIAL_MAX: usize = 128;
/// Ticks (100 Hz) a daemon may take to log in and mount before it is stopped
/// as failed. `ftpfuse` itself waits up to 20 s for the network at boot.
pub const MOUNT_TICKS: u64 = 4500;

/// The protocol of every mount today.
pub const KIND_FTP: &str = "ftp";

/// A request that passed [`validate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// Empty for the anonymous login.
    pub user: String,
    pub password: String,
}

/// Check a `Mount` request; the error is the reason, for the caller.
pub fn validate(
    name: &str,
    host: &str,
    port: u32,
    user: &str,
    password: &str,
) -> Result<Request, &'static str> {
    let name_ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
    if name.is_empty() || name.len() > NAME_MAX || !name.chars().all(name_ok) {
        return Err("the name must be 1 to 32 of a-z, 0-9, - and _");
    }
    // `ftpfuse` splits `host:port` at the first colon: the port travels
    // separately, so the host may not carry one.
    if host.contains(':') {
        return Err("give the port in its own field");
    }
    // Letters, digits, dots and dashes only (no `=`, no space), and no
    // leading `-`, which `ftpfuse` would read as an option.
    let host_ok = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    let fits = !host.is_empty() && host.len() <= HOST_MAX && !host.starts_with('-');
    if !fits || !host.chars().all(host_ok) {
        return Err("the host must be a name or an address");
    }
    let port = match port {
        0 => DEFAULT_PORT,
        1..=65535 => port as u16,
        _ => return Err("the port must be 1 to 65535"),
    };
    for value in [user, password] {
        if value.len() > CREDENTIAL_MAX || value.chars().any(char::is_control) {
            return Err("the user name and password must be at most 128 printable characters");
        }
    }
    if user.is_empty() && !password.is_empty() {
        return Err("a password needs a user name");
    }
    Ok(Request {
        name: String::from(name),
        host: String::from(host),
        port,
        user: String::from(user),
        password: String::from(password),
    })
}

/// `ftpfuse`'s `argv` (program name first) for `request`, its files owned by
/// `uid:gid`. An empty user leaves `ftpfuse`'s anonymous login.
pub fn daemon_args(program: &str, request: &Request, uid: u32, gid: u32) -> Vec<String> {
    let mut argv = Vec::with_capacity(6);
    argv.push(String::from(program));
    argv.push(format!("{}:{}", request.host, request.port));
    if !request.user.is_empty() {
        argv.push(format!("user={}", request.user));
        argv.push(format!("pass={}", request.password));
    }
    argv.push(format!("name={}", request.name));
    argv.push(format!("owner={uid}:{gid}"));
    argv
}

/// What a daemon's exit `status` says, for [`State::Failed`]. The numbers
/// are `ftpfuse`'s `Failure` codes; 128 + n is a task ended by signal n.
pub fn exit_reason(status: u64) -> String {
    let text = match status {
        0 => "the daemon stopped",
        1 => "the daemon crashed",
        2 => "the daemon refused its arguments",
        3 => "the network is not available",
        4 => "cannot find the host",
        5 => "cannot connect or log in (check the host, port, user and password)",
        6 => "cannot mount: the name is in use",
        7 => "the connection to the server was lost",
        129..=191 => return format!("the daemon was stopped (signal {})", status - 128),
        _ => return format!("the daemon exited with status {status}"),
    };
    String::from(text)
}
