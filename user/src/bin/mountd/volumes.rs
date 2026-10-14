//! The automount of removable volumes (docs/usb-hotplug-plan.md C): every
//! provider disk (a USB stick `usbd` serves) gets one `fatfuse`, started
//! as the session user, which finds a FAT volume on it and mounts it at
//! `/media/<label>`. The decisions are `mounttable::volumes`, host-tested;
//! this file lists the disks, follows who is logged in and spawns.
//!
//! A daemon is ended by asking it (docs/usb-hotplug-plan.md D): the kernel's
//! `STOP` makes it flush, close the filesystem (the volume's dirty flag is
//! cleared) and unmount itself, and only a daemon that has not registered a
//! mount yet, whose stick is gone, or that does not answer in time is killed.
//! A request in flight at a `SIGKILL` is a torn FAT update.
//!
//! Serial lines: `MOUNTD:VOLUME START usb<n> gen=<g> owner=<uid>:<gid>
//! pid=<pid>`, `MOUNTD:VOLUME STOP pid=<pid>`, `MOUNTD:VOLUME KILL pid=<pid>
//! <why>`, `MOUNTD:VOLUME MOUNT usb<n> /media/<name>`, `MOUNTD:VOLUME END
//! usb<n> gen=<g> <what the daemon said>`.

use alloc::vec::Vec;
use user::trace::{self, Channel};

use mounttable::volumes::{self, disk_name, Action, DiskView, Outcome, State, Volumes};
use user::logins::Logins;
use user::messenger::{accounts, registry};
use user::sys::{self, fuse, BlockDisk, Personality, SpawnCred};

/// Ticks (100 Hz) between looks at the disks: a stick is mounted within a
/// second of appearing, and the cost of looking is one syscall.
const POLL_TICKS: u64 = 100;
/// Ticks `accountsd` may take to name the group of the session user.
const LOOKUP_TICKS: u64 = 50;
/// Ticks a shutdown waits for the volume daemons it asked to finish
/// (`init` gives the service three seconds in all).
const SHUTDOWN_WAIT_TICKS: u64 = 220;

pub(crate) struct Automount {
    pub(crate) volumes: Volumes,
    logins: Logins,
    next_poll: u64,
    /// The session user and their primary group, once known.
    owner: Option<(u32, u32)>,
}

impl Automount {
    pub(crate) fn new() -> Automount {
        Automount {
            volumes: Volumes::new(),
            logins: Logins::new(Channel::Mountd),
            next_poll: 0,
            owner: None,
        }
    }

    /// When the next look is due (ticks), if automount runs at all.
    pub(crate) fn deadline(&self) -> Option<u64> {
        cfg!(lazyos_usb).then_some(self.next_poll)
    }

    /// Look at the disks and the session once, when it is time; start and
    /// stop daemons as `mounttable::volumes` decides.
    pub(crate) fn step(&mut self) {
        let now = sys::clock();
        if !cfg!(lazyos_usb) || now < self.next_poll {
            return;
        }
        self.next_poll = now + POLL_TICKS;
        self.follow_session();
        let mut found = [BlockDisk::default(); 8];
        let Ok(count) = sys::block_disks(&mut found) else {
            return;
        };
        let disks: Vec<DiskView> = found[..count]
            .iter()
            .map(|d| DiskView {
                id: d.id as u32,
                generation: d.generation,
                writable: d.writable,
                claimed: d.claimed,
            })
            .collect();
        for action in self.volumes.plan(now, &disks, self.owner) {
            match action {
                Action::Start {
                    disk,
                    generation,
                    owner,
                } => self.start(disk, generation, owner),
                Action::Stop { pid } => self.ask_to_stop(pid),
                Action::Kill { pid } => {
                    let _ = sys::kill(pid, sys::SIG_KILL);
                    trace::line!(Channel::Mountd, "VOLUME KILL pid={pid} gone or unresponsive");
                }
            }
        }
        self.note_mounts();
    }

    /// Ask daemon `pid` to flush and end. A daemon that has registered no
    /// mount has served nothing and holds nothing: it is ended outright.
    fn ask_to_stop(&self, pid: u64) {
        let provider = fuse::providers()
            .ok()
            .and_then(|all| all.into_iter().find(|p| p.owner == pid));
        let asked = provider.is_some_and(|p| fuse::stop(p.id, true).is_ok());
        if asked {
            trace::line!(Channel::Mountd, "VOLUME STOP pid={pid}");
        } else {
            let _ = sys::kill(pid, sys::SIG_KILL);
            trace::line!(Channel::Mountd, "VOLUME KILL pid={pid} no mount to finish");
        }
    }

    /// Learn the mount names of daemons that have registered theirs.
    fn note_mounts(&mut self) {
        let waiting = self
            .volumes
            .entries()
            .iter()
            .any(|v| v.pid.is_some() && v.name.is_none());
        if !waiting {
            return;
        }
        let Ok(providers) = fuse::providers() else {
            return;
        };
        for provider in providers {
            if self.volumes.mounted_as(provider.owner, &provider.name) {
                let disk = self
                    .volumes
                    .entries()
                    .iter()
                    .find(|v| v.pid == Some(provider.owner))
                    .map_or(0, |v| v.disk);
                trace::line!(
                    Channel::Mountd,
                    "VOLUME MOUNT {} {}/{}",
                    disk_name(disk),
                    fhs::mount::MEDIA,
                    provider.name
                );
            }
        }
    }

    /// The service is stopping: ask every daemon to finish, wait (bounded)
    /// for them, and end what is left.
    pub(crate) fn shutdown(&mut self) {
        let mut pending: Vec<u64> = self.volumes.daemons().collect();
        for &pid in &pending {
            self.ask_to_stop(pid);
        }
        let deadline = sys::clock() + SHUTDOWN_WAIT_TICKS;
        while !pending.is_empty() {
            let Some((pid, status)) = sys::wait(deadline) else {
                break;
            };
            pending.retain(|&p| p != pid);
            self.exited(pid, status);
        }
        for pid in pending {
            let _ = sys::kill(pid, sys::SIG_KILL);
            trace::line!(Channel::Mountd, "VOLUME KILL pid={pid} did not finish");
        }
    }

    fn start(&mut self, disk: u32, generation: u64, owner: (u32, u32)) {
        let argv = volumes::daemon_args(fhs::bin::FATFUSE, disk, generation, owner);
        let argv: Vec<&str> = argv.iter().map(alloc::string::String::as_str).collect();
        // Inherit: `_mountd` and `CAP_FS_PROVIDER`, which is what a daemon
        // needs to serve a mount and to claim a disk raw.
        match sys::spawnv(
            fhs::bin::FATFUSE,
            &argv,
            &[],
            Personality::Native,
            SpawnCred::Inherit,
        ) {
            Ok(pid) => {
                self.volumes.started(disk, generation, pid);
                trace::line!(
                    Channel::Mountd,
                    "VOLUME START usb{disk} gen={generation} owner={}:{} pid={pid}",
                    owner.0,
                    owner.1
                );
            }
            Err(errno) => {
                self.volumes
                    .failed(disk, generation, "cannot start the volume daemon");
                trace::line!(
                    Channel::Mountd,
                    "VOLUME END usb{disk} gen={generation} spawn failed: errno {errno}"
                );
            }
        }
    }

    /// A child exited: whether it was a volume daemon.
    pub(crate) fn exited(&mut self, pid: u64, status: u64) -> bool {
        let Some(volume) = self.volumes.exited(pid, status) else {
            return false;
        };
        let said = match &volume.state {
            State::Ended(Outcome::NoVolume) => "no FAT volume",
            State::Ended(Outcome::Removed) => "removed",
            State::Ended(Outcome::Stopped) => "stopped",
            State::Ended(Outcome::Ejected) => "ejected",
            State::Ended(Outcome::Failed(reason)) => reason.as_str(),
            _ => "ended",
        };
        trace::line!(
            Channel::Mountd,
            "VOLUME END usb{} gen={} {said}",
            volume.disk,
            volume.generation
        );
        true
    }

    /// Who the volumes belong to: the user with a session, and their group.
    fn follow_session(&mut self) {
        if self.logins.changed().is_none() && self.owner.is_some() {
            return;
        }
        let user = self.logins.users().first().copied();
        self.owner = user.map(|uid| (uid, group_of(uid)));
    }
}

/// `uid`'s primary group from `accountsd` (a user's own group, by the
/// accounts the image ships, when it does not answer in time).
fn group_of(uid: u32) -> u32 {
    let Ok(endpoint) = registry::resolve(accounts::NAME) else {
        return uid;
    };
    let deadline = Some(sys::clock() + LOOKUP_TICKS);
    let found = accounts::lookup_uid_by(&endpoint, uid, deadline);
    let _ = endpoint.release();
    found.ok().flatten().map_or(uid, |record| record.gid)
}
