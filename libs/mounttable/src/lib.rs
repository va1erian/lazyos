//! `mountd`'s decisions as data (docs/smb-plan.md §3.4), so they are tested
//! on the host: which requests are well formed ([`validate`],
//! [`validate_smb`], [`validate_kind`]), the table of mounts and their
//! states ([`Table`]), the `ftpfuse` or `smbfuse` command line and
//! environment that serve one ([`daemon_args`], [`daemon_env`]) and what a
//! daemon's exit status means ([`exit_reason`]). The service itself only
//! adds the syscalls.
//!
//! Every field of a request is hostile: it reaches a child's `argv`, so a
//! value that the daemon would read as another option (a host starting with
//! `-` or containing `=`) or that carries a control character is refused
//! here, before anything is started. An SMB password never reaches `argv`
//! (docs/smb-plan.md §6): `smbfuse` gets it in its environment.

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
/// which every `ftpfuse` it starts inherits. Not 907: that is `_greeter`'s,
/// which logind and accountsd trust to log people in.
pub const MOUNTD_UID: u32 = 910;

/// The uids that may serve a user-space filesystem (syscall 35, together
/// with `CAP_FS_PROVIDER`; docs/smb-plan.md §3.1): `mountd` and the daemons
/// it starts, and root (a console shell running `memfuse` or `smbfuse` by
/// hand). A session user never holds the capability, and a service that is
/// given it by mistake still cannot mount.
pub const FS_PROVIDER_UIDS: &[u32] = &[0, MOUNTD_UID];

/// Most mounts the service keeps, failed ones included.
pub const MAX_MOUNTS: usize = 8;
/// The FTP control port a request with port 0 gets.
pub const DEFAULT_PORT: u16 = 21;
/// The SMB port an SMB request with port 0 gets.
pub const SMB_PORT: u16 = 445;
/// Longest SMB share name (`smbwire::name::share_ok`).
pub const SHARE_MAX: usize = 80;
/// The variable `smbfuse` reads its password from.
pub const SMB_PASSWORD_VAR: &str = "LAZYOS_SMB_PASSWORD";
/// Longest mount name: it becomes a directory name under `/mnt`.
pub const NAME_MAX: usize = 32;
/// Longest host: a DNS name.
pub const HOST_MAX: usize = 253;
/// Longest user name or password.
pub const CREDENTIAL_MAX: usize = 128;
/// Ticks (100 Hz) a daemon may take to log in and mount before it is stopped
/// as failed. `ftpfuse` itself waits up to 20 s for the network at boot.
pub const MOUNT_TICKS: u64 = 4500;

/// The wire spelling of an FTP mount (`MountInfo.kind`).
pub const KIND_FTP: &str = "ftp";
/// The wire spelling of an SMB mount.
pub const KIND_SMB: &str = "smb";

/// The protocol a mount speaks, and so the daemon that serves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `ftpfuse`.
    Ftp,
    /// `smbfuse`.
    Smb,
}

impl Kind {
    /// The wire spelling.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Ftp => KIND_FTP,
            Kind::Smb => KIND_SMB,
        }
    }

    /// The port a request with port 0 gets.
    pub fn default_port(self) -> u16 {
        match self {
            Kind::Ftp => DEFAULT_PORT,
            Kind::Smb => SMB_PORT,
        }
    }
}

/// A request that passed [`validate`] or [`validate_smb`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub kind: Kind,
    pub name: String,
    pub host: String,
    pub port: u16,
    /// The SMB share; empty for FTP.
    pub share: String,
    /// Empty for the anonymous FTP login.
    pub user: String,
    pub password: String,
}

/// Check a `Mount` request of any kind: `kind` is `ftp` (or empty, what an
/// FTP-only caller sends) or `smb`, and an FTP request names no share.
pub fn validate_kind(
    kind: &str,
    name: &str,
    host: &str,
    port: u32,
    share: &str,
    user: &str,
    password: &str,
) -> Result<Request, &'static str> {
    match kind {
        "" | KIND_FTP if share.is_empty() => validate(name, host, port, user, password),
        "" | KIND_FTP => Err("an FTP mount names no share"),
        KIND_SMB => validate_smb(name, host, port, share, user, password),
        _ => Err("the kind must be ftp or smb"),
    }
}

/// Check an SMB `Mount` request: the FTP rules, a share name (1 to 80
/// characters, none of `\ / : * ? " < > |` and no control character) and
/// a user (an SMB logon is never anonymous: `smbwire` refuses guest
/// sessions).
pub fn validate_smb(
    name: &str,
    host: &str,
    port: u32,
    share: &str,
    user: &str,
    password: &str,
) -> Result<Request, &'static str> {
    let bad = |c: char| c.is_control() || "\\/:*?\"<>|".contains(c);
    let fits = !share.is_empty() && share.chars().count() <= SHARE_MAX;
    if !fits || share == "." || share == ".." || share.chars().any(bad) {
        return Err("the share must be 1 to 80 characters, without \\ / : * ? \" < > |");
    }
    if user.is_empty() {
        return Err("an SMB share needs a user name");
    }
    let port = if port == 0 { u32::from(SMB_PORT) } else { port };
    let mut request = validate(name, host, port, user, password)?;
    request.kind = Kind::Smb;
    request.share = String::from(share);
    Ok(request)
}

/// Check an FTP `Mount` request; the error is the reason, for the caller.
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
        kind: Kind::Ftp,
        name: String::from(name),
        host: String::from(host),
        port,
        share: String::new(),
        user: String::from(user),
        password: String::from(password),
    })
}

/// The daemon's `argv` (program name first) for `request`, its files owned
/// by `uid:gid`: `ftpfuse` (an empty user leaves its anonymous login) or
/// `smbfuse` (whose password is in [`daemon_env`], never here).
pub fn daemon_args(program: &str, request: &Request, uid: u32, gid: u32) -> Vec<String> {
    let mut argv = Vec::with_capacity(8);
    argv.push(String::from(program));
    if request.kind == Kind::Smb {
        argv.push(String::from("-p"));
        argv.push(format!("{}", request.port));
        argv.push(String::from("-U"));
        argv.push(request.user.clone());
        argv.push(format!("//{}/{}", request.host, request.share));
        argv.push(format!("name={}", request.name));
        argv.push(format!("owner={uid}:{gid}"));
        return argv;
    }
    argv.push(format!("{}:{}", request.host, request.port));
    if !request.user.is_empty() {
        argv.push(format!("user={}", request.user));
        argv.push(format!("pass={}", request.password));
    }
    argv.push(format!("name={}", request.name));
    argv.push(format!("owner={uid}:{gid}"));
    argv
}

/// The daemon's environment (`KEY=VALUE` items): `smbfuse`'s password.
pub fn daemon_env(request: &Request) -> Vec<String> {
    match request.kind {
        Kind::Smb => alloc::vec![format!("{SMB_PASSWORD_VAR}={}", request.password)],
        Kind::Ftp => Vec::new(),
    }
}

/// What a daemon's exit `status` says, for [`State::Failed`]. The numbers
/// are `ftpfuse`'s and `smbfuse`'s `Failure` codes; 128 + n is a task ended
/// by signal n.
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
        8 => "the server refused the share (check its name and your access)",
        129..=191 => return format!("the daemon was stopped (signal {})", status - 128),
        _ => return format!("the daemon exited with status {status}"),
    };
    String::from(text)
}
