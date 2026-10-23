//! The view's life: made when the page view first has a size, told what the person decided
//! (blocking, notifications) each time it is made, and made again after it is lost.
//!
//! * A page whose process ended (a crash, the memory, the hang bar's Stop) is lost by webd
//!   (`ViewLost`): the page is blank and says so (the state's `crashed`), as it was when the engine
//!   ran in this process, and the next load makes a new view.
//! * webd going away (its channel closed, no service) is retried every [`RETRY_MS`] and the page on
//!   show is loaded again in the new view; until then the status says the engine is not running.

use xui_webd_client::transport::ScopedTransport;
use xui_webd_client::{Error, FullWebView, Lost};
use xui_webkit::engine::NotifyAnswer;
use xui_webkit::mailbox::PageState;

use messenger_generated::os_lazy_webd_view_v1 as wire;

use crate::{RemoteEngine, RETRY_MS};

/// Failed tries to make the view before the page says why (at one a second: webd registers in a few
/// seconds after login, and LazyWeb may open first).
const QUIET_TRIES: u32 = 5;
/// How much longer than [`RETRY_MS`] a refused view waits before it is asked for again (a refusal is
/// the configuration or a quota: it rarely changes by itself).
const REFUSED_BACKOFF: u64 = 10;

/// Where the view stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// No size yet: nothing asked of webd.
    Waiting,
    Live,
    /// webd is not there (or went away): asked again every [`RETRY_MS`].
    NotRunning,
    /// webd refused the view (this errno): asked again every [`RETRY_MS`] times `REFUSED_BACKOFF`.
    Refused(i64),
    /// The page's process ended; the next load makes a new view.
    Ended,
}

impl Health {
    pub(crate) fn retrying(self) -> bool {
        matches!(self, Health::NotRunning | Health::Refused(_))
    }
}

/// webd's word for a permission answer.
pub(crate) fn answer_code(answer: NotifyAnswer) -> u32 {
    match answer {
        NotifyAnswer::Allow => wire::PERMISSION_ANSWER_ALLOW,
        NotifyAnswer::Block => wire::PERMISSION_ANSWER_BLOCK,
        NotifyAnswer::Dismiss => wire::PERMISSION_ANSWER_DISMISS,
    }
}

/// What the status line says about a lost view.
fn lost_text(lost: Lost) -> &'static str {
    match lost {
        Lost::ServiceGone => "The web engine (webd) stopped; reconnecting",
        Lost::Engine(wire::LOST_REASON_OUT_OF_MEMORY) => "The page's process ran out of memory",
        Lost::Engine(wire::LOST_REASON_CRASHED) => "The page's process crashed",
        Lost::Engine(_) => "The page's process ended",
    }
}

impl<T: ScopedTransport> RemoteEngine<T> {
    /// Makes the view when there is a size and none is live; loads what waited.
    pub(crate) fn ensure_view(&self) {
        let mut inner = self.inner.borrow_mut();
        let Some((width, height)) = inner.size else {
            return;
        };
        if inner.view.is_live() {
            return;
        }
        let made = inner.view.create(width, height);
        drop(inner);
        match made {
            Ok(()) => self.made(),
            Err(Error::NotRunning | Error::Lost(_) | Error::NoView) => {
                self.unhealthy(Health::NotRunning, "The web engine (webd) is not running")
            }
            Err(Error::Refused(code)) => self.unhealthy(
                Health::Refused(code),
                "The web engine (webd) refused the browser's page",
            ),
        }
    }

    /// The view is up: tell it what the person decided, run what waited, load what waited.
    fn made(&self) {
        let pending = {
            let mut inner = self.inner.borrow_mut();
            inner.health = super::Health::Live;
            inner.tries = 0;
            inner.said = None;
            inner.loaded = false;
            inner.finished_base = inner.state.finished;
            let inner = &mut *inner;
            let view = &mut inner.view;
            if let Some((enabled, exceptions)) = &inner.blocking {
                let _ = view.set_blocking(*enabled, exceptions);
            }
            for (origin, answer) in &inner.permissions {
                let _ = view.notify_permission(origin, answer_code(*answer));
            }
            for origin in std::mem::take(&mut inner.forgotten) {
                let _ = view.notify_forget(&origin);
            }
            for job in std::mem::take(&mut inner.deferred) {
                job(view);
            }
            inner.pending.take()
        };
        self.notice_channel();
        if let Some(url) = pending {
            self.issue_load(&url);
        }
    }

    /// No view: say why, once (a webd that has not registered yet, at start, only after
    /// [`QUIET_TRIES`]), and come back later when it is worth asking again.
    fn unhealthy(&self, health: Health, why: &str) {
        let now = self.now_ms();
        let quiet = {
            let mut inner = self.inner.borrow_mut();
            let wait = match health {
                Health::Refused(_) => RETRY_MS * REFUSED_BACKOFF,
                _ => RETRY_MS,
            };
            inner.health = health;
            inner.retry_at = now + wait;
            inner.tries = inner.tries.saturating_add(1);
            let starting = health == Health::NotRunning
                && inner.tries < QUIET_TRIES
                && inner.state.committed_url.is_empty();
            let said = inner.said == Some(health);
            if !starting && !said {
                inner.said = Some(health);
            }
            starting || said
        };
        self.notice_channel();
        if quiet {
            return;
        }
        self.post_own(|state| {
            state.loading = false;
            state.crashed = true;
            state.error = why.to_owned();
        });
    }

    /// A view webd could not make, or one webd took away, is asked for again when it is time.
    pub(crate) fn retry_if_due(&self, now: u64) {
        let due = {
            let inner = self.inner.borrow();
            inner.health.retrying() && now >= inner.retry_at
        };
        if due {
            self.ensure_view();
        }
    }

    /// The view is lost: blank page, and either a new view later (webd went away: the page is
    /// loaded again) or at the next load (the page's process ended).
    pub(crate) fn lost(&self, lost: Lost) {
        let reload = {
            let mut inner = self.inner.borrow_mut();
            inner.view.destroy();
            inner.moves.clear();
            let page = inner.state.committed_url.clone();
            let page = if page.is_empty() {
                inner.state.url.clone()
            } else {
                page
            };
            inner.aliases.original(&page)
        };
        match lost {
            Lost::ServiceGone => {
                {
                    let mut inner = self.inner.borrow_mut();
                    if !reload.is_empty() && inner.pending.is_none() {
                        inner.pending = Some(reload);
                    }
                }
                self.unhealthy(Health::NotRunning, lost_text(lost));
            }
            Lost::Engine(_) => {
                self.inner.borrow_mut().health = Health::Ended;
                self.notice_channel();
                self.post_own(|state| {
                    state.loading = false;
                    state.crashed = true;
                    state.error = lost_text(lost).to_owned();
                });
            }
        }
    }

    /// Loads `url` now, or makes the view first.
    pub(crate) fn load_now(&self, url: &str) {
        if self.inner.borrow().view.is_live() {
            self.issue_load(url);
            return;
        }
        self.inner.borrow_mut().pending = Some(url.to_owned());
        self.ensure_view();
    }

    /// Asks the live view for `url` (as HTML for a long `data:` page); a refusal is a failed load.
    fn issue_load(&self, url: &str) {
        let result = {
            let mut inner = self.inner.borrow_mut();
            let inner = &mut *inner;
            inner.loaded = true;
            match inner.aliases.prepare(url) {
                Some((html, base)) => inner.view.load_html(&html, &base),
                None => inner.view.load(url),
            }
        };
        self.answer(result, Some(url));
    }

    /// What a call's error means: a lost view is lost; a refused load is a failed one.
    pub(crate) fn answer(&self, result: Result<(), Error>, load: Option<&str>) {
        match result {
            Ok(()) => {}
            Err(Error::Lost(lost)) => {
                if let Some(url) = load {
                    self.inner.borrow_mut().pending = Some(url.to_owned());
                }
                self.lost(lost);
            }
            Err(Error::Refused(_)) if load.is_some() => self.post_own(|state| {
                state.loading = false;
                state.error = String::from("This address cannot be opened");
            }),
            Err(_) => {}
        }
    }

    /// Posts the page's state as the view reported it, its count of loads carried across views and
    /// its addresses told back as the window knows them.
    pub(crate) fn post_view(&self, mut state: PageState) {
        let mut inner = self.inner.borrow_mut();
        state.finished = state.finished.saturating_add(inner.finished_base);
        state.url = inner.aliases.original(&state.url);
        state.committed_url = inner.aliases.original(&state.committed_url);
        inner.state = state.clone();
        drop(inner);
        self.mailbox.post_state(state);
    }

    /// Posts the page's state as last posted, changed by `change` (the engine's own news).
    pub(crate) fn post_own(&self, change: impl FnOnce(&mut PageState)) {
        let state = {
            let mut inner = self.inner.borrow_mut();
            change(&mut inner.state);
            inner.state.clone()
        };
        self.mailbox.post_state(state);
    }

    /// Runs `job` on the live view, or once one is made.
    pub(crate) fn on_view(&self, job: impl FnOnce(&mut FullWebView<T>) + 'static) {
        let mut inner = self.inner.borrow_mut();
        if inner.view.is_live() {
            job(&mut inner.view);
        } else {
            inner.deferred.push(Box::new(job));
        }
    }
}
