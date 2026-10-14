//! Eject and the volume list of `os.lazy.mount.v1` (docs/usb-hotplug-plan.md
//! D). The rules for who may eject what, and when an eject is overdue, are
//! `mounttable::volumes`, host-tested; this file asks the kernel
//! (`sys::fuse::{providers, stop, holders}`) and words the answer.
//!
//! An eject is a request to the volume's `fatfuse`, not a kill: the daemon
//! writes everything back, drops the filesystem (which clears the volume's
//! dirty flag), flushes the device and unmounts itself. Nothing new reaches
//! the volume once the kernel has accepted the stop, so what the daemon
//! finishes is final. `mountd` learns the outcome from the daemon's exit
//! status (0 after an eject is `ejected`).
//!
//! Serial lines: `MOUNTD:VOLUME EJECT usb<n> ...`, `MOUNTD:VOLUME BUSY
//! usb<n>` (the refusal, with the first holder).

use alloc::string::String;
use alloc::vec::Vec;
use user::trace::{self, Channel};

use mounttable::volumes::{disk_name, EjectError, Outcome, State, Volume};
use user::messenger::mount::wire::VolumeInfo;
use user::messenger::{errno, Error};
use user::sys::{self, fuse};

use crate::volumes::Automount;

/// What an eject answered.
pub(crate) struct Ejection {
    pub state: &'static str,
    pub detail: String,
}

fn answer(state: &'static str, detail: &str) -> Ejection {
    Ejection {
        state,
        detail: String::from(detail),
    }
}

impl Automount {
    /// Eject the volume on `disk` for `uid` (`force`: even with files open).
    pub(crate) fn eject(&mut self, disk: u32, uid: u32, force: bool) -> Result<Ejection, Error> {
        let (generation, pid) = match self.volumes.eject_target(disk, uid) {
            Ok(target) => target,
            Err(EjectError::Unknown) => return Err(Error::Errno(-errno::ENOENT)),
            Err(EjectError::NotYours) => return Err(Error::Errno(-errno::EPERM)),
            Err(EjectError::Ejecting) => return Ok(answer("ejecting", "")),
            Err(EjectError::Ejected) => return Ok(answer("ejected", "")),
            Err(EjectError::Ended) => {
                let (state, detail) = self.describe_disk(disk);
                return Ok(Ejection { state, detail });
            }
        };
        // Its daemon is not running yet (the next look at the disks starts
        // it): there is nothing to finish, ask again in a moment.
        let Some(pid) = pid else {
            return Err(Error::Errno(-errno::EAGAIN));
        };
        let provider = fuse::providers()
            .map_err(|code| Error::Errno(-code))?
            .into_iter()
            .find(|p| p.owner == pid);
        let Some(provider) = provider else {
            // No volume mounted yet (or none on this stick): the daemon has
            // nothing to flush, and was only reading.
            let _ = sys::kill(pid, sys::SIG_KILL);
            self.volumes.ejected_unmounted(disk, generation);
            trace::line!(
                Channel::Mountd,
                "VOLUME EJECT {} nothing mounted",
                disk_name(disk)
            );
            return Ok(answer("ejected", "nothing was mounted"));
        };
        match fuse::stop(provider.id, provider.owner, force) {
            Ok(()) => {
                self.volumes.ejecting(disk, generation, sys::clock());
                trace::line!(
                    Channel::Mountd,
                    "VOLUME EJECT {} /media/{} force={force}",
                    disk_name(disk),
                    provider.name
                );
                Ok(answer("ejecting", ""))
            }
            Err(sys::EBUSY) => {
                let holders = fuse::holders(provider.id, provider.owner).unwrap_or_default();
                trace::line!(
                    Channel::Mountd,
                    "VOLUME BUSY {} {}",
                    disk_name(disk),
                    holders.lines().next().unwrap_or("")
                );
                Ok(Ejection {
                    state: "busy",
                    detail: holders,
                })
            }
            // The daemon went away between the listing and the stop: its
            // exit is on its way.
            Err(errno::ESRCH) => Ok(answer("ejecting", "")),
            Err(code) => Err(Error::Errno(-code)),
        }
    }

    /// The caller's volumes (every one for root), in plug order.
    pub(crate) fn listing(&self, uid: u32) -> Vec<VolumeInfo> {
        self.volumes
            .entries()
            .iter()
            .filter(|v| uid == 0 || v.owner.0 == uid)
            .filter(|v| !matches!(v.state, State::Leaving { .. }))
            .map(info)
            .collect()
    }

    fn describe_disk(&self, disk: u32) -> (&'static str, String) {
        self.volumes
            .entries()
            .iter()
            .rev()
            .find(|v| v.disk == disk)
            .map_or(("failed", String::new()), describe)
    }
}

fn info(volume: &Volume) -> VolumeInfo {
    let (state, detail) = describe(volume);
    let name = volume.name.clone().unwrap_or_default();
    VolumeInfo {
        disk: disk_name(volume.disk),
        path: if name.is_empty() {
            String::new()
        } else {
            alloc::format!("{}/{name}", fhs::mount::MEDIA)
        },
        name,
        state: String::from(state),
        detail,
        owner: volume.owner.0,
        writable: volume.writable,
    }
}

/// The state word and its reason, for a volume.
fn describe(volume: &Volume) -> (&'static str, String) {
    let none = String::new;
    match &volume.state {
        State::Starting => ("starting", none()),
        State::Running if volume.name.is_some() => ("mounted", none()),
        State::Running => ("starting", none()),
        State::Ejecting { .. } => ("ejecting", none()),
        State::Leaving { .. } => ("ejecting", none()),
        State::Ended(Outcome::Ejected) => ("ejected", none()),
        State::Ended(Outcome::Left) => ("ejecting", none()),
        State::Ended(Outcome::NoVolume) => {
            ("unsupported", String::from("no FAT volume on the disk"))
        }
        State::Ended(Outcome::Removed) => (
            "removed",
            String::from("the stick was pulled while it was mounted"),
        ),
        State::Ended(Outcome::Stopped) => ("failed", String::from("the volume was unmounted")),
        State::Ended(Outcome::Failed(why)) => ("failed", why.clone()),
    }
}
