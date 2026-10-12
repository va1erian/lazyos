//! The crate's one error type. Each variant names the step that failed so
//! `wifid` can print it as evidence (`WIFID:FAIL:<step>`), the way
//! `rtl8168::SetupError` does.

/// Why a step of bring-up or operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A register poll did not see the expected value in time; `addr` is the
    /// register polled.
    Timeout { step: &'static str, addr: u32 },
    /// The firmware image is malformed (bad signature, sizes that do not add
    /// up, a section past the end).
    BadFirmware(&'static str),
    /// The firmware did not report ready, or a checksum/download check failed.
    FirmwareDownload(&'static str),
    /// The efuse could not be read or its content is not usable.
    Efuse(&'static str),
    /// The chip revision or a hardware fact is one this driver does not support.
    Unsupported(&'static str),
    /// A caller's argument does not fit (a channel, a key length, a queue).
    Invalid(&'static str),
    /// A queue or ring has no room.
    Full(&'static str),
    /// DMA memory could not be had.
    NoMemory,
    /// The bus reported an error (the hci's own failure).
    Bus(&'static str),
}

/// Result alias used across the crate.
pub type Result<T> = core::result::Result<T, Error>;
