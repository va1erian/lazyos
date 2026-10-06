//! The desktop pictures a desktop image ships in `/system/share/wallpapers`
//! (assets installed with the shell, `assets/manifest.txt`).

use crate::assets_tests::checked_in;

/// `(file name, bytes)` of every picture a desktop image installs, sorted.
fn pictures() -> Vec<(String, Vec<u8>)> {
    let prefix = format!("{}/", fhs::share::WALLPAPERS);
    checked_in(true)
        .into_iter()
        .filter_map(|(dest, asset)| {
            let name = dest.strip_prefix(&prefix)?.to_string();
            Some((name, std::fs::read(&asset.file).unwrap()))
        })
        .collect()
}

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
fn four_pictures_ship_with_the_shell_only() {
    let names: Vec<String> = pictures().into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        [
            "Aurora.jpg",
            "Dunes.jpg",
            "LazyOS-Green.jpg",
            "LazyOS-Night.jpg"
        ]
    );
    let console = checked_in(false);
    assert!(console
        .iter()
        .all(|(dest, _)| !dest.starts_with(fhs::share::WALLPAPERS)));
}

#[test]
fn pictures_fill_the_hidpi_screen_and_stay_small() {
    // LazyShell refuses a picture over 16 Mpx; these are the 2x desktop's
    // 2560x1440, scaled down on a 1280x720 screen.
    for (name, bytes) in pictures() {
        assert_eq!(jpeg_size(&bytes), (2560, 1440), "{name}");
        assert!(bytes.len() < 512 * 1024, "{name}: {} bytes", bytes.len());
    }
}
