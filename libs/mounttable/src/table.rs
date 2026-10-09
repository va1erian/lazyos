//! The mounts `mountd` knows about, and how each one's state moves.
//!
//! ```text
//! add ──▶ Connecting ──(mount point appears)──▶ Mounted
//!              │                                   │
//!              └──(daemon exits, or MOUNT_TICKS)───┴──▶ Failed(reason)
//! ```
//!
//! A failed mount stays until it is removed, so its reason can be read. The
//! table never holds a password: it lives only in the daemon's `argv`.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{exit_reason, Kind, Request, MAX_MOUNTS, MOUNT_TICKS};

/// The reason of a mount whose daemon took longer than [`MOUNT_TICKS`].
pub const TIMED_OUT: &str = "the server did not answer in time";

/// Where a mount is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// The daemon is logging in; it was started at this tick.
    Connecting {
        since: u64,
    },
    Mounted,
    Failed(String),
}

impl State {
    /// The wire spelling (`os.lazy.mount.v1` `MountInfo.state`).
    pub fn name(&self) -> &'static str {
        match self {
            State::Connecting { .. } => "connecting",
            State::Mounted => "mounted",
            State::Failed(_) => "failed",
        }
    }

    /// Why it failed, empty otherwise.
    pub fn detail(&self) -> &str {
        match self {
            State::Failed(reason) => reason,
            _ => "",
        }
    }
}

/// One mount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub host: String,
    pub port: u16,
    /// The SMB share, empty for FTP.
    pub share: String,
    pub user: String,
    /// The requester: the files' owner, and who may remove it.
    pub owner: u32,
    /// The daemon serving it, while one runs.
    pub pid: Option<u64>,
    pub state: State,
}

/// Why the table refused a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The name is already listed (`EEXIST`).
    Exists,
    /// Every slot is taken (`EAGAIN`).
    Full,
    /// No mount has that name (`ENOENT`).
    NotFound,
    /// The mount is another user's (`EPERM`).
    Denied,
}

/// The mounts, in the order they were asked for.
#[derive(Default, Debug)]
pub struct Table {
    entries: Vec<Entry>,
}

impl Table {
    pub fn new() -> Table {
        Table::default()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Check that `request` may be added, before its daemon is started.
    pub fn admit(&self, request: &Request) -> Result<(), Error> {
        if self.entries.iter().any(|e| e.name == request.name) {
            return Err(Error::Exists);
        }
        if self.entries.len() >= MAX_MOUNTS {
            return Err(Error::Full);
        }
        Ok(())
    }

    /// Record a mount whose daemon `pid` started at tick `now`. The caller
    /// ran [`Table::admit`] first.
    pub fn add(&mut self, request: &Request, owner: u32, pid: u64, now: u64) {
        self.entries.push(Entry {
            kind: request.kind,
            name: request.name.clone(),
            host: request.host.clone(),
            port: request.port,
            share: request.share.clone(),
            user: request.user.clone(),
            owner,
            pid: Some(pid),
            state: State::Connecting { since: now },
        });
    }

    /// Remove `name` for `caller` (its owner, or root). Returns the daemon to
    /// stop, if one still runs.
    pub fn remove(&mut self, name: &str, caller: u32) -> Result<Option<u64>, Error> {
        let index = self
            .entries
            .iter()
            .position(|e| e.name == name)
            .ok_or(Error::NotFound)?;
        if caller != 0 && caller != self.entries[index].owner {
            return Err(Error::Denied);
        }
        Ok(self.entries.remove(index).pid)
    }

    /// The daemon `pid` exited with `status`: its mount failed (or, once
    /// mounted, was lost). Returns the mount, when the pid was one of ours.
    pub fn exited(&mut self, pid: u64, status: u64) -> Option<&Entry> {
        let entry = self.entries.iter_mut().find(|e| e.pid == Some(pid))?;
        entry.pid = None;
        entry.state = State::Failed(exit_reason(status));
        Some(entry)
    }

    /// Names of the mounts still connecting, to look for their mount point.
    pub fn connecting(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|e| matches!(e.state, State::Connecting { .. }))
            .map(|e| e.name.as_str())
    }

    /// `name`'s mount point appeared.
    pub fn mounted(&mut self, name: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name == name) {
            if matches!(entry.state, State::Connecting { .. }) {
                entry.state = State::Mounted;
            }
        }
    }

    /// Fail every mount still connecting [`MOUNT_TICKS`] after it started.
    /// Returns their names and daemons, for the caller to stop. The exit that
    /// follows finds no pid and changes nothing.
    pub fn expire(&mut self, now: u64) -> Vec<(String, u64)> {
        let mut stale = Vec::new();
        for entry in &mut self.entries {
            if let State::Connecting { since } = entry.state {
                if now.saturating_sub(since) >= MOUNT_TICKS {
                    if let Some(pid) = entry.pid.take() {
                        stale.push((entry.name.clone(), pid));
                    }
                    entry.state = State::Failed(String::from(TIMED_OUT));
                }
            }
        }
        stale
    }

    /// The next tick [`Table::expire`] has work, if anything is connecting.
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries
            .iter()
            .filter_map(|e| match e.state {
                State::Connecting { since } => Some(since + MOUNT_TICKS),
                _ => None,
            })
            .min()
    }

    /// Every daemon still running, for a stop of the whole service.
    pub fn daemons(&self) -> impl Iterator<Item = u64> + '_ {
        self.entries.iter().filter_map(|e| e.pid)
    }
}
