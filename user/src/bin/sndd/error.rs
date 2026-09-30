//! `sndd`'s error type: every failure names the layer it came from, so a serial
//! log line is enough to tell a missing device from a device that misbehaved.

use alloc::format;
use alloc::string::String;

#[derive(Debug)]
pub(super) enum Error {
    /// No virtio-sound function is present.
    NoDevice,
    /// The device syscall failed with this errno.
    Dev(i64),
    /// The virtio transport or a queue failed.
    Virtio(virtio::Error),
    /// The device answered a request with this status word.
    Status(u32),
    /// The device did not answer in time.
    Timeout,
    /// The device offered nothing usable (no playback stream).
    NoStream,
    /// A request or reply did not fit its buffer.
    Range,
    /// The request is malformed (unknown format, zero period, ...).
    Params,
    /// The stream cannot do anything close to the request.
    Unsupported,
    /// The card's stream is already in use.
    Busy,
    /// The Messenger fabric failed (registering the service, receiving).
    Messenger(&'static str),
}

impl From<virtio::Error> for Error {
    fn from(error: virtio::Error) -> Error {
        Error::Virtio(error)
    }
}

impl Error {
    pub(super) fn describe(&self) -> String {
        match self {
            Error::NoDevice => "no virtio-sound device".into(),
            Error::Dev(errno) => format!("device syscall failed (errno {errno})"),
            Error::Virtio(error) => format!("virtio: {error:?}"),
            Error::Status(status) => format!("device replied status {status:#x}"),
            Error::Timeout => "device timed out".into(),
            Error::NoStream => "no playback stream".into(),
            Error::Range => "buffer range".into(),
            Error::Params => "stream parameters".into(),
            Error::Unsupported => "unsupported stream".into(),
            Error::Busy => "stream busy".into(),
            Error::Messenger(text) => format!("messenger: {text}"),
        }
    }
}
