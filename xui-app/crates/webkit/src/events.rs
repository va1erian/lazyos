//! What the engine reports besides frames and page state: discrete news the window acts on
//! (the link under the pointer, a download, a dialog, a link to hand to another app).
//!
//! The engine posts from WebKit's run loop, the window drains from its own thread. Like frames
//! and state the queue wakes the window once per burst, and it is bounded: a page that loops on
//! `location = "mailto:..."` or `window.open` cannot make it grow without limit (what a burst
//! overflows is dropped, never the news of a download or a dialog, which one WebKit call each
//! produces).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::abi::ListStatus;
use crate::engine::DialogKind;

/// The most events kept for the window; see the module note.
pub const MAX_QUEUED: usize = 1024;

/// The pointer shape the page asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorKind {
    #[default]
    Default,
    Hand,
    Text,
    Wait,
    Hidden,
    ResizeHorizontal,
    ResizeVertical,
    ResizeNwSe,
    ResizeNeSw,
}

impl CursorKind {
    /// The ABI value (`lzw_ui_cursor`).
    pub const fn code(self) -> i32 {
        match self {
            CursorKind::Default => 0,
            CursorKind::Hand => 1,
            CursorKind::Text => 2,
            CursorKind::Wait => 3,
            CursorKind::Hidden => 4,
            CursorKind::ResizeHorizontal => 5,
            CursorKind::ResizeVertical => 6,
            CursorKind::ResizeNwSe => 7,
            CursorKind::ResizeNeSw => 8,
        }
    }

    /// The shape an ABI value names (the arrow for an unknown one).
    pub const fn from_code(code: i32) -> CursorKind {
        match code {
            1 => CursorKind::Hand,
            2 => CursorKind::Text,
            3 => CursorKind::Wait,
            4 => CursorKind::Hidden,
            5 => CursorKind::ResizeHorizontal,
            6 => CursorKind::ResizeVertical,
            7 => CursorKind::ResizeNwSe,
            8 => CursorKind::ResizeNeSw,
            _ => CursorKind::Default,
        }
    }
}

/// One thing the engine reported.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The link under the pointer; empty when there is none.
    Hover(String),
    /// The pointer shape over the page.
    Cursor(CursorKind),
    /// A navigation to a scheme the engine does not load (`mailto:`, ...), for the app that
    /// registered it. `by_user` is false when no user gesture started it: the window refuses.
    Launch {
        url: String,
        by_user: bool,
    },
    /// A link to one of the browser's own commands (`x-lazyweb:...`) that the navigation policy
    /// accepted: it came from one of the browser's pages, from a user gesture.
    Command {
        url: String,
    },
    /// A page asked for a new window (`target=_blank`, `window.open`) with a user gesture; the
    /// window opens `url` in its one view.
    Popup {
        url: String,
    },
    /// A download began: `path` is where the engine writes it (the window chose it).
    DownloadStarted {
        id: u64,
        url: String,
        name: String,
        total: Option<u64>,
        path: String,
    },
    DownloadProgress {
        id: u64,
        received: u64,
        total: Option<u64>,
    },
    DownloadDone {
        id: u64,
    },
    DownloadFailed {
        id: u64,
        reason: String,
    },
    /// A page opened a JavaScript dialog and waits for [`Engine::dialog_reply`](crate::engine::Engine::dialog_reply).
    Dialog {
        id: u64,
        kind: DialogKind,
        text: String,
        default: String,
    },
    /// A page asked to show a notification (`new Notification(...)`, WebKit's provider `show`).
    /// `origin` is the page's security origin as WebKit writes it (`https://example.com`),
    /// `page_url` the address of the page that asked; the texts are the page's own, untrusted.
    /// The window answers with [`Engine::notify_shown`](crate::engine::Engine::notify_shown) once
    /// it is on screen, or says nothing when it drops it.
    NotifyShow {
        id: u64,
        origin: String,
        page_url: String,
        title: String,
        body: String,
        tag: String,
    },
    /// The page took notification `id` down itself (`Notification.close()`).
    NotifyCancel {
        id: u64,
    },
    /// The page's `Notification` object `id` is gone (its document went away). The toast is the
    /// user's now and stays; a click can no longer reach a script.
    NotifyGone {
        id: u64,
    },
    /// A page called `Notification.requestPermission()` and waits for
    /// [`Engine::notify_permission`](crate::engine::Engine::notify_permission).
    /// `user_gesture` is whether real input from the person started it (WebKit refuses the call
    /// without one, and the window checks again).
    NotifyPermission {
        origin: String,
        user_gesture: bool,
    },
    /// The requests for `origin` that were waiting were refused by the engine itself (their page
    /// navigated away or its process went): the question is not the person's to answer any more,
    /// and the engine needs no answer.
    NotifyPermissionGone {
        origin: String,
    },
    /// What became of rule list `id` after `blocking_load` or `blocking_compile`.
    BlockingList {
        id: String,
        status: ListStatus,
    },
    /// The number of loads the rule lists blocked on the page on show (0 when a page commits).
    Blocked(u32),
    /// The page's WebProcess answers WebKit's responsiveness pings again (`true`), or has not for
    /// six seconds (`false`): the window offers a way out (`Engine::recover`). `true` is also what
    /// a process that went away reports, there being nothing to wait for.
    PageResponsive(bool),
}

impl Event {
    /// Whether a newer event of the same kind makes this one obsolete (the pointer moved on).
    fn superseded_by(&self, newer: &Event) -> bool {
        match (self, newer) {
            (Event::Hover(_), Event::Hover(_))
            | (Event::Cursor(_), Event::Cursor(_))
            | (Event::Blocked(_), Event::Blocked(_))
            | (Event::PageResponsive(_), Event::PageResponsive(_)) => true,
            (Event::DownloadProgress { id: a, .. }, Event::DownloadProgress { id: b, .. }) => {
                a == b
            }
            _ => false,
        }
    }

    /// Whether the event may be dropped when the queue is full: news a page can produce in a
    /// loop (a notification costs the page a call, and the window limits them per site anyway).
    /// Downloads, dialogs and permission requests (each needs a user gesture) are one WebKit call
    /// each, and the window must see all of them: the page waits for an answer.
    fn droppable(&self) -> bool {
        matches!(
            self,
            Event::Hover(_)
                | Event::Cursor(_)
                | Event::Launch { .. }
                | Event::Command { .. }
                | Event::Popup { .. }
                | Event::NotifyShow { .. }
                | Event::NotifyCancel { .. }
                | Event::NotifyGone { .. }
                | Event::Blocked(_)
        )
    }
}

/// The queue between the engine's thread and the window's.
#[derive(Default)]
pub struct Events {
    queue: Mutex<VecDeque<Event>>,
    waiting: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Events {
    /// Queues `event`; true when the window must be woken (nothing was waiting for it).
    pub fn push(&self, event: Event) -> bool {
        {
            let mut queue = lock(&self.queue);
            queue.retain(|old| !old.superseded_by(&event));
            if queue.len() >= MAX_QUEUED && event.droppable() {
                return false;
            }
            queue.push_back(event);
        }
        !self.waiting.swap(true, Ordering::AcqRel)
    }

    /// Whether events wait for the window (it has been woken for them, or will be).
    pub fn pending(&self) -> bool {
        self.waiting.load(Ordering::Acquire)
    }

    /// Everything queued, oldest first.
    pub fn take(&self) -> Vec<Event> {
        let mut queue = lock(&self.queue);
        // Cleared under the lock `push` takes: an event pushed after this point sets the flag
        // again and wakes the window, one pushed before it is in what is returned.
        self.waiting.store(false, Ordering::Release);
        queue.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_wakes_once_and_keeps_every_discrete_event_in_order() {
        let events = Events::default();
        assert!(events.push(Event::Popup { url: "a".into() }));
        assert!(!events.push(Event::Popup { url: "b".into() }));
        assert!(!events.push(Event::DownloadDone { id: 3 }));
        assert_eq!(
            events.take(),
            vec![
                Event::Popup { url: "a".into() },
                Event::Popup { url: "b".into() },
                Event::DownloadDone { id: 3 },
            ]
        );
        assert!(events.take().is_empty());
        assert!(
            events.push(Event::Hover("x".into())),
            "collected: wakes again"
        );
    }

    #[test]
    fn the_pointers_news_keeps_only_the_newest() {
        let events = Events::default();
        events.push(Event::Hover("http://a/".into()));
        events.push(Event::Cursor(CursorKind::Hand));
        events.push(Event::Hover(String::new()));
        events.push(Event::Cursor(CursorKind::Text));
        assert_eq!(
            events.take(),
            vec![Event::Hover(String::new()), Event::Cursor(CursorKind::Text)]
        );
    }

    #[test]
    fn the_pages_responsiveness_keeps_only_the_newest_report() {
        let events = Events::default();
        events.push(Event::PageResponsive(false));
        events.push(Event::PageResponsive(true));
        assert_eq!(events.take(), vec![Event::PageResponsive(true)]);
    }

    #[test]
    fn the_blocked_count_keeps_only_the_newest_and_a_list_result_is_never_dropped() {
        let events = Events::default();
        events.push(Event::Blocked(1));
        events.push(Event::Blocked(2));
        events.push(Event::BlockingList {
            id: "easylist".into(),
            status: ListStatus::Ready,
        });
        events.push(Event::Blocked(3));
        assert_eq!(
            events.take(),
            vec![
                Event::BlockingList {
                    id: "easylist".into(),
                    status: ListStatus::Ready,
                },
                Event::Blocked(3),
            ]
        );
        // A page that loops cannot grow the queue; the list's result still arrives.
        for n in 0..(MAX_QUEUED * 2) {
            events.push(Event::Launch {
                url: format!("mailto:{n}"),
                by_user: false,
            });
        }
        events.push(Event::BlockingList {
            id: "x".into(),
            status: ListStatus::CompileFailed,
        });
        let taken = events.take();
        assert!(matches!(taken.last(), Some(Event::BlockingList { .. })));
    }

    #[test]
    fn progress_is_replaced_per_download() {
        let events = Events::default();
        let progress = |id, received| Event::DownloadProgress {
            id,
            received,
            total: Some(100),
        };
        events.push(progress(1, 10));
        events.push(progress(2, 20));
        events.push(progress(1, 30));
        assert_eq!(events.take(), vec![progress(2, 20), progress(1, 30)]);
    }

    #[test]
    fn a_page_that_loops_cannot_grow_the_queue_but_downloads_and_dialogs_get_through() {
        let events = Events::default();
        for n in 0..(MAX_QUEUED * 3) {
            events.push(Event::Launch {
                url: format!("mailto:{n}"),
                by_user: false,
            });
        }
        events.push(Event::Dialog {
            id: 1,
            kind: DialogKind::Alert,
            text: "hi".into(),
            default: String::new(),
        });
        events.push(Event::DownloadFailed {
            id: 2,
            reason: "x".into(),
        });
        let taken = events.take();
        assert_eq!(taken.len(), MAX_QUEUED + 2);
        assert!(matches!(taken[MAX_QUEUED], Event::Dialog { id: 1, .. }));
    }

    #[test]
    fn a_page_that_posts_notifications_in_a_loop_cannot_grow_the_queue_either() {
        let events = Events::default();
        for n in 0..(MAX_QUEUED * 2) {
            events.push(Event::NotifyShow {
                id: n as u64,
                origin: "https://a.test".into(),
                page_url: "https://a.test/".into(),
                title: "t".into(),
                body: String::new(),
                tag: String::new(),
            });
        }
        events.push(Event::NotifyPermission {
            origin: "https://a.test".into(),
            user_gesture: true,
        });
        events.push(Event::DownloadDone { id: 9 });
        let taken = events.take();
        assert_eq!(
            taken.len(),
            MAX_QUEUED + 2,
            "the request and the download got through"
        );
        assert!(matches!(taken[MAX_QUEUED], Event::NotifyPermission { .. }));
        assert!(matches!(
            taken[MAX_QUEUED + 1],
            Event::DownloadDone { id: 9 }
        ));
    }

    #[test]
    fn cursor_codes_round_trip_and_unknown_is_the_arrow() {
        for kind in [
            CursorKind::Default,
            CursorKind::Hand,
            CursorKind::Text,
            CursorKind::Wait,
            CursorKind::Hidden,
            CursorKind::ResizeHorizontal,
            CursorKind::ResizeVertical,
            CursorKind::ResizeNwSe,
            CursorKind::ResizeNeSw,
        ] {
            assert_eq!(CursorKind::from_code(kind.code()), kind);
        }
        assert_eq!(CursorKind::from_code(99), CursorKind::Default);
        assert_eq!(CursorKind::from_code(-1), CursorKind::Default);
    }
}
