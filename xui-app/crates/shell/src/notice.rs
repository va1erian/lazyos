//! The "app stopped" notice (issue #549): what the shell tells the user when
//! `init` gives up on an app they opened, and the dialog's geometry.
//!
//! `init` publishes one `system/events/app/<id>` event when a launched app
//! ends failed (it failed while starting, kept crashing, or failed with no
//! restart policy). The shell turns it into a small window: a heading naming
//! the app, one sentence saying what happened, the reason the app reported
//! (when it did), and two buttons, *Restart* and *Close*. Text is wrapped
//! with a caller-supplied measure so the model stays free of fonts.

use crate::Rect;

/// The dialog's width (design pixels).
pub const WIDTH: i32 = 440;
/// Inner margin.
pub const PAD: i32 = 16;
/// Gap between paragraphs.
pub const GAP: i32 = 8;
/// A button's size and the gap between buttons.
pub const BUTTON_W: i32 = 88;
pub const BUTTON_H: i32 = 28;
pub const BUTTON_GAP: i32 = 8;
/// Most lines the reason may take; longer text ends in an ellipsis.
pub const MAX_REASON_LINES: usize = 8;

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

/// A dialog button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Restart,
    Close,
}

impl Button {
    /// The label it shows.
    pub fn label(self) -> &'static str {
        match self {
            Button::Restart => "Restart",
            Button::Close => "Close",
        }
    }

    /// Its UI-probe name (`notice:restart`, `notice:close`).
    pub fn probe_name(self) -> &'static str {
        match self {
            Button::Restart => "restart",
            Button::Close => "close",
        }
    }
}

impl Failure {
    /// The window title.
    pub fn title(&self) -> String {
        format!("{} stopped", self.display_name())
    }

    /// The heading inside the dialog.
    pub fn heading(&self) -> String {
        format!("{} stopped unexpectedly", self.display_name())
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

    fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.app
        } else {
            self.name.trim()
        }
    }
}

/// Break `text` into lines no wider than `width` by `measure`; a word wider
/// than the line is split by characters. Never returns an empty list.
pub fn wrap(text: &str, width: i32, measure: &dyn Fn(&str) -> i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if measure(&candidate) <= width {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        // The word alone: split it if it still does not fit.
        for ch in word.chars() {
            let mut next = line.clone();
            next.push(ch);
            if measure(&next) > width && !line.is_empty() {
                lines.push(std::mem::replace(&mut line, ch.to_string()));
            } else {
                line = next;
            }
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// One line of text in the dialog, at its top `y`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub y: i32,
    pub text: String,
    /// The heading line (drawn bold).
    pub heading: bool,
}

/// The laid-out dialog, in design pixels relative to its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub lines: Vec<Line>,
    pub restart: Rect,
    pub close: Rect,
}

impl Layout {
    /// Lay `failure` out with lines `line_h` tall, wrapping by `measure`.
    pub fn new(failure: &Failure, line_h: i32, measure: &dyn Fn(&str) -> i32) -> Layout {
        let text_w = WIDTH - 2 * PAD;
        let mut lines = Vec::new();
        let mut y = PAD;
        let mut paragraph = |text: &str, heading: bool, limit: usize, y: &mut i32| {
            let mut wrapped = wrap(text, text_w, measure);
            if wrapped.len() > limit {
                wrapped.truncate(limit);
                if let Some(last) = wrapped.last_mut() {
                    ellipsize(last, text_w, measure);
                }
            }
            for text in wrapped {
                lines.push(Line {
                    y: *y,
                    text,
                    heading,
                });
                *y += line_h;
            }
            *y += GAP;
        };
        paragraph(&failure.heading(), true, 2, &mut y);
        paragraph(&failure.sentence(), false, 4, &mut y);
        if let Some(reason) = failure.reason_text() {
            paragraph(&reason, false, MAX_REASON_LINES, &mut y);
        }
        let buttons_y = y + GAP;
        let close = Rect::new(WIDTH - PAD - BUTTON_W, buttons_y, BUTTON_W, BUTTON_H);
        let restart = close.offset(-(BUTTON_W + BUTTON_GAP), 0);
        Layout {
            width: WIDTH,
            height: buttons_y + BUTTON_H + PAD,
            lines,
            restart,
            close,
        }
    }

    /// The button at `(x, y)`, if any.
    pub fn hit(&self, x: i32, y: i32) -> Option<Button> {
        if self.restart.contains(x, y) {
            Some(Button::Restart)
        } else if self.close.contains(x, y) {
            Some(Button::Close)
        } else {
            None
        }
    }

    /// A button's rectangle.
    pub fn button(&self, button: Button) -> Rect {
        match button {
            Button::Restart => self.restart,
            Button::Close => self.close,
        }
    }
}

/// End `line` with an ellipsis, shortening it until it fits `width`.
fn ellipsize(line: &mut String, width: i32, measure: &dyn Fn(&str) -> i32) {
    loop {
        let candidate = format!("{}...", line.trim_end());
        if measure(&candidate) <= width || line.is_empty() {
            *line = candidate;
            return;
        }
        line.pop();
    }
}

#[cfg(test)]
mod tests;
