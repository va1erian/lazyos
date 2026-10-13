//! Start-up mode selection for apps that run either as the session's display
//! owner or as a `xuid` client (issue #215).
//!
//! `counter`, `sysmon` and `fabricmon` were written as display owners (the
//! M0-M2 milestones and the CI viewer sessions drive them that way). In a
//! `xuid` desktop session the compositor already holds the display grant, so
//! the same binaries must take the client path instead.

use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::backend::Backend;

use crate::backend::LazyOSBackend;
use crate::trace::{self, Channel};

/// The argument `init`'s app registry passes to a desktop app.
pub const CLIENT_ARG: &str = "--client";

impl LazyOSBackend {
    /// Choose the mode at start-up.
    ///
    /// An explicit [`CLIENT_ARG`] always means client mode, so an app launched
    /// before `xuid` has bound the display cannot race it for the grant.
    /// Otherwise the app tries to own the display and falls back to client
    /// mode when the grant is already held (by `xuid`).
    pub fn connect() -> Result<LazyOSBackend, i64> {
        if std::env::args().any(|arg| arg == CLIENT_ARG) {
            return Self::new_client();
        }
        Self::new().or_else(|_| Self::new_client())
    }

    /// The window size an app should ask for, in design pixels (the app
    /// wraps it in `Dip`): the whole screen when it owns the display,
    /// `windowed` (its preferred size) when a compositor lays it out. A client
    /// surface's size is only known once its window opens.
    pub fn window_size(&self, windowed: (i32, i32)) -> (i32, i32) {
        if self.is_client() {
            windowed
        } else {
            let (width, height) = self.screen();
            let scale = self.scale() as i32;
            (width / scale, height / scale)
        }
    }
}

/// Runs a desktop app: connects (owner or client mode), opens a window titled
/// `title` at `windowed` design pixels (the whole screen as display owner) in
/// the desktop's theme, builds the app with `make`, runs it, releases the
/// display and exits the process.
///
/// Failures are reported on the serial console as `<channel>:BIND:FAIL:<errno>`
/// (no display) and `<channel>:RUN:FAIL:<error>` (the window or a widget could
/// not be built), through [`crate::trace`]; the process exits 0 after a clean
/// quit, 1 otherwise. `make` gets the backend for app-specific evidence
/// (`on_first_frame`) and requests (`request_size`).
pub fn run<A, F>(channel: Channel, title: &str, windowed: (i32, i32), make: F) -> !
where
    A: App,
    F: FnOnce(&mut Ui<A::Msg>, &Rc<LazyOSBackend>) -> xui_core::backend::Result<A>,
{
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            trace::fail!(channel, "BIND", "{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(windowed);
    let outcome = xui_core::app(title)
        .size(width, height)
        .backend(Rc::clone(&backend) as Rc<dyn Backend>)
        .run(|ui| make(ui, &backend));
    backend.unbind();
    std::process::exit(i32::from(finish(channel, outcome)))
}

/// The process exit status for an app's `run` outcome, reporting a failure as
/// `<channel>:RUN:FAIL:<error>`.
///
/// A window closed before its first attach (issue #498: the user, or monkey
/// input, dismissed it while it opened) is not a failure: it prints
/// `<channel>:RUN:CLOSED` and exits 0, so `init` does not mark the app
/// degraded.
pub fn finish<E: std::fmt::Display>(channel: Channel, outcome: Result<(), E>) -> u8 {
    match outcome {
        Ok(()) => 0,
        // The flag is process-wide, so the error must also be the close the
        // backend reported (its text), not an unrelated failure of the app.
        Err(error)
            if crate::client_window::closed_while_opening()
                && error
                    .to_string()
                    .contains(&crate::client_window::OpenError::Closed.to_string()) =>
        {
            trace::line!(channel, "RUN:CLOSED");
            0
        }
        Err(error) => {
            trace::fail!(channel, "RUN", "{error}");
            1
        }
    }
}
