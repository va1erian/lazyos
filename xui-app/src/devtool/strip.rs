//! The strip every developer tool shows under its menu bar: one thin band that
//! says [`devtool::STRIP_TEXT`], with a small "x" at its right end that
//! dismisses it.
//!
//! It is a custom-painted widget (xui has no banner widget): the window's
//! background tinted with the theme's `warning` colour, a bar of that colour
//! on the left and a hairline under it, so it reads as a notice in the light
//! and the dark theme alike. Sizes are `Dip`s, so it follows the desktop's
//! scale (docs/hidpi-plan.md) without `xui_app::hidpi`.
//!
//! The "x" raises the message the tool gave [`strip`]; the tool then calls
//! [`Notice::dismiss`] and [`Strip::sync`]. Once the shared [`Notice`] says
//! the band is hidden, the strip measures to a height of 0 and paints nothing,
//! so the layout reclaims the space without a hole. The title suffix and the
//! Help page are not the strip's and stay (see [`super::notice`]).
//!
//! ```ignore
//! column().children((menu_bar(...), strip(notice, Msg::DismissNotice).bind(&self.strip), body))
//! // after the first layout, for a session script:
//! self.strip.get().probe(ui, &self.title);   // UI:WIDGET name=devtool:strip (+ devtool:strip_dismiss)
//! ```

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{build, Build};
use xui_core::backend::{Event, NodeKind, NodeSpec, TextStyle, WidgetId};
use xui_core::geometry::{Point, Rect, Size};
use xui_core::layout::Constraints;
use xui_core::message::MouseButton;
use xui_core::widget::{Control, Placeable};
use xui_core::{Canvas, Dip, Theme};

use super::notice::Notice;
use crate::probe;

/// The probe name of the strip (`UI:WIDGET ... name=devtool:strip`).
pub const PROBE_NAME: &str = "devtool:strip";
/// The probe name of its dismiss button.
pub const DISMISS_PROBE_NAME: &str = "devtool:strip_dismiss";
/// The button's accessible label (also the node's text).
pub const DISMISS_LABEL: &str = "Dismiss";

/// The strip's text size.
const TEXT_SIZE: Dip = Dip(12.0);
/// The width of the warning bar on the left edge.
const BAR: Dip = Dip(3.0);
/// Space between the bar, the text and the right edge.
const PAD_X: Dip = Dip(10.0);
/// Space above and below the text.
const PAD_Y: Dip = Dip(4.0);
/// How much of the `warning` colour the background takes (0 to 1).
const TINT: f32 = 0.18;
/// How much of a line's width wrapping can use: a word does not break, so a
/// line holds a little less than the text's width suggests.
const WRAP_SLACK: f32 = 0.9;
/// The dismiss button's side.
const BUTTON: Dip = Dip(18.0);

/// The strip widget; build it with [`strip`].
pub struct Strip<M: 'static> {
    control: Control<M>,
    notice: Rc<Notice>,
}

impl<M: 'static> Strip<M> {
    /// The strip's node.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Where the strip is in the window, in device pixels.
    pub fn bounds(&self, ui: &Ui<M>) -> Rect {
        ui.bounds(self.control.id())
    }

    /// Whether the strip is showing (the shared [`Notice`]).
    pub fn visible(&self) -> bool {
        self.notice.visible()
    }

    /// The notice changed (this window or another of the tool dismissed or
    /// restored it): measure again and repaint.
    pub fn sync(&self) {
        self.control.set_visible(self.notice.visible());
        self.control.invalidate_layout();
        self.control.invalidate();
    }

    /// Prints `UI:WIDGET ... name=devtool:strip window=<window>` and the
    /// dismiss button's rectangle (`devtool:strip_dismiss`) when the UI probe
    /// is on, so a session script finds them by name. Call it after the
    /// layout has been placed (the first frame, a resize). A dismissed strip
    /// prints nothing: the controls are not on screen.
    pub fn probe(&self, ui: &Ui<M>, window: &str) {
        let r = self.bounds(ui);
        if !self.visible() || r.width() <= 0 || r.height() <= 0 {
            return;
        }
        probe::widget(window, PROBE_NAME, r.left, r.top, r.width(), r.height());
        let b = button_rect(r.width(), r.height(), ui.dpi());
        probe::widget(
            window,
            DISMISS_PROBE_NAME,
            r.left + b.left,
            r.top + b.top,
            b.width(),
            b.height(),
        );
    }
}

/// The dismiss button inside a strip of `width` x `height` pixels, in the
/// strip's own coordinates: a square at the right end, centred vertically.
fn button_rect(width: i32, height: i32, dpi: u32) -> Rect {
    let side = px(BUTTON, dpi);
    let right = width - px(PAD_X, dpi) / 2;
    let top = ((height - side) / 2).max(0);
    Rect::new(right - side, top, right, top + side)
}

/// Whether a click at `(x, y)` in the strip's own coordinates is on the button.
fn hits_button(width: i32, height: i32, dpi: u32, x: i32, y: i32) -> bool {
    button_rect(width, height, dpi).contains(Point::new(x, y))
}

/// The style the text is painted (and measured) in.
fn text_style(theme: &Theme) -> TextStyle {
    TextStyle::new(theme.text, TEXT_SIZE).middle().wrapped()
}

/// The strip's background in `theme`: the window's own, leaning to amber.
fn tint(theme: &Theme) -> xui_core::Color {
    theme.background.lerp(theme.warning, TINT)
}

fn px(value: Dip, dpi: u32) -> i32 {
    value.to_px(dpi).value()
}

/// How many lines `text_width` pixels of text take in `avail` pixels.
fn line_count(text_width: i32, avail: i32) -> i32 {
    let usable = ((avail as f32) * WRAP_SLACK).max(1.0);
    ((text_width as f32 / usable).ceil() as i32).max(1)
}

/// What the painter and the event mapper share: the strip's last size and scale.
#[derive(Default)]
struct Geometry {
    size: Cell<(i32, i32)>,
    dpi: Cell<u32>,
}

fn paint(canvas: &mut dyn Canvas, theme: &Theme, notice: &Notice, geometry: &Geometry) {
    let bounds = canvas.bounds();
    if !notice.visible() || bounds.width() <= 0 || bounds.height() <= 0 {
        return;
    }
    let dpi = canvas.dpi();
    geometry.size.set((bounds.width(), bounds.height()));
    geometry.dpi.set(dpi);
    canvas.fill_rect(bounds, tint(theme));
    let bar = px(BAR, dpi);
    canvas.fill_rect(
        Rect::new(bounds.left, bounds.top, bounds.left + bar, bounds.bottom),
        theme.warning,
    );
    let rule = Rect::new(bounds.left, bounds.bottom - 1, bounds.right, bounds.bottom);
    canvas.fill_rect(rule, theme.border);
    let (pad_x, pad_y) = (px(PAD_X, dpi), px(PAD_Y, dpi));
    let local = button_rect(bounds.width(), bounds.height(), dpi);
    let button = local.offset(bounds.left, bounds.top);
    let text = Rect::new(
        bounds.left + bar + pad_x,
        bounds.top + pad_y,
        button.left - pad_x,
        bounds.bottom - pad_y,
    );
    canvas.draw_text(devtool::STRIP_TEXT, text, &text_style(theme));
    // The button: a hairline square with a multiplication sign.
    canvas.fill_rect(button, theme.border);
    canvas.fill_rect(button.shrink(1), tint(theme));
    let style = TextStyle::new(theme.text, TEXT_SIZE).middle().centered();
    canvas.draw_text("\u{00d7}", button, &style);
}

impl<M: 'static> Placeable<M> for Strip<M> {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The text's height, times the lines it needs at the width it is given:
    /// one line in an ordinary window, two or three in a narrow one. A
    /// dismissed strip is 0 high.
    fn measure(&self, ui: &Ui<M>, constraints: Constraints) -> Size {
        let dpi = constraints.dpi;
        if !self.notice.visible() {
            return Size::new(constraints.max_width.unwrap_or(0), 0);
        }
        let theme = ui.theme();
        let text = ui.measure_text(devtool::STRIP_TEXT, &text_style(&theme), dpi);
        let chrome_x = px(BAR, dpi) + 2 * px(PAD_X, dpi) + px(BUTTON, dpi) + px(PAD_X, dpi) / 2;
        let width = constraints.max_width.unwrap_or(text.width + chrome_x);
        let lines = line_count(text.width, (width - chrome_x).max(1));
        let height = (lines * text.height + 2 * px(PAD_Y, dpi) + 1).max(px(BUTTON, dpi) + 4);
        Size::new(width, height)
    }

    fn layout_text(&self) -> Option<String> {
        Some(devtool::STRIP_TEXT.to_owned())
    }
}

/// The strip, as a layout entry: put it between the menu bar and the body.
/// `notice` is the tool's shared [`Notice`]; `on_dismiss` is raised when the
/// user presses the "x" (the tool then dismisses the notice and calls
/// [`Strip::sync`] on its strips). Bind a `Handle<Strip<M>>` to reach
/// [`Strip::probe`].
pub fn strip<M: Clone + 'static>(notice: Rc<Notice>, on_dismiss: M) -> Build<Strip<M>, M> {
    build(move |ui| {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Container, Rect::default()))?;
        // The accessible name of the one control in the band; xui has no
        // accessibility hook for painted widgets, so it is the node's text.
        control.set_text(DISMISS_LABEL);
        control.set_visible(notice.visible());
        let theme = ui.theme_handle();
        let geometry = Rc::new(Geometry::default());
        let shown = Rc::clone(&notice);
        let drawn = Rc::clone(&geometry);
        control.set_painter(Rc::new(move |canvas| paint(canvas, &theme.get(), &shown, &drawn)));
        let visible = Rc::clone(&notice);
        control.on_events(move |event| {
            let Event::MouseUp {
                x,
                y,
                button: MouseButton::Left,
                ..
            } = *event
            else {
                return None;
            };
            let (w, h) = geometry.size.get();
            (visible.visible() && hits_button(w, h, geometry.dpi.get(), x, y))
                .then(|| on_dismiss.clone())
        });
        Ok(Strip { control, notice })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_window_takes_one_line_and_a_narrow_one_more() {
        assert_eq!(line_count(500, 800), 1);
        assert_eq!(line_count(500, 500), 2, "the slack keeps the last word whole");
        assert_eq!(line_count(500, 200), 3);
        assert_eq!(line_count(0, 100), 1);
        assert_eq!(line_count(500, 0), 500, "a zero width is clamped, not divided by");
    }

    #[test]
    fn the_tint_is_visibly_not_the_background_in_both_themes() {
        for theme in [Theme::light(), Theme::dark()] {
            assert_ne!(tint(&theme), theme.background);
            assert_ne!(tint(&theme), theme.warning, "the bar stands out of the band");
            // The text keeps a readable contrast on the tinted band.
            assert!(theme.text.contrast_ratio(tint(&theme)) >= 4.5);
        }
    }

    #[test]
    fn the_button_sits_at_the_right_end_inside_the_band_at_both_scales() {
        for (dpi, scale) in [(96, 1), (192, 2)] {
            let (w, h) = (800 * scale, 24 * scale);
            let b = button_rect(w, h, dpi);
            assert_eq!(b.width(), 18 * scale);
            assert!(b.right <= w && b.left > w / 2, "at the right end");
            assert!(b.top >= 0 && b.bottom <= h, "inside the band");
            assert!(hits_button(w, h, dpi, b.left + 1, b.top + 1));
            assert!(!hits_button(w, h, dpi, b.left - 2, b.top + 1), "text is not the button");
            assert!(!hits_button(w, h, dpi, 5, h / 2));
        }
    }
}
