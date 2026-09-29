#![forbid(unsafe_code)]

//! The set's colour vocabulary: a [`Tone`] names a role, a [`Palette`] gives
//! each role a colour.

use xui_core::backend::Rgba;

/// A named colour role an icon shape is filled or stroked with.
///
/// The roles are fixed by the artwork; [`Palette`] decides what each one looks
/// like, so an app can retint the whole set (or just its outline) in one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// The dark outline and details.
    Ink,
    /// Warm off-white: paper, plastic, highlights on dark parts.
    Cream,
    /// Teal accent.
    Teal,
    /// Rose accent.
    Rose,
    /// Amber accent.
    Amber,
    /// Cobalt accent.
    Cobalt,
    /// Terracotta accent.
    Clay,
    /// Pale lavender-grey metal and plastic.
    Lavender,
    /// A dark screen or speaker cone.
    Screen,
    /// A pale glass tint.
    Mint,
    /// The night half of the theme icon.
    Night,
    /// A translucent glint drawn over a filled area.
    Shine,
    /// Bright glyph colour on a coloured badge.
    Chalk,
}

impl Tone {
    const COUNT: usize = 13;
}

/// A colour for every [`Tone`].
///
/// ```
/// use xui_core::backend::Rgba;
/// use xui_icons::{Palette, Tone};
///
/// let palette = Palette::GLOBAL_VILLAGE.with(Tone::Ink, Rgba::rgb(0xFF, 0xFF, 0xFF));
/// assert_eq!(palette.get(Tone::Ink), Rgba::rgb(0xFF, 0xFF, 0xFF));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    colors: [Rgba; Tone::COUNT],
}

impl Palette {
    /// The set's own colours: deep indigo ink with cream, teal, rose, amber,
    /// cobalt and clay.
    pub const GLOBAL_VILLAGE: Palette = Palette {
        colors: [
            Rgba::rgb(0x0B, 0x08, 0x22),
            Rgba::rgb(0xF1, 0xE3, 0xBD),
            Rgba::rgb(0x19, 0xB5, 0xA5),
            Rgba::rgb(0xE2, 0x36, 0x6F),
            Rgba::rgb(0xF4, 0xA8, 0x1D),
            Rgba::rgb(0x41, 0x62, 0xE0),
            Rgba::rgb(0xD4, 0x56, 0x2F),
            Rgba::rgb(0x9A, 0x93, 0xD0),
            Rgba::rgb(0x1B, 0x15, 0x52),
            Rgba::rgb(0x9F, 0xE6, 0xDD),
            Rgba::rgb(0x22, 0x1A, 0x66),
            Rgba::with_alpha(0xFF, 0xF6, 0xDC, 0xD9),
            Rgba::rgb(0xFF, 0xF2, 0xD0),
        ],
    };

    /// The colour of `tone`.
    pub const fn get(&self, tone: Tone) -> Rgba {
        self.colors[tone as usize]
    }

    /// This palette with `tone` set to `color`.
    #[must_use]
    pub const fn with(mut self, tone: Tone, color: Rgba) -> Palette {
        self.colors[tone as usize] = color;
        self
    }
}

impl Default for Palette {
    fn default() -> Palette {
        Palette::GLOBAL_VILLAGE
    }
}
