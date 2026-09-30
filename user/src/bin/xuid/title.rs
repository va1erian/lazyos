//! Window titles set after creation (`SetTitle`, method 29).
//!
//! The compositor draws the title in its own chrome, so it must not trust the
//! client's string: it is bounded, stripped of control characters and never
//! allowed to blank the window.

use alloc::string::String;
use libmessenger::Parcel;
use user::messenger::display::wire;
use user::messenger::{self, Message};

use super::compositor::Compositor;
use super::protocol::{empty_reply, error_reply};
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
    pub(super) fn set_title(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_set_title_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let Some(surface) = surface_by_id(&self.surfaces, args.surface) else {
            return error_reply(message.method(), messenger::errno::ENOENT);
        };
        if surface.owner != message.sender {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        let title = sanitize(&args.title);
        // The desktop layer has no chrome, and an empty result keeps the old
        // name rather than blanking the taskbar entry.
        if surface.desktop || title.is_empty() || title == surface.title {
            return empty_reply(message.method());
        }
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == args.surface) {
            surface.title = title;
        }
        self.notify_surface(args.surface, wire::CHANGE_TITLE);
        // The taskbar and the Alt+Tab list are laid out from the titles.
        self.repaint_full();
        empty_reply(message.method())
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
