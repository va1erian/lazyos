//! Helpers shared by `beep`'s modes: connecting to the driver, sleeping, and
//! turning Messenger failures into log text.

use alloc::format;
use alloc::string::String;

use user::messenger::audio as api;
use user::messenger::Error as MsgError;
use user::sys;

/// Ticks (100 Hz) to wait for the driver to register before giving up.
const CONNECT_TICKS: u64 = 500;

/// The PIT tick counter.
pub(super) fn now() -> u64 {
    sys::clock()
}

/// Sleep one PIT tick (`wait` doubles as a timer when there is no child).
pub(super) fn nap() {
    let _ = sys::wait(sys::clock() + 1);
}

/// Prefix a Messenger failure with what was being attempted.
pub(super) fn fail(what: &str) -> impl Fn(MsgError) -> String + '_ {
    move |error| format!("{what}: {}", error.message())
}

/// Resolve the driver, retrying while it is still starting up.
pub(super) fn connect() -> Result<api::Client, String> {
    let deadline = sys::clock() + CONNECT_TICKS;
    loop {
        match api::Client::connect() {
            Ok(client) => return Ok(client),
            Err(error) if sys::clock() >= deadline => {
                return Err(format!("no audio service: {}", error.message()))
            }
            Err(_) => nap(),
        }
    }
}

/// Whether `result` failed with exactly `errno` (a positive errno value).
pub(super) fn is_errno<T>(result: &Result<T, MsgError>, errno: i64) -> bool {
    matches!(result, Err(MsgError::Errno(code)) if *code == -errno)
}
