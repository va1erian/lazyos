//! What a view tells its app: the decoded events, and why a view can be lost.

use messenger_generated::os_lazy_webd_light_v1 as wire;

/// What the page says about itself (`State`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageState {
    pub title: String,
    pub loading: bool,
    /// Loads that have finished: the first paint of a `set_html` is told from the old page's by
    /// this number growing.
    pub finished: u32,
    /// Empty, or a short reason a load failed.
    pub error: String,
}

/// Why the view cannot go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lost {
    /// The engine gave the view up; the number is a `LostReason` of the interface.
    Engine(u32),
    /// The service's end closed, or a call found no service or no such view (webd quit or
    /// restarted).
    ServiceGone,
}

impl Lost {
    /// A short word for traces and messages.
    pub fn name(self) -> &'static str {
        match self {
            Lost::Engine(wire::LOST_REASON_REFUSED) => "refused",
            Lost::Engine(wire::LOST_REASON_CRASHED) => "crashed",
            Lost::Engine(wire::LOST_REASON_OUT_OF_MEMORY) => "out_of_memory",
            Lost::Engine(wire::LOST_REASON_OTHER) => "other",
            Lost::Engine(_) => "unknown",
            Lost::ServiceGone => "service_gone",
        }
    }
}

/// One thing that happened to the view, as [`crate::LightWebView::poll`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewEvent {
    /// A newer picture is in [`crate::LightWebView::picture`] (`seq` counts from 1; frames the
    /// client was too slow to look at are skipped, never shown late).
    Frame {
        seq: u64,
        width: u32,
        height: u32,
    },
    State(PageState),
    Scroll {
        x: i32,
        y: i32,
        at_bottom: bool,
    },
    /// A link was clicked, a form submitted or a meta refresh fired, and nothing loaded.
    LinkClicked(String),
    /// The user copied this text out of the page.
    ClipboardWrite(String),
    /// The view is gone; [`crate::LightWebView::recreate`] makes a new one.
    Lost(Lost),
}

/// Why a call did not do what was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// There is no view: it was never created, or it was destroyed.
    NoView,
    /// The view is lost; nothing can be asked of it.
    Lost(Lost),
    /// The web engine is not running (`webd` has no registered name): show "not running" and
    /// offer to start it.
    NotRunning,
    /// The service refused the call with this errno (the interface lists each one's meaning).
    Refused(i64),
}

impl std::fmt::Display for Error {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoView => write!(out, "no view"),
            Error::Lost(why) => write!(out, "the view was lost ({})", why.name()),
            Error::NotRunning => write!(out, "the web engine is not running"),
            Error::Refused(errno) => write!(out, "refused (errno {errno})"),
        }
    }
}

impl std::error::Error for Error {}

/// A decoded event of the wire, before the view applies it.
pub(crate) enum Wire {
    Frame(wire::FrameArgs),
    State(wire::StateArgs),
    Scroll(wire::ScrollArgs),
    Link(wire::LinkClickedArgs),
    Clipboard(wire::ClipboardWriteArgs),
    Lost(wire::ViewLostArgs),
    Ping,
}

/// Decode an event; `None` for a method that is not an event or a body that does not decode (a
/// service of another version is not trusted to be well formed).
pub(crate) fn decode(method: u32, body: &[u8]) -> Option<Wire> {
    Some(match method {
        wire::METHOD_FRAME => Wire::Frame(wire::decode_frame_args(body).ok()?),
        wire::METHOD_STATE => Wire::State(wire::decode_state_args(body).ok()?),
        wire::METHOD_SCROLL => Wire::Scroll(wire::decode_scroll_args(body).ok()?),
        wire::METHOD_LINKCLICKED => Wire::Link(wire::decode_link_clicked_args(body).ok()?),
        wire::METHOD_CLIPBOARDWRITE => {
            Wire::Clipboard(wire::decode_clipboard_write_args(body).ok()?)
        }
        wire::METHOD_VIEWLOST => Wire::Lost(wire::decode_view_lost_args(body).ok()?),
        wire::METHOD_PING => Wire::Ping,
        _ => return None,
    })
}
