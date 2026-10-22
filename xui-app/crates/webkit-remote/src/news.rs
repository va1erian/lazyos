//! What a full view's events become for the window: the page's state, the seam's [`Event`]s, the
//! bands, and the answers webd needs at once (a held navigation is refused here; the window's
//! policy acts on it instead).

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd_client::full::{FullEvent, FullState};
use xui_webd_client::transport::ScopedTransport;
use xui_webkit::abi::{byte_count, ListStatus, NavKind};
use xui_webkit::bands::{self, Band};
use xui_webkit::engine::DialogKind;
use xui_webkit::events::{CursorKind, Event};
use xui_webkit::mailbox::PageState;

use crate::{Navigation, RemoteEngine};

/// The page's state as the seam has it (`crashed` is a lost view, never a state).
pub fn page_state(state: &FullState) -> PageState {
    PageState {
        url: state.url.clone(),
        committed_url: state.committed_url.clone(),
        secure: state.secure,
        title: state.title.clone(),
        loading: state.loading,
        can_back: state.can_back,
        can_forward: state.can_forward,
        progress: state.progress.min(1000),
        error: state.error.clone(),
        crashed: false,
        finished: state.finished,
    }
}

/// The seam's event for a view event that is only news (`None` for the rest).
pub fn plain(event: &FullEvent) -> Option<Event> {
    Some(match event.clone() {
        FullEvent::Hover(url) => Event::Hover(url),
        FullEvent::Cursor(shape) => Event::Cursor(CursorKind::from_code(shape as i32)),
        FullEvent::ClipboardWrite(text) => Event::Clipboard(text),
        // Only a request a click or a key started opens anything, as before.
        FullEvent::NewWindowRequested { url, gesture: true } => Event::Popup { url },
        FullEvent::Dialog {
            dialog,
            kind,
            message,
            default_value,
        } => Event::Dialog {
            id: dialog,
            kind: DialogKind::from_code(kind as i32),
            text: message,
            default: default_value,
        },
        FullEvent::DownloadStarted {
            download,
            url,
            name,
            total,
        } => Event::DownloadStarted {
            id: download,
            url,
            name,
            total: byte_count(total),
        },
        FullEvent::DownloadProgress {
            download,
            received,
            total,
        } => Event::DownloadProgress {
            id: download,
            received: byte_count(received).unwrap_or(0),
            total: byte_count(total),
        },
        FullEvent::DownloadDone { download, name } => Event::DownloadDone { id: download, name },
        FullEvent::DownloadFailed { download, reason } => Event::DownloadFailed {
            id: download,
            reason,
        },
        // WebKit refuses a request no gesture started; the window checks its own input again.
        FullEvent::NotifyPermissionRequest { origin } => Event::NotifyPermission {
            origin,
            user_gesture: true,
        },
        FullEvent::NotifyShow {
            notification,
            origin,
            page_url,
            title,
            body,
            tag,
        } => Event::NotifyShow {
            id: notification,
            origin,
            page_url,
            title,
            body,
            tag,
        },
        FullEvent::NotifyCancel { notification } => Event::NotifyCancel { id: notification },
        FullEvent::NotifyGone { notification } => Event::NotifyGone { id: notification },
        FullEvent::BlockingList { list, status } => Event::BlockingList {
            id: list,
            status: list_status(status),
        },
        FullEvent::Blocked { count } => Event::Blocked(count),
        FullEvent::Responsive(responsive) => Event::PageResponsive(responsive),
        _ => return None,
    })
}

/// webd's `BlockingStatus` as the seam's (the same order; anything unknown is a failed compile).
fn list_status(status: u32) -> ListStatus {
    match status {
        wire::BLOCKING_STATUS_READY => ListStatus::Ready,
        wire::BLOCKING_STATUS_NOT_STORED => ListStatus::NotStored,
        wire::BLOCKING_STATUS_STALE => ListStatus::Stale,
        wire::BLOCKING_STATUS_UNAVAILABLE => ListStatus::Unavailable,
        _ => ListStatus::CompileFailed,
    }
}

/// webd's `NavigationKind` as WebKit's (the seam's numbering is WebKit's too).
fn nav_kind(kind: u32) -> NavKind {
    match kind {
        wire::NAVIGATION_KIND_LINK_CLICKED => NavKind::LinkClicked,
        wire::NAVIGATION_KIND_FORM_SUBMITTED => NavKind::FormSubmitted,
        wire::NAVIGATION_KIND_BACK_FORWARD => NavKind::BackForward,
        wire::NAVIGATION_KIND_RELOAD => NavKind::Reload,
        wire::NAVIGATION_KIND_FORM_RESUBMITTED => NavKind::FormResubmitted,
        _ => NavKind::Other,
    }
}

impl<T: ScopedTransport> RemoteEngine<T> {
    /// One event of the view.
    pub(crate) fn news(&self, event: FullEvent) {
        match event {
            FullEvent::State(state) => self.post_view(page_state(&state)),
            FullEvent::Band(band) => {
                bands::post(Band {
                    doc_x: band.doc_x,
                    doc_y: band.doc_y,
                    width: band.width,
                    height: band.height,
                    rgba: band.rgba,
                });
                self.mailbox.announce_bands();
            }
            FullEvent::NavigationRequested {
                request,
                url,
                kind,
                gesture,
            } => {
                // Never loaded by the view: webd would load only the web anyway, and the window's
                // policy has no "allow" for another scheme.
                let answered = self
                    .inner
                    .borrow_mut()
                    .view
                    .navigation_answer(request, false);
                self.answer(answered, None);
                let committed_url = self.inner.borrow().state.committed_url.clone();
                let navigation = Navigation {
                    url,
                    kind: nav_kind(kind),
                    gesture,
                    committed_url,
                };
                if let Some(news) = (self.navigator)(&navigation) {
                    self.mailbox.post_event(news);
                }
            }
            FullEvent::Lost(lost) => self.lost(lost),
            other => {
                if let Some(news) = plain(&other) {
                    self.mailbox.post_event(news);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_orders_match_the_seams() {
        for (wire, seam) in [
            (wire::BLOCKING_STATUS_READY, ListStatus::Ready),
            (wire::BLOCKING_STATUS_NOT_STORED, ListStatus::NotStored),
            (wire::BLOCKING_STATUS_STALE, ListStatus::Stale),
            (wire::BLOCKING_STATUS_COMPILE_FAILED, ListStatus::CompileFailed),
            (wire::BLOCKING_STATUS_UNAVAILABLE, ListStatus::Unavailable),
        ] {
            assert_eq!(list_status(wire), seam);
            assert_eq!(ListStatus::from_code(wire as i32), seam, "same numbers");
        }
        assert_eq!(nav_kind(wire::NAVIGATION_KIND_LINK_CLICKED), NavKind::LinkClicked);
        assert_eq!(nav_kind(99), NavKind::Other);
        assert_eq!(
            CursorKind::from_code(wire::CURSOR_SHAPE_RESIZE_NESW as i32),
            CursorKind::ResizeNeSw
        );
        assert_eq!(
            DialogKind::from_code(wire::DIALOG_KIND_PROMPT as i32),
            DialogKind::Prompt
        );
    }

    #[test]
    fn a_window_request_without_a_gesture_opens_nothing() {
        let quiet = FullEvent::NewWindowRequested {
            url: "https://a.test/".into(),
            gesture: false,
        };
        assert_eq!(plain(&quiet), None);
        let clicked = FullEvent::NewWindowRequested {
            url: "https://a.test/".into(),
            gesture: true,
        };
        assert_eq!(
            plain(&clicked),
            Some(Event::Popup {
                url: "https://a.test/".into()
            })
        );
    }
}
