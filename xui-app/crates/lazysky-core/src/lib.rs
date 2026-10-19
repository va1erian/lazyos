//! LazySky's headless core (docs/lazysky-plan.md): everything a Bluesky
//! client does that is not drawing. No xui type, no LazyOS crate and no OS
//! call appears here; the front end supplies a [`Platform`] and drains
//! [`Events`]. `cargo tree -p lazysky-core` must list no `xui-*`.
//!
//! The core is driven by [`Request`]s and answers with [`Event`]s over
//! channels, so any front end (xui, a terminal, a test) can use it. Neither
//! [`Core::send`] nor [`Events::try_next`] ever blocks.

pub mod feed;
pub mod images;
pub mod net;
pub mod platform;
pub mod proto;
pub mod save;
pub mod settings;
mod worker;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub use images::{Bitmap, ImageKey, Shape};
pub use net::{Http, HttpOptions, Response};
pub use platform::{Platform, Roots};
pub use proto::{Page, Post};
pub use settings::{Settings, ThemeChoice};

pub use lazyhttps::install_tls_provider;

/// A password. It has no `Debug` output of its own worth reading and is
/// never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Which posts of an account a [`FeedSource::Author`] lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorFilter {
    /// Posts, without replies.
    Posts,
    /// Posts with pictures or video.
    Media,
}

/// Which list of posts a [`Request::Feed`] fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedSource {
    /// The signed-in user's home timeline.
    Timeline,
    /// An account's posts (`actor` is a handle or a DID).
    Author { actor: String, filter: AuthorFilter },
    /// The posts an account liked.
    ActorLikes { actor: String },
    /// The signed-in user's bookmarks.
    Bookmarks,
    /// Posts matching `query`.
    SearchPosts { query: String },
}

/// Which list of accounts a [`Request::Actors`] fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorList {
    LikedBy(String),
    RepostedBy(String),
    Search(String),
}

/// What a front end asks for. Every request carries a `seq`, echoed in its
/// events, so a UI can drop answers for pages it has left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Signs in with a handle and an app password.
    Login {
        seq: u64,
        handle: String,
        password: Secret,
    },
    /// Forgets the session.
    Logout { seq: u64 },
    /// One page of a feed, from `cursor` (or the newest).
    Feed {
        seq: u64,
        source: FeedSource,
        cursor: Option<String>,
    },
    /// A post with its ancestors and replies.
    Thread { seq: u64, uri: String },
    /// A page of accounts.
    Actors {
        seq: u64,
        list: ActorList,
        cursor: Option<String>,
    },
    /// Pretty-prints a post's JSON (the Raw view).
    Raw {
        seq: u64,
        json: Arc<serde_json::Value>,
    },
    /// Likes `subject`.
    Like { seq: u64, subject: proto::StrongRef },
    /// Reposts `subject`.
    Repost { seq: u64, subject: proto::StrongRef },
    /// Deletes one of the signed-in user's own records (a post, or the
    /// record of a like or repost to undo it).
    DeleteRecord { seq: u64, uri: String },
    /// One account's profile (a handle or a DID).
    Profile { seq: u64, actor: String },
    /// A page of notifications.
    Notifications { seq: u64, cursor: Option<String> },
    /// How many notifications are unread.
    UnreadCount { seq: u64 },
    /// Marks every notification seen up to now.
    MarkSeen { seq: u64 },
    /// Points sign-in at another service (while signed out).
    Configure { seq: u64, service: String },
    /// Writes `settings.toml` (on the api thread, never the UI's).
    SaveSettings { seq: u64, settings: Settings },
    /// Saves the picture at `url` to `path` (through `<path>.part`).
    SaveImage {
        seq: u64,
        url: String,
        path: std::path::PathBuf,
    },
    /// Saves every picture of an account's own posts into `dir`. Progress
    /// arrives as [`Event::BulkProgress`], the end as [`Event::BulkDone`];
    /// [`Core::cancel_bulk`] stops it.
    BulkDownload {
        seq: u64,
        actor: String,
        dir: std::path::PathBuf,
    },
    /// Writes a post, uploading up to four picture files first.
    CreatePost {
        seq: u64,
        text: String,
        /// `(root, parent)` when it is a reply.
        reply: Option<(proto::StrongRef, proto::StrongRef)>,
        pictures: Vec<std::path::PathBuf>,
    },
}

/// The signed-in account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub did: String,
    pub handle: String,
}

/// What the core reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    LoggedIn {
        seq: u64,
        account: Account,
    },
    LoginFailed {
        seq: u64,
        reason: String,
    },
    LoggedOut {
        seq: u64,
    },
    /// A page of a feed. `cursor` is the one the request carried.
    Feed {
        seq: u64,
        source: FeedSource,
        cursor: Option<String>,
        page: Page,
    },
    /// A request failed. `signed_out` says the session is gone for good.
    Failed {
        seq: u64,
        reason: String,
        signed_out: bool,
    },
    /// Something worth a status line that is not an answer.
    Status {
        line: String,
    },
    /// One account's profile.
    Profile {
        seq: u64,
        profile: proto::Profile,
    },
    /// A page of notifications.
    Notifications {
        seq: u64,
        items: Vec<proto::Notification>,
        cursor: Option<String>,
    },
    /// The unread notification count.
    Unread {
        seq: u64,
        count: u64,
    },
    /// A request with nothing to return finished (seen, configured, saved).
    Done {
        seq: u64,
    },
    /// A picture was saved.
    Saved {
        seq: u64,
        path: std::path::PathBuf,
        bytes: u64,
    },
    /// A bulk download is under way.
    BulkProgress {
        seq: u64,
        /// Posts looked at so far.
        scanned_posts: usize,
        /// Pictures found so far.
        found: usize,
        /// Pictures settled (saved, already there, failed or cancelled).
        finished: usize,
        /// The scan is still reading pages.
        scanning: bool,
    },
    /// A bulk download ended.
    BulkDone {
        seq: u64,
        saved: usize,
        /// Already on disk: left alone.
        skipped: usize,
        errors: usize,
        cancelled: bool,
    },
    /// A thread.
    Thread {
        seq: u64,
        thread: proto::Thread,
    },
    /// A page of accounts.
    Actors {
        seq: u64,
        actors: Vec<proto::Actor>,
        cursor: Option<String>,
    },
    /// A post's JSON, indented.
    Raw {
        seq: u64,
        text: String,
    },
    /// A like, repost or post was written; `uri` is its record.
    Created {
        seq: u64,
        uri: String,
        cid: String,
    },
    /// A record was deleted.
    Deleted {
        seq: u64,
        uri: String,
    },
    /// A picture, at the size asked for, or why there is none.
    Image {
        key: ImageKey,
        result: Result<Bitmap, String>,
    },
}

/// The core's handle: a request sender and the worker threads behind it.
pub struct Core {
    requests: Sender<worker::Message>,
    closing: Arc<AtomicBool>,
    api: Option<thread::JoinHandle<()>>,
    images: images::ImagePool,
    bulk_cancel: Arc<AtomicBool>,
}

/// The receiving end of the core's events.
pub struct Events {
    rx: Receiver<Event>,
}

impl Events {
    /// The next event, or `None` when nothing is waiting.
    pub fn try_next(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Waits up to `timeout` for the next event (tests and the CLI only;
    /// never call it from a UI thread).
    pub fn wait(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Core {
    /// Starts the `api` thread, which owns the one HTTP client and the
    /// session.
    pub fn start(platform: Arc<dyn Platform>, settings: Settings) -> (Core, Events) {
        let (requests, request_rx) = mpsc::channel();
        let (event_tx, rx) = mpsc::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&closing);
        let images =
            images::ImagePool::start(platform.ca_roots(), event_tx.clone(), images::THREADS);
        let bulk_cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&bulk_cancel);
        let api = thread::Builder::new()
            .name("sky-api".into())
            .spawn(move || worker::run(platform, settings, request_rx, event_tx, flag, cancel))
            .expect("the api thread starts");
        (
            Core {
                requests,
                closing,
                api: Some(api),
                images,
                bulk_cancel,
            },
            Events { rx },
        )
    }

    /// Queues `request`; never blocks.
    pub fn send(&self, request: Request) {
        let _ = self.requests.send(worker::Message::Request(request));
    }

    /// Stops a running bulk download: no new page is read and the pictures
    /// still waiting are skipped. Never blocks.
    pub fn cancel_bulk(&self) {
        self.bulk_cancel.store(true, Ordering::SeqCst);
    }

    /// Replaces the pictures the image threads work through (nearest first):
    /// what still waits and is not listed is dropped (R6). Answers arrive as
    /// [`Event::Image`]. Never blocks.
    pub fn want_images(&self, keys: Vec<ImageKey>) {
        self.images.set_wanted(keys);
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.requests.send(worker::Message::Shutdown);
        if let Some(api) = self.api.take() {
            let _ = api.join();
        }
    }
}
