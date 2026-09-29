//! Start-up mode selection for apps that run either as the session's display
//! owner or as a `xuid` client (issue #215).
//!
//! `counter`, `sysmon` and `fabricmon` were written as display owners (the
//! M0-M2 milestones and the CI viewer sessions drive them that way). In a
//! `xuid` desktop session the compositor already holds the display grant, so
//! the same binaries must take the client path instead.

use crate::backend::LazyOSBackend;

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

    /// The window size an app should ask for: the whole screen when it owns
    /// the display, `windowed` (its preferred size) when a compositor lays it
    /// out. A client surface's size is only known once its window opens.
    pub fn window_size(&self, windowed: (i32, i32)) -> (i32, i32) {
        if self.is_client() {
            windowed
        } else {
            self.screen()
        }
    }
}
