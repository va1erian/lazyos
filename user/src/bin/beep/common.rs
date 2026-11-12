//! Helpers shared by `beep`'s modes: connecting to the mixer, sleeping, and
//! turning audio failures into log text.

use alloc::format;
use alloc::string::String;

use core::fmt::Display;

use audioclient::{CallError, Client};
use user::audio;
use user::messenger::client::Native;
use user::sys;

/// The PIT tick counter.
pub(super) fn now() -> u64 {
    sys::clock()
}

/// Prefix an audio failure with what was being attempted.
pub(super) fn fail<E: Display>(what: &str) -> impl Fn(E) -> String + '_ {
    move |error| format!("{what}: {error}")
}

/// Resolve the mixer (`os.lazy.audio`), retrying while it is still starting.
pub(super) fn connect() -> Result<Client<Native>, String> {
    audio::connect_wait(audio::NAME).map_err(|error| format!("no audio service: {error}"))
}

/// Whether `result` failed with exactly `errno` (a positive errno value).
pub(super) fn is_errno<T>(result: &Result<T, CallError>, errno: i64) -> bool {
    matches!(result, Err(error) if error.code() == -errno)
}
