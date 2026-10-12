//! Colours: the 16-colour palettes of the named schemes, the 256-colour cube
//! and grey ramp, 24-bit colour, and the live palette a program can change
//! with OSC 4/10/11/12 and the host with a new scheme.

use crate::cell::Color;

/// An 8-bit-per-channel colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(value: u32) -> Rgb {
        Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    /// `self` moved `t` (0..1) of the way to `other`.
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Rgb(mix(self.0, other.0), mix(self.1, other.1), mix(self.2, other.2))
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f32 {
        let lin = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(self.0) + 0.7152 * lin(self.1) + 0.0722 * lin(self.2)
    }

    /// WCAG contrast ratio against `other`.
    pub fn contrast(self, other: Rgb) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// `rrrr/gggg/bbbb`, the form an OSC colour query answers with (8-bit
    /// values doubled to 16 bits).
    pub fn spec16(self) -> String {
        format!("{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}", self.0, self.1, self.2)
    }
}

/// A colour scheme: the default foreground and background, the cursor, and
/// the 16 ANSI colours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Scheme {
    pub name: &'static str,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub ansi: [Rgb; 16],
}

const fn ansi(values: [u32; 16]) -> [Rgb; 16] {
    let mut out = [Rgb(0, 0, 0); 16];
    let mut i = 0;
    while i < 16 {
        out[i] = Rgb::hex(values[i]);
        i += 1;
    }
    out
}

/// The scheme names a setting may use.
pub const NAMES: [&str; 5] = [
    "desktop",
    "solarized-dark",
    "solarized-light",
    "gruvbox-dark",
    "tango",
];

const DESKTOP_DARK: Scheme = Scheme {
    name: "desktop",
    fg: Rgb::hex(0xdce1f0),
    bg: Rgb::hex(0x151928),
    cursor: Rgb::hex(0x5fd08f),
    ansi: ansi([
        0x2e3440, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf, 0x5c6370,
        0xff7b86, 0xb5e08f, 0xf2cf8c, 0x7ec4ff, 0xd68cf0, 0x6fd0dc, 0xffffff,
    ]),
};

const DESKTOP_LIGHT: Scheme = Scheme {
    name: "desktop",
    fg: Rgb::hex(0x1f2328),
    bg: Rgb::hex(0xfbfbfb),
    cursor: Rgb::hex(0x0969da),
    ansi: ansi([
        0x1f2328, 0xcf222e, 0x1a7f37, 0x9a6700, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781, 0x57606a,
        0xa40e26, 0x116329, 0x7d4e00, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
    ]),
};

const SOLARIZED: [u32; 16] = [
    0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36,
    0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
];

const SOLARIZED_DARK: Scheme = Scheme {
    name: "solarized-dark",
    fg: Rgb::hex(0x839496),
    bg: Rgb::hex(0x002b36),
    cursor: Rgb::hex(0x93a1a1),
    ansi: ansi(SOLARIZED),
};

const SOLARIZED_LIGHT: Scheme = Scheme {
    name: "solarized-light",
    fg: Rgb::hex(0x657b83),
    bg: Rgb::hex(0xfdf6e3),
    cursor: Rgb::hex(0x586e75),
    ansi: ansi(SOLARIZED),
};

const GRUVBOX_DARK: Scheme = Scheme {
    name: "gruvbox-dark",
    fg: Rgb::hex(0xebdbb2),
    bg: Rgb::hex(0x282828),
    cursor: Rgb::hex(0xebdbb2),
    ansi: ansi([
        0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984, 0x928374,
        0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
    ]),
};

const TANGO: Scheme = Scheme {
    name: "tango",
    fg: Rgb::hex(0xd3d7cf),
    bg: Rgb::hex(0x2e3436),
    cursor: Rgb::hex(0xd3d7cf),
    ansi: ansi([
        0x2e3436, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, 0x555753,
        0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
    ]),
};

/// The scheme called `name`. `desktop` (and any unknown name) follows the
/// desktop's dark or light mode.
pub fn scheme_by_name(name: &str, dark: bool) -> Scheme {
    match name {
        "solarized-dark" => SOLARIZED_DARK,
        "solarized-light" => SOLARIZED_LIGHT,
        "gruvbox-dark" => GRUVBOX_DARK,
        "tango" => TANGO,
        _ if dark => DESKTOP_DARK,
        _ => DESKTOP_LIGHT,
    }
}

/// Entry `index` of the xterm 256-colour palette: 0-15 the scheme's ANSI
/// colours, 16-231 a 6x6x6 cube, 232-255 a 24-step grey ramp.
pub fn xterm_256(index: u8, ansi: &[Rgb; 16]) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => ansi[usize::from(index)],
        16..=231 => {
            let n = usize::from(index - 16);
            Rgb(LEVELS[n / 36], LEVELS[n / 6 % 6], LEVELS[n % 6])
        }
        _ => {
            let grey = 8 + 10 * (index - 232);
            Rgb(grey, grey, grey)
        }
    }
}

/// The colours in force: the scheme's, as a program has changed them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Palette {
    base: Scheme,
    table: Vec<Rgb>,
    fg: Rgb,
    bg: Rgb,
    cursor: Rgb,
}

impl Palette {
    pub fn new(scheme: Scheme) -> Palette {
        let mut palette = Palette {
            base: scheme,
            table: Vec::new(),
            fg: scheme.fg,
            bg: scheme.bg,
            cursor: scheme.cursor,
        };
        palette.reset();
        palette
    }

    /// The scheme this palette started from.
    pub fn scheme(&self) -> &Scheme {
        &self.base
    }

    /// Back to the scheme's colours (RIS, OSC 104/110/111/112 all).
    pub fn reset(&mut self) {
        self.table = (0..=255u8).map(|i| xterm_256(i, &self.base.ansi)).collect();
        self.reset_defaults();
    }

    pub fn reset_defaults(&mut self) {
        (self.fg, self.bg, self.cursor) = (self.base.fg, self.base.bg, self.base.cursor);
    }

    /// Replace the scheme, dropping what a program changed.
    pub fn set_scheme(&mut self, scheme: Scheme) {
        self.base = scheme;
        self.reset();
    }

    pub fn get(&self, index: u8) -> Rgb {
        self.table[usize::from(index)]
    }

    pub fn reset_index(&mut self, index: u8) {
        self.table[usize::from(index)] = xterm_256(index, &self.base.ansi);
    }

    pub fn set(&mut self, index: u8, color: Rgb) {
        self.table[usize::from(index)] = color;
    }

    pub fn fg(&self) -> Rgb {
        self.fg
    }

    pub fn bg(&self) -> Rgb {
        self.bg
    }

    pub fn cursor(&self) -> Rgb {
        self.cursor
    }

    pub fn set_fg(&mut self, color: Rgb) {
        self.fg = color;
    }

    pub fn set_bg(&mut self, color: Rgb) {
        self.bg = color;
    }

    pub fn set_cursor(&mut self, color: Rgb) {
        self.cursor = color;
    }

    /// The colour a cell attribute asks for. `bold_is_bright` turns a bold
    /// ANSI colour 0-7 into its bright twin.
    pub fn resolve(&self, color: Color, foreground: bool, bold: bool, bold_is_bright: bool) -> Rgb {
        match color {
            Color::Default if foreground => self.fg,
            Color::Default => self.bg,
            Color::Indexed(i) if foreground && bold && bold_is_bright && i < 8 => self.get(i + 8),
            Color::Indexed(i) => self.get(i),
            Color::Rgb(r, g, b) => Rgb(r, g, b),
        }
    }
}

/// Parse an OSC colour spec: `rgb:R/G/B` with 1-4 hex digits a channel, or
/// `#RGB` / `#RRGGBB` / `#RRRGGGBBB` / `#RRRRGGGGBBBB`.
pub fn parse_color(spec: &str) -> Option<Rgb> {
    fn channel(digits: &str) -> Option<u8> {
        if digits.is_empty() || digits.len() > 4 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        let max = 16u32.pow(digits.len() as u32) - 1;
        Some(((value * 255 + max / 2) / max) as u8)
    }
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut parts = rest.split('/');
        let color = Rgb(channel(parts.next()?)?, channel(parts.next()?)?, channel(parts.next()?)?);
        return parts.next().is_none().then_some(color);
    }
    let hex = spec.strip_prefix('#')?;
    let width = match hex.len() {
        3 => 1,
        6 => 2,
        9 => 3,
        12 => 4,
        _ => return None,
    };
    let part = |at: usize| hex.get(at * width..(at + 1) * width).and_then(channel);
    Some(Rgb(part(0)?, part(1)?, part(2)?))
}
