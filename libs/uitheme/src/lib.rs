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

mod scale;
pub use scale::*;

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
/// sRGB channel value -> linear light, scaled to `0..=65535` (the WCAG
/// transfer curve, precomputed: `no_std` has no `powf`).
#[rustfmt::skip]
const LINEAR: [u16; 256] = [
    0, 20, 40, 60, 80, 99, 119, 139, 159, 179, 199, 219,
    241, 264, 288, 313, 340, 367, 396, 427, 458, 491, 526, 562,
    599, 637, 677, 718, 761, 805, 851, 898, 947, 997, 1048, 1101,
    1156, 1212, 1270, 1330, 1391, 1453, 1517, 1583, 1651, 1720, 1790, 1863,
    1937, 2013, 2090, 2170, 2250, 2333, 2418, 2504, 2592, 2681, 2773, 2866,
    2961, 3058, 3157, 3258, 3360, 3464, 3570, 3678, 3788, 3900, 4014, 4129,
    4247, 4366, 4488, 4611, 4736, 4864, 4993, 5124, 5257, 5392, 5530, 5669,
    5810, 5953, 6099, 6246, 6395, 6547, 6700, 6856, 7014, 7174, 7335, 7500,
    7666, 7834, 8004, 8177, 8352, 8528, 8708, 8889, 9072, 9258, 9445, 9635,
    9828, 10022, 10219, 10417, 10619, 10822, 11028, 11235, 11446, 11658, 11873, 12090,
    12309, 12530, 12754, 12980, 13209, 13440, 13673, 13909, 14146, 14387, 14629, 14874,
    15122, 15371, 15623, 15878, 16135, 16394, 16656, 16920, 17187, 17456, 17727, 18001,
    18277, 18556, 18837, 19121, 19407, 19696, 19987, 20281, 20577, 20876, 21177, 21481,
    21787, 22096, 22407, 22721, 23038, 23357, 23678, 24002, 24329, 24658, 24990, 25325,
    25662, 26001, 26344, 26688, 27036, 27386, 27739, 28094, 28452, 28813, 29176, 29542,
    29911, 30282, 30656, 31033, 31412, 31794, 32179, 32567, 32957, 33350, 33745, 34143,
    34544, 34948, 35355, 35764, 36176, 36591, 37008, 37429, 37852, 38278, 38706, 39138,
    39572, 40009, 40449, 40891, 41337, 41785, 42236, 42690, 43147, 43606, 44069, 44534,
    45002, 45473, 45947, 46423, 46903, 47385, 47871, 48359, 48850, 49344, 49841, 50341,
    50844, 51349, 51858, 52369, 52884, 53401, 53921, 54445, 54971, 55500, 56032, 56567,
    57105, 57646, 58190, 58737, 59287, 59840, 60396, 60955, 61517, 62082, 62650, 63221,
    63795, 64372, 64952, 65535
];

/// WCAG relative luminance of `color`, scaled to `0..=65535`.
pub fn relative_luminance(color: u32) -> u32 {
    let lin = |shift| u32::from(LINEAR[channel(color, shift) as usize]);
    (2126 * lin(16) + 7152 * lin(8) + 722 * lin(0)) / 10_000
}

/// `true` when `a` on `b` has more contrast than `c` on `b`, comparing WCAG
/// ratios `(L1 + 0.05) / (L2 + 0.05)` without division.
fn more_contrast(a: u32, c: u32, background: u32) -> bool {
    // 0.05 in the 0..=65535 scale.
    const FLARE: u64 = 3277;
    let ratio = |ink: u32| {
        let (ink, bg) = (relative_luminance(ink), relative_luminance(background));
        let (hi, lo) = (
            u64::from(ink.max(bg)) + FLARE,
            u64::from(ink.min(bg)) + FLARE,
        );
        (hi, lo)
    };
    let ((a_hi, a_lo), (c_hi, c_lo)) = (ratio(a), ratio(c));
    a_hi * c_lo > c_hi * a_lo
}

/// The text colour that stays readable on `background`: whichever of
/// [`DARK_TEXT`] and [`LIGHT_TEXT`] has the higher WCAG contrast ratio on it.
/// A user-chosen accent or title colour can be anything, so chrome text is
/// always picked from what it sits on rather than from the mode.
pub fn text_on(background: u32) -> u32 {
    if more_contrast(DARK_TEXT, LIGHT_TEXT, background) {
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

/// Midnight (docs/xui-theme-proposals.md, B): the navy of xui's dark widget
/// theme, so the chrome and the window contents read as one surface.
const DARK: Base = Base {
    background: rgb(18, 24, 49),
    window_bg: rgb(35, 42, 64),
    title: rgb(50, 59, 92),
    text: LIGHT_TEXT,
    border: rgb(59, 69, 102),
    empty: rgb(16, 18, 28),
    taskbar: rgb(22, 27, 44),
    entry: rgb(42, 49, 80),
    entry_min: rgb(32, 38, 60),
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
        assert_eq!(p.background, rgb(18, 24, 49));
        assert_eq!(p.title_bg, rgb(50, 59, 92));
        assert_eq!(p.title_bg_focus, rgb(44, 112, 74));
        assert_eq!(p.taskbar_bg, rgb(22, 27, 44));
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
        assert_eq!(relative_luminance(0xFFFFFF), 65535);
        assert_eq!(relative_luminance(0x000000), 0);
    }

    /// WCAG ratio of `ink` on `background`, for the regression checks.
    fn ratio(ink: u32, background: u32) -> f64 {
        let l = |c| f64::from(relative_luminance(c)) / 65535.0;
        let (a, b) = (l(ink), l(background));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn mid_tone_accents_get_the_readable_ink() {
        // Orange has BT.601 luma ~127 but takes dark ink (~4.7:1, light ~3.1:1).
        assert_eq!(text_on(0xC46E1E), DARK_TEXT);
        // Every preset and some saturated custom colours: the chosen ink is
        // never the lower-contrast one, and the presets meet WCAG AA (4.5:1).
        for accent in [DEFAULT_ACCENT, 0x336699, 0xC46E1E, 0x6E46A0, 0xAA3232] {
            assert!(ratio(text_on(accent), accent) >= 4.5, "{accent:06x}");
        }
        for color in [
            0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0x00FFFF, 0xFF00FF, 0x808080,
        ] {
            let other = if text_on(color) == DARK_TEXT {
                LIGHT_TEXT
            } else {
                DARK_TEXT
            };
            assert!(
                ratio(text_on(color), color) >= ratio(other, color),
                "{color:06x}"
            );
        }
    }

    #[test]
    fn mix_endpoints() {
        assert_eq!(mix(0x102030, 0xFFFFFF, 0, 2), 0x102030);
        assert_eq!(mix(0x102030, 0xFFFFFF, 2, 2), 0xFFFFFF);
    }
}
