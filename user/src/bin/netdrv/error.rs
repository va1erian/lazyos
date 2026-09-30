//! `netdrv`'s error type: every failure names the layer it came from, so a
//! serial log line is enough to tell a missing device from a device that
//! misbehaved.

use alloc::format;
use alloc::string::String;

#[derive(Debug)]
pub(super) enum Error {
    /// No virtio-net function is present.
    NoDevice,
    /// The device syscall failed with this errno.
    Dev(i64),
    /// The virtio transport or a queue failed.
    Virtio(virtio::Error),
    /// The driver core gave up: the device broke the virtio contract.
    Fatal(nicdrv::Fatal),
    /// A structure lies outside the BAR the kernel reported.
    Range,
    /// The device offers no usable MAC address (absent, multicast or zero).
    NoMac,
    /// The Messenger fabric failed (registering the service, receiving).
    Messenger(&'static str),
    /// The self-test did not see its ARP reply.
    SelfTest(&'static str),
}

impl From<virtio::Error> for Error {
    fn from(error: virtio::Error) -> Error {
        Error::Virtio(error)
    }
}

impl From<nicdrv::Fatal> for Error {
    fn from(error: nicdrv::Fatal) -> Error {
        Error::Fatal(error)
    }
}

impl Error {
    pub(super) fn describe(&self) -> String {
        match self {
            Error::NoDevice => "no virtio-net device".into(),
            Error::Dev(errno) => format!("device syscall failed (errno {errno})"),
            Error::Virtio(error) => format!("virtio: {error:?}"),
            Error::Fatal(error) => format!("the device broke the virtio contract: {error:?}"),
            Error::Range => "structure outside its BAR".into(),
            Error::NoMac => "the device has no usable MAC address".into(),
            Error::Messenger(text) => format!("messenger: {text}"),
            Error::SelfTest(text) => format!("self-test: {text}"),
        }
    }
}
