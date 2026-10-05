//! The pointer's shape, for apps (docs/cursor-plan.md).
//!
//! Widgets already choose shapes themselves (`Ui::set_cursor`: a hyperlink the
//! hand, an edit the I-beam), and `LazyOSBackend` passes them on to the
//! compositor, which needs the app's package to ask for
//! `os.lazy.display.cursor.v1`. What an app adds here is the busy pointer
//! while it works.

use xui_core::backend::WindowId;

use crate::backend::LazyOSBackend;

/// Show the busy pointer over all of `window` while `busy`, then give the
/// widgets' own shapes back. Cheap to call repeatedly.
pub fn set_busy(backend: &LazyOSBackend, window: WindowId, busy: bool) {
    backend.set_busy(window, busy);
}
