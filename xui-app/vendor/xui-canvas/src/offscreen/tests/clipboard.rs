//! The offscreen backend's text clipboard: the in-process fallback, so a test
//! can copy and paste with no OS clipboard.

use super::*;

#[test]
fn the_offscreen_backend_round_trips_text() {
    let backend = OffscreenBackend::new();

    backend.set_clipboard_text("");
    assert_eq!(
        backend.clipboard_text(),
        None,
        "an empty clipboard reads as no text"
    );

    backend.set_clipboard_text("héllo 🎵");
    assert_eq!(backend.clipboard_text(), Some("héllo 🎵".to_string()));
}
