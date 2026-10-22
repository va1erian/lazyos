//! What a full view tells its app: the decoded events of `os.lazy.webd.view.v1` (idl/webd.midl) and
//! the page's state as the last `State` said it.

use messenger_generated::os_lazy_webd_view_v1 as wire;

use crate::event::Lost;

pub use wire::{Rect, Shift};

/// What the page says about itself (`State`), every field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FullState {
    pub title: String,
    pub loading: bool,
    /// Navigations that finished loading since the view was made: a load is done when this grows
    /// and `loading` is false.
    pub finished: u32,
    /// Empty, or a short reason the last load failed.
    pub error: String,
    /// The address on show (it changes when a load starts).
    pub url: String,
    /// The page on screen.
    pub committed_url: String,
    pub can_back: bool,
    pub can_forward: bool,
    /// Thousandths.
    pub progress: u32,
    /// The committed page came over verified HTTPS.
    pub secure: bool,
}

/// A frame that reached the picture (or was refused: see [`FrameInfo::applied`]).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameInfo {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    /// Every pixel is new; otherwise a delta (`shift`, then `rects`).
    pub whole: bool,
    pub shift: Option<Shift>,
    pub rects: Vec<Rect>,
    /// The page's scroll offset the pixels were painted at, and whether it cannot scroll further down.
    pub offset: (i32, i32, bool),
    /// The picture now shows it. `false`: a delta that had nothing to apply to (a frame missed, a
    /// rectangle out of the view); the view asked for a whole frame.
    pub applied: bool,
}

/// Rows of the page outside the view, painted ahead of a scroll (`Band`), copied out of their slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Band {
    pub doc_x: i32,
    pub doc_y: i32,
    pub width: u32,
    pub height: u32,
    /// RGBA8, `width * 4` bytes per row.
    pub rgba: Vec<u8>,
}

/// One thing that happened to a full view, as [`crate::full::FullWebView::poll`] reports it.
#[derive(Clone, Debug, PartialEq)]
pub enum FullEvent {
    Frame(FrameInfo),
    State(FullState),
    Scroll {
        x: i32,
        y: i32,
        at_bottom: bool,
    },
    Band(Band),
    /// The pointer is over a link (its address), or no longer (`""`).
    Hover(String),
    /// The pointer shape the page wants (a `CursorShape`).
    Cursor(u32),
    LinkClicked(String),
    ClipboardWrite(String),
    /// The page asked for a new window; nothing opened.
    NewWindowRequested {
        url: String,
        gesture: bool,
    },
    /// A navigation the policy does not cover is held: answer with
    /// [`crate::full::FullWebView::navigation_answer`] within 10 seconds. `kind` is a `NavigationKind`.
    NavigationRequested {
        request: u64,
        url: String,
        kind: u32,
        gesture: bool,
    },
    /// A JavaScript dialog (a `DialogKind`) waits for [`crate::full::FullWebView::dialog_reply`].
    Dialog {
        dialog: u64,
        kind: u32,
        message: String,
        default_value: String,
    },
    DownloadStarted {
        download: u64,
        url: String,
        name: String,
        /// The announced size, or -1.
        total: i64,
    },
    DownloadProgress {
        download: u64,
        received: i64,
        total: i64,
    },
    DownloadDone {
        download: u64,
        name: String,
    },
    DownloadFailed {
        download: u64,
        reason: String,
    },
    NotifyPermissionRequest {
        origin: String,
    },
    NotifyClicked {
        notification: u64,
    },
    /// What became of rule list `list` (a `BlockingStatus`).
    BlockingList {
        list: String,
        status: u32,
    },
    /// Loads the rule lists blocked on the page on show.
    Blocked {
        count: u32,
    },
    Responsive(bool),
    /// webd gave the address to the program the person chose for it.
    HandedOff(String),
    /// (`notify_in_client`) the page asked to show a notification its profile allowed: the app shows
    /// it and answers with [`crate::full::FullWebView::notify_state`]. The texts are the page's own.
    NotifyShow {
        notification: u64,
        origin: String,
        page_url: String,
        title: String,
        body: String,
        tag: String,
    },
    /// The page closed notification `notification` itself: it is over.
    NotifyCancel {
        notification: u64,
    },
    /// The page's object for `notification` is gone: the toast stays, a click reaches no script.
    NotifyGone {
        notification: u64,
    },
    /// The view is gone; [`crate::full::FullWebView::create`] makes a new one.
    Lost(Lost),
}

/// A `BlockingStatus` as a word, for traces.
pub fn blocking_status_name(status: u32) -> &'static str {
    match status {
        wire::BLOCKING_STATUS_READY => "ready",
        wire::BLOCKING_STATUS_NOT_STORED => "not_stored",
        wire::BLOCKING_STATUS_STALE => "stale",
        wire::BLOCKING_STATUS_COMPILE_FAILED => "compile_failed",
        wire::BLOCKING_STATUS_UNAVAILABLE => "unavailable",
        _ => "unknown",
    }
}

/// A decoded event of the wire, before the view applies it.
pub(crate) enum Wire {
    Frame(wire::FrameArgs),
    Band(wire::BandArgs),
    State(wire::StateArgs),
    Lost(wire::ViewLostArgs),
    /// Any other event, already turned into what the app sees, and the view it is about.
    Plain(u64, FullEvent),
    Ping,
}

/// Decode an event; `None` for a method that is not an event or a body that does not decode.
pub(crate) fn decode(method: u32, body: &[u8]) -> Option<Wire> {
    let plain = |view: u64, event: FullEvent| Some(Wire::Plain(view, event));
    match method {
        wire::METHOD_FRAME => Some(Wire::Frame(wire::decode_frame_args(body).ok()?)),
        wire::METHOD_BAND => Some(Wire::Band(wire::decode_band_args(body).ok()?)),
        wire::METHOD_STATE => Some(Wire::State(wire::decode_state_args(body).ok()?)),
        wire::METHOD_VIEWLOST => Some(Wire::Lost(wire::decode_view_lost_args(body).ok()?)),
        wire::METHOD_PING => Some(Wire::Ping),
        wire::METHOD_SCROLL => {
            let a = wire::decode_scroll_args(body).ok()?;
            plain(
                a.view,
                FullEvent::Scroll {
                    x: a.x,
                    y: a.y,
                    at_bottom: a.at_bottom,
                },
            )
        }
        wire::METHOD_HOVER => {
            let a = wire::decode_hover_args(body).ok()?;
            plain(a.view, FullEvent::Hover(a.url))
        }
        wire::METHOD_CURSOR => {
            let a = wire::decode_cursor_args(body).ok()?;
            plain(a.view, FullEvent::Cursor(a.shape))
        }
        wire::METHOD_LINKCLICKED => {
            let a = wire::decode_link_clicked_args(body).ok()?;
            plain(a.view, FullEvent::LinkClicked(a.url))
        }
        wire::METHOD_CLIPBOARDWRITE => {
            let a = wire::decode_clipboard_write_args(body).ok()?;
            plain(a.view, FullEvent::ClipboardWrite(a.text))
        }
        wire::METHOD_NEWWINDOWREQUESTED => {
            let a = wire::decode_new_window_requested_args(body).ok()?;
            let (url, gesture) = (a.url, a.gesture);
            plain(a.view, FullEvent::NewWindowRequested { url, gesture })
        }
        wire::METHOD_NAVIGATIONREQUESTED => {
            let a = wire::decode_navigation_requested_args(body).ok()?;
            let event = FullEvent::NavigationRequested {
                request: a.request,
                url: a.url,
                kind: a.kind,
                gesture: a.gesture,
            };
            plain(a.view, event)
        }
        wire::METHOD_DIALOG => {
            let a = wire::decode_dialog_args(body).ok()?;
            let event = FullEvent::Dialog {
                dialog: a.dialog,
                kind: a.kind,
                message: a.message,
                default_value: a.default_value,
            };
            plain(a.view, event)
        }
        _ => decode_news(method, body),
    }
}

/// The downloads, notifications, blocking and liveness events.
fn decode_news(method: u32, body: &[u8]) -> Option<Wire> {
    let (view, event) = match method {
        wire::METHOD_DOWNLOADSTARTED => {
            let a = wire::decode_download_started_args(body).ok()?;
            let event = FullEvent::DownloadStarted {
                download: a.download,
                url: a.url,
                name: a.name,
                total: a.total,
            };
            (a.view, event)
        }
        wire::METHOD_DOWNLOADPROGRESS => {
            let a = wire::decode_download_progress_args(body).ok()?;
            let event = FullEvent::DownloadProgress {
                download: a.download,
                received: a.received,
                total: a.total,
            };
            (a.view, event)
        }
        wire::METHOD_DOWNLOADDONE => {
            let a = wire::decode_download_done_args(body).ok()?;
            let (download, name) = (a.download, a.name);
            (a.view, FullEvent::DownloadDone { download, name })
        }
        wire::METHOD_DOWNLOADFAILED => {
            let a = wire::decode_download_failed_args(body).ok()?;
            let (download, reason) = (a.download, a.reason);
            (a.view, FullEvent::DownloadFailed { download, reason })
        }
        wire::METHOD_NOTIFYPERMISSIONREQUEST => {
            let a = wire::decode_notify_permission_request_args(body).ok()?;
            let origin = a.origin;
            (a.view, FullEvent::NotifyPermissionRequest { origin })
        }
        wire::METHOD_NOTIFYCLICKED => {
            let a = wire::decode_notify_clicked_args(body).ok()?;
            let notification = a.notification;
            (a.view, FullEvent::NotifyClicked { notification })
        }
        wire::METHOD_BLOCKINGLIST => {
            let a = wire::decode_blocking_list_args(body).ok()?;
            let (list, status) = (a.list, a.status);
            (a.view, FullEvent::BlockingList { list, status })
        }
        wire::METHOD_BLOCKED => {
            let a = wire::decode_blocked_args(body).ok()?;
            (a.view, FullEvent::Blocked { count: a.count })
        }
        wire::METHOD_RESPONSIVE => {
            let a = wire::decode_responsive_args(body).ok()?;
            (a.view, FullEvent::Responsive(a.responsive))
        }
        wire::METHOD_HANDEDOFF => {
            let a = wire::decode_handed_off_args(body).ok()?;
            (a.view, FullEvent::HandedOff(a.url))
        }
        wire::METHOD_NOTIFYSHOW => {
            let a = wire::decode_notify_show_args(body).ok()?;
            let event = FullEvent::NotifyShow {
                notification: a.notification,
                origin: a.origin,
                page_url: a.page_url,
                title: a.title,
                body: a.body,
                tag: a.tag,
            };
            (a.view, event)
        }
        wire::METHOD_NOTIFYCANCEL => {
            let a = wire::decode_notify_cancel_args(body).ok()?;
            let notification = a.notification;
            (a.view, FullEvent::NotifyCancel { notification })
        }
        wire::METHOD_NOTIFYGONE => {
            let a = wire::decode_notify_gone_args(body).ok()?;
            let notification = a.notification;
            (a.view, FullEvent::NotifyGone { notification })
        }
        _ => return None,
    };
    Some(Wire::Plain(view, event))
}

/// The state event as the app sees it.
pub(crate) fn state(args: wire::StateArgs) -> FullState {
    FullState {
        title: args.title,
        loading: args.loading,
        finished: args.finished,
        error: args.error,
        url: args.url,
        committed_url: args.committed_url,
        can_back: args.can_back,
        can_forward: args.can_forward,
        progress: args.progress,
        secure: args.secure,
    }
}
