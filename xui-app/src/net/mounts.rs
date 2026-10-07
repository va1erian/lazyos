//! Typed client of the network mount service `mountd` (`os.lazy.mount.v1`,
//! `idl/mount.midl`) for the Network Drives app, and the hand-off of a
//! mounted folder to Files (`init.Launch`).
//!
//! Every call runs on the UI thread, so each is bounded by [`CALL_TICKS`]:
//! `mountd` answers at once and does the slow part (logging in) in the
//! daemon it starts. A missing `mountd` (an image without the network
//! stack) is [`Error::NotRunning`], never a hang.

use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_mount_v1 as wire;
use mounttable::Request;

use crate::platform::messenger::Service;

/// `mountd`'s registered name.
const NAME: &str = "os.lazy.mount";
/// `init`'s registered name.
const INIT: &str = "os.lazy.init";
/// The app that opens a folder.
const FILES_APP: &str = "os.lazy.files";
/// The longest one call may wait (PIT ticks, 100 Hz).
const CALL_TICKS: u64 = 200;
const EINVAL: i64 = 22;
const EPIPE: i64 = 32;

pub use wire::MountInfo;

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

/// One call on `service`; a restarted service (`EPIPE`, the cached endpoint
/// is dead) is retried once.
fn call(
    service: &'static str,
    interface: u64,
    method: u32,
    body: Vec<u8>,
) -> Result<Vec<u8>, Error> {
    let once = |body: Vec<u8>| {
        let endpoint = Service::try_connect(service).ok_or(Error::NotRunning)?;
        endpoint
            .call_within(interface, method, ERROR_FIELD, body, CALL_TICKS)
            .map(|reply| reply.body)
            .map_err(Error::Code)
    };
    match once(body.clone()) {
        Err(Error::Code(code)) if -code == EPIPE => once(body),
        other => other,
    }
}

fn decoded<T, E>(result: Result<T, E>) -> Result<T, Error> {
    result.map_err(|_| Error::Code(-EINVAL))
}

/// Every mount `mountd` knows about.
pub fn list() -> Result<Vec<MountInfo>, Error> {
    let body = call(NAME, wire::INTERFACE_ID, wire::METHOD_LIST, Vec::new())?;
    Ok(decoded(wire::decode_list_reply(&body))?.mounts)
}

/// Ask for a mount; the answer is the folder it will appear at.
pub fn mount(request: &Request) -> Result<String, Error> {
    let body = decoded(wire::encode_mount_args(&wire::MountArgs {
        name: request.name.clone(),
        host: request.host.clone(),
        port: u32::from(request.port),
        user: request.user.clone(),
        password: request.password.clone(),
    }))?;
    let body = call(NAME, wire::INTERFACE_ID, wire::METHOD_MOUNT, body)?;
    Ok(decoded(wire::decode_mount_reply(&body))?.path)
}

/// Stop and forget the mount `name`.
pub fn unmount(name: &str) -> Result<(), Error> {
    let body = decoded(wire::encode_unmount_args(&wire::UnmountArgs {
        name: name.to_owned(),
    }))?;
    call(NAME, wire::INTERFACE_ID, wire::METHOD_UNMOUNT, body).map(drop)
}

/// Open the folder `path` in Files, in this session.
pub fn open_in_files(path: &str) -> Result<(), Error> {
    let body = decoded(init_wire::encode_launch_args(&init_wire::LaunchArgs {
        app: FILES_APP.to_owned(),
        args: path.to_owned(),
        session: 0,
    }))?;
    call(INIT, init_wire::INTERFACE_ID, init_wire::METHOD_LAUNCH, body).map(drop)
}
