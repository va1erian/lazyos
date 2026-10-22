//! What the service asks of the rest of the system for a full view, as a trait the host implements
//! over Messenger (`xui-app/webd`) and the tests record: handing an address to the program the person
//! chose for it (`mimed`, `Handoff.Browser`), and the shell's toasts for a page's notifications.
//!
//! Both are made from the service thread, which is the process's first thread: the shell's
//! notification service accepts no other (docs/lazywebplus.md, "Toasts are made on WebKit's main
//! thread"; webd's first thread is its service, docs/webd-plan.md "P2 slice 2").

/// A toast the shell reported about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastNews {
    /// The person clicked toast `toast`.
    Clicked { toast: u64 },
    /// Toast `toast` went away (dismissed, timed out, replaced).
    Closed { toast: u64 },
    /// The line the news came on is gone (the shell restarted): no toast shown so far will be
    /// reported again, so the service forgets them all.
    Lost,
}

/// The system around webd. Every method answers at once; the defaults refuse, for a service
/// (or a test) that has no such system.
pub trait Outside {
    /// Open `url` with the program the person chose for its scheme; `true` when it was handed over.
    fn open_url(&self, _url: &str) -> bool {
        false
    }

    /// Show a toast; its id, or `None` when the shell would not (or its news could not be heard: a
    /// toast whose click and close never come back would hold a view's toast count for good).
    fn toast(&self, _title: &str, _body: &str) -> Option<u64> {
        None
    }

    /// Take toast `toast` down.
    fn close_toast(&self, _toast: u64) {}

    /// What the shell reported since the last call, oldest first.
    fn toast_news(&self) -> Vec<ToastNews> {
        Vec::new()
    }
}

/// No system: nothing is handed over and no toast is shown.
pub struct Nowhere;

impl Outside for Nowhere {}
