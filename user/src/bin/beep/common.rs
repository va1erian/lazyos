//! Helpers shared by `beep`'s modes: connecting to the mixer, sleeping, and
//! turning audio failures into log text.

use alloc::format;
use alloc::string::String;

use audioclient::Error;
use user::audio::{self, Native};
use user::sys;

/// Ticks (100 Hz) to wait for the mixer to register before giving up.
const CONNECT_TICKS: u64 = 500;

/// The PIT tick counter.
pub(super) fn now() -> u64 {
    sys::clock()
}

/// Prefix an audio failure with what was being attempted.
pub(super) fn fail(what: &str) -> impl Fn(Error) -> String + '_ {
    move |error| format!("{what}: {error}")
}

/// Resolve the mixer (`os.lazy.audio`), retrying while it is still starting.
pub(super) fn connect() -> Result<Native, String> {
    audio::connect_wait(audio::NAME, CONNECT_TICKS)
        .map_err(|error| format!("no audio service: {error}"))
}

/// Whether `result` failed with exactly `errno` (a positive errno value).
pub(super) fn is_errno<T>(result: &Result<T, Error>, errno: i64) -> bool {
    matches!(result, Err(Error::Errno(code)) if *code == errno)
}
