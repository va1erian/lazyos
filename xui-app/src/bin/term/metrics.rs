//! The character cell, from the font. The cell is as wide as the advance of
//! `M` and as tall as the font size times the line spacing, both rounded to
//! whole physical pixels at the UI scale, so a column never lands between
//! pixels and the grid does not drift.

/// How wide a string of `M`s is measured to find the advance (a longer
/// string averages the rounding the measurement does).
pub const PROBE_LEN: usize = 32;

/// The pixel geometry of the grid at one font, size and scale.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Metrics {
    /// The UI scale (1, 2, ...).
    pub scale: i32,
    /// The font size in design pixels.
    pub size: f32,
    /// Cell width and height in physical pixels.
    pub cell_w: i32,
    pub line_h: i32,
    /// The padding around the grid.
    pub pad: i32,
    /// Added to a row's top to place its text (centres the glyph box in the
    /// line; negative when the line is tighter than the font).
    pub text_dy: i32,
    /// A stroke: underline, strike, box-drawing light lines, cursor outline.
    pub light: i32,
    /// Whether a whole run of text can be drawn as one string: the font's
    /// natural advance is a whole number of pixels, so a run of `n` glyphs is
    /// exactly `n` cells wide. Otherwise each glyph is placed in its cell.
    pub exact: bool,
}

/// The design padding around the grid.
pub const PAD: i32 = 10;

impl Metrics {
    /// From a measurement: `probe_width` is the physical width of
    /// [`PROBE_LEN`] `M`s and `text_height` the height of one line of text at
    /// `size` design pixels and `scale`; `spacing` is the line height in
    /// percent of the font size.
    pub fn new(
        size: f32,
        scale: i32,
        spacing: u32,
        probe_width: i32,
        text_height: i32,
    ) -> Metrics {
        let scale = scale.max(1);
        let advance = probe_width as f32 / PROBE_LEN as f32;
        let cell_w = (advance.round() as i32).max(1);
        let line_h = ((size * scale as f32 * spacing as f32 / 100.0).round() as i32).max(1);
        Metrics {
            scale,
            size,
            cell_w,
            line_h,
            pad: PAD * scale,
            text_dy: (line_h - text_height) / 2,
            light: scale.max(line_h / 20),
            exact: probe_width == cell_w * PROBE_LEN as i32,
        }
    }

    /// The text size in physical pixels.
    #[cfg(test)]
    pub fn text_px(&self) -> f32 {
        self.size * self.scale as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jetbrains_mono_at_the_default_size_is_the_old_grid() {
        // 0.6 em * 15 px = 9 px: 32 of them are 288 wide, text is 19 tall.
        let m = Metrics::new(15.0, 1, 130, 288, 19);
        assert_eq!((m.cell_w, m.line_h, m.pad, m.text_dy), (9, 20, 10, 0));
        assert!(m.exact);
        assert_eq!(m.light, 1);
    }

    #[test]
    fn a_fractional_advance_rounds_the_cell_and_places_glyphs_singly() {
        // 0.6 em * 17 px = 10.2 px: 32 of them measure 326.
        let m = Metrics::new(17.0, 1, 130, 326, 21);
        assert_eq!(m.cell_w, 10);
        assert!(!m.exact, "326 is not 32 * 10");
        assert_eq!(m.line_h, 22, "17 * 1.3 rounded");
    }

    #[test]
    fn the_scale_multiplies_pixels_not_design_values() {
        let one = Metrics::new(15.0, 1, 130, 288, 19);
        let two = Metrics::new(15.0, 2, 130, 576, 38);
        assert_eq!((two.cell_w, two.pad), (2 * one.cell_w, 2 * one.pad));
        assert_eq!(two.line_h, 39, "15 * 2 * 1.3, rounded on its own");
        assert_eq!(two.light, 2);
        assert!((two.text_px() - 30.0).abs() < 1e-6);
    }

    #[test]
    fn line_spacing_sets_the_height() {
        for (spacing, expect) in [(100, 15), (130, 20), (150, 23), (200, 30)] {
            assert_eq!(Metrics::new(15.0, 1, spacing, 288, 19).line_h, expect, "{spacing}%");
        }
    }

    #[test]
    fn a_tight_line_centres_the_glyph_box_above_and_below() {
        let m = Metrics::new(15.0, 1, 100, 288, 19);
        assert_eq!(m.text_dy, -2, "the 19 px box in a 15 px line");
    }

    #[test]
    fn degenerate_measurements_never_give_a_zero_cell() {
        let m = Metrics::new(6.0, 1, 100, 0, 0);
        assert_eq!((m.cell_w, m.line_h), (1, 6));
        let m = Metrics::new(0.0, 0, 100, -5, 0);
        assert!(m.cell_w >= 1 && m.line_h >= 1 && m.scale >= 1);
    }

    #[test]
    fn strokes_thicken_with_big_fonts() {
        assert_eq!(Metrics::new(15.0, 1, 130, 288, 19).light, 1);
        assert_eq!(Metrics::new(40.0, 1, 130, 768, 50).light, 2);
        assert_eq!(Metrics::new(72.0, 1, 130, 1382, 90).light, 4);
    }
}
