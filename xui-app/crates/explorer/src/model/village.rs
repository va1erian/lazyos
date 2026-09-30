#![forbid(unsafe_code)]

//! Tile icons: the file classification, the Lucide fallback and the optional
//! multi-colour Global Village drawing (the `xui-icons` crate).
//!
//! Every `village-icons` check lives in this one module. The app's model calls
//! [`icon_ref`] for the single-colour fallback and [`paint`] for the coloured
//! path; with the feature off, `paint` declines and the view falls back to
//! `icon_ref`.

use std::ffi::OsStr;
use std::path::Path;

use xui_core::backend::Canvas;
use xui_core::geometry::Rect;
use xui_core::icon::{IconRef, Lucide};
use xui_core::theme::Theme;

use super::entry::Entry;
use crate::platform::Kind;

/// What an entry is, for picking an icon. The Lucide fallback and the Village
/// mapping share this classification, so the extension lists live once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileClass {
    /// A directory.
    Folder,
    /// An image file.
    Image,
    /// An audio file.
    Music,
    /// An archive.
    Archive,
    /// A source or config file (Lucide gives it a distinct outline).
    Code,
    /// Anything else.
    Document,
}

impl FileClass {
    /// Classifies `entry` by kind and, for a file, by its extension.
    pub(crate) fn of(entry: &Entry) -> FileClass {
        match entry.kind {
            Kind::Dir => FileClass::Folder,
            Kind::File | Kind::Symlink => FileClass::from_name(&entry.name),
        }
    }

    /// Classifies a file name by its (ASCII, case-insensitive) extension.
    fn from_name(name: &OsStr) -> FileClass {
        let Some(extension) = Path::new(name).extension().and_then(OsStr::to_str) else {
            return FileClass::Document;
        };
        match extension.to_ascii_lowercase().as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "svg" => FileClass::Image,
            "mp3" | "wav" | "flac" | "ogg" | "m4a" | "opus" => FileClass::Music,
            "zip" | "tar" | "gz" | "7z" | "rar" | "xz" | "bz2" => FileClass::Archive,
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "go"
            | "java" | "json" | "toml" | "yaml" | "yml" | "sh" | "ps1" | "bat" => FileClass::Code,
            _ => FileClass::Document,
        }
    }
}

/// The single-colour Lucide icon for `entry`, used when the coloured path is
/// off. A folder shows the open icon while `flashing`.
pub(crate) fn icon_ref(entry: &Entry, flashing: bool) -> IconRef {
    let icon = match FileClass::of(entry) {
        FileClass::Folder if flashing => Lucide::FolderOpen,
        FileClass::Folder => Lucide::Folder,
        FileClass::Image => Lucide::Image,
        FileClass::Music => Lucide::File,
        FileClass::Archive => Lucide::Package,
        FileClass::Code => Lucide::FileCode,
        FileClass::Document => Lucide::File,
    };
    icon.into()
}

/// Draws the multi-colour Global Village icon for `entry` into `rect`, in the
/// palette the live `theme` calls for. Returns `false` (draw nothing) when the
/// `village-icons` feature is off, so the view uses [`icon_ref`].
#[cfg(feature = "village-icons")]
pub(crate) fn paint(
    entry: &Entry,
    flashing: bool,
    canvas: &mut dyn Canvas,
    rect: Rect,
    theme: &Theme,
    _dpi: u32,
) -> bool {
    use xui_icons::{Palette, Tone, Village, draw};

    /// The Global Village palette on a light surface.
    const LIGHT: Palette = Palette::GLOBAL_VILLAGE;
    /// The set's near-black ink is invisible on a dark surface, so retint just
    /// the outline to a pale periwinkle; the fills keep their own colours.
    const DARK: Palette = Palette::GLOBAL_VILLAGE.with(Tone::Ink, DARK_INK);
    /// The outline colour used on a dark theme.
    const DARK_INK: xui_core::backend::Rgba = xui_core::backend::Rgba::rgb(0xEC, 0xE6, 0xFF);

    let icon = match FileClass::of(entry) {
        FileClass::Folder if flashing => Village::FolderOpen,
        FileClass::Folder => Village::Folder,
        FileClass::Image => Village::Image,
        FileClass::Music => Village::Music,
        FileClass::Archive => Village::Archive,
        FileClass::Code | FileClass::Document => Village::Document,
    };
    let palette = if theme.is_dark { &DARK } else { &LIGHT };
    draw(canvas, icon, rect, palette);
    true
}

/// Does nothing and declines, so the view draws the Lucide fallback.
#[cfg(not(feature = "village-icons"))]
pub(crate) fn paint(
    _entry: &Entry,
    _flashing: bool,
    _canvas: &mut dyn Canvas,
    _rect: Rect,
    _theme: &Theme,
    _dpi: u32,
) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn entry(name: &str, kind: Kind) -> Entry {
        Entry {
            name: OsString::from(name),
            display: name.to_string(),
            detail: String::new(),
            kind,
            size: None,
        }
    }

    fn class(name: &str) -> FileClass {
        FileClass::from_name(OsStr::new(name))
    }

    #[test]
    fn extensions_classify_case_insensitively() {
        assert_eq!(class("photo.PNG"), FileClass::Image);
        assert_eq!(class("clip.mp3"), FileClass::Music);
        assert_eq!(class("bundle.tar"), FileClass::Archive);
        assert_eq!(class("main.RS"), FileClass::Code);
        assert_eq!(class("notes.txt"), FileClass::Document);
        assert_eq!(class("no-extension"), FileClass::Document);
    }

    #[test]
    fn a_directory_is_a_folder_and_a_symlink_to_a_file_is_a_document() {
        assert_eq!(FileClass::of(&entry("docs", Kind::Dir)), FileClass::Folder);
        assert_eq!(
            FileClass::of(&entry("main.rs", Kind::Symlink)),
            FileClass::Code,
            "a symlink is classified by its own name"
        );
    }

    #[test]
    fn the_fallback_icon_follows_the_class_and_flash() {
        assert_eq!(
            icon_ref(&entry("docs", Kind::Dir), false),
            IconRef::Lucide(Lucide::Folder)
        );
        assert_eq!(
            icon_ref(&entry("docs", Kind::Dir), true),
            IconRef::Lucide(Lucide::FolderOpen)
        );
        assert_eq!(
            icon_ref(&entry("photo.png", Kind::File), false),
            IconRef::Lucide(Lucide::Image)
        );
        assert_eq!(
            icon_ref(&entry("bundle.zip", Kind::File), false),
            IconRef::Lucide(Lucide::Package)
        );
    }

    /// The feature-off path declines, so the view keeps the Lucide fallback.
    #[cfg(not(feature = "village-icons"))]
    #[test]
    fn the_coloured_path_is_off_and_declines() {
        use xui_canvas::Surface;
        use xui_core::theme::Theme;

        let mut surface = Surface::new(48, 48);
        let rect = Rect::new(4, 4, 40, 40);
        let claimed = surface.with_canvas_at(rect, 96, |canvas| {
            paint(
                &entry("docs", Kind::Dir),
                false,
                canvas,
                rect,
                &Theme::light(),
                96,
            )
        });
        assert!(!claimed, "the feature-off path declines");
    }

    /// The feature-on path claims the tile and really draws the multi-colour
    /// icon into the LazyOS canvas backend (`SkiaCanvas`), for every class and
    /// on both themes.
    #[cfg(feature = "village-icons")]
    #[test]
    fn the_coloured_path_draws_every_class_on_both_themes() {
        use xui_canvas::Surface;
        use xui_core::Color;
        use xui_core::theme::Theme;

        let rect = Rect::new(4, 4, 40, 40);
        let background = Color::rgb(255, 255, 255);

        // Returns (claimed, drew pixels) for one tile on one theme.
        let draw = |entry: &Entry, flashing: bool, theme: &Theme| -> (bool, bool) {
            let mut surface = Surface::new(48, 48);
            surface.fill(background);
            let before = surface.pixels().to_vec();
            let claimed = surface.with_canvas_at(rect, 96, |canvas| {
                paint(entry, flashing, canvas, rect, theme, 96)
            });
            (claimed, surface.pixels() != before.as_slice())
        };

        let cases = [
            (entry("docs", Kind::Dir), false),
            (entry("docs", Kind::Dir), true),
            (entry("photo.png", Kind::File), false),
            (entry("clip.mp3", Kind::File), false),
            (entry("bundle.zip", Kind::File), false),
            (entry("main.rs", Kind::File), false),
            (entry("notes.txt", Kind::File), false),
        ];
        for (entry, flashing) in cases {
            let (claimed, drew) = draw(&entry, flashing, &Theme::light());
            assert!(claimed, "{} claims the coloured tile", entry.display);
            assert!(drew, "{} draws pixels", entry.display);
        }

        // The dark theme retints the near-black ink; it must still draw.
        let (claimed, drew) = draw(&entry("docs", Kind::Dir), false, &Theme::dark());
        assert!(claimed && drew, "the dark palette draws");

        // An empty rectangle draws nothing (the backend would clip it away).
        let mut surface = Surface::new(48, 48);
        surface.fill(background);
        let before = surface.pixels().to_vec();
        let claimed = surface.with_canvas_at(rect, 96, |canvas| {
            paint(
                &entry("docs", Kind::Dir),
                false,
                canvas,
                Rect::new(0, 0, 0, 0),
                &Theme::light(),
                96,
            )
        });
        assert!(claimed, "an empty rect still claims the tile");
        assert_eq!(surface.pixels(), before.as_slice(), "nothing is drawn");
    }
}
