//! The "app stopped" notice (issues #549, #747): what the shell tells the
//! user when `init` gives up on an app they opened.
//!
//! `init` publishes one `system/events/app/<id>` event when a launched app
//! ends failed (it failed while starting, kept crashing, or failed with no
//! restart policy). The shell turns it into an Error toast of its own
//! ([`Failure::request`]): the title names the app, the body says what
//! happened and the reason the app reported (when it did), and the actions
//! are *Restart* and *Close*. It stays until dismissed: with no history, a
//! missed failure would otherwise be gone, and this is the one notice with
//! no tray item to fall back on.

use crate::notify::note::{Level, Note, Request, Timeout, BODY_MAX, TITLE_MAX};

/// The action ids of the notice's toast.
pub const ACTION_RESTART: u32 = 1;
pub const ACTION_CLOSE: u32 = 2;

/// What the shell heard: one app failure from `init`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Failure {
    /// The app id (`Launch` takes it again for *Restart*).
    pub app: String,
    /// Its display name.
    pub name: String,
    /// The status in words (`exit code 2`).
    pub summary: String,
    /// The app's own reason; empty when it gave none.
    pub reason: String,
    /// Whether it failed while starting.
    pub startup: bool,
}

impl Failure {
    /// The toast's title.
    pub fn title(&self) -> String {
        format!("{} stopped", self.display_name())
    }

    /// The sentence saying what happened.
    pub fn sentence(&self) -> String {
        if self.startup {
            format!(
                "It failed while starting ({}), so it was not started again.",
                self.summary
            )
        } else {
            format!(
                "It stopped with an error ({}) and was not restarted.",
                self.summary
            )
        }
    }

    /// The reason paragraph, when the app gave one.
    pub fn reason_text(&self) -> Option<String> {
        let reason = self.reason.trim();
        (!reason.is_empty()).then(|| format!("Reason: {reason}"))
    }

    /// The toast that tells the user: an Error with *Restart* and *Close*
    /// that stays until dismissed. Text over the toast's limits is cut with
    /// an ellipsis, so a long reason never makes the notice fail.
    pub fn request(&self) -> Request {
        let mut body = self.sentence();
        if let Some(reason) = self.reason_text() {
            body.push(' ');
            body.push_str(&reason);
        }
        let mut note = Note::new(
            Level::Error,
            &cut(&self.title(), TITLE_MAX),
            &cut(&body, BODY_MAX),
        )
        .with_actions(&[(ACTION_RESTART, "Restart"), (ACTION_CLOSE, "Close")]);
        note.timeout = Timeout::Sticky;
        Request {
            note,
            replaces: None,
        }
    }

    fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.app
        } else {
            self.name.trim()
        }
    }
}

/// `text` shortened to at most `max` characters, ending in an ellipsis when
/// it was cut; whitespace runs (and so line breaks) become single spaces.
fn cut(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let kept: String = flat.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", kept.trim_end())
}

#[cfg(test)]
mod tests;
