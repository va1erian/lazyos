//! A toast's face: the geometry of its icon, texts, progress bar, actions
//! and close mark, and the hit test (docs/notify-plan.md).
//!
//! Text is wrapped with a caller-supplied measure so the model stays free of
//! fonts and is tested with a fixed-width one. All coordinates are design
//! pixels relative to the toast's top-left.

use super::note::Note;
use crate::Rect;

/// The toast's width.
pub const WIDTH: i32 = 320;
/// Inner margin.
pub const PAD: i32 = 10;
/// The level icon's side, and the gap after it.
pub const ICON: i32 = 20;
pub const ICON_GAP: i32 = 8;
/// One text line's height; the app-name line above the title is smaller.
pub const LINE_H: i32 = 16;
pub const HEADER_H: i32 = 14;
/// The close mark's side.
pub const CLOSE: i32 = 16;
/// The progress bar's height and the gap around it and the actions.
pub const PROGRESS_H: i32 = 4;
pub const GAP: i32 = 6;
/// An action button's height, its smallest width and the padding round its
/// label.
pub const ACTION_H: i32 = 24;
pub const ACTION_MIN_W: i32 = 56;
pub const ACTION_PAD: i32 = 10;
/// Gap between action buttons.
pub const ACTION_GAP: i32 = 6;
/// Most lines of title and of body.
pub const TITLE_LINES: usize = 2;
pub const BODY_LINES: usize = 3;
/// A horizontal drag this long (design pixels) dismisses the toast.
pub const FLICK: i32 = 48;

/// One line of text, at its top `y`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub y: i32,
    pub text: String,
}

/// One action button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub id: u32,
    pub label: String,
    pub rect: Rect,
}

/// What a point on a toast is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The close mark.
    Close,
    /// Action `id`.
    Action(u32),
    /// Anywhere else on the toast: the body.
    Body,
}

/// A laid-out toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub width: i32,
    pub height: i32,
    pub icon: Rect,
    /// The sender's verified name, above the title.
    pub name: Line,
    pub title: Vec<Line>,
    pub body: Vec<Line>,
    pub progress: Option<Rect>,
    pub actions: Vec<Button>,
    pub close: Rect,
    /// The strip along the bottom edge the countdown bar draws in.
    pub countdown: Rect,
    /// Where the texts start, and their width.
    pub text_x: i32,
    pub text_w: i32,
}

impl Card {
    /// Lay `note` out under the sender's `name`; `measure` gives a text's
    /// width in design pixels (`bold` for the title) .
    pub fn new(note: &Note, name: &str, measure: &dyn Fn(&str, bool) -> i32) -> Card {
        let text_x = PAD + ICON + ICON_GAP;
        let text_w = WIDTH - text_x - PAD;
        let close = Rect::new(WIDTH - PAD - CLOSE + 2, PAD - 2, CLOSE, CLOSE);
        // The name shares the line with the close mark.
        let name_w = text_w - CLOSE - 4;
        let mut name_text = name.to_owned();
        if measure(&name_text, true) > name_w {
            ellipsize(&mut name_text, name_w, &|t| measure(t, true));
        }
        let mut y = PAD;
        let name_line = Line { y, text: name_text };
        y += HEADER_H + 2;

        let lines = |text: &str, bold: bool, limit: usize, y: &mut i32| -> Vec<Line> {
            if text.trim().is_empty() {
                return Vec::new();
            }
            let measure_one = |t: &str| measure(t, bold);
            let mut wrapped = wrap(text, text_w, &measure_one);
            if wrapped.len() > limit {
                wrapped.truncate(limit);
                if let Some(last) = wrapped.last_mut() {
                    ellipsize(last, text_w, &measure_one);
                }
            }
            wrapped
                .into_iter()
                .map(|text| {
                    let line = Line { y: *y, text };
                    *y += LINE_H;
                    line
                })
                .collect()
        };
        let title = lines(&note.title, true, TITLE_LINES, &mut y);
        let body = lines(&note.body, false, BODY_LINES, &mut y);

        let progress = note.progress.map(|_| {
            y += GAP;
            let bar = Rect::new(text_x, y, text_w, PROGRESS_H);
            y += PROGRESS_H;
            bar
        });

        let mut actions = Vec::new();
        if !note.actions.is_empty() {
            y += GAP;
            let mut right = WIDTH - PAD;
            for action in note.actions.iter().rev() {
                let width = (measure(&action.label, false) + 2 * ACTION_PAD).max(ACTION_MIN_W);
                right -= width;
                actions.push(Button {
                    id: action.id,
                    label: action.label.clone(),
                    rect: Rect::new(right, y, width, ACTION_H),
                });
                right -= ACTION_GAP;
            }
            actions.reverse();
            y += ACTION_H;
        }
        // The icon column is at least as tall as the icon.
        let height = (y - PAD).max(ICON) + 2 * PAD;
        Card {
            width: WIDTH,
            height,
            icon: Rect::new(PAD, PAD, ICON, ICON),
            name: name_line,
            title,
            body,
            progress,
            actions,
            close,
            countdown: Rect::new(0, height - 2, WIDTH, 2),
            text_x,
            text_w,
        }
    }

    /// What is at `(x, y)`, or `None` outside the card.
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !Rect::new(0, 0, self.width, self.height).contains(x, y) {
            return None;
        }
        if self.close.contains(x, y) {
            return Some(Hit::Close);
        }
        self.actions
            .iter()
            .find(|button| button.rect.contains(x, y))
            .map(|button| Hit::Action(button.id))
            .or(Some(Hit::Body))
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

/// End `line` with an ellipsis, shortening it until it fits `width`.
pub fn ellipsize(line: &mut String, width: i32, measure: &dyn Fn(&str) -> i32) {
    loop {
        let candidate = format!("{}...", line.trim_end());
        if measure(&candidate) <= width || line.is_empty() {
            *line = candidate;
            return;
        }
        line.pop();
    }
}
