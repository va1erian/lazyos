//! The developer-tool notice's two user actions, shared by both windows: the
//! strip's "x" and View > Show developer-tool notice
//! ([`xui_app::devtool::Notice`], remembered per user and per tool). Each
//! prints its evidence line and returns what the status bar should say when
//! the choice could not be saved or forgotten.

use xui_app::devtool::Notice;
use xui_app::trace::{self, Channel};

/// The tool's system name: the folder under `$HOME/.apps` that holds the choice.
pub const SYSTEM_NAME: &str = "os.lazy.playground";

/// Hide the strip: `PLAYGROUND:NOTICE:HIDDEN`.
pub fn dismiss(notice: &Notice) -> Option<String> {
    let outcome = notice.dismiss();
    trace::line!(Channel::Playground, "NOTICE:HIDDEN");
    outcome
        .err()
        .map(|e| format!("The notice is hidden for now; the choice was not saved: {e}"))
}

/// Show the strip again and forget the choice: `PLAYGROUND:NOTICE:SHOWN`.
pub fn show(notice: &Notice) -> Option<String> {
    let outcome = notice.restore();
    trace::line!(Channel::Playground, "NOTICE:SHOWN");
    outcome
        .err()
        .map(|e| format!("The notice is shown; the stored choice could not be cleared: {e}"))
}

/// A tool that starts with the notice already hidden says so once
/// (`PLAYGROUND:NOTICE:HIDDEN:stored`).
pub fn announce_stored(notice: &Notice) {
    if !notice.visible() {
        trace::line!(Channel::Playground, "NOTICE:HIDDEN:stored");
    }
}
