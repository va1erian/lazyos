#![forbid(unsafe_code)]

//! The seam to a system clipboard that carries `image/png`.
//!
//! The portable crate names no backend, so a host that has one (LazyOS's
//! `clipboardd`, which offers bytes inline up to 8 KiB) implements
//! [`SystemClipboard`] and hands it to
//! [`PaintApp::set_system_clipboard`](super::PaintApp::set_system_clipboard).
//! The in-app slot always works; the system one only carries a copy small
//! enough to fit, and a picture another app copied is adopted on paste.

/// An `image/png` clipboard shared with other applications.
pub trait SystemClipboard {
    /// The largest PNG the clipboard accepts, in bytes.
    fn max_bytes(&self) -> usize;

    /// Publishes `png` as the newest `image/png` offer. Best effort: a failure
    /// only means other applications do not see the copy.
    fn offer_png(&self, png: &[u8]);

    /// The newest `image/png` offer, if there is one.
    fn request_png(&self) -> Option<Vec<u8>>;
}
