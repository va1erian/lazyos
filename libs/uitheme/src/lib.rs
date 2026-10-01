//! Desktop theme schema (`sys/ui/*` in confd) and palette resolution.
//!
//! One definition of the keys and their meaning, shared by `xuid` (which
//! paints from the resolved [`Palette`]) and the Settings app (which writes
//! the keys), so neither hand-copies the schema. Pure `no_std` logic with host
//! tests.
//!
//! A [`Settings`] holds the mode preset plus optional per-colour overrides.
//! [`resolve`] turns it into the concrete [`Palette`]: the accent drives the
//! focused title, focused taskbar entry, focus border and overlay selection
//! unless a more specific override is set.

#![cfg_attr(not(test), no_std)]

use confd::Value;

/// Prefix of every key; `system/confd/changed/sys/ui/#` follows changes.
pub const PREFIX: &str = "sys/ui";
pub const KEY_MODE: &str = "sys/ui/mode";
pub const KEY_BG: &str = "sys/ui/bg";
pub const KEY_ACCENT: &str = "sys/ui/accent";
pub const KEY_TITLE_ACTIVE: &str = "sys/ui/title_active";
pub const KEY_TITLE_INACTIVE: &str = "sys/ui/title_inactive";
pub const KEY_TASKBAR: &str = "sys/ui/taskbar";
pub const KEY_ANIM: &str = "sys/ui/anim";

/// Every colour override key (a mode change clears these).
pub const COLOR_KEYS: [&str; 5] = [
    KEY_BG,
    KEY_ACCENT,
    KEY_TITLE_ACTIVE,
    KEY_TITLE_INACTIVE,
    KEY_TASKBAR,
];
/// Every key, in the order the app lists them.
pub const ALL_KEYS: [&str; 7] = [
    KEY_MODE,
    KEY_BG,
    KEY_ACCENT,
    KEY_TITLE_ACTIVE,
    KEY_TITLE_INACTIVE,
    KEY_TASKBAR,
    KEY_ANIM,
];

/// Dark or light preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Dark => "dark",
            Mode::Light => "light",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "dark" => Some(Mode::Dark),
            "light" => Some(Mode::Light),
            _ => None,
        }
    }
}

/// The stored settings: a preset plus optional overrides (`0xRRGGBB`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    pub bg: Option<u32>,
    pub accent: Option<u32>,
    pub title_active: Option<u32>,
    pub title_inactive: Option<u32>,
    pub taskbar: Option<u32>,
    pub anim: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            mode: Mode::Dark,
            bg: None,
            accent: None,
            title_active: None,
            title_inactive: None,
            taskbar: None,
            anim: true,
        }
    }
}

fn color(value: Option<&Value>) -> Option<u32> {
    match value {
        Some(Value::U64(n)) if *n <= 0x00FF_FFFF => Some(*n as u32),
        _ => None,
    }
}

impl Settings {
    /// Apply one confd entry (`None` = deleted). Unknown keys and values of
    /// the wrong kind fall back to the default, so a bad write can never
    /// break the desktop.
    pub fn apply(&mut self, path: &str, value: Option<&Value>) {
        match path {
            KEY_MODE => {
                self.mode = match value {
                    Some(Value::Str(s)) => Mode::parse(s).unwrap_or(Mode::Dark),
                    _ => Mode::Dark,
                }
            }
            KEY_BG => self.bg = color(value),
            KEY_ACCENT => self.accent = color(value),
            KEY_TITLE_ACTIVE => self.title_active = color(value),
            KEY_TITLE_INACTIVE => self.title_inactive = color(value),
            KEY_TASKBAR => self.taskbar = color(value),
            KEY_ANIM => {
                self.anim = match value {
                    Some(Value::Bool(b)) => *b,
                    _ => true,
                }
            }
            _ => {}
        }
    }
}

/// Concrete colours (`0xRRGGBB`) for every themed surface, plus the mode and
/// accent they were resolved from (reported to apps by `GetTheme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub mode: Mode,
    pub accent: u32,
    pub background: u32,
    pub window_bg: u32,
    pub title_bg: u32,
    pub title_bg_focus: u32,
    /// Text on the inactive title bar ([`text_on`] its background).
    pub title_text: u32,
    /// Text on the focused title bar ([`text_on`] its background).
    pub title_text_focus: u32,
    pub border: u32,
    pub border_focus: u32,
    pub empty_bg: u32,
    pub taskbar_bg: u32,
    pub taskbar_entry: u32,
    pub taskbar_entry_min: u32,
    pub taskbar_entry_focus: u32,
    pub overlay_bg: u32,
    pub overlay_border: u32,
    pub overlay_selected: u32,
    pub overlay_text: u32,
    /// Ink for text on the empty-window placeholder.
    pub empty_text: u32,
}

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    (r << 16) | (g << 8) | b
}

/// The compiled-in accent (the original green).
pub const DEFAULT_ACCENT: u32 = rgb(44, 112, 74);

fn channel(color: u32, shift: u32) -> u32 {
    (color >> shift) & 0xFF
}

/// Text drawn on a light surface (the light preset's ink).
pub const DARK_TEXT: u32 = rgb(20, 24, 36);
/// Text drawn on a dark surface (the dark preset's ink).
pub const LIGHT_TEXT: u32 = rgb(228, 232, 245);
/// Perceived brightness (`0..=255`) at or above which a surface takes
/// [`DARK_TEXT`]. A little above the midpoint, because saturated mid-tone
/// accents (green, orange, red) read better with light text.
const LIGHT_SURFACE: u32 = 150;

/// Perceived brightness of `color` (ITU-R BT.601 luma), `0..=255`.
pub fn luma(color: u32) -> u32 {
    (299 * channel(color, 16) + 587 * channel(color, 8) + 114 * channel(color, 0)) / 1000
}

/// The text colour that stays readable on `background`: dark ink on light
/// surfaces, light ink on dark ones. A user-chosen accent or title colour can
/// be anything, so chrome text is always picked from what it sits on rather
/// than from the mode.
pub fn text_on(background: u32) -> u32 {
    if luma(background) >= LIGHT_SURFACE {
        DARK_TEXT
    } else {
        LIGHT_TEXT
    }
}

/// Blend `color` toward `toward` by `num/den`.
pub fn mix(color: u32, toward: u32, num: u32, den: u32) -> u32 {
    let one = |shift| {
        let a = channel(color, shift);
        let b = channel(toward, shift);
        (a * (den - num) + b * num) / den
    };
    (one(16) << 16) | (one(8) << 8) | one(0)
}

/// The neutral (non-accent) surfaces of one mode.
struct Base {
    background: u32,
    window_bg: u32,
    title: u32,
    /// Ink for the mode's own neutral surfaces (the empty-window placeholder).
    text: u32,
    border: u32,
    empty: u32,
    taskbar: u32,
    entry: u32,
    entry_min: u32,
    overlay_bg: u32,
    overlay_border: u32,
    overlay_text: u32,
}

const DARK: Base = Base {
    background: rgb(18, 22, 36),
    window_bg: rgb(30, 36, 54),
    title: rgb(52, 60, 92),
    text: LIGHT_TEXT,
    border: rgb(92, 106, 152),
    empty: rgb(16, 18, 28),
    taskbar: rgb(24, 28, 44),
    entry: rgb(52, 60, 92),
    entry_min: rgb(38, 44, 66),
    overlay_bg: rgb(20, 24, 38),
    overlay_border: rgb(122, 138, 196),
    overlay_text: rgb(220, 226, 240),
};

const LIGHT: Base = Base {
    background: rgb(214, 220, 232),
    window_bg: rgb(244, 246, 250),
    title: rgb(150, 160, 190),
    text: DARK_TEXT,
    border: rgb(120, 132, 168),
    empty: rgb(232, 235, 242),
    taskbar: rgb(196, 203, 220),
    entry: rgb(170, 180, 205),
    entry_min: rgb(186, 194, 214),
    overlay_bg: rgb(238, 241, 248),
    overlay_border: rgb(110, 124, 170),
    overlay_text: rgb(24, 28, 40),
};

/// Resolve `settings` into the palette `xuid` paints with.
pub fn resolve(settings: &Settings) -> Palette {
    let accent = settings.accent.unwrap_or(DEFAULT_ACCENT);
    let base = match settings.mode {
        Mode::Dark => &DARK,
        Mode::Light => &LIGHT,
    };
    let title_bg = settings.title_inactive.unwrap_or(base.title);
    let title_bg_focus = settings.title_active.unwrap_or(accent);
    Palette {
        mode: settings.mode,
        accent,
        background: settings.bg.unwrap_or(base.background),
        window_bg: base.window_bg,
        title_bg,
        title_bg_focus,
        title_text: text_on(title_bg),
        title_text_focus: text_on(title_bg_focus),
        border: base.border,
        border_focus: mix(accent, rgb(255, 255, 255), 1, 2),
        empty_bg: base.empty,
        taskbar_bg: settings.taskbar.unwrap_or(base.taskbar),
        taskbar_entry: base.entry,
        taskbar_entry_min: base.entry_min,
        taskbar_entry_focus: accent,
        overlay_bg: base.overlay_bg,
        overlay_border: base.overlay_border,
        overlay_selected: accent,
        overlay_text: base.overlay_text,
        empty_text: base.text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Value {
        Value::Str(s.into())
    }

    #[test]
    fn default_dark_matches_the_original_palette() {
        let p = resolve(&Settings::default());
        assert_eq!(p.background, rgb(18, 22, 36));
        assert_eq!(p.title_bg, rgb(52, 60, 92));
        assert_eq!(p.title_bg_focus, rgb(44, 112, 74));
        assert_eq!(p.taskbar_bg, rgb(24, 28, 44));
        assert_eq!(p.taskbar_entry_focus, rgb(44, 112, 74));
    }

    #[test]
    fn light_differs_from_dark() {
        let light = Settings {
            mode: Mode::Light,
            ..Settings::default()
        };
        assert_ne!(
            resolve(&light).window_bg,
            resolve(&Settings::default()).window_bg
        );
    }

    #[test]
    fn accent_drives_focus_surfaces_and_overrides_win() {
        let mut s = Settings {
            accent: Some(0x336699),
            ..Settings::default()
        };
        let p = resolve(&s);
        assert_eq!(p.title_bg_focus, 0x336699);
        assert_eq!(p.overlay_selected, 0x336699);
        s.title_active = Some(0xAA0000);
        let p = resolve(&s);
        assert_eq!(p.title_bg_focus, 0xAA0000);
        assert_eq!(p.taskbar_entry_focus, 0x336699);
    }

    #[test]
    fn apply_reads_and_clears_keys() {
        let mut s = Settings::default();
        s.apply(KEY_MODE, Some(&text("light")));
        s.apply(KEY_BG, Some(&Value::U64(0x102030)));
        s.apply(KEY_ANIM, Some(&Value::Bool(false)));
        assert_eq!(s.mode, Mode::Light);
        assert_eq!(s.bg, Some(0x102030));
        assert!(!s.anim);
        s.apply(KEY_BG, None);
        s.apply(KEY_ANIM, None);
        assert_eq!(s.bg, None);
        assert!(s.anim);
    }

    #[test]
    fn apply_ignores_bad_input() {
        let mut s = Settings::default();
        s.apply(KEY_BG, Some(&Value::U64(0x1_0000_0000)));
        s.apply(KEY_BG, Some(&Value::Bool(true)));
        s.apply(KEY_MODE, Some(&text("neon")));
        s.apply("sys/ui/unknown", Some(&Value::U64(1)));
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn title_text_follows_its_background_not_the_mode() {
        // The issue: light mode with the default green accent drew dark text
        // on the dark focused title bar.
        let light = resolve(&Settings {
            mode: Mode::Light,
            ..Settings::default()
        });
        assert_eq!(light.title_text_focus, LIGHT_TEXT);
        assert_eq!(light.title_text, DARK_TEXT);
        let dark = resolve(&Settings::default());
        assert_eq!(
            (dark.title_text, dark.title_text_focus),
            (LIGHT_TEXT, LIGHT_TEXT)
        );
        // A pale custom title takes dark ink even in dark mode.
        let pale = resolve(&Settings {
            title_active: Some(0xDCCEAA),
            ..Settings::default()
        });
        assert_eq!(pale.title_text_focus, DARK_TEXT);
    }

    #[test]
    fn text_on_picks_the_higher_contrast_ink() {
        assert_eq!(text_on(0x000000), LIGHT_TEXT);
        assert_eq!(text_on(0xFFFFFF), DARK_TEXT);
        assert_eq!(text_on(DEFAULT_ACCENT), LIGHT_TEXT);
        assert_eq!(luma(0xFFFFFF), 255);
        assert_eq!(luma(0x000000), 0);
    }

    #[test]
    fn mix_endpoints() {
        assert_eq!(mix(0x102030, 0xFFFFFF, 0, 2), 0x102030);
        assert_eq!(mix(0x102030, 0xFFFFFF, 2, 2), 0xFFFFFF);
    }
}
