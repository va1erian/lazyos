//! The layers the Terminal draws over the grid: the confirmation bar, the find
//! bar, the context menu, the preferences panel, the size badge and the bell's
//! flash. They live in one value shared with the painter (`Rc<RefCell<..>>`),
//! which the Terminal changes and then repaints.

use lazyterm::{Palette, Point, Rgb};
use xui_core::{Canvas, Color, Rect};

use super::badge;
use super::findbar::{self, Find};
use super::geometry::confirm_rects;
use super::look::Look;
use super::menu::{self, Menu};
use super::prefs::Prefs;
use super::prefs_paint;
use super::prompt::Prompt;
use super::view::{overlaps, Layout};

#[derive(Default)]
pub struct Chrome {
    pub prompt: Option<Prompt>,
    pub badge: Option<String>,
    pub menu: Option<Menu>,
    pub find: Option<Find>,
    pub prefs: Option<Prefs>,
    /// The visual bell is flashing.
    pub flash: bool,
    /// The link under the pointer while Ctrl is held: underlined (cells, inclusive).
    pub hover: Option<(Point, Point)>,
}

impl Chrome {
    /// Whether a layer that takes the keyboard is up.
    pub fn modal(&self) -> bool {
        self.prompt.is_some() || self.menu.is_some() || self.prefs.is_some() || self.find.is_some()
    }
}

fn color(rgb: Rgb) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

/// The bar's colours and one line of text, with its two buttons.
fn paint_confirm(canvas: &mut dyn Canvas, palette: &Palette, layout: &Layout, scale: i32, prompt: &Prompt, damage: Rect) {
    let c = confirm_rects(layout);
    if !overlaps(c.bar, damage) {
        return;
    }
    let fg = color(palette.fg());
    let style = xui_core::TextStyle::new(fg, xui_core::Dip(15.0));
    canvas.fill_rect(c.bar, color(palette.bg().lerp(palette.fg(), 0.16)));
    canvas.fill_rect(Rect::new(c.bar.left, c.bar.top, c.bar.right, c.bar.top + scale), color(palette.cursor()));
    let text = |canvas: &mut dyn Canvas, text: &str, rect: Rect| {
        let at = Rect::new(rect.left, rect.top + (rect.height() - 20 * scale) / 2, rect.right, rect.bottom);
        canvas.draw_text(text, at, &style);
    };
    text(canvas, &prompt.text, Rect::new(c.bar.left + layout.pad, c.bar.top, c.paste.left, c.bar.bottom));
    for (rect, label) in [(c.paste, prompt.ok), (c.cancel, prompt.cancel)] {
        canvas.fill_rect(rect, color(palette.bg().lerp(palette.fg(), 0.32)));
        text(canvas, label, Rect::new(rect.left + layout.pad, rect.top, rect.right, rect.bottom));
    }
}

/// Draw every layer that is up, over the grid already painted.
pub fn paint(canvas: &mut dyn Canvas, chrome: &Chrome, look: &Look, palette: &Palette, laid: &Layout, dark: bool, damage: Rect) {
    let scale = look.metrics.scale;
    let window = (laid.width, laid.height);
    if let Some(find) = &chrome.find {
        findbar::paint(canvas, find, laid, palette, scale, look.cursor_on);
    }
    if let Some(prompt) = &chrome.prompt {
        paint_confirm(canvas, palette, laid, scale, prompt, damage);
    }
    if let Some(menu) = &chrome.menu {
        let at = menu::layout(menu, window, scale);
        menu::paint(canvas, menu, &at, palette, scale);
    }
    if let Some(prefs) = &chrome.prefs {
        prefs_paint::paint(canvas, prefs, window, scale, palette, dark, look.cursor_on);
    }
    if let Some(text) = &chrome.badge {
        badge::paint(canvas, palette, look, laid, text, damage);
    }
    if chrome.flash {
        // A frame around the window in the cursor colour.
        let t = 4 * scale;
        let c = color(palette.cursor());
        for r in [
            Rect::new(0, 0, laid.width, t),
            Rect::new(0, laid.height - t, laid.width, laid.height),
            Rect::new(0, 0, t, laid.height),
            Rect::new(laid.width - t, 0, laid.width, laid.height),
        ] {
            canvas.fill_rect(r, c);
        }
    }
}
