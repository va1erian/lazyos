#![forbid(unsafe_code)]

//! The editor's palette, derived from xui's semantic [`Theme`] tokens.
//!
//! Deriving every colour from the theme means the editor gets light and dark
//! mode for free.

use xui_core::Color;
use xui_core::theme::Theme;

use crate::lexer::TokenClass;

/// The colours the editor paints with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorTheme {
    /// The text area background.
    pub background: Color,
    /// Body text.
    pub text: Color,
    /// The gutter's background.
    pub gutter_background: Color,
    /// Line numbers and gutter marks.
    pub gutter_text: Color,
    /// The current line's highlight.
    pub current_line: Color,
    /// The selection fill while the editor has focus.
    pub selection: Color,
    /// The selection fill while it does not.
    pub selection_unfocused: Color,
    /// The caret.
    pub caret: Color,
    /// The editor's border.
    pub border: Color,
    /// The border while focused.
    pub border_focused: Color,
    /// Error markers (squiggles and line tints).
    pub error: Color,
    /// Warning markers.
    pub warning: Color,
    /// A breakpoint dot in the gutter.
    pub breakpoint: Color,
    /// The scrollbar thumb.
    pub scrollbar: Color,
    /// The scrollbar track.
    pub scrollbar_track: Color,
    /// Keywords.
    pub keyword: Color,
    /// Identifiers and other plain names.
    pub identifier: Color,
    /// Numeric literals.
    pub number: Color,
    /// String literals.
    pub string: Color,
    /// Interpolated segments inside back-tick strings.
    pub interpolation: Color,
    /// Comments.
    pub comment: Color,
    /// Doc comments.
    pub doc_comment: Color,
    /// Operators.
    pub operator: Color,
    /// Brackets and separators.
    pub punctuation: Color,
    /// Function names.
    pub function: Color,
    /// The fill behind a matched bracket pair.
    pub bracket_match: Color,
}

impl EditorTheme {
    /// Derives the palette from xui's semantic tokens.
    pub fn from_theme(theme: Theme) -> EditorTheme {
        EditorTheme {
            background: theme.input_background,
            text: theme.text,
            gutter_background: theme.surface,
            gutter_text: theme.text_secondary,
            current_line: theme.hover,
            selection: theme.selection,
            selection_unfocused: theme.selection_unfocused,
            caret: theme.text,
            border: theme.input_border,
            border_focused: theme.border_focused,
            error: theme.danger,
            warning: theme.warning,
            breakpoint: theme.danger,
            scrollbar: theme.scrollbar,
            scrollbar_track: theme.scrollbar_track,
            // xui has no syntax-specific tokens, so the classes are derived
            // from its semantic ones. Blending keeps related classes (keyword
            // and function, string and interpolation) visually distinct while
            // still following the theme in both light and dark modes.
            keyword: theme.accent,
            identifier: theme.text,
            number: theme.warning,
            string: theme.danger,
            interpolation: theme.warning.lerp(theme.accent, 0.5),
            comment: theme.text_disabled,
            doc_comment: theme.accent.lerp(theme.text_secondary, 0.4),
            operator: theme.text_secondary,
            punctuation: theme.text,
            function: theme.accent.lerp(theme.background, 0.2),
            bracket_match: theme.selection,
        }
    }

    /// The colour for a lexical class.
    pub fn token_color(&self, class: TokenClass) -> Color {
        match class {
            TokenClass::Keyword => self.keyword,
            TokenClass::Identifier => self.identifier,
            TokenClass::Number => self.number,
            TokenClass::String => self.string,
            TokenClass::Interpolation => self.interpolation,
            TokenClass::Comment => self.comment,
            TokenClass::DocComment => self.doc_comment,
            TokenClass::Operator => self.operator,
            TokenClass::Punctuation => self.punctuation,
            TokenClass::Function => self.function,
        }
    }
}

impl Default for EditorTheme {
    fn default() -> EditorTheme {
        EditorTheme::from_theme(Theme::light())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_palettes_differ() {
        let light = EditorTheme::from_theme(Theme::light());
        let dark = EditorTheme::from_theme(Theme::dark());
        assert_ne!(light.text, dark.text);
        assert_ne!(light.background, dark.background);
    }

    #[test]
    fn the_selection_uses_the_theme_selection_token() {
        let theme = Theme::light();
        assert_eq!(EditorTheme::from_theme(theme).selection, theme.selection);
    }

    #[test]
    fn every_token_class_has_a_colour() {
        let theme = EditorTheme::default();
        let classes = [
            TokenClass::Keyword,
            TokenClass::Identifier,
            TokenClass::Number,
            TokenClass::String,
            TokenClass::Interpolation,
            TokenClass::Comment,
            TokenClass::DocComment,
            TokenClass::Operator,
            TokenClass::Punctuation,
            TokenClass::Function,
        ];
        for class in classes {
            let _ = theme.token_color(class);
        }
    }

    #[test]
    fn the_syntax_palette_follows_the_light_and_dark_theme() {
        let light = EditorTheme::from_theme(Theme::light());
        let dark = EditorTheme::from_theme(Theme::dark());
        for class in [
            TokenClass::Keyword,
            TokenClass::Identifier,
            TokenClass::Number,
            TokenClass::String,
            TokenClass::Interpolation,
            TokenClass::Comment,
            TokenClass::DocComment,
            TokenClass::Operator,
            TokenClass::Punctuation,
            TokenClass::Function,
        ] {
            assert_ne!(
                light.token_color(class),
                dark.token_color(class),
                "{class:?} should differ between light and dark"
            );
        }
        assert_ne!(light.bracket_match, dark.bracket_match);
    }
}
