//! Typed client of the network mount service `mountd` (`os.lazy.mount.v1`,
//! `idl/mount.midl`) for the Network Drives app, and the hand-off of a
//! mounted folder to Files (`init.Launch`).
//!
//! Every call runs on the UI thread, so each waits only what `idl/mount.midl`
//! states: `mountd` answers at once and does the slow part (logging in) in the
//! daemon it starts. A missing `mountd` (an image without the network
//! stack) is [`Error::NotRunning`], never a hang.

use std::rc::Rc;

use libmessenger::client::{CallError, Deadline};
use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_mount_v1 as wire;
use mounttable::Request;

use crate::platform::messenger::{client, Observed};

/// The app that opens a folder.
const FILES_APP: &str = "os.lazy.files";
/// The longest `init.Launch` may wait here (PIT ticks, 100 Hz).
const LAUNCH_TICKS: u64 = 200;

pub use wire::{MountInfo, VolumeInfo};

/// Why a call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `mountd` (or `init`) is not registered.
    NotRunning,
    /// The call failed with this negative errno.
    Code(i64),
}

impl Error {
    /// The failure in words, for the message line.
    pub fn describe(self) -> String {
        match self {
            Error::NotRunning => String::from(
                "The mount service is not running (it comes with networking: run_demo.py --net).",
            ),
            Error::Code(code) => super::drives::refusal(code),
        }
    }
}

/// `mountd`'s client. A call is resent once only when the cached endpoint was
/// dead (`EPIPE`, the service restarted), which resolves afresh.
fn mountd() -> Result<Rc<wire::Client<Observed>>, Error> {
    client(wire::Client::try_connect).map_err(|_| Error::NotRunning)
}

fn failed(error: CallError) -> Error {
    Error::Code(error.code())
}

/// Every mount `mountd` knows about.
pub fn list() -> Result<Vec<MountInfo>, Error> {
    mountd()?.list().map_err(failed)
}

/// Ask for a mount; the answer is the folder it will appear at.
pub fn mount(request: &Request) -> Result<String, Error> {
    mountd()?
        .mount(
            &request.name,
            &request.host,
            u32::from(request.port),
            &request.user,
            &request.password,
            request.kind.name(),
            &request.share,
        )
        .map_err(failed)
}

/// Stop and forget the mount `name`.
pub fn unmount(name: &str) -> Result<(), Error> {
    mountd()?.unmount(name).map_err(failed)
}

/// Open the folder `path` in Files, in this session.
pub fn open_in_files(path: &str) -> Result<(), Error> {
    let init = client(init_wire::Client::try_connect).map_err(|_| Error::NotRunning)?;
    init.launch_within(FILES_APP, path, 0, Deadline::ticks(LAUNCH_TICKS))
        .map(drop)
        .map_err(failed)
}

/// The caller's removable volumes (the Removable Drives applet's list).
pub fn volumes() -> Result<Vec<VolumeInfo>, Error> {
    mountd()?.volumes().map_err(failed)
}

/// Eject the volume on `disk` (`usb<n>`): `(state, detail)` as `mountd`
/// answers (`ejecting`, `ejected`, or `busy` with the holders). An eject in
/// flight is answered `ejecting` the second time, which is the same outcome.
pub fn eject(disk: &str, force: bool) -> Result<(String, String), Error> {
    let reply = mountd()?.eject(disk, force).map_err(failed)?;
    Ok((reply.state, reply.detail))
}
