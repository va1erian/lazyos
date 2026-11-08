//! The confirmation bar along the bottom of the window: a question and two
//! buttons. It asks before a multi-line paste, before opening a link whose
//! target the user has not seen, and before closing the window over a running
//! program. Only one is up at a time.

use super::geometry::contains;
use super::view::Layout;

/// What the bar is asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Paste,
    /// Open this URL (already sanitized).
    Link(String),
    /// Close the window while a program still runs.
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub kind: Kind,
    pub text: String,
    pub ok: &'static str,
    pub cancel: &'static str,
}

/// The longest target shown in a prompt (a longer one is cut with an
/// ellipsis; the whole of it is still what opens).
const SHOWN: usize = 120;

impl Prompt {
    pub fn paste(lines: usize) -> Prompt {
        Prompt { kind: Kind::Paste, text: format!("Paste {lines} lines?"), ok: "Paste", cancel: "Cancel" }
    }

    pub fn link(url: &str) -> Prompt {
        let shown: String = url.chars().take(SHOWN).collect();
        let dots = if url.chars().count() > SHOWN { "..." } else { "" };
        Prompt { kind: Kind::Link(url.to_string()), text: format!("Open {shown}{dots} ?"), ok: "Open", cancel: "Cancel" }
    }

    pub fn close() -> Prompt {
        Prompt {
            kind: Kind::Close,
            text: "A program is still running. Close anyway?".to_string(),
            ok: "Close",
            cancel: "Cancel",
        }
    }

    /// The evidence name of the prompt's probe rectangles.
    pub fn probe_names(&self) -> [&'static str; 3] {
        match self.kind {
            Kind::Paste => ["paste_confirm", "paste_button", "cancel_button"],
            Kind::Link(_) => ["link_confirm", "link_open_button", "cancel_button"],
            Kind::Close => ["close_confirm", "close_button", "cancel_button"],
        }
    }
}

/// Which button a click at (`x`, `y`) is on: `Some(true)` the accepting one.
pub fn button_at(layout: &Layout, x: i32, y: i32) -> Option<bool> {
    let rects = super::geometry::confirm_rects(layout);
    if contains(rects.paste, x, y) {
        Some(true)
    } else if contains(rects.cancel, x, y) {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompts_say_what_they_ask() {
        assert_eq!(Prompt::paste(3).text, "Paste 3 lines?");
        assert_eq!(Prompt::close().text, "A program is still running. Close anyway?");
        let link = Prompt::link("http://example.com/x");
        assert_eq!(link.text, "Open http://example.com/x ?");
        assert_eq!(link.kind, Kind::Link("http://example.com/x".into()));
    }

    #[test]
    fn a_long_target_is_cut_for_the_bar_but_not_for_opening() {
        let url = format!("http://example.com/{}", "a".repeat(500));
        let p = Prompt::link(&url);
        assert!(p.text.chars().count() < 140 && p.text.contains("..."));
        assert_eq!(p.kind, Kind::Link(url));
    }

    #[test]
    fn the_buttons_are_found_by_position() {
        let layout = Layout { width: 640, height: 400, pad: 10, line_h: 20, cell_w: 9, visible: 19 };
        let rects = crate::geometry::confirm_rects(&layout);
        assert_eq!(button_at(&layout, rects.paste.left + 1, rects.paste.top + 1), Some(true));
        assert_eq!(button_at(&layout, rects.cancel.left + 1, rects.cancel.top + 1), Some(false));
        assert_eq!(button_at(&layout, 5, 5), None);
    }
}
