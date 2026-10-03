//! The desktop pictures a desktop image ships in `/system/share/wallpapers`.

use crate::os_image::OsFiles;
use crate::wallpapers_embed::{embed, PICTURES};

/// The `(width, height)` in a JPEG's first frame header.
fn jpeg_size(bytes: &[u8]) -> (u32, u32) {
    assert!(bytes.starts_with(&[0xFF, 0xD8]), "not a JPEG");
    let mut at = 2;
    loop {
        assert_eq!(bytes[at], 0xFF, "marker expected at {at}");
        let marker = bytes[at + 1];
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if matches!(marker, 0xC0..=0xC2) {
            let field =
                |i: usize| u32::from(u16::from_be_bytes([bytes[at + i], bytes[at + i + 1]]));
            return (field(7), field(5));
        }
        at += 2 + length;
    }
}

#[test]
fn every_picture_lands_in_the_wallpapers_directory() {
    let mut files = OsFiles::default();
    embed(&mut files);
    assert_eq!(files.len(), PICTURES.len());
    for file in files.files() {
        assert!(
            file.path.starts_with(fhs::share::WALLPAPERS),
            "{}",
            file.path
        );
    }
}

#[test]
fn pictures_fill_the_hidpi_screen_and_stay_small() {
    // LazyShell refuses a picture over 16 Mpx; these are the 2x desktop's
    // 2560x1440, scaled down on a 1280x720 screen.
    for (name, bytes) in PICTURES {
        assert_eq!(jpeg_size(bytes), (2560, 1440), "{name}");
        assert!(bytes.len() < 512 * 1024, "{name}: {} bytes", bytes.len());
    }
}

#[test]
fn names_are_sorted_like_the_settings_list() {
    // Settings sorts the directory; keep the table in the same order.
    let names: Vec<&str> = PICTURES.iter().map(|(name, _)| *name).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
}
