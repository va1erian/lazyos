//! Which picture a tray cell shows: the fallback chain of docs/tray-plan.md
//! section 6.2.
//!
//! An item's own source comes first when it is usable; then the package's
//! `icons/app-16.png` (`app-32.png` at 2x), which every package ships; then
//! the Lucide `app-window` outline, which is built in and always draws. The
//! painter walks the chain and shows the first picture it can load, so a bad
//! icon, an unreadable file or an unpackaged built-in all still get one.

use super::item::{Image, Source};

/// The icon's side in design pixels (32 screen pixels at 2x).
pub const ICON_DP: i32 = 16;
/// The outline every chain ends with.
pub const FALLBACK_LUCIDE: &str = "app-window";
/// The package icon at 1x and at 2x (docs/packages.md: every package ships
/// both).
pub const PACKAGE_ICON_1X: &str = "app-16.png";
pub const PACKAGE_ICON_2X: &str = "app-32.png";

/// One picture to try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Picture<'a> {
    /// A Lucide outline the named-icon library knows, in the bar's ink.
    Lucide(&'a str),
    /// An alpha mask in the bar's ink.
    Mask(&'a Image),
    /// Full-colour pixels, drawn as given.
    Pixels(&'a Image),
    /// A PNG to read (with the desktop's size cap and header check).
    File(String),
}

/// The pictures to try for an item whose (validated) source is `source`, at
/// UI `scale`, for an app whose install directory's `icons/` is `icons_dir`
/// (`None` for an unpackaged built-in). `known` says whether the named-icon
/// library has a Lucide name. The last entry always draws.
pub fn chain<'a>(
    source: Option<&'a Source>,
    scale: i32,
    icons_dir: Option<&str>,
    known: impl Fn(&str) -> bool,
) -> Vec<Picture<'a>> {
    let mut out = Vec::with_capacity(3);
    match source {
        Some(Source::Lucide(name)) if known(name) => out.push(Picture::Lucide(name)),
        Some(Source::Mask(mask)) => out.push(Picture::Mask(mask)),
        Some(Source::Pixels(images)) => {
            if let Some(image) = closest(images, ICON_DP * scale.max(1)) {
                out.push(Picture::Pixels(image));
            }
        }
        Some(Source::Package(name)) => {
            if let Some(dir) = icons_dir {
                out.push(Picture::File(join(dir, name)));
            }
        }
        // An unknown Lucide name, or no source at all.
        Some(Source::Lucide(_)) | None => {}
    }
    if let Some(dir) = icons_dir {
        let own = if scale >= 2 {
            PACKAGE_ICON_2X
        } else {
            PACKAGE_ICON_1X
        };
        out.push(Picture::File(join(dir, own)));
    }
    out.push(Picture::Lucide(FALLBACK_LUCIDE));
    out
}

/// The image whose larger side is closest to `side` pixels; on a tie the
/// larger one, which scales down more cleanly.
pub fn closest(images: &[Image], side: i32) -> Option<&Image> {
    let side = u32::try_from(side).unwrap_or(0);
    images.iter().min_by_key(|image| {
        let own = image.width.max(image.height);
        (own.abs_diff(side), core::cmp::Reverse(own))
    })
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}
