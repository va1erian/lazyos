//! The offscreen backend records the icon an app sets on a window.

use super::*;

#[test]
fn the_offscreen_backend_records_the_last_window_icon() {
    let backend = OffscreenBackend::new();
    let window = backend
        .open_window(&PlatformSpec::new("t"))
        .expect("window");
    assert_eq!(
        backend.window_icon(window),
        None,
        "no icon until one is set"
    );

    let first = xui_core::Image::from_rgba(1, 1, vec![1, 2, 3, 255]).expect("image");
    let second = xui_core::Image::from_rgba(2, 1, vec![9; 8]).expect("image");
    backend.set_window_icon(window, &first);
    backend.set_window_icon(window, &second);
    assert_eq!(backend.window_icon(window), Some(second));
}
