//! The desktop launchers' package icons (issue #509): each installed app's
//! `icons/app-32.png`, at the path `init`'s `ListApps` reports (its
//! `app-128.png` sibling on a 2x desktop).
//!
//! The file sits in `/apps`, which only `pkgd` writes, but its bytes come
//! from a package anyone may have installed, so it is read with a size cap
//! and its PNG header is checked before the decoder allocates anything: a
//! header claiming a huge image cannot make the shell reserve its pixels. A
//! missing or bad icon is simply `None`, and the launcher draws the built-in
//! picture instead.

use std::io::Read;
use std::rc::Rc;

use xui_core::image::Image;

/// Most bytes read from one icon file (a 32x32 RGBA PNG is a few KiB).
const MAX_ICON_BYTES: u64 = 64 * 1024;
/// Largest side accepted, in pixels (the biggest icon a package ships).
const MAX_SIDE: u32 = 128;
/// The icon file `ListApps` names, and the large sibling a package ships
/// beside it (`crates/app-icons`).
const SMALL_ICON: &str = "app-32.png";
const LARGE_ICON: &str = "app-128.png";
/// The PNG signature followed by the first chunk's length (13) and type.
const PNG_HEAD: [u8; 16] = [
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
];

/// Decoded icons by path, so a theme change or a launcher re-read does not
/// read and decode them again.
#[derive(Default)]
pub struct IconCache {
    entries: Vec<(String, Option<Rc<Image>>)>,
}

impl IconCache {
    /// The icon for UI `scale` at `path` (a package's `app-32.png`): at
    /// scale 2 and above its `app-128.png` sibling, drawn down to the tile
    /// instead of a 32 px icon drawn up (docs/hidpi-plan.md), falling back to
    /// `path` itself when the package ships no large icon.
    pub fn get_scaled(&mut self, path: &str, scale: i32) -> Option<Rc<Image>> {
        let large = path
            .strip_suffix(SMALL_ICON)
            .filter(|_| scale > 1)
            .map(|dir| format!("{dir}{LARGE_ICON}"));
        large
            .and_then(|large| self.get_quiet(&large))
            .or_else(|| self.get(path))
    }

    /// The icon at `path`, or `None` for an empty path or an unreadable or
    /// malformed file. Each path is tried once.
    pub fn get(&mut self, path: &str) -> Option<Rc<Image>> {
        let image = self.get_quiet(path);
        if image.is_none() && !path.is_empty() {
            println!("SHELL:ICON:MISSING {path}");
        }
        image
    }

    /// [`IconCache::get`] without the log line (a missing large icon is
    /// expected).
    fn get_quiet(&mut self, path: &str) -> Option<Rc<Image>> {
        if path.is_empty() {
            return None;
        }
        if let Some((_, image)) = self.entries.iter().find(|(known, _)| known == path) {
            return image.clone();
        }
        let image = load(path).map(Rc::new);
        self.entries.push((path.to_owned(), image.clone()));
        image
    }
}

/// Read and decode one icon file.
fn load(path: &str) -> Option<Image> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_ICON_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_ICON_BYTES || !small_png(&bytes) {
        return None;
    }
    Image::decode_png(&bytes).ok()
}

/// Whether `bytes` start like a PNG whose `IHDR` gives both sides in
/// `1..=MAX_SIDE`.
fn small_png(bytes: &[u8]) -> bool {
    if bytes.len() < 24 || bytes[..16] != PNG_HEAD {
        return false;
    }
    let side =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let (width, height) = (side(16), side(20));
    (1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = PNG_HEAD.to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    #[test]
    fn only_small_png_headers_pass() {
        assert!(small_png(&header(32, 32)));
        assert!(small_png(&header(128, 1)));
        assert!(!small_png(&header(0, 32)));
        assert!(!small_png(&header(129, 32)));
        assert!(!small_png(&header(32, u32::MAX)));
        assert!(!small_png(&header(32, 32)[..20]));
        let mut jpeg = header(32, 32);
        jpeg[0] = 0xFF;
        assert!(!small_png(&jpeg));
    }

    #[test]
    fn a_missing_icon_is_none_and_remembered() {
        let mut cache = IconCache::default();
        assert!(cache.get("").is_none());
        assert!(cache.get("/no/such/icon.png").is_none());
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.get("/no/such/icon.png").is_none());
        assert_eq!(cache.entries.len(), 1);
    }
}
