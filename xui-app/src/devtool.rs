//! One look and one behaviour for every developer tool
//! (`docs/devtool-packages-plan.md` section 5; the texts are `libs/devtool`'s,
//! so the Installer's consent screen and these windows never drift).
//!
//! A tool wires itself in four places:
//!
//! 1. **Title.** Start with [`run`] (or `launch::run(channel, &title(NAME), ..)`)
//!    so the window is called "Name (developer tool)".
//! 2. **Strip.** Put [`strip()`] under the menu bar, bound to a
//!    `Handle<Strip<Msg>>` and given the tool's shared [`Notice`]
//!    (`Notice::for_tool(SYSTEM_NAME)`) and the message its "x" raises; on
//!    that message call `notice.dismiss()` and `strip.sync()`, and give the
//!    View menu a "Show developer-tool notice" row that calls
//!    `notice.restore()`. The choice is remembered per user and per tool
//!    (`$HOME/.apps/<system_name>/devtool-notice-dismissed`); it hides the band
//!    only (the title suffix and Help stay). After the first layout call
//!    `handle.get().probe(ui, &title)` so sessions find `devtool:strip` and
//!    `devtool:strip_dismiss` (neither while the strip is hidden).
//! 3. **Help.** Build a [`HelpPage`] (what the tool does about each shared
//!    risk) and show [`help_view`] in the Help window or tab.
//! 4. **Review.** Keep a [`ReviewGate`] beside the editor; before Run, if
//!    [`ReviewGate::needs_confirmation`], open [`review_dialog`] and Run only
//!    on [`ReviewAnswer::Run`].
//!
//! [`run`] also prints `<CH>:DEVTOOL:UP` once the window is built, through
//! [`crate::trace`] on the tool's own channel. It adds to the app's `UP` and
//! `QUIT` markers; it never replaces them.

mod help;
mod notice;
mod review;
mod strip;

use std::rc::Rc;

use xui_core::app::Ui;

use crate::backend::LazyOSBackend;
use crate::trace::{self, Channel};

pub use help::{help_layout, help_view, HelpPage, HelpRow, RiskKind, NO_SAFEGUARD};
pub use review::{
    answer_of, review_dialog, review_message, Provenance, ReviewAnswer, ReviewGate,
};
pub use notice::{marker_path, FileStore, Notice, Store as NoticeStore, MARKER as NOTICE_MARKER};
pub use strip::{
    strip, Strip, DISMISS_LABEL as STRIP_DISMISS_LABEL, DISMISS_PROBE_NAME as STRIP_DISMISS_PROBE,
    PROBE_NAME as STRIP_PROBE,
};
// The shared texts, so a tool reaches them through this module even where
// `devtool` names this module and not the `libs/devtool` crate.
pub use ::devtool::{Risk, CONSENT_TEXT, REVIEW_PROMPT, STRIP_TEXT, TITLE_SUFFIX};

/// The risks every developer tool states (`libs/devtool`'s table), for a
/// screen that draws them itself.
pub fn risks() -> &'static [Risk] {
    ::devtool::RISKS
}

/// The text of the evidence line after the channel prefix: `<CH>:DEVTOOL:UP`.
const ANNOUNCE: &str = "DEVTOOL:UP";

/// The window title of the tool called `name`: `"Name (developer tool)"`.
/// Idempotent, so a name that already carries the suffix is not doubled.
pub fn title(name: &str) -> String {
    if name.ends_with(devtool::TITLE_SUFFIX) {
        name.to_owned()
    } else {
        format!("{name}{}", devtool::TITLE_SUFFIX)
    }
}

/// Prints `<CH>:DEVTOOL:UP`: this process is a developer tool. [`run`] calls
/// it; a tool that starts its window another way calls it itself once the
/// window is up.
pub fn announce(channel: Channel) {
    trace::line!(channel, "{ANNOUNCE}");
}

/// `launch::run` for a developer tool: the window is titled [`title`]`(name)`
/// and `<CH>:DEVTOOL:UP` is printed as the app is built, before `make` runs.
/// Failures are reported as `launch::run` reports them.
pub fn run<A, F>(channel: Channel, name: &str, windowed: (i32, i32), make: F) -> !
where
    A: xui_core::app::App,
    F: FnOnce(&mut Ui<A::Msg>, &Rc<LazyOSBackend>) -> xui_core::backend::Result<A>,
{
    crate::launch::run(channel, &title(name), windowed, move |ui, backend| {
        announce(channel);
        make(ui, backend)
    })
}

#[cfg(test)]
mod snapshot_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_gets_the_suffix_once() {
        assert_eq!(title("Messenger Playground"), "Messenger Playground (developer tool)");
        assert_eq!(
            title("Messenger Playground (developer tool)"),
            "Messenger Playground (developer tool)"
        );
        assert_eq!(title(""), " (developer tool)");
    }

    #[test]
    fn the_evidence_line_is_the_channel_then_devtool_up() {
        let mut line = tracefmt::LineBuf::<{ tracefmt::LINE_MAX }>::new();
        let _ = tracefmt::write::line(&mut line, Channel::Ui, format_args!("{ANNOUNCE}"));
        assert_eq!(line.finish(), b"UI:DEVTOOL:UP\n");
    }
}
