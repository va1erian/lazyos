//! The Terminal's settings as `confd` keeps them (`user/<uid>/term/...`,
//! docs/terminal-plan.md 3.3): parsing and validating values. Reading the
//! store and following its changes is the host's job; every value that
//! comes in is untrusted and is clamped or replaced by its default here.

use crate::palette::NAMES;

/// A configuration value as the store hands it over.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Text(String),
}

/// The cursor's shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

impl CursorShape {
    pub fn parse(name: &str) -> Option<CursorShape> {
        match name {
            "block" => Some(CursorShape::Block),
            "underline" => Some(CursorShape::Underline),
            "bar" => Some(CursorShape::Bar),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Underline => "underline",
            CursorShape::Bar => "bar",
        }
    }
}

/// What a BEL does.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Bell {
    None,
    #[default]
    Visual,
    Sound,
    Notify,
}

impl Bell {
    pub fn parse(name: &str) -> Option<Bell> {
        match name {
            "none" => Some(Bell::None),
            "visual" => Some(Bell::Visual),
            "sound" => Some(Bell::Sound),
            "notify" => Some(Bell::Notify),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Bell::None => "none",
            Bell::Visual => "visual",
            Bell::Sound => "sound",
            Bell::Notify => "notify",
        }
    }
}

/// The keys the Terminal reads, relative to `user/<uid>/term/`.
pub const KEYS: [&str; 14] = [
    "font/family",
    "font/size",
    "font/line_spacing",
    "cursor/shape",
    "cursor/blink",
    "colors/scheme",
    "colors/bold_is_bright",
    "scrollback/lines",
    "select/copy_on_select",
    "select/word_chars",
    "paste/confirm_multiline",
    "window/size",
    "window/last_size",
    "bell",
];

/// The smallest grid a window may hold, and the largest a setting may ask for.
pub const MIN_COLS: usize = 20;
pub const MIN_ROWS: usize = 5;
pub const MAX_GRID_COLS: usize = 500;
pub const MAX_GRID_ROWS: usize = 200;

/// A grid size written `COLSxROWS` (`80x24`), or `None` when it is not one or
/// is outside the bounds.
pub fn parse_grid(text: &str) -> Option<(usize, usize)> {
    let (cols, rows) = text.trim().split_once(['x', 'X'])?;
    let (cols, rows): (usize, usize) = (cols.trim().parse().ok()?, rows.trim().parse().ok()?);
    ((MIN_COLS..=MAX_GRID_COLS).contains(&cols) && (MIN_ROWS..=MAX_GRID_ROWS).contains(&rows))
        .then_some((cols, rows))
}

pub const MIN_SIZE: i64 = 6;
pub const MAX_SIZE: i64 = 72;
pub const MIN_SPACING: i64 = 100;
pub const MAX_SPACING: i64 = 200;
/// The most scrollback lines a setting may ask for (the limit is memory, and
/// this keeps a typo from asking for all of it).
pub const MAX_SCROLLBACK: i64 = 200_000;
/// The longest family name or word-character string kept.
const MAX_TEXT: usize = 64;

/// The settings in force.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Settings {
    pub family: String,
    /// Font size in design pixels.
    pub size: u32,
    /// Line height, percent of the font size.
    pub line_spacing: u32,
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
    pub scheme: String,
    pub bold_is_bright: bool,
    pub scrollback: usize,
    pub copy_on_select: bool,
    pub word_chars: String,
    pub confirm_multiline: bool,
    /// `window/size`: the initial grid, when set. `window/last_size`: the
    /// grid the window had when it was last closed, used when no size is set.
    pub window_size: Option<(usize, usize)>,
    pub last_size: Option<(usize, usize)>,
    pub bell: Bell,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            family: String::from("JetBrains Mono"),
            size: 15,
            line_spacing: 130,
            cursor_shape: CursorShape::Block,
            cursor_blink: true,
            scheme: String::from("desktop"),
            bold_is_bright: false,
            scrollback: crate::DEFAULT_SCROLLBACK,
            copy_on_select: false,
            word_chars: String::from("-_./~:@"),
            confirm_multiline: true,
            window_size: None,
            last_size: None,
            bell: Bell::Visual,
        }
    }
}

/// Printable text of at most [`MAX_TEXT`] characters, or `None`.
fn clean(text: &str) -> Option<String> {
    let text: String = text.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

impl Settings {
    /// Apply one key (relative to `term/`). A value of the wrong type, or an
    /// unknown key, changes nothing; a value out of range is clamped; an
    /// unknown name falls back to the default. Returns whether the key is one
    /// the Terminal knows.
    pub fn apply(&mut self, key: &str, value: &Value) -> bool {
        let defaults = Settings::default();
        match (key, value) {
            ("font/family", Value::Text(text)) => {
                self.family = clean(text).unwrap_or(defaults.family);
            }
            ("font/size", Value::Int(n)) => {
                self.size = (*n).clamp(MIN_SIZE, MAX_SIZE) as u32;
            }
            ("font/line_spacing", Value::Int(n)) => {
                self.line_spacing = (*n).clamp(MIN_SPACING, MAX_SPACING) as u32;
            }
            ("cursor/shape", Value::Text(name)) => {
                self.cursor_shape = CursorShape::parse(name).unwrap_or_default();
            }
            ("cursor/blink", Value::Bool(on)) => self.cursor_blink = *on,
            ("colors/scheme", Value::Text(name)) => {
                self.scheme = if NAMES.contains(&name.as_str()) {
                    name.clone()
                } else {
                    defaults.scheme
                };
            }
            ("colors/bold_is_bright", Value::Bool(on)) => self.bold_is_bright = *on,
            ("scrollback/lines", Value::Int(n)) => {
                self.scrollback = (*n).clamp(0, MAX_SCROLLBACK) as usize;
            }
            ("select/copy_on_select", Value::Bool(on)) => self.copy_on_select = *on,
            ("select/word_chars", Value::Text(chars)) => {
                // An empty string is a valid choice: letters and digits only.
                self.word_chars = chars
                    .chars()
                    .filter(|c| !c.is_control() && !c.is_whitespace())
                    .take(MAX_TEXT)
                    .collect();
            }
            ("paste/confirm_multiline", Value::Bool(on)) => self.confirm_multiline = *on,
            // An invalid size is no size.
            ("window/size", Value::Text(text)) => self.window_size = parse_grid(text),
            ("window/last_size", Value::Text(text)) => self.last_size = parse_grid(text),
            ("bell", Value::Text(name)) => self.bell = Bell::parse(name).unwrap_or_default(),
            _ => return KEYS.contains(&key),
        }
        true
    }

    /// The grid the window opens with: the setting, else the one remembered.
    pub fn initial_grid(&self) -> Option<(usize, usize)> {
        self.window_size.or(self.last_size)
    }

    /// The effective font size for a zoom of `steps` (two design pixels a
    /// step), kept within the same bounds as the setting.
    pub fn zoomed_size(&self, steps: i32) -> u32 {
        (i64::from(self.size) + 2 * i64::from(steps)).clamp(MIN_SIZE, MAX_SIZE) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    #[test]
    fn defaults_are_the_plan_table() {
        let d = Settings::default();
        assert_eq!((d.family.as_str(), d.size, d.line_spacing), ("JetBrains Mono", 15, 130));
        assert_eq!((d.cursor_shape, d.cursor_blink), (CursorShape::Block, true));
        assert_eq!((d.scheme.as_str(), d.bold_is_bright), ("desktop", false));
        assert_eq!((d.scrollback, d.copy_on_select), (10_000, false));
        assert_eq!((d.word_chars.as_str(), d.confirm_multiline), ("-_./~:@", true));
    }

    #[test]
    fn every_key_is_applied() {
        let mut s = Settings::default();
        let all = [
            ("font/family", text("Liberation Mono")),
            ("font/size", Value::Int(20)),
            ("font/line_spacing", Value::Int(150)),
            ("cursor/shape", text("bar")),
            ("cursor/blink", Value::Bool(false)),
            ("colors/scheme", text("tango")),
            ("colors/bold_is_bright", Value::Bool(true)),
            ("scrollback/lines", Value::Int(500)),
            ("select/copy_on_select", Value::Bool(true)),
            ("select/word_chars", text("_.")),
            ("paste/confirm_multiline", Value::Bool(false)),
            ("window/size", text("100x30")),
            ("window/last_size", text("64x20")),
            ("bell", text("notify")),
        ];
        assert_eq!(all.len(), KEYS.len());
        for (key, value) in &all {
            assert!(s.apply(key, value), "{key}");
        }
        assert_eq!(s.family, "Liberation Mono");
        assert_eq!((s.size, s.line_spacing, s.scrollback), (20, 150, 500));
        assert_eq!((s.cursor_shape, s.cursor_blink, s.scheme.as_str()), (CursorShape::Bar, false, "tango"));
        assert!(s.bold_is_bright && s.copy_on_select && !s.confirm_multiline);
        assert_eq!(s.word_chars, "_.");
        assert_eq!((s.window_size, s.last_size), (Some((100, 30)), Some((64, 20))));
        assert_eq!(s.bell, Bell::Notify);
        s.apply("bell", &text("loud"));
        assert_eq!(s.bell, Bell::Visual, "an unknown name is the default");
    }

    #[test]
    fn grid_sizes_are_validated() {
        assert_eq!(parse_grid("80x24"), Some((80, 24)));
        assert_eq!(parse_grid(" 120 X 40 "), Some((120, 40)));
        for bad in ["", "80", "80x", "x24", "19x24", "80x4", "501x24", "80x201", "-80x24", "80x24x1", "eighty x 24", "80*24"] {
            assert_eq!(parse_grid(bad), None, "{bad:?}");
        }
        let mut s = Settings::default();
        s.apply("window/size", &text("100x30"));
        s.apply("window/last_size", &text("90x25"));
        assert_eq!(s.initial_grid(), Some((100, 30)), "the setting wins");
        s.apply("window/size", &text("nonsense"));
        assert_eq!(s.initial_grid(), Some((90, 25)), "an invalid size is no size");
        s.apply("window/last_size", &Value::Int(5));
        assert_eq!(s.initial_grid(), Some((90, 25)), "the wrong type changes nothing");
    }

    #[test]
    fn numbers_are_clamped() {
        let mut s = Settings::default();
        s.apply("font/size", &Value::Int(-5));
        assert_eq!(s.size, 6);
        s.apply("font/size", &Value::Int(1 << 40));
        assert_eq!(s.size, 72);
        s.apply("font/line_spacing", &Value::Int(0));
        assert_eq!(s.line_spacing, 100);
        s.apply("font/line_spacing", &Value::Int(i64::MAX));
        assert_eq!(s.line_spacing, 200);
        s.apply("scrollback/lines", &Value::Int(-1));
        assert_eq!(s.scrollback, 0, "zero disables, negative is zero");
        s.apply("scrollback/lines", &Value::Int(i64::MAX));
        assert_eq!(s.scrollback, MAX_SCROLLBACK as usize);
    }

    #[test]
    fn unknown_names_are_the_default() {
        let mut s = Settings::default();
        s.apply("cursor/shape", &text("bar"));
        s.apply("cursor/shape", &text("sparkle"));
        assert_eq!(s.cursor_shape, CursorShape::Block);
        s.apply("colors/scheme", &text("tango"));
        s.apply("colors/scheme", &text("no such scheme"));
        assert_eq!(s.scheme, "desktop");
        s.apply("font/family", &text("  \u{7}  "));
        assert_eq!(s.family, "JetBrains Mono", "a blank family is the default");
    }

    #[test]
    fn the_wrong_type_or_an_unknown_key_changes_nothing() {
        let mut s = Settings::default();
        assert!(s.apply("font/size", &text("big")), "known key, ignored value");
        assert!(s.apply("cursor/blink", &Value::Int(1)));
        assert!(!s.apply("font/colour", &Value::Int(1)), "unknown key");
        assert!(!s.apply("../../sys/ui/mode", &text("x")));
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn text_is_bounded_and_clean() {
        let mut s = Settings::default();
        s.apply("font/family", &text(&"x".repeat(1000)));
        assert_eq!(s.family.len(), 64);
        s.apply("font/family", &text("Mono\nSpace\u{1b}[2J"));
        assert!(!s.family.chars().any(|c| c.is_control()));
        s.apply("select/word_chars", &text("a b\t-\n"));
        assert_eq!(s.word_chars, "ab-", "no blanks or controls in word characters");
        s.apply("select/word_chars", &text(""));
        assert_eq!(s.word_chars, "");
    }

    #[test]
    fn zoom_steps_stay_within_the_bounds() {
        let s = Settings::default();
        assert_eq!(s.zoomed_size(0), 15);
        assert_eq!(s.zoomed_size(2), 19);
        assert_eq!(s.zoomed_size(-2), 11);
        assert_eq!(s.zoomed_size(-100), 6);
        assert_eq!(s.zoomed_size(1000), 72);
        assert_eq!(s.zoomed_size(i32::MAX), 72);
        assert_eq!(s.zoomed_size(i32::MIN), 6);
    }
}
