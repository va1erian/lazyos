//! One screen cell: a character, how many columns it covers, and the
//! attributes it was drawn with. Attributes are stored faithfully; painting
//! them is the host's business (the Terminal paints in one colour until T3).

/// A foreground or background colour as a program set it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Color {
    /// The scheme's default foreground or background.
    #[default]
    Default,
    /// An index into the 256-colour palette (0-15 are the ANSI colours).
    Indexed(u8),
    /// A 24-bit colour (`38;2;r;g;b`).
    Rgb(u8, u8, u8),
}

/// SGR attribute bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Flags(pub u16);

impl Flags {
    pub const BOLD: u16 = 1;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const INVERSE: u16 = 1 << 4;
    pub const HIDDEN: u16 = 1 << 5;
    pub const STRIKE: u16 = 1 << 6;

    pub fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    pub fn set(&mut self, bit: u16, on: bool) {
        if on {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
}

/// The graphic rendition of a cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Attr {
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
}

impl Attr {
    /// What an erase leaves behind: the current background, nothing else
    /// (xterm's "background colour erase").
    pub fn erased(self) -> Attr {
        Attr {
            fg: Color::Default,
            bg: self.bg,
            flags: Flags::default(),
        }
    }
}

/// A character cell. `width` is 1 for an ordinary character, 2 for the left
/// half of a wide one, and 0 for its right half (the "spacer", whose `ch` is a
/// blank). A width-0 cell in the last column with no wide character before it
/// is the pad left when a wide character did not fit and wrapped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub width: u8,
    pub attr: Attr,
}

impl Cell {
    pub fn blank(attr: Attr) -> Cell {
        Cell {
            ch: ' ',
            width: 1,
            attr: attr.erased(),
        }
    }

    /// Whether the cell carries text (not a spacer).
    pub fn is_glyph(&self) -> bool {
        self.width != 0
    }
}

impl Default for Cell {
    fn default() -> Cell {
        Cell::blank(Attr::default())
    }
}
