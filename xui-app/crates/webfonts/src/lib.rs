//! The fonts web views are drawn with.
//!
//! A desktop image installs the Liberation fonts in
//! [`fhs::share::FONTS_LIBERATION`]: Sans, Serif and Mono, each with real
//! bold, italic and bold italic faces, and metric-compatible with Arial,
//! Times New Roman and Courier New, so a page laid out for those breaks its
//! lines where its author saw them. Blitz draws its own glyphs from font
//! files and finds none by itself on LazyOS, so each face is registered with
//! [`xui_blitz::register_font`]; without the italic faces Blitz would
//! synthesize a backwards slant. The files are read from disk at start rather
//! than compiled in: LazyOS cannot memory-map a font file, so the bytes go to
//! the shaper, about 4 MB, only in the process that asks.
//!
//! A family whose regular face is missing (an image built without the
//! desktop assets, a damaged file) is left to the font system's default.

use std::path::Path;

use xui_blitz::FontFamilies;

/// Each Liberation family: its file name stem and the family name its faces
/// declare.
const FAMILIES: [(&str, &str); 3] = [
    ("LiberationSans", "Liberation Sans"),
    ("LiberationSerif", "Liberation Serif"),
    ("LiberationMono", "Liberation Mono"),
];

/// The faces of each family, by file name suffix; `Regular` first.
const STYLES: [&str; 4] = ["Regular", "Bold", "Italic", "BoldItalic"];

/// What [`register`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// Faces read and registered.
    pub faces: usize,
    /// Faces looked for.
    pub wanted: usize,
}

/// Registers the web fonts with `xui_blitz` and says which family each CSS
/// generic family is. Call once, before the first view opens (a font change
/// only takes effect on the next page load).
pub fn register() -> Loaded {
    register_from(Path::new(fhs::share::FONTS_LIBERATION))
}

/// [`register`] reading the font files from `dir`.
pub fn register_from(dir: &Path) -> Loaded {
    let mut faces = 0;
    let mut families = [""; 3];
    for (slot, (stem, name)) in families.iter_mut().zip(FAMILIES) {
        for style in STYLES {
            let Ok(bytes) = std::fs::read(dir.join(format!("{stem}-{style}.ttf"))) else {
                continue;
            };
            xui_blitz::register_font(bytes);
            faces += 1;
            if style == "Regular" {
                *slot = name;
            }
        }
    }
    let [sans_serif, serif, monospace] = families.map(str::to_string);
    xui_blitz::set_font_families(FontFamilies {
        sans_serif,
        serif,
        monospace,
    });
    Loaded {
        faces,
        wanted: FAMILIES.len() * STYLES.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_folder_registers_nothing() {
        let loaded = register_from(Path::new("/nonexistent/fonts"));
        assert_eq!(loaded, Loaded { faces: 0, wanted: 12 });
    }
}
