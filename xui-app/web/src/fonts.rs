//! The fonts web pages are drawn with.
//!
//! The window's own text (menus, address bar, status bar) keeps the UI face
//! every xui app uses, Droid Sans. Pages get the Liberation fonts a desktop
//! image installs in [`fhs::share::FONTS_LIBERATION`]: Sans, Serif and Mono,
//! each with real bold, italic and bold italic faces (the Droid set has no
//! italic, so the shaper slanted the regular one), and metric-compatible
//! with Arial, Times New Roman and Courier New, so a page laid out for those
//! breaks its lines where its author saw them. They are read from disk at
//! start rather than compiled in: LazyOS cannot memory-map a font file, so
//! the bytes go to the shaper, about 4 MB, only in this process.
//!
//! A family whose regular face is missing (an image built without the
//! desktop assets, a damaged file) falls back to the Droid family it
//! replaces. Serial evidence: `WEB:FONTS:<faces loaded>/<faces looked for>`.

use std::path::Path;

use xui_app::font;
use xui_netsurf::FontFamilies;

/// Each Liberation family: its file name stem, the family name its faces
/// declare, and the Droid family it falls back to.
const FAMILIES: [(&str, &str, &str); 3] = [
    ("LiberationSans", "Liberation Sans", font::UI_FAMILY),
    ("LiberationSerif", "Liberation Serif", font::SERIF_FAMILY),
    ("LiberationMono", "Liberation Mono", font::MONO_FAMILY),
];

/// The faces of each family, by file name suffix; `Regular` first.
const STYLES: [&str; 4] = ["Regular", "Bold", "Italic", "BoldItalic"];

/// Registers the UI fonts and the web fonts with the shaper, and returns the
/// families a page's CSS families resolve to. Call before the backend is
/// created: the shaper builds its font database on first use.
pub fn register() -> FontFamilies {
    font::register_writer();
    let dir = Path::new(fhs::share::FONTS_LIBERATION);
    let mut faces = 0;
    let mut families = [""; 3];
    for (slot, (stem, name, fallback)) in families.iter_mut().zip(FAMILIES) {
        *slot = fallback;
        for style in STYLES {
            let Ok(bytes) = std::fs::read(dir.join(format!("{stem}-{style}.ttf"))) else {
                continue;
            };
            font::add(bytes);
            faces += 1;
            if style == "Regular" {
                *slot = name;
            }
        }
    }
    println!("WEB:FONTS:{faces}/{}", FAMILIES.len() * STYLES.len());
    let [sans_serif, serif, monospace] = families.map(str::to_string);
    FontFamilies {
        sans_serif,
        serif,
        monospace,
    }
}
