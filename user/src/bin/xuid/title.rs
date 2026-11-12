//! Window titles set after creation (`SetTitle`, method 29).
//!
//! The compositor draws the title in its own chrome, so it must not trust the
//! client's string: it is bounded, stripped of control characters and never
//! allowed to blank the window.

use alloc::string::String;
use libmessenger::server::{Caller, Fault};
use user::messenger::display::wire;
use user::messenger::errno;

use super::compositor::Compositor;
use super::protocol::refuse;
use super::window::surface_by_id;

/// Longest title kept, in bytes.
pub(super) const MAX_TITLE_BYTES: usize = 128;

/// `raw` as a chrome-safe title: control characters removed, at most
/// [`MAX_TITLE_BYTES`] bytes cut at a character boundary, surrounding blanks
/// trimmed. Empty when nothing printable is left.
pub(super) fn sanitize(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars().filter(|ch| !ch.is_control()) {
        if out.len() + ch.len_utf8() > MAX_TITLE_BYTES {
            break;
        }
        out.push(ch);
    }
    out.trim().into()
}

impl Compositor {
    /// `SetTitle`: rename a window, repaint its chrome and tell the shell.
    pub(super) fn set_title(&mut self, cx: &Caller, id: u64, title: &str) -> Result<(), Fault> {
        let Some(surface) = surface_by_id(&self.surfaces, id) else {
            return Err(refuse(errno::ENOENT));
        };
        if surface.owner != cx.sender {
            return Err(refuse(errno::EACCES));
        }
        let title = sanitize(title);
        // The desktop and panels have no chrome, and an empty result keeps
        // the old name rather than blanking the shell's taskbar entry.
        if !surface.is_window() || title.is_empty() || title == surface.title {
            return Ok(());
        }
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == id) {
            surface.title = title;
        }
        self.notify_surface(id, wire::CHANGE_TITLE);
        // The title bar and the Alt+Tab list show it.
        self.repaint_full();
        Ok(())
    }
}

/// Boot check of the title rules: `XUID:TITLE:PASS` or `XUID:TITLE:FAIL`.
pub(super) fn selftest_titles() -> &'static str {
    let long: String = core::iter::repeat_n('a', MAX_TITLE_BYTES + 40).collect();
    // A 3-byte character straddling the limit must be dropped whole.
    let wide: String = core::iter::repeat_n('\u{20ac}', 60).collect();
    let ok = sanitize("notes.txt - Editor") == "notes.txt - Editor"
        && sanitize("a\u{1b}[31mb\n\tc") == "a[31mbc"
        && sanitize("\u{7}\u{8}  ").is_empty()
        && sanitize("  padded  ") == "padded"
        && sanitize(&long).len() == MAX_TITLE_BYTES
        && sanitize(&wide).len() == 42 * 3
        && sanitize(&wide).chars().all(|ch| ch == '\u{20ac}');
    if ok {
        "XUID:TITLE:PASS
"
    } else {
        "XUID:TITLE:FAIL
"
    }
}
