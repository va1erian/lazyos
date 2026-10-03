//! The desktop's widget theme, for the IDE's "System" theme (issue #542).
//!
//! The platform is installed before the IDE connects to the compositor, so
//! the theme is recorded here once the backend can ask (`GetTheme`), and
//! [`LazyOsPlatform`](crate::platform::LazyOsPlatform) answers
//! `prefers_dark` and `system_theme` from it.

use std::sync::Mutex;

use xui_core::Theme;

static THEME: Mutex<Option<Theme>> = Mutex::new(None);

/// Records the desktop's theme (`None`: no compositor answered).
pub fn set_theme(theme: Option<Theme>) {
    if let Ok(mut slot) = THEME.lock() {
        *slot = theme;
    }
}

/// The desktop's theme, once [`set_theme`] recorded one.
pub fn theme() -> Option<Theme> {
    THEME.lock().ok().and_then(|slot| *slot)
}

/// Whether the desktop is in dark mode; light until a theme is recorded.
pub fn is_dark() -> bool {
    theme().is_some_and(|theme| theme.is_dark)
}
