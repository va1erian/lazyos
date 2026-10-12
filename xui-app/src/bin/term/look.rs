//! How the grid looks right now: the font and its metrics, the cursor's
//! shape and phase, whether the window has the focus. The Terminal updates
//! it from settings, zoom, the blink and focus events; the painter reads it.

use lazyterm::font::Styles;
use lazyterm::settings::CursorShape;
use xui_core::app::Ui;
use xui_core::{Color, Dip, TextStyle};

use super::metrics::{Metrics, PROBE_LEN};

#[derive(Clone, Debug)]
pub struct Look {
    pub metrics: Metrics,
    pub family: String,
    /// Which faces of the family exist (a missing bold is drawn by
    /// overstriking the regular one).
    pub styles: Styles,
    pub bold_is_bright: bool,
    pub cursor_shape: CursorShape,
    /// The blink phase: the cursor is drawn.
    pub cursor_on: bool,
    pub focused: bool,
    /// Counts changes of anything that alters every row (font, size, spacing,
    /// scheme), so the damage tracking repaints the whole grid once.
    pub generation: u64,
}

/// What the damage tracking compares between two looks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LookKey {
    pub generation: u64,
    pub cursor_on: bool,
    pub focused: bool,
    pub shape: u8,
}

impl Look {
    pub fn new(metrics: Metrics, family: &str, styles: Styles) -> Look {
        Look {
            metrics,
            family: family.to_string(),
            styles,
            bold_is_bright: false,
            cursor_shape: CursorShape::Block,
            cursor_on: true,
            focused: true,
            generation: 0,
        }
    }

    pub fn key(&self) -> LookKey {
        LookKey {
            generation: self.generation,
            cursor_on: self.cursor_on,
            focused: self.focused,
            shape: self.cursor_shape as u8,
        }
    }

    /// The text style for one run's weight and slant, in `color`.
    pub fn text_style(&self, color: Color, bold: bool, italic: bool) -> TextStyle {
        let mut style = TextStyle::new(color, Dip(self.metrics.size)).family(self.family.clone());
        if bold && self.styles.bold {
            style = style.bold();
        }
        style.italic(italic)
    }
}

/// Measure `family` at `size` design pixels (and the window's scale) with the
/// line `spacing` percent: the grid's metrics.
pub fn measure<M: 'static>(ui: &Ui<M>, family: &str, size: f32, spacing: u32) -> Metrics {
    let dpi = ui.dpi();
    let scale = (dpi / 96).max(1) as i32;
    let style = TextStyle::new(Color::rgb(0, 0, 0), Dip(size)).family(family);
    let probe = ui.measure_text(&"M".repeat(PROBE_LEN), &style, dpi);
    Metrics::new(size, scale, spacing, probe.width, probe.height)
}
