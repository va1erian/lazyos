//! An SMB 2.1 share as a user-space filesystem (docs/smb-plan.md §4.4,
//! stage F3): [`SmbFs`] implements `libs/fused`'s [`fused::daemon::FuseFs`]
//! over `libs/smbwire`'s synchronous [`Client`], so the `smbfuse` daemon only
//! adds the socket, the logon and the serve loop. Host-tested against
//! `smbwire`'s in-memory server.
//!
//! | FUSE op | SMB2 |
//! |---|---|
//! | `lookup` | the parent's fresh listing, else `CREATE` (attributes) + `CLOSE` |
//! | `readdir` | `QUERY_DIRECTORY` (FileIdBothDirectoryInformation) |
//! | `read` / `write` / `truncate` | `READ` / `WRITE` / `SET_INFO` end-of-file on a kept handle |
//! | `create` / `mkdir` | `CREATE` with the create disposition |
//! | `unlink` / `rmdir` | `SET_INFO` delete-on-close, then `CLOSE` |
//! | `rename` | `SET_INFO` FileRenameInformation |
//! | `flush` | `FLUSH` of every file written through a kept handle |
//! | `statfs` | `QUERY_INFO` FileFsFullSizeInformation |
//!
//! **Coherence.** Other clients change the share, so attributes and listings
//! are believed for [`FRESH_TICKS`], the kernel's own `fs::fuse::ATTR_TICKS`;
//! reads and writes always go to the server.
//!
//! **Reconnects.** A lost connection or session drops the session and every
//! handle and cache with it; the next request logs on again through
//! [`Connect`]. An operation that is safe to repeat (lookups, listings,
//! reads, writes at an offset, truncates, `statfs`) is retried once on the
//! new session; one that is not (create, remove, rename) fails with `EIO`,
//! since the server may have done it before the connection broke.
//!
//! **Attributes.** SMB has no Unix owner or mode bits: every file is
//! reported as the mount's owner, directories `0755`, files `0644` (`0444`
//! when read-only on the server). `chmod`, `chown` and time changes are
//! accepted and not applied, so `cp -p` and `touch` work.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod cache;
mod fs;
mod handles;
#[cfg(test)]
mod tests;

use alloc::string::String;

use fused::daemon::Errno;
use fused::inodes::Inodes;
use fused::wire::errno;
use smbwire::client::{Client, Transport};
use smbwire::msg::FileId;
use smbwire::{status, Error};

pub use handles::{IDLE_TICKS, MAX_HANDLES};

/// How long attributes and listings are believed, ticks (100 Hz): 1 s.
pub const FRESH_TICKS: u64 = 100;
/// The `statfs` magic (Linux's `SMB2_MAGIC_NUMBER`).
pub const MAGIC: u64 = 0xFE53_4D42;
/// Seconds between 1601 (FILETIME) and 1970.
const FILETIME_UNIX: u64 = 11_644_473_600;

/// A new session: connected, logged on and with the share's tree
/// connected. `smbfuse` dials the server again; tests hand out a client of
/// the in-memory server.
pub trait Connect<T: Transport> {
    fn connect(&mut self) -> Result<Client<T>, Error>;
}

/// What the files are reported as, and the clock.
#[derive(Clone, Copy)]
pub struct Options {
    pub uid: u32,
    pub gid: u32,
    /// Unix seconds, for the root's times and a server time of 0.
    pub started: i64,
    /// Ticks (100 Hz), for the caches and the handles.
    pub clock: fn() -> u64,
}

/// Counters, for the daemon's log and the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Sessions lost and logged on again.
    pub reconnects: u64,
    /// Logons that failed after a loss.
    pub reconnect_failures: u64,
}

/// An SMB share served as a directory tree.
pub struct SmbFs<T: Transport, C: Connect<T>> {
    client: Option<Client<T>>,
    connector: C,
    opts: Options,
    cache: cache::Cache,
    handles: handles::Handles,
    inodes: Inodes,
    stats: Stats,
}

/// Whether `error` means the session is gone (and a new one may succeed).
fn lost(error: &Error) -> bool {
    match error {
        Error::Transport(_) | Error::Closed => true,
        Error::Status { status: s, .. } => {
            *s == status::USER_SESSION_DELETED || *s == status::NETWORK_SESSION_EXPIRED
        }
        _ => false,
    }
}

/// The errno of a failed SMB operation.
pub fn errno_of(error: &Error) -> Errno {
    let Error::Status { status: s, .. } = error else {
        return match error {
            Error::BadName => errno::EINVAL,
            _ => errno::EIO,
        };
    };
    match *s {
        status::OBJECT_NAME_NOT_FOUND | status::OBJECT_PATH_NOT_FOUND | status::DELETE_PENDING => {
            errno::ENOENT
        }
        status::OBJECT_NAME_COLLISION => errno::EEXIST,
        status::ACCESS_DENIED | status::SHARING_VIOLATION => errno::EACCES,
        status::DIRECTORY_NOT_EMPTY => errno::ENOTEMPTY,
        status::NOT_A_DIRECTORY => errno::ENOTDIR,
        status::FILE_IS_A_DIRECTORY => errno::EISDIR,
        status::OBJECT_NAME_INVALID => errno::EINVAL,
        status::NOT_SUPPORTED => errno::EOPNOTSUPP,
        status::DISK_FULL => errno::ENOSPC,
        status::MEDIA_WRITE_PROTECTED => errno::EROFS,
        _ => errno::EIO,
    }
}

/// Unix seconds of a FILETIME; `None` for 0 (unknown) or before 1970.
fn unix_time(filetime: u64) -> Option<i64> {
    let seconds = (filetime / 10_000_000).checked_sub(FILETIME_UNIX)?;
    (filetime != 0).then_some(seconds as i64)
}

/// The parent directory and last name of a non-root path.
pub(crate) fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        String::from(name)
    } else {
        alloc::format!("{dir}/{name}")
    }
}

impl<T: Transport, C: Connect<T>> SmbFs<T, C> {
    /// Serve the share `client` is connected to; `connector` makes the next
    /// session when this one is lost.
    pub fn new(client: Client<T>, connector: C, opts: Options) -> SmbFs<T, C> {
        SmbFs {
            client: Some(client),
            connector,
            opts,
            cache: cache::Cache::default(),
            handles: handles::Handles::default(),
            inodes: Inodes::new(),
            stats: Stats::default(),
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Files kept open right now.
    pub fn open_handles(&self) -> usize {
        self.handles.count()
    }

    /// The live session's client (tests reach the server through it).
    pub fn client_mut(&mut self) -> Option<&mut Client<T>> {
        self.client.as_mut()
    }

    /// Between requests: close the handles nobody used for [`IDLE_TICKS`].
    pub fn idle(&mut self) {
        let idle = self.handles.take_idle(self.now());
        self.close_all(&idle);
    }

    /// Close every handle and log off (the daemon is stopping).
    pub fn shutdown(&mut self) {
        let all = self.handles.take_under("");
        self.close_all(&all);
        if let Some(client) = self.client.as_mut() {
            client.logoff();
        }
        self.client = None;
    }

    fn now(&self) -> u64 {
        (self.opts.clock)()
    }

    /// The live session; the caller made sure there is one ([`Self::run`]).
    fn client(&mut self) -> Result<&mut Client<T>, Error> {
        self.client.as_mut().ok_or(Error::Closed)
    }

    /// Run `op` on the session, logging on again first when it was lost.
    /// When the session is lost under `op`, everything tied to it is
    /// forgotten and, if `repeatable`, `op` runs once more on a new one.
    fn run<R>(
        &mut self,
        repeatable: bool,
        mut op: impl FnMut(&mut Self) -> Result<R, Error>,
    ) -> Result<R, Errno> {
        for attempt in 0..2 {
            if self.client.is_none() {
                match self.connector.connect() {
                    Ok(client) => {
                        self.client = Some(client);
                        self.stats.reconnects += 1;
                    }
                    Err(_) => {
                        self.stats.reconnect_failures += 1;
                        return Err(errno::EIO);
                    }
                }
            }
            match op(self) {
                Ok(value) => return Ok(value),
                Err(error) if lost(&error) => {
                    self.drop_session();
                    if !repeatable || attempt == 1 {
                        return Err(errno::EIO);
                    }
                }
                Err(error) => return Err(errno_of(&error)),
            }
        }
        Err(errno::EIO)
    }

    /// The session is gone: its handles are dead and what it saw is old.
    fn drop_session(&mut self) {
        self.client = None;
        self.handles.clear();
        self.cache.clear();
    }

    /// Close `ids`; a close that fails changes nothing for the caller (the
    /// server drops a handle with its session anyway).
    fn close_all(&mut self, ids: &[FileId]) {
        for id in ids {
            let Some(client) = self.client.as_mut() else {
                return;
            };
            if let Err(error) = client.close(id) {
                if lost(&error) {
                    self.drop_session();
                }
            }
        }
    }
}
