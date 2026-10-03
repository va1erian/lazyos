//! Theme reads and writes over the `sys/ui/*` keys (schema in `uitheme`).
//!
//! Choosing a mode clears every colour override so the preset applies cleanly;
//! individual colours are then optional overrides on top of it.

use uitheme::{Mode, Settings};
use xui_core::{Color, Theme};

use crate::store::{ConfigStore, StoreError, Value};

/// `(name, 0xRRGGBB)` accent presets; the first is the compiled-in default.
pub const ACCENTS: [(&str, u32); 5] = [
    ("Green", uitheme::DEFAULT_ACCENT),
    ("Blue", 0x336699),
    ("Orange", 0xC46E1E),
    ("Purple", 0x6E46A0),
    ("Red", 0xAA3232),
];

/// Desktop background swatches. Choosing one sets an override; "Reset to
/// defaults" returns to the mode's own background.
pub const BACKGROUNDS: [(&str, u32); 6] = [
    ("Navy", 0x0E1C3C),
    ("Forest", 0x10281C),
    ("Charcoal", 0x202024),
    ("Slate", 0x3A4256),
    ("Sand", 0xDCCEAA),
    ("Sky", 0xB4CFE8),
];

/// Read every theme key.
pub fn load(store: &dyn ConfigStore) -> Settings {
    let mut settings = Settings::default();
    for key in uitheme::ALL_KEYS {
        settings.apply(key, store.get(key).as_ref());
    }
    settings
}

/// Select dark or light and drop all colour overrides.
pub fn set_mode(store: &dyn ConfigStore, mode: Mode) -> Result<(), StoreError> {
    store.set(uitheme::KEY_MODE, Value::Str(mode.as_str().to_owned()))?;
    for key in uitheme::COLOR_KEYS {
        store.delete(key)?;
    }
    Ok(())
}

/// Set (`Some`) or clear (`None`) one colour override key.
pub fn set_color(store: &dyn ConfigStore, key: &str, rgb: Option<u32>) -> Result<(), StoreError> {
    match rgb {
        Some(rgb) => store.set(key, Value::U64(u64::from(rgb & 0x00FF_FFFF))),
        None => store.delete(key),
    }
}

/// Drop every theme key and the desktop picture, returning to the
/// compiled-in dark defaults.
pub fn reset(store: &dyn ConfigStore) -> Result<(), StoreError> {
    for key in uitheme::ALL_KEYS {
        store.delete(key)?;
    }
    store.delete(uitheme::KEY_WALLPAPER)
}

/// Switch the desktop animations on or off (`sys/ui/anim`, followed by
/// `xuid`). On is the default, so it is stored as a plain bool either way.
pub fn set_animations(store: &dyn ConfigStore, on: bool) -> Result<(), StoreError> {
    store.set(uitheme::KEY_ANIM, Value::Bool(on))
}

/// The xui widget theme matching the desktop: xui's own light or dark palette
/// with the desktop accent, so apps look like part of the same desktop. Text
/// on the accent is picked from the accent itself, which may be any colour.
pub fn xui_theme(mode: Mode, accent: u32) -> Theme {
    let mut theme = match mode {
        Mode::Light => Theme::light(),
        Mode::Dark => Theme::dark(),
    };
    let accent = accent & 0x00FF_FFFF;
    theme.accent = Color::hex(accent);
    theme.border_focused = Color::hex(accent);
    theme.text_on_accent = Color::hex(uitheme::text_on(accent));
    theme
}

/// The xui theme for the stored settings.
pub fn xui_theme_for(settings: &Settings) -> Theme {
    xui_theme(settings.mode, uitheme::resolve(settings).accent)
}

/// The accent preset in effect, by index into [`ACCENTS`].
pub fn accent_index(settings: &Settings) -> Option<usize> {
    let accent = settings.accent.unwrap_or(uitheme::DEFAULT_ACCENT);
    ACCENTS.iter().position(|(_, rgb)| *rgb == accent)
}

/// The background swatch in effect, by index into [`BACKGROUNDS`]; `None` when
/// no override is set or it is a custom colour.
pub fn background_index(settings: &Settings) -> Option<usize> {
    let bg = settings.bg?;
    BACKGROUNDS.iter().position(|(_, rgb)| *rgb == bg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;

    #[test]
    fn empty_store_loads_the_defaults() {
        let s = load(&MemStore::new());
        assert_eq!(s, Settings::default());
        assert_eq!(accent_index(&s), Some(0));
        assert_eq!(background_index(&s), None);
    }

    #[test]
    fn mode_change_clears_overrides() {
        let store = MemStore::new();
        set_color(&store, uitheme::KEY_ACCENT, Some(0x336699)).unwrap();
        set_color(&store, uitheme::KEY_BG, Some(0x0E1C3C)).unwrap();
        set_mode(&store, Mode::Light).unwrap();
        let s = load(&store);
        assert_eq!(s.mode, Mode::Light);
        assert_eq!((s.accent, s.bg), (None, None));
    }

    #[test]
    fn color_round_trips_and_masks_to_24_bits() {
        let store = MemStore::new();
        set_color(&store, uitheme::KEY_TASKBAR, Some(0xFF11_2233)).unwrap();
        assert_eq!(load(&store).taskbar, Some(0x112233));
        set_color(&store, uitheme::KEY_TASKBAR, None).unwrap();
        assert_eq!(load(&store).taskbar, None);
    }

    #[test]
    fn reset_removes_every_key() {
        let store = MemStore::new();
        set_mode(&store, Mode::Light).unwrap();
        set_color(&store, uitheme::KEY_ACCENT, Some(1)).unwrap();
        crate::wallpaper_ops::set(&store, Some("/pictures/a.png")).unwrap();
        reset(&store).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn write_failure_surfaces_and_leaves_state() {
        let store = MemStore::new();
        *store.fail_writes.borrow_mut() = Some("denied".into());
        assert!(set_mode(&store, Mode::Light).is_err());
        assert_eq!(load(&store), Settings::default());
    }

    #[test]
    fn custom_colors_match_no_swatch() {
        let s = Settings {
            accent: Some(0x123456),
            bg: Some(0x654321),
            ..Settings::default()
        };
        assert_eq!(accent_index(&s), None);
        assert_eq!(background_index(&s), None);
        let s = Settings {
            bg: Some(BACKGROUNDS[2].1),
            ..Settings::default()
        };
        assert_eq!(background_index(&s), Some(2));
    }

    #[test]
    fn xui_theme_follows_mode_and_accent() {
        let dark = xui_theme(Mode::Dark, 0x336699);
        assert!(dark.is_dark);
        assert_eq!(dark.accent, Color::hex(0x336699));
        assert_eq!(dark.text_on_accent, Color::hex(uitheme::LIGHT_TEXT));
        let light = xui_theme(Mode::Light, 0xDCCEAA);
        assert!(!light.is_dark);
        assert_eq!(light.text_on_accent, Color::hex(uitheme::DARK_TEXT));
        // The default settings give dark with the default accent.
        let theme = xui_theme_for(&Settings::default());
        assert!(theme.is_dark);
        assert_eq!(theme.accent, Color::hex(uitheme::DEFAULT_ACCENT));
    }

    #[test]
    fn animations_toggle_round_trips() {
        let store = MemStore::new();
        assert!(load(&store).anim);
        set_animations(&store, false).unwrap();
        assert!(!load(&store).anim);
        set_animations(&store, true).unwrap();
        assert!(load(&store).anim);
    }

    #[test]
    fn presets_are_distinct() {
        for (i, a) in ACCENTS.iter().enumerate() {
            for b in &ACCENTS[i + 1..] {
                assert_ne!(a.1, b.1);
            }
        }
    }
}
