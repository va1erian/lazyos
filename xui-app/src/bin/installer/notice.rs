//! The warning band on a developer tool's consent screen.
//!
//! A custom-painted block of wrapped text in the look of the tools' own strip
//! (`xui_app::devtool::strip`): the window's background tinted with the
//! theme's `warning` colour, a bar of that colour on the left and a hairline
//! under it, so the class warning reads as a notice in the light and the dark
//! theme alike and is the first thing on the screen. Unlike the strip it
//! carries any text (the words `pkgd` sent) and has no handlers.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{build, Build};
use xui_core::backend::{NodeKind, NodeSpec, TextStyle, WidgetId};
use xui_core::geometry::{Rect, Size};
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Canvas, Dip, Theme};

/// The text size.
const TEXT_SIZE: Dip = Dip(13.0);
/// The width of the warning bar on the left edge.
const BAR: Dip = Dip(4.0);
/// Space between the bar, the text and the right edge.
const PAD_X: Dip = Dip(12.0);
/// Space above and below the text.
const PAD_Y: Dip = Dip(8.0);
/// How much of the `warning` colour the background takes (0 to 1).
const TINT: f32 = 0.18;
/// How much of a line's width wrapping can use: a word does not break, so a
/// line holds a little less than the text's width suggests.
const WRAP_SLACK: f32 = 0.9;

/// The notice widget; build it with [`notice`].
pub struct Notice<M: 'static> {
    control: Control<M>,
    text: Rc<str>,
}

fn text_style(theme: &Theme) -> TextStyle {
    TextStyle::new(theme.text, TEXT_SIZE).wrapped()
}

/// The band's background in `theme`: the window's own, leaning to amber.
pub fn tint(theme: &Theme) -> xui_core::Color {
    theme.background.lerp(theme.warning, TINT)
}

fn px(value: Dip, dpi: u32) -> i32 {
    value.to_px(dpi).value()
}

/// How many lines `text_width` pixels of text take in `avail` pixels.
pub fn line_count(text_width: i32, avail: i32) -> i32 {
    let usable = ((avail as f32) * WRAP_SLACK).max(1.0);
    ((text_width as f32 / usable).ceil() as i32).max(1)
}

fn paint(canvas: &mut dyn Canvas, theme: &Theme, text: &str) {
    let dpi = canvas.dpi();
    let bounds = canvas.bounds();
    canvas.fill_rect(bounds, tint(theme));
    let bar = px(BAR, dpi);
    canvas.fill_rect(
        Rect::new(bounds.left, bounds.top, bounds.left + bar, bounds.bottom),
        theme.warning,
    );
    let rule = Rect::new(bounds.left, bounds.bottom - 1, bounds.right, bounds.bottom);
    canvas.fill_rect(rule, theme.border);
    let (pad_x, pad_y) = (px(PAD_X, dpi), px(PAD_Y, dpi));
    let area = Rect::new(
        bounds.left + bar + pad_x,
        bounds.top + pad_y,
        bounds.right - pad_x,
        bounds.bottom - pad_y,
    );
    canvas.draw_text(text, area, &text_style(theme));
}

impl<M: 'static> Placeable<M> for Notice<M> {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// The text's height times the lines it needs at the width it is given.
    fn measure(&self, ui: &Ui<M>, constraints: Constraints) -> Size {
        let dpi = constraints.dpi;
        let theme = ui.theme();
        let text = ui.measure_text(&self.text, &text_style(&theme), dpi);
        let chrome_x = px(BAR, dpi) + 2 * px(PAD_X, dpi);
        let width = constraints.max_width.unwrap_or(text.width + chrome_x);
        let lines = line_count(text.width, (width - chrome_x).max(1));
        Size::new(width, lines * text.height + 2 * px(PAD_Y, dpi) + 1)
    }

    fn layout_text(&self) -> Option<String> {
        Some(self.text.to_string())
    }
}

/// A warning band saying `text`, as a layout entry.
pub fn notice<M: 'static>(text: impl Into<String>) -> Build<Notice<M>, M> {
    let text: Rc<str> = Rc::from(text.into());
    build(move |ui| {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Container, Rect::default()))?;
        let theme = ui.theme_handle();
        let shown = Rc::clone(&text);
        control.set_painter(Rc::new(move |canvas| {
            paint(canvas, &theme.get(), &shown);
        }));
        Ok(Notice { control, text })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_window_takes_one_line_and_a_narrow_one_more() {
        assert_eq!(line_count(500, 800), 1);
        assert_eq!(
            line_count(500, 500),
            2,
            "the slack keeps the last word whole"
        );
        assert_eq!(line_count(500, 200), 3);
        assert_eq!(line_count(0, 100), 1);
    }

    #[test]
    fn the_band_is_visibly_not_the_background_and_stays_readable_in_both_themes() {
        for theme in [Theme::light(), Theme::dark()] {
            assert_ne!(tint(&theme), theme.background);
            assert_ne!(tint(&theme), theme.warning);
            assert!(theme.text.contrast_ratio(tint(&theme)) >= 4.5);
        }
    }
}
