//! Automounting removable volumes: which disks get a `fatfuse`, and when one
//! is stopped (docs/usb-hotplug-plan.md C). Data, so the host tests cover
//! every transition; `mountd` only adds the syscalls.
//!
//! ```text
//!   a disk appears ──▶ Start(fatfuse disk=<id>:<gen> owner=<session user>)
//!        │                    │ exits: code says why (no FAT volume, medium
//!        │                    │ pulled, ...): the entry stays, as the answer
//!        ▼                    ▼ for this plug, until the disk is gone
//!   the disk is gone, or its slot holds another disk, or the session user
//!   changed ──▶ Stop (the daemon, if it still runs) and forget the entry
//! ```
//!
//! One daemon per **plug** (a disk id and its generation): a stick that is
//! not a FAT volume is looked at once, not on every poll, and a replug is a
//! new generation and so a new look. Nothing mounts while nobody is logged
//! in: the files of a volume are owned by the session user.

use alloc::string::String;
use alloc::vec::Vec;

use crate::exit_reason;

/// Disks one plan can cover (the kernel serves eight).
pub const MAX_VOLUMES: usize = 8;

/// The daemon's exit codes that are not failures of the volume's own making
/// (`user/src/bin/fatfuse.rs` `Failure`).
pub mod code {
    pub const DISK: u64 = 10;
    pub const BUSY: u64 = 11;
    pub const NO_VOLUME: u64 = 12;
    pub const DAMAGED: u64 = 13;
    pub const MOUNT: u64 = 14;
    pub const LOST: u64 = 15;
}

/// A live provider disk, as the kernel lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskView {
    pub id: u32,
    pub generation: u64,
    pub writable: bool,
    /// Some task holds the raw claim (ours, once a daemon has started).
    pub claimed: bool,
}

/// What `mountd` should do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start a daemon for this plug, as `owner`.
    Start {
        disk: u32,
        generation: u64,
        owner: (u32, u32),
    },
    /// Stop the daemon `pid`: its disk is gone, or its owner logged out.
    Stop { pid: u64 },
}

/// Where a volume's daemon is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Asked to start; the caller has not reported its pid yet.
    Starting,
    /// The daemon runs (mounted, or looking for a volume).
    Running,
    /// It exited: what it said.
    Ended(Outcome),
}

/// What an exited daemon said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing there for it: the disk holds no FAT volume.
    NoVolume,
    /// The medium was pulled while mounted (the usual end).
    Removed,
    /// A clean stop.
    Stopped,
    /// Anything else, with the words for a log or a notice.
    Failed(String),
}

impl Outcome {
    fn of(status: u64) -> Outcome {
        match status {
            0 => Outcome::Stopped,
            code::NO_VOLUME => Outcome::NoVolume,
            code::LOST => Outcome::Removed,
            other => Outcome::Failed(exit_reason(other)),
        }
    }
}

/// One plug of one disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub disk: u32,
    pub generation: u64,
    pub owner: (u32, u32),
    pub pid: Option<u64>,
    pub state: State,
}

/// The volumes `mountd` has started a daemon for.
#[derive(Debug, Default)]
pub struct Volumes {
    entries: Vec<Volume>,
}

impl Volumes {
    pub fn new() -> Volumes {
        Volumes::default()
    }

    pub fn entries(&self) -> &[Volume] {
        &self.entries
    }

    /// What to do now: `disks` is the kernel's list, `session` the user the
    /// volumes belong to (`None` while nobody is logged in).
    pub fn plan(&mut self, disks: &[DiskView], session: Option<(u32, u32)>) -> Vec<Action> {
        let mut actions = Vec::new();
        // A plug that is over, or whose owner left, is forgotten (and its
        // daemon, if it still runs, is stopped).
        self.entries.retain(|volume| {
            let there = disks
                .iter()
                .any(|d| d.id == volume.disk && d.generation == volume.generation);
            let kept = there && session == Some(volume.owner);
            if !kept {
                if let Some(pid) = volume.pid {
                    actions.push(Action::Stop { pid });
                }
            }
            kept
        });
        let Some(owner) = session else {
            return actions;
        };
        for disk in disks {
            if self.entries.len() >= MAX_VOLUMES {
                break;
            }
            let known = self
                .entries
                .iter()
                .any(|v| v.disk == disk.id && v.generation == disk.generation);
            // A disk someone else claimed is theirs (another daemon, a tool).
            if known || disk.claimed {
                continue;
            }
            self.entries.push(Volume {
                disk: disk.id,
                generation: disk.generation,
                owner,
                pid: None,
                state: State::Starting,
            });
            actions.push(Action::Start {
                disk: disk.id,
                generation: disk.generation,
                owner,
            });
        }
        actions
    }

    /// The daemon for `disk`/`generation` runs as `pid`.
    pub fn started(&mut self, disk: u32, generation: u64, pid: u64) {
        if let Some(v) = self.find(disk, generation) {
            v.pid = Some(pid);
            v.state = State::Running;
        }
    }

    /// The daemon for `disk`/`generation` could not be started.
    pub fn failed(&mut self, disk: u32, generation: u64, reason: &str) {
        if let Some(v) = self.find(disk, generation) {
            v.state = State::Ended(Outcome::Failed(String::from(reason)));
        }
    }

    /// Daemon `pid` exited with `status`. Returns the volume, if it was one
    /// of ours.
    pub fn exited(&mut self, pid: u64, status: u64) -> Option<&Volume> {
        let v = self.entries.iter_mut().find(|v| v.pid == Some(pid))?;
        v.pid = None;
        v.state = State::Ended(Outcome::of(status));
        Some(v)
    }

    /// Every daemon still running, for a stop of the whole service.
    pub fn daemons(&self) -> impl Iterator<Item = u64> + '_ {
        self.entries.iter().filter_map(|v| v.pid)
    }

    fn find(&mut self, disk: u32, generation: u64) -> Option<&mut Volume> {
        self.entries
            .iter_mut()
            .find(|v| v.disk == disk && v.generation == generation)
    }
}

/// The `fatfuse` command line for one plug (program name first).
pub fn daemon_args(program: &str, disk: u32, generation: u64, owner: (u32, u32)) -> Vec<String> {
    alloc::vec![
        String::from(program),
        alloc::format!("disk={disk}:{generation}"),
        alloc::format!("owner={}:{}", owner.0, owner.1),
    ]
}
