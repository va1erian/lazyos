//! The automount of removable volumes (docs/usb-hotplug-plan.md C): every
//! provider disk (a USB stick `usbd` serves) gets one `fatfuse`, started
//! as the session user, which finds a FAT volume on it and mounts it at
//! `/media/<label>`. The decisions are `mounttable::volumes`, host-tested;
//! this file lists the disks, follows who is logged in and spawns.
//!
//! Serial lines: `MOUNTD:VOLUME START usb<n> gen=<g> owner=<uid>:<gid>
//! pid=<pid>`, `MOUNTD:VOLUME STOP usb<n> gen=<g>`, `MOUNTD:VOLUME END
//! usb<n> gen=<g> <what the daemon said>`.

use alloc::vec::Vec;
use user::trace::{self, Channel};

use mounttable::volumes::{self, Action, DiskView, Outcome, State, Volumes};
use user::logins::Logins;
use user::messenger::{accounts, registry};
use user::sys::{self, BlockDisk, Personality, SpawnCred};

/// Ticks (100 Hz) between looks at the disks: a stick is mounted within a
/// second of appearing, and the cost of looking is one syscall.
const POLL_TICKS: u64 = 100;
/// Ticks `accountsd` may take to name the group of the session user.
const LOOKUP_TICKS: u64 = 50;

pub(crate) struct Automount {
    volumes: Volumes,
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
        for action in self.volumes.plan(&disks, self.owner) {
            match action {
                Action::Start {
                    disk,
                    generation,
                    owner,
                } => self.start(disk, generation, owner),
                Action::Stop { pid } => {
                    let _ = sys::kill(pid, sys::SIG_KILL);
                    trace::line!(Channel::Mountd, "VOLUME STOP pid={pid}");
                }
            }
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

    /// Every volume daemon still running.
    pub(crate) fn daemons(&self) -> impl Iterator<Item = u64> + '_ {
        self.volumes.daemons()
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
