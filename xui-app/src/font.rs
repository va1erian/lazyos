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

/// Droid Sans Bold (Apache-2.0), the bold weight the Docs app's headings and
/// `<b>`/`<strong>` resolve to; without it the shaper would fake the weight.
pub const BOLD_BYTES: &[u8] = include_bytes!("../../assets/fonts/DroidSans-Bold.ttf");

/// Registers the bold face and the monospace face (code blocks) next to the UI
/// face, keeping Droid Sans the default family. Call before the backend is
/// created, like [`register_mono`].
pub fn register_docs() {
    xui_canvas::add_font(BOLD_BYTES.to_vec());
    register_mono();
}

/// Registers a further font file read at run time (LazyWeb's web fonts),
/// with the same timing rule as [`register_mono`]: before the backend is
/// created. The shaper parses the bytes in memory.
pub fn add(bytes: Vec<u8>) {
    xui_canvas::add_font(bytes);
}

/// Droid Serif Regular (Apache-2.0), LazyWriter's Serif family. There is no
/// serif bold or italic face: the shaper synthesises both.
pub const SERIF_BYTES: &[u8] = include_bytes!("../../assets/fonts/DroidSerif-Regular.ttf");

/// The family name Droid Serif declares, for a run's `family`.
pub const SERIF_FAMILY: &str = "Droid Serif";

/// Registers LazyWriter's three families: Sans (Droid Sans regular and bold),
/// Serif (Droid Serif) and Mono (JetBrains Mono), keeping Droid Sans the
/// default family for the UI and for text that names none. There is no
/// italic face, so italic is synthesised by the shaper. Call before the
/// backend is created, like [`register_mono`].
pub fn register_writer() {
    xui_canvas::add_font(BOLD_BYTES.to_vec());
    xui_canvas::add_font(SERIF_BYTES.to_vec());
    register_mono();
}
