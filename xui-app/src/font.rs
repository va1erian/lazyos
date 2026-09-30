//! The bundled font, compiled into the app.
//!
//! LazyOS has no system font store, and its Linux ABI implements anonymous
//! `mmap` only, so the shaper cannot memory-map a font file even if one were
//! written to disk. The backend hands these bytes to the shaper directly.

/// Droid Sans (Apache-2.0, see `assets/fonts/`), the UI face for every xui app
/// and the sans face `xuid` draws its chrome with. It is the only face
/// registered, so the shaper's default family resolves to it.
pub const BYTES: &[u8] = include_bytes!("../../assets/fonts/DroidSans.ttf");

/// JetBrains Mono Regular (SIL OFL), for the Terminal's fixed character grid.
pub const MONO_BYTES: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

/// The family name JetBrains Mono declares, for `set_default_family`.
pub const MONO_FAMILY: &str = "JetBrains Mono";

/// The family name Droid Sans declares, for `set_default_family`.
pub const UI_FAMILY: &str = "Droid Sans";

/// Registers JetBrains Mono next to the UI face for an app that draws a
/// monospace grid (the Editor), keeping Droid Sans the default family so the
/// menus and status bar are unchanged. Call before the backend is created:
/// the shaper builds its font database lazily on the first measure or draw.
pub fn register_mono() {
    xui_canvas::add_font(MONO_BYTES.to_vec());
    xui_canvas::set_default_family(UI_FAMILY);
}
