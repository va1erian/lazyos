//! The `dragdemo` denial-probe role (issue #194 split): a short-lived child of
//! the target that enters another clipboard session and asks for the dropped
//! token, proving the compositor's drag delivery cannot bypass the clipboard's
//! session scope.
//!
//! Split out of `dragdemo.rs`; the code is unchanged.

use alloc::format;
use user::messenger::{errno, Error};
use user::sys;

use super::app::connect_clipboard;
use super::PROBE_SESSION;

/// The probe child: resolve `clipboardd`, enter another session and ask for the
/// dropped token; the service must refuse it with `-EACCES`.
pub(super) fn probe(token: u64, mime: &str) -> ! {
    let Some(client) = connect_clipboard() else {
        sys::write_str("DND:DENIED:FAIL:no clipboardd\n");
        sys::exit(1);
    };
    let probe = sys::Cred::new(1000, 1000, 0, 0, PROBE_SESSION);
    if sys::cred_set(None, &probe).is_err() {
        sys::write_str("DND:DENIED:FAIL:could not enter the probe session\n");
        sys::exit(1);
    }
    match client.paste_token(token, mime) {
        Err(Error::Errno(code)) if code == -errno::EACCES => {
            sys::write_str("DND:DENIED:PASS\n");
            sys::exit(0)
        }
        Ok(_) => sys::write_str("DND:DENIED:FAIL:cross-session paste was allowed\n"),
        Err(error) => sys::write_str(&format!("DND:DENIED:FAIL:{}\n", error.message())),
    }
    sys::exit(1)
}
