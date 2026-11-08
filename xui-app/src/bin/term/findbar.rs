//! The find bar: a text field, Prev/Next, a case toggle and a close button
//! along the bottom of the window (Ctrl+Shift+F). The model edits the text and
//! owns the [`Search`]; the geometry and painting are here too, so the whole is
//! host-tested and snapshotted.

use lazyterm::search::MAX_NEEDLE;
use lazyterm::{Palette, Search};
use xui_core::{Canvas, Color, Key, Modifiers, Rect};

use super::geometry::contains;
use super::view::Layout;

/// What an edit or key did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    None,
    Close,
    /// The text or the case changed: search again.
    Edited,
    Next,
    Prev,
    /// The case button was clicked.
    Case,
}

pub struct Find {
    pub text: String,
    /// The caret as a character index into `text`.
    pub caret: usize,
    pub case: bool,
    pub search: Search,
}

impl Default for Find {
    fn default() -> Find {
        Find { text: String::new(), caret: 0, case: false, search: Search::new("", false) }
    }
}

impl Find {
    /// Start a new search over the current text and case.
    pub fn restart(&mut self) {
        self.search = Search::new(&self.text, self.case);
    }

    fn byte(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(i, _)| i)
    }

    /// Typed text (control characters are not text).
    pub fn insert(&mut self, c: char) -> Step {
        if c.is_control() || self.text.chars().count() >= MAX_NEEDLE {
            return Step::None;
        }
        let at = self.byte(self.caret);
        self.text.insert(at, c);
        self.caret += 1;
        Step::Edited
    }

    fn backspace(&mut self) -> Step {
        if self.caret == 0 {
            return Step::None;
        }
        self.caret -= 1;
        let at = self.byte(self.caret);
        self.text.remove(at);
        Step::Edited
    }

    fn delete(&mut self) -> Step {
        if self.caret >= self.text.chars().count() {
            return Step::None;
        }
        let at = self.byte(self.caret);
        self.text.remove(at);
        Step::Edited
    }

    pub fn toggle_case(&mut self) -> Step {
        self.case = !self.case;
        Step::Edited
    }

    /// A key press (not text): editing keys, Enter and Shift+Enter, Escape.
    pub fn key(&mut self, key: Key, mods: Modifiers) -> Step {
        let len = self.text.chars().count();
        match key {
            Key::ESCAPE => Step::Close,
            Key::RETURN if mods.shift => Step::Prev,
            Key::RETURN => Step::Next,
            Key::BACK => self.backspace(),
            Key::DELETE => self.delete(),
            Key::LEFT => {
                self.caret = self.caret.saturating_sub(1);
                Step::None
            }
            Key::RIGHT => {
                self.caret = (self.caret + 1).min(len);
                Step::None
            }
            Key::HOME => {
                self.caret = 0;
                Step::None
            }
            Key::END => {
                self.caret = len;
                Step::None
            }
            Key::UP => Step::Prev,
            Key::DOWN => Step::Next,
            // Alt+C toggles the case, as in other find bars.
            Key::C if mods.alt => self.toggle_case(),
            _ => Step::None,
        }
    }

    /// The count label: `3/12`, `No matches`, `12+` when the limit cut it off,
    /// and an ellipsis while the scan is still running.
    pub fn status(&self) -> String {
        if self.text.is_empty() {
            return String::new();
        }
        let s = &self.search;
        let more = if s.truncated() { "+" } else { "" };
        match (s.count(), s.current_index(), s.done()) {
            (0, _, true) => "No matches".to_string(),
            (0, _, false) => "...".to_string(),
            (n, Some(i), true) => format!("{i}/{n}{more}"),
            (n, _, true) => format!("{n}{more}"),
            (n, _, false) => format!("{n}..."),
        }
    }
}

/// The bar's parts in window pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FindRects {
    pub bar: Rect,
    pub field: Rect,
    pub case: Rect,
    pub prev: Rect,
    pub next: Rect,
    pub count: Rect,
    pub close: Rect,
}

pub fn rects(layout: &Layout) -> FindRects {
    let unit = layout.pad;
    let bar = Rect::new(0, layout.height - 3 * unit, layout.width, layout.height);
    let (top, height) = (bar.top + unit / 2, 2 * unit);
    let button = 3 * unit;
    let close = Rect::new(layout.width - unit - button, top, layout.width - unit, top + height);
    let next = Rect::new(close.left - unit / 2 - button, top, close.left - unit / 2, top + height);
    let prev = Rect::new(next.left - unit / 2 - button, top, next.left - unit / 2, top + height);
    let count = Rect::new(prev.left - unit - 9 * unit, top, prev.left - unit, top + height);
    let case = Rect::new(count.left - unit / 2 - button, top, count.left - unit / 2, top + height);
    let field = Rect::new(unit, top, case.left - unit / 2, top + height);
    FindRects { bar, field, case, prev, next, count, close }
}

/// A click in the bar: which part, as a step (the field only takes the focus,
/// which it already has).
pub fn click(layout: &Layout, x: i32, y: i32) -> Option<Step> {
    let r = rects(layout);
    if contains(r.close, x, y) {
        Some(Step::Close)
    } else if contains(r.next, x, y) {
        Some(Step::Next)
    } else if contains(r.prev, x, y) {
        Some(Step::Prev)
    } else if contains(r.case, x, y) {
        Some(Step::Case)
    } else {
        None
    }
}

fn color(rgb: lazyterm::Rgb) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

/// Draw the bar.
pub fn paint(canvas: &mut dyn Canvas, find: &Find, layout: &Layout, palette: &Palette, scale: i32, caret_on: bool) {
    let r = rects(layout);
    let s = scale.max(1);
    let (bg, fg) = (palette.bg(), palette.fg());
    canvas.fill_rect(r.bar, color(bg.lerp(fg, 0.16)));
    canvas.fill_rect(Rect::new(r.bar.left, r.bar.top, r.bar.right, r.bar.top + s), color(palette.cursor()));
    let style = xui_core::TextStyle::new(color(fg), xui_core::Dip(15.0));
    let dim = xui_core::TextStyle::new(color(fg.lerp(bg, 0.45)), xui_core::Dip(15.0));
    let text_at = |rect: Rect, inset: i32| Rect::new(rect.left + inset, rect.top + (rect.height() - 20 * s) / 2, rect.right, rect.bottom);
    // The field: a darker well with the text and a caret.
    canvas.fill_rect(r.field, color(bg.lerp(fg, 0.05)));
    if find.text.is_empty() {
        canvas.draw_text("Find", text_at(r.field, 6 * s), &dim);
    } else {
        canvas.draw_text(&find.text, text_at(r.field, 6 * s), &style);
    }
    if caret_on {
        // The caret sits after `caret` characters of the (proportional) UI
        // font: estimated at 8 design pixels a character.
        let x = r.field.left + 6 * s + find.caret as i32 * 8 * s;
        canvas.fill_rect(Rect::new(x, r.field.top + 3 * s, x + s, r.field.bottom - 3 * s), color(fg));
    }
    let button = |canvas: &mut dyn Canvas, rect: Rect, label: &str, on: bool| {
        let fill = if on { palette.cursor().lerp(bg, 0.45) } else { bg.lerp(fg, 0.32) };
        canvas.fill_rect(rect, color(fill));
        canvas.draw_text(label, text_at(rect, 8 * s), &style);
    };
    button(canvas, r.case, "Aa", find.case);
    button(canvas, r.prev, " <", false);
    button(canvas, r.next, " >", false);
    button(canvas, r.close, " x", false);
    canvas.draw_text(&find.status(), text_at(r.count, 2 * s), &dim);
}

/// The probe rectangles of the bar, by name.
pub fn probes(layout: &Layout) -> [(&'static str, Rect); 6] {
    let r = rects(layout);
    [
        ("find_bar", r.bar),
        ("find_field", r.field),
        ("find_case", r.case),
        ("find_prev", r.prev),
        ("find_next", r.next),
        ("find_close", r.close),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT: Layout = Layout { width: 640, height: 400, pad: 10, line_h: 20, cell_w: 9, visible: 19 };
    const NONE: Modifiers = Modifiers { ctrl: false, shift: false, alt: false, win: false };
    const SHIFT: Modifiers = Modifiers { shift: true, ..NONE };

    fn typed(f: &mut Find, text: &str) {
        for c in text.chars() {
            f.insert(c);
        }
    }

    #[test]
    fn typing_and_editing_the_field() {
        let mut f = Find::default();
        typed(&mut f, "hello");
        assert_eq!((f.text.as_str(), f.caret), ("hello", 5));
        assert_eq!(f.key(Key::BACK, NONE), Step::Edited);
        assert_eq!(f.text, "hell");
        f.key(Key::HOME, NONE);
        assert_eq!(f.key(Key::BACK, NONE), Step::None, "nothing before the caret");
        assert_eq!(f.key(Key::DELETE, NONE), Step::Edited);
        assert_eq!(f.text, "ell");
        f.key(Key::END, NONE);
        f.key(Key::LEFT, NONE);
        f.insert('X');
        assert_eq!(f.text, "elXl");
        assert_eq!(f.insert('\n'), Step::None, "control characters are not text");
        assert_eq!(f.insert('\u{1b}'), Step::None);
    }

    #[test]
    fn multibyte_text_edits_by_character() {
        let mut f = Find::default();
        typed(&mut f, "a\u{e9}\u{4e2d}");
        assert_eq!(f.caret, 3);
        f.key(Key::LEFT, NONE);
        f.key(Key::BACK, NONE);
        assert_eq!(f.text, "a\u{4e2d}");
        f.key(Key::END, NONE);
        f.key(Key::BACK, NONE);
        assert_eq!(f.text, "a");
    }

    #[test]
    fn the_needle_is_bounded() {
        let mut f = Find::default();
        for _ in 0..MAX_NEEDLE + 40 {
            f.insert('x');
        }
        assert_eq!(f.text.chars().count(), MAX_NEEDLE);
    }

    #[test]
    fn enter_steps_and_escape_closes() {
        let mut f = Find::default();
        assert_eq!(f.key(Key::RETURN, NONE), Step::Next);
        assert_eq!(f.key(Key::RETURN, SHIFT), Step::Prev);
        assert_eq!(f.key(Key::ESCAPE, NONE), Step::Close);
        assert_eq!(f.key(Key::C, Modifiers { alt: true, ..NONE }), Step::Edited);
        assert!(f.case);
    }

    #[test]
    fn the_status_reads_the_search() {
        let mut t = lazyterm::Term::new(20, 4);
        t.feed(b"one two one\r\n");
        let mut f = Find::default();
        assert_eq!(f.status(), "");
        typed(&mut f, "one");
        f.restart();
        assert_eq!(f.status(), "...", "nothing scanned yet");
        while !f.search.update(&t, 100, t.screen_top()) {}
        assert_eq!(f.status(), "1/2");
        typed(&mut f, "zzz");
        f.restart();
        while !f.search.update(&t, 100, t.screen_top()) {}
        assert_eq!(f.status(), "No matches");
    }

    #[test]
    fn the_parts_are_in_the_bar_and_do_not_overlap() {
        let r = rects(&LAYOUT);
        let parts = [r.field, r.case, r.count, r.prev, r.next, r.close];
        for p in parts {
            assert!(p.left >= 0 && p.right <= LAYOUT.width && p.top >= r.bar.top && p.bottom <= r.bar.bottom);
        }
        for w in parts.windows(2) {
            assert!(w[0].right <= w[1].left, "{:?} {:?}", w[0], w[1]);
        }
        assert_eq!(click(&LAYOUT, r.next.left + 1, r.next.top + 1), Some(Step::Next));
        assert_eq!(click(&LAYOUT, r.prev.left + 1, r.prev.top + 1), Some(Step::Prev));
        assert_eq!(click(&LAYOUT, r.close.left + 1, r.close.top + 1), Some(Step::Close));
        assert_eq!(click(&LAYOUT, r.case.left + 1, r.case.top + 1), Some(Step::Case));
        assert_eq!(click(&LAYOUT, 3, 3), None);
        assert_eq!(probes(&LAYOUT).len(), 6);
    }
}
