//! The desktop's light/dark mode, for the IDE's "System" theme (issue #542).
//!
//! The platform is installed before the IDE connects to the compositor, so
//! the mode is recorded here once the backend can ask (`GetTheme`), and
//! [`LazyOsPlatform::prefers_dark`](crate::platform::LazyOsPlatform) reads it.

use std::sync::atomic::{AtomicBool, Ordering};

static DARK: AtomicBool = AtomicBool::new(false);

/// Records whether the desktop is in dark mode.
pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

/// Whether the desktop is in dark mode; light until [`set_dark`] says so.
pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}
