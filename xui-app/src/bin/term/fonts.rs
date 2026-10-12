//! The fonts the Terminal can draw with: JetBrains Mono (compiled in) and
//! every monospace `.ttf`/`.otf` found under `/system/share/fonts` and the
//! user's `~/.local/share/fonts`. The shaper builds its font database on its
//! first use, so everything is read and registered once, before the window
//! exists; a family is offered only when the advances of `i` and `M` match.

use std::path::{Path, PathBuf};

use lazyterm::font::{parse, Catalog};

/// The family compiled into the binary: the fallback for any other.
pub const BUILT_IN: &str = xui_app::font::MONO_FAMILY;
/// A guard against absurd input only: a file bigger than this is not read.
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// How many subdirectories deep a font directory is searched.
const DEPTH: u32 = 2;
/// What was found.
#[derive(Default)]
pub struct Fonts {
    pub catalog: Catalog,
    /// Files registered with the shaper.
    pub files: usize,
}

/// The directories searched: the system's and the user's.
pub fn directories() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from(fhs::share::FONTS)];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if home.is_absolute() {
            dirs.push(home.join(fhs::share::USER_FONTS_SUBDIR));
        }
    }
    dirs
}

/// Register the built-in face and every monospace font under `dirs` with the
/// shaper. Call before the first measure or draw.
pub fn register(dirs: &[PathBuf]) -> Fonts {
    let mut fonts = Fonts::default();
    // JetBrains Mono, compiled in: the Regular and its three other faces.
    for face in std::iter::once(xui_app::font::MONO_BYTES).chain(xui_app::font::MONO_STYLES) {
        xui_canvas::add_font(face.to_vec());
        if let Some(info) = parse(face) {
            fonts.catalog.add(&info);
        }
    }
    for dir in dirs {
        scan(dir, DEPTH, &mut fonts);
    }
    fonts
}

fn scan(dir: &Path, depth: u32, fonts: &mut Fonts) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if depth > 0 {
                scan(&path, depth - 1, fonts);
            }
            continue;
        }
        let is_font = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"));
        if !is_font || path.metadata().map_or(true, |m| m.len() > MAX_FILE) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Some(info) = parse(&bytes).filter(|info| info.monospaced) {
            fonts.catalog.add(&info);
            xui_canvas::add_font(bytes);
            fonts.files += 1;
        }
    }
}
