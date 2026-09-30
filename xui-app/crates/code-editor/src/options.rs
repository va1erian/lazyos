#![forbid(unsafe_code)]

//! Editor options: the tab width, gutter visibility and font.
//!
//! The font is a single monospace face at a fixed size: the editor is a
//! monospace grid, with no wrapping and no proportional fonts.

use xui_core::Color;
use xui_core::backend::TextStyle;
use xui_core::units::Dip;

/// The font the editor draws with.
#[derive(Clone, Debug, PartialEq)]
pub struct FontConfig {
    /// The family, or `None` for the backend's default UI font. A monospace
    /// family should be set for a real editor window.
    pub family: Option<String>,
    /// The size as a design value.
    pub size: Dip,
}

impl Default for FontConfig {
    fn default() -> FontConfig {
        FontConfig {
            family: None,
            size: Dip(12.0),
        }
    }
}

impl FontConfig {
    /// A [`TextStyle`] in `color` for this font, used for both measuring and
    /// drawing.
    pub fn style(&self, color: Color) -> TextStyle {
        let mut style = TextStyle::new(color, self.size);
        if let Some(family) = &self.family {
            style = style.family(family.clone());
        }
        style
    }
}

/// The editor's display options.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The number of spaces a tab advances to.
    pub tab_width: usize,
    /// Whether the line-number gutter is shown.
    pub show_gutter: bool,
    /// The font.
    pub font: FontConfig,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            tab_width: 4,
            show_gutter: true,
            font: FontConfig::default(),
        }
    }
}

impl Options {
    /// The string one indent level inserts.
    pub fn indent(&self) -> String {
        " ".repeat(self.tab_width.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_indent_is_the_tab_width_in_spaces() {
        let options = Options {
            tab_width: 4,
            ..Options::default()
        };
        assert_eq!(options.indent(), "    ");
    }

    #[test]
    fn a_family_becomes_a_text_style_family() {
        let font = FontConfig {
            family: Some("Consolas".into()),
            size: Dip(14.0),
        };
        let style = font.style(Color::rgb(0, 0, 0));
        assert_eq!(style.family.as_deref(), Some("Consolas"));
        assert_eq!(style.size, Dip(14.0));
    }
}
