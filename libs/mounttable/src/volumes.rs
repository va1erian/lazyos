//! Automounting removable volumes: which disks get a `fatfuse`, and when one
//! is stopped (docs/usb-hotplug-plan.md C). Data, so the host tests cover
//! every transition; `mountd` only adds the syscalls.
//!
//! ```text
//!   a disk appears ──▶ Start(fatfuse disk=<id>:<gen> owner=<session user>)
//!        │                    │ exits: code says why (no FAT volume, medium
//!        │                    │ pulled, ...): the entry stays, as the answer
//!        ▼                    ▼ for this plug, until the disk is gone
//!   the session user changed ──▶ Stop (ask the daemon to flush and end;
//!        │                      Kill after LEAVE_TICKS) and forget the entry
//!        │                      once it has exited
//!   the disk is gone, or its slot holds another disk ──▶ Kill (nothing is
//!                               left to flush) and forget the entry
//!   an eject (`ejecting`) ──▶ Ejecting: the daemon was asked to flush and
//!                               end; exit 0 is Ejected, EJECT_TICKS without
//!                               one is a Kill and a failure
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

/// How long (ticks, 100 Hz) a daemon asked to stop at logout may take to
/// flush and exit before it is killed: 30 s, a slow stick's few writes.
pub const LEAVE_TICKS: u64 = 3000;
/// How long an eject may take: the block layer's request patience, 45 s.
pub const EJECT_TICKS: u64 = 4500;

/// The daemon's exit codes that are not failures of the volume's own making
/// (`user/src/bin/fatfuse.rs` `Failure`).
pub mod code {
    pub const DISK: u64 = 10;
    pub const BUSY: u64 = 11;
    pub const NO_VOLUME: u64 = 12;
    pub const DAMAGED: u64 = 13;
    pub const MOUNT: u64 = 14;
    pub const LOST: u64 = 15;
    /// The final flush of an eject was refused by the stick.
    pub const FLUSH: u64 = 16;
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
    /// Ask the daemon `pid` to flush its volume and end (its owner logged
    /// out). It is a request: `Kill` follows when it does not exit.
    Stop { pid: u64 },
    /// End the daemon `pid` outright: its disk is gone, or it did not
    /// answer a `Stop` or an eject in time.
    Kill { pid: u64 },
}

/// Where a volume's daemon is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Asked to start; the caller has not reported its pid yet.
    Starting,
    /// The daemon runs (mounted, or looking for a volume).
    Running,
    /// The user asked to eject: the daemon was told to flush and end.
    Ejecting { since: u64 },
    /// Its owner left: the daemon was told to end and is being waited for.
    Leaving { since: u64 },
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
    /// A clean stop nobody asked for (the kernel took the mount away).
    Stopped,
    /// The volume was flushed, closed and unmounted at the user's request:
    /// the stick may be pulled.
    Ejected,
    /// Its owner logged out and the daemon finished: the entry is forgotten
    /// at the next look, so the next session (even the same user's) mounts
    /// the stick again.
    Left,
    /// Anything else, with the words for a log or a notice.
    Failed(String),
}

impl Outcome {
    fn of(status: u64, asked: bool) -> Outcome {
        match status {
            0 if asked => Outcome::Ejected,
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
    /// The disk is not write protected.
    pub writable: bool,
    /// The mount's name (`/media/<name>`), once the daemon registered it.
    pub name: Option<String>,
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

    /// What to do now (`now`: ticks): `disks` is the kernel's list, `session`
    /// the user the volumes belong to (`None` while nobody is logged in).
    pub fn plan(
        &mut self,
        now: u64,
        disks: &[DiskView],
        session: Option<(u32, u32)>,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut kept = Vec::with_capacity(self.entries.len());
        for mut volume in core::mem::take(&mut self.entries) {
            let there = disks
                .iter()
                .any(|d| d.id == volume.disk && d.generation == volume.generation);
            let mine = session == Some(volume.owner);
            match (volume.pid, there) {
                // A volume whose owner left and whose daemon finished is
                // not an answer for this plug: forget it, mount again.
                (None, _) if volume.state == State::Ended(Outcome::Left) => {}
                (None, true) if mine => kept.push(volume),
                // Over: the daemon has exited and the plug or its owner is
                // gone.
                (None, _) => {}
                // The medium is gone: nothing is left to flush.
                (Some(pid), false) => actions.push(Action::Kill { pid }),
                (Some(_), true) => {
                    settle_running(now, mine, &mut volume, &mut actions);
                    kept.push(volume);
                }
            }
        }
        self.entries = kept;
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
                writable: disk.writable,
                name: None,
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

    /// The volume of disk `disk` that `uid` may eject (root may eject any),
    /// as `(generation, pid)`; `pid` is `None` for a daemon that has not
    /// been started yet.
    pub fn eject_target(&self, disk: u32, uid: u32) -> Result<(u64, Option<u64>), EjectError> {
        let volume = self
            .entries
            .iter()
            .rev()
            .find(|v| v.disk == disk && !matches!(v.state, State::Leaving { .. }))
            .ok_or(EjectError::Unknown)?;
        if uid != 0 && volume.owner.0 != uid {
            return Err(EjectError::NotYours);
        }
        match &volume.state {
            State::Starting | State::Running => Ok((volume.generation, volume.pid)),
            State::Ejecting { .. } => Err(EjectError::Ejecting),
            State::Leaving { .. } => Err(EjectError::Unknown),
            State::Ended(Outcome::Ejected) => Err(EjectError::Ejected),
            State::Ended(_) => Err(EjectError::Ended),
        }
    }

    /// The daemon `pid` serves its volume at `/media/<name>`. True the first
    /// time it is told (the caller logs the mount).
    pub fn mounted_as(&mut self, pid: u64, name: &str) -> bool {
        let Some(v) = self.entries.iter_mut().find(|v| v.pid == Some(pid)) else {
            return false;
        };
        if v.name.as_deref() == Some(name) {
            return false;
        }
        v.name = Some(String::from(name));
        true
    }

    /// The daemon of `disk`/`generation` was asked to flush and end for an
    /// eject.
    pub fn ejecting(&mut self, disk: u32, generation: u64, now: u64) {
        if let Some(v) = self.find(disk, generation) {
            v.state = State::Ejecting { since: now };
        }
    }

    /// The daemon of `disk`/`generation` had nothing mounted and was ended
    /// outright for an eject: the stick may be pulled.
    pub fn ejected_unmounted(&mut self, disk: u32, generation: u64) {
        if let Some(v) = self.find(disk, generation) {
            v.pid = None;
            v.state = State::Ended(Outcome::Ejected);
        }
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
        // A clean exit is an eject only when one was asked for.
        let asked = matches!(v.state, State::Ejecting { .. });
        let left = matches!(v.state, State::Leaving { .. });
        v.state = State::Ended(if left && status == 0 {
            Outcome::Left
        } else {
            Outcome::of(status, asked)
        });
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

/// Why an eject was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EjectError {
    /// No volume of that disk is known.
    Unknown,
    /// The volume belongs to another user.
    NotYours,
    /// An eject is already under way.
    Ejecting,
    /// It was ejected already: the stick may be pulled.
    Ejected,
    /// The volume had ended (never mounted, or the medium went away).
    Ended,
}

/// The `fatfuse` command line for one plug (program name first).
pub fn daemon_args(program: &str, disk: u32, generation: u64, owner: (u32, u32)) -> Vec<String> {
    alloc::vec![
        String::from(program),
        alloc::format!("disk={disk}:{generation}"),
        alloc::format!("owner={}:{}", owner.0, owner.1),
    ]
}

/// A volume whose daemon runs and whose disk is there: its owner may have
/// left (ask it to end, then insist), or an eject may be overdue.
fn settle_running(now: u64, mine: bool, volume: &mut Volume, actions: &mut Vec<Action>) {
    let Some(pid) = volume.pid else { return };
    match volume.state {
        State::Leaving { since } => {
            if now.saturating_sub(since) >= LEAVE_TICKS {
                volume.state = State::Ended(Outcome::Failed(String::from(
                    "the volume did not finish in time",
                )));
                volume.pid = None;
                actions.push(Action::Kill { pid });
            }
        }
        State::Ejecting { since } if mine => {
            if now.saturating_sub(since) >= EJECT_TICKS {
                volume.state = State::Ended(Outcome::Failed(String::from(
                    "the stick stopped answering while it was ejected",
                )));
                volume.pid = None;
                actions.push(Action::Kill { pid });
            }
        }
        _ if mine => {}
        _ => {
            volume.state = State::Leaving { since: now };
            actions.push(Action::Stop { pid });
        }
    }
}

/// How a disk is named to people and in requests: `usb<id>`, as the kernel
/// lists it.
pub fn disk_name(id: u32) -> String {
    alloc::format!("usb{id}")
}

/// The disk id of `name` (`usb<id>`), `None` for anything else.
pub fn parse_disk_name(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("usb")?;
    // Plain decimal: no sign, no leading zeros but "0", no overlong run.
    if digits.is_empty()
        || digits.len() > 6
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    digits.parse().ok()
}
