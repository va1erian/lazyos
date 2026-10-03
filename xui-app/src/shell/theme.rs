//! The shell's colours: the `sys/ui/*` settings in `confd` resolved through
//! `libs/uitheme`, the same palette the compositor paints window chrome with,
//! so the taskbar, the menu and the wallpaper always match the windows.
//!
//! The feed also follows the taskbar clock format (`sys/time/clock24`,
//! `sys/time/show_seconds`, the Settings app's Time & Date page), which `xuid`
//! followed while it painted the taskbar, and the desktop picture
//! (`sys/ui/wallpaper`, [`super::wallpaper`]).
//!
//! The settings are re-read every [`POLL_TICKS`] (a few bounded `Get`s); a
//! missing `confd` keeps the defaults (or the last values read).

use lazyshell::clock::{self, ClockFormat};
use uitheme::{Mode, Palette, Settings};
use xui_core::{Color, Theme};

use super::services;
use crate::sys;

/// How often the settings are re-read (100 Hz ticks): three seconds.
const POLL_TICKS: u64 = 300;

/// Follows `sys/ui/*` and the clock format.
pub struct ThemeFeed {
    settings: Settings,
    clock: ClockFormat,
    /// The desktop picture's path; empty for the plain background colour.
    wallpaper: String,
    next_poll: u64,
}

impl ThemeFeed {
    /// The default (dark) settings; nothing read yet.
    pub fn new() -> ThemeFeed {
        ThemeFeed {
            settings: Settings::default(),
            clock: ClockFormat::default(),
            wallpaper: String::new(),
            next_poll: 0,
        }
    }

    /// The palette for the current settings.
    pub fn palette(&self) -> Palette {
        uitheme::resolve(&self.settings)
    }

    /// The taskbar clock format in effect.
    pub fn clock_format(&self) -> ClockFormat {
        self.clock
    }

    /// The desktop picture's path; empty when none is set.
    pub fn wallpaper(&self) -> &str {
        &self.wallpaper
    }

    /// Whether the dark preset is selected.
    pub fn is_dark(&self) -> bool {
        self.settings.mode == Mode::Dark
    }

    /// Re-read the settings when due; `true` when they changed.
    pub fn poll(&mut self) -> bool {
        let now = sys::clock_ticks();
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now.saturating_add(POLL_TICKS);
        let mut next = Settings::default();
        for key in uitheme::ALL_KEYS {
            match services::confd_get(key) {
                Ok(value) => next.apply(key, value.as_ref()),
                // confd is not there (yet): keep what we have.
                Err(_) => return false,
            }
        }
        let format = match (
            services::confd_get(clock::CLOCK24_KEY),
            services::confd_get(clock::SHOW_SECONDS_KEY),
        ) {
            (Ok(hour24), Ok(seconds)) => clock::format_from(hour24.as_ref(), seconds.as_ref()),
            _ => self.clock,
        };
        let wallpaper = match services::confd_get(uitheme::KEY_WALLPAPER) {
            Ok(value) => uitheme::wallpaper_path(value.as_ref())
                .unwrap_or_default()
                .to_owned(),
            Err(_) => self.wallpaper.clone(),
        };
        let changed =
            next != self.settings || format != self.clock || wallpaper != self.wallpaper;
        self.settings = next;
        self.clock = format;
        self.wallpaper = wallpaper;
        changed
    }
}

impl Default for ThemeFeed {
    fn default() -> ThemeFeed {
        ThemeFeed::new()
    }
}

/// A `0xRRGGBB` palette colour as an xui colour.
pub fn color(rgb: u32) -> Color {
    Color::hex(rgb & 0x00FF_FFFF)
}

/// The xui theme for the desktop surface: the preset's widget theme on the
/// wallpaper colour, so the icon view's background is the wallpaper. `dark`
/// is what the launcher labels sit on: the mode, or the picture when one is
/// shown (it may be dark in the light mode).
pub fn desktop_theme(palette: &Palette, dark: bool) -> Theme {
    let mut theme = if dark { Theme::dark() } else { Theme::light() };
    theme.background = color(palette.background);
    theme.accent = color(palette.overlay_selected);
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_theme_paints_on_the_wallpaper() {
        let palette = uitheme::resolve(&Settings::default());
        let theme = desktop_theme(&palette, true);
        assert_eq!(theme.background, color(palette.background));
        assert!(theme.is_dark);
        assert_eq!(color(0x12_34_56), Color::rgb(0x12, 0x34, 0x56));
    }
}
