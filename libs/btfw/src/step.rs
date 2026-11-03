//! The vocabulary `btd` and a loader share: what goes in, what comes out.
//!
//! A loader is a pure step function in the style of `bthci::Init`: it owns no
//! file, socket or clock. The caller sends the command a step returns,
//! feeds back the events the controller answers with, reads the firmware file
//! a step names (through `libs/fwload`) and hands the bytes in, and passes the
//! time with every call so the loader's own timeouts need no timer thread.

use alloc::string::String;
use alloc::vec::Vec;

/// One input to [`crate::Loader::step`].
#[derive(Debug)]
pub enum Input<'a> {
    /// The controller is up (HCI Reset has completed): begin.
    Start,
    /// One HCI event packet exactly as the transport delivered it.
    Event(&'a [u8]),
    /// The bytes of the file the last [`Step::NeedFile`] named.
    File(Vec<u8>),
    /// That file does not exist or could not be read.
    NoFile,
    /// Time passed with nothing to report (see [`crate::Loader::deadline`]).
    Tick,
    /// The USB device went away (unplugged, or the controller reset itself).
    Detached,
}

/// What the caller does next.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Send this HCI command packet, then feed the answering events back.
    Send(Vec<u8>),
    /// Read this file and answer with [`Input::File`] or [`Input::NoFile`].
    NeedFile(FileRequest),
    /// Nothing to do until an event, a tick at the deadline, or a detach.
    Wait,
    Done(Done),
    Failed(Failure),
}

/// A firmware file the loader needs, named for `libs/fwload` (relative to the
/// firmware tree, `intel/ibt-0041-0041.sfi`).
#[derive(Debug, PartialEq, Eq)]
pub struct FileRequest {
    pub name: String,
    /// A missing required file fails the load; a missing optional one is
    /// answered with [`Input::NoFile`] and the load goes on without it.
    pub required: bool,
}

/// The load finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    /// This controller needs no firmware from us: carry on with `bthci::Init`.
    NoFirmwareNeeded,
    /// The controller runs its operational firmware: carry on with
    /// `bthci::Init`. `downloaded` is false when it already ran it.
    Operational { downloaded: bool, config: Config },
    /// The controller reset itself and is coming back as a new USB device
    /// (the old one is gone): `usbd` publishes a fresh interface, and the
    /// caller starts again from HCI Reset when it appears.
    Reenumerate,
}

/// What became of the optional device configuration file (Intel `.ddc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Config {
    NotUsed,
    Missing,
    /// This many records were written.
    Applied(u16),
    /// The file was malformed or the controller refused a record; the
    /// controller works without it.
    Failed,
}

/// The load failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    pub reason: Reason,
    /// A recovery reset was sent: the controller will drop off the bus and
    /// come back in its bootloader, and one more attempt may succeed.
    pub reenumerate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The controller refused a command with this HCI status.
    CommandFailed { opcode: u16, status: u8 },
    /// No answer in time to this command (or `0` for an awaited event).
    Timeout { opcode: u16 },
    /// An answer too short or inconsistent to use.
    BadResponse { opcode: u16 },
    /// A controller (or mode) this crate does not load.
    Unsupported(&'static str),
    /// A required firmware file is missing.
    FileMissing,
    /// A firmware file that does not parse; the text says where.
    BadFirmware(&'static str),
    /// The Intel bootloader reported a failed download (its result byte).
    DownloadRejected { result: u8 },
    /// The USB device went away when it should not have.
    Detached,
}

pub(crate) fn failed(reason: Reason) -> Step {
    Step::Failed(Failure {
        reason,
        reenumerate: false,
    })
}
