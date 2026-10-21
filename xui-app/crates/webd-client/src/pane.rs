//! A pane: the life of one light view for an app that shows pages in it (Docs, Mail), apart from its
//! window so the host tests can run it against the real service half.
//!
//! The app gives it HTML ([`Pane::set_html`], replacing the last page: a pane **reuses one view** for
//! every page, because a view that is destroyed leaves a parked proxy behind in webd, docs/webd.md
//! "Limits") and calls [`Pane::tick`] from a timer with the area the picture fills. The pane
//!
//! * waits for `webd` to be there (the app may start first) and says so after a while
//!   ([`Status::NotRunning`], for the "the web engine is not running" screen and its *Start*
//!   button), retrying quietly;
//! * creates the view at the first size it is given, loads the newest page, and follows size changes;
//! * turns the view's events into [`PaneEvent`]s: links (never followed: a light view loads nothing),
//!   copied text, and the first picture of each page ([`PaneEvent::Painted`]);
//! * makes a new view when the view is lost and loads the newest page again, and gives up
//!   ([`Status::Failed`]) after [`GIVE_UP_AFTER`] losses in a row with nothing painted between them,
//!   so a webd that cannot hold a view does not become a loop.

use crate::event::{Error, ViewEvent};
use crate::picture::Picture;
use crate::transport::Transport;
use crate::view::{LightConfig, LightWebView};

/// Milliseconds between tries to create the view while `webd` is not there.
pub const RETRY_MS: u64 = 250;
/// Milliseconds the pane waits before it says `webd` is not running: an app that starts with the
/// session is up a moment before the service registers its name.
pub const NOT_RUNNING_AFTER_MS: u64 = 1500;
/// Milliseconds after a loss before the new view is made.
pub const RECREATE_AFTER_MS: u64 = 500;
/// Losses in a row (no picture between them) after which the pane stops trying by itself.
pub const GIVE_UP_AFTER: u32 = 3;

/// What the pane can show right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Making the first view (or waiting for `webd` for less than [`NOT_RUNNING_AFTER_MS`]).
    Starting,
    /// A view exists.
    Live,
    /// `webd` has no registered name: it is not running.
    NotRunning,
    /// The view cannot be had: the service refused, or it kept losing the view. The text is for a
    /// person, never page content.
    Failed(String),
}

/// What happened, for the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneEvent {
    /// The pane's status changed.
    Status(Status),
    /// A link was clicked, a form submitted or a meta refresh fired; nothing loaded. The app decides
    /// what to do with the (absolute) URL.
    Link(String),
    /// The user copied this text; the app owns the clipboard.
    Copy(String),
    /// The page set with the last [`Pane::set_html`] has been drawn into [`Pane::picture`] (once per
    /// page).
    Painted,
    /// The last [`Pane::set_html`] was refused (too large, say); the old page stays.
    LoadRefused(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No view: create one at `at` (milliseconds) or later.
    Creating {
        at: u64,
    },
    Running,
    /// Gave up; [`Pane::retry`] starts again.
    Dead,
}

/// The state of one light view for an app.
pub struct Pane<T: Transport> {
    view: LightWebView<T>,
    phase: Phase,
    status: Status,
    /// The newest page and its base URL, and whether the live view has been given it.
    page: Option<String>,
    loaded: bool,
    /// While a page waits to be drawn: the view's finished-load count before it was loaded, and
    /// whether a later load has finished since.
    awaiting: Option<Awaiting>,
    /// Consecutive losses with no picture between them.
    losses: u32,
    /// When the first failed attempt of the current run of `NotRunning` was.
    waiting_since: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct Awaiting {
    baseline: u32,
    finished: bool,
}

impl<T: Transport> Pane<T> {
    pub fn new(transport: T, config: LightConfig) -> Self {
        Pane {
            view: LightWebView::new(transport, config),
            phase: Phase::Creating { at: 0 },
            status: Status::Starting,
            page: None,
            loaded: false,
            awaiting: None,
            losses: 0,
            waiting_since: None,
        }
    }

    /// Show `html` (UTF-8, at `about:blank`) in place of the page on show. Takes effect at the next
    /// [`Pane::tick`]; a page set twice before then is loaded once.
    pub fn set_html(&mut self, html: String) {
        self.page = Some(html);
        self.loaded = false;
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The picture on show; empty before the first frame.
    pub fn picture(&self) -> &Picture {
        self.view.picture()
    }

    /// Frames copied into the picture so far (a new view counts again from zero, so compare it
    /// for change, never for order).
    pub fn applied(&self) -> u64 {
        self.view.counters().applied
    }

    /// The view, for input (`mouse`, `wheel`, `key`, `focus`, `set_visible`).
    pub fn view(&mut self) -> &mut LightWebView<T> {
        &mut self.view
    }

    /// The page's title, as of its last finished load.
    pub fn title(&self) -> &str {
        &self.view.page().title
    }

    /// Where the page is scrolled: `(x, y, at_bottom)`.
    pub fn scroll(&self) -> (i32, i32, bool) {
        self.view.scroll()
    }

    /// Try again now: the *Start* button once webd was started, a key after a failure. Forgets the
    /// losses and the time spent waiting.
    pub fn retry(&mut self) {
        self.losses = 0;
        self.waiting_since = None;
        self.phase = Phase::Creating { at: 0 };
        self.status = Status::Starting;
    }

    /// One turn: `size` is the area the picture fills in pixels, `now_ms` any monotonic clock.
    pub fn tick(&mut self, size: (u32, u32), now_ms: u64) -> Vec<PaneEvent> {
        let mut out = Vec::new();
        if size.0 == 0 || size.1 == 0 {
            return out;
        }
        match self.phase {
            Phase::Creating { at } if now_ms >= at => self.create(size, now_ms, &mut out),
            Phase::Running => self.running(size, now_ms, &mut out),
            _ => {}
        }
        out
    }

    fn create(&mut self, size: (u32, u32), now_ms: u64, out: &mut Vec<PaneEvent>) {
        match self.view.create(size.0, size.1) {
            Ok(()) => {
                self.waiting_since = None;
                self.enter_running(out);
                // The new view has no page: give it the newest.
                self.loaded = false;
                self.awaiting = None;
            }
            Err(Error::NotRunning) => {
                self.phase = Phase::Creating {
                    at: now_ms + RETRY_MS,
                };
                let since = *self.waiting_since.get_or_insert(now_ms);
                if now_ms.saturating_sub(since) >= NOT_RUNNING_AFTER_MS {
                    self.set_status(Status::NotRunning, out);
                }
            }
            Err(error) => {
                self.phase = Phase::Dead;
                self.set_status(Status::Failed(error.to_string()), out);
            }
        }
    }

    fn enter_running(&mut self, out: &mut Vec<PaneEvent>) {
        self.phase = Phase::Running;
        self.set_status(Status::Live, out);
    }

    fn set_status(&mut self, status: Status, out: &mut Vec<PaneEvent>) {
        if self.status != status {
            self.status = status.clone();
            out.push(PaneEvent::Status(status));
        }
    }

    fn running(&mut self, size: (u32, u32), now_ms: u64, out: &mut Vec<PaneEvent>) {
        if size != self.view.size() && self.view.is_live() {
            if let Err(error) = self.view.resize(size.0, size.1) {
                if !matches!(error, Error::Lost(_)) {
                    self.set_status(Status::Failed(error.to_string()), out);
                    self.phase = Phase::Dead;
                    return;
                }
            }
        }
        for event in self.view.poll() {
            match event {
                ViewEvent::Frame { .. } => self.frame(out),
                ViewEvent::LinkClicked(url) => out.push(PaneEvent::Link(url)),
                ViewEvent::ClipboardWrite(text) => out.push(PaneEvent::Copy(text)),
                ViewEvent::State(state) => self.state(&state),
                ViewEvent::Lost(_) => {
                    self.lose(now_ms, out);
                    return;
                }
                ViewEvent::Scroll { .. } => {}
            }
        }
        // The page goes in after the poll, so a view found lost is not given it.
        if !self.loaded {
            self.load(out);
        }
        // A call that found the view gone says so only on the next poll: look again.
        if self.view.lost().is_some() && self.phase == Phase::Running {
            self.lose(now_ms, out);
        }
    }

    fn load(&mut self, out: &mut Vec<PaneEvent>) {
        let Some(html) = self.page.clone() else {
            return;
        };
        let baseline = self.view.page().finished;
        match self.view.set_html(&html, "about:blank") {
            Ok(()) => {
                self.loaded = true;
                self.awaiting = Some(Awaiting {
                    baseline,
                    finished: false,
                });
            }
            Err(Error::Refused(errno)) => {
                // This page only: the old one stays, and the next `set_html` may fit.
                self.loaded = true;
                self.awaiting = None;
                out.push(PaneEvent::LoadRefused(format!("refused (errno {errno})")));
            }
            // A lost view is found by `poll`.
            Err(_) => {}
        }
    }

    fn state(&mut self, state: &crate::event::PageState) {
        let Some(awaiting) = self.awaiting.as_mut() else {
            return;
        };
        if !awaiting.finished && !state.loading && state.finished > awaiting.baseline {
            awaiting.finished = true;
            // The picture webd sends next is of the finished page.
            self.view.refresh();
        }
    }

    fn frame(&mut self, out: &mut Vec<PaneEvent>) {
        if self.awaiting.is_some_and(|awaiting| awaiting.finished) {
            self.awaiting = None;
            self.losses = 0;
            out.push(PaneEvent::Painted);
        }
    }

    fn lose(&mut self, now_ms: u64, out: &mut Vec<PaneEvent>) {
        self.losses += 1;
        self.awaiting = None;
        if self.losses >= GIVE_UP_AFTER {
            self.phase = Phase::Dead;
            self.set_status(
                Status::Failed("the web engine keeps failing".to_owned()),
                out,
            );
        } else {
            self.phase = Phase::Creating {
                at: now_ms + RECREATE_AFTER_MS,
            };
        }
    }
}
