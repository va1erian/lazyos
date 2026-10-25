//! Web notifications through the shell's toasts (docs/lazyweb.md, "Notifications").
//!
//! A page asks the person once (`Notification.requestPermission()`: a bar above the page, not a
//! toast) and the answer is kept per site in the profile ([`store`]). A page the person allowed
//! posts with `new Notification(...)`; WebKit's UI process hands the request to the window
//! ([`Event::NotifyShow`](xui_webkit::events::Event::NotifyShow)), which shows it as a toast of the
//! shell (`os.lazy.notify`) under this browser's own name: the toast's title is the site
//! ([`origin`]), never the page's words, and the level, the icon and the timeout are the
//! browser's, so nothing a page sends can look like a download or a system warning. A site is held
//! to a few live toasts and a slow pace ([`live`]), so it cannot use up the browser's allowance.
//!
//! This module is the model and its link to the shell; no window and no engine are needed to test
//! it. The window's part (the bar, the events) is `app/notifications.rs`, the LazyOS glue (the
//! shell's `Listen` channel) is `ffi.rs`.
//!
//! Markers (UI-probe images only: they name sites), after `LAZYWEB:NOTIFY:`: `ASK:<origin>` a page
//! asked, `ALLOW|BLOCK:<origin>` the answer, `DISMISS:<origin>` the bar was closed without one,
//! `SHOW:<origin>:<id>` a toast is on screen, `CLICK:<id>`, `CLOSE:<id>`, `DROP:<origin>:<why>` a
//! post over the limits, `DENY:<origin>:<why>` something that was not allowed, `FORGET:<origin>`.

/// The origin, the toast's words and the limits are their own crate (`lazywebnotify`), shared with webd.
pub use lazywebnotify::{live, origin};
pub mod shell;
pub mod store;

use std::rc::Rc;

use xui_webkit::engine::{Engine, NotifyAnswer};

pub use live::{Admit, Dropped, Live, Registry};
pub use origin::Origin;
pub use store::{Decision, Permissions, Site};

use crate::marker_text;
use crate::transfers::{Toast, ToastLevel};

/// Sites whose permission request waits for the person at once; more are dismissed.
pub const MAX_ASKS: usize = 4;

/// Why a toast went away, as the shell says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The person dismissed it.
    User,
    /// Its time ran out.
    Timeout,
    /// Another toast took its place.
    Replaced,
    /// This browser closed it.
    Sender,
    /// The shell restarted.
    Restart,
    /// Do-not-disturb or the app's mute hid it.
    Muted,
    /// A reason a newer shell sent.
    Other,
}

/// What the shell reported about the toasts this browser asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellNews {
    /// Notification `key` is toast `toast`.
    Posted { key: u64, toast: u64 },
    /// The shell would not show notification `key` (no shell, rate limited).
    Failed { key: u64 },
    /// The person clicked toast `toast`.
    Activated { toast: u64 },
    /// Toast `toast` is gone.
    Closed { toast: u64, reason: Reason },
}

/// The window's line to the shell's toasts: requests go out without waiting, news comes back to be
/// drained from the window's thread.
pub trait Toasts {
    /// Shows `toast` for notification `key`; the answer is a [`ShellNews::Posted`] or
    /// [`ShellNews::Failed`].
    fn post(&self, key: u64, toast: Toast);
    /// Takes toast `toast` down.
    fn close(&self, toast: u64);
    /// What the shell reported since the last call, oldest first.
    fn drain(&self) -> Vec<ShellNews>;
}

/// What the person chose on the permission bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarAnswer {
    Allow,
    Block,
    /// Closed without an answer: the page may ask again.
    Later,
}

/// A page's request for a notification, as the engine reported it.
pub struct Incoming<'a> {
    pub id: u64,
    pub origin: &'a str,
    pub page_url: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub tag: &'a str,
}

/// What a click on a toast asks of the window.
#[derive(Debug, PartialEq, Eq)]
pub enum Click {
    /// The page's `click` handler ran (the page is still there).
    Delivered,
    /// The page is gone: the window opens this address, the site's page that asked.
    Open(String),
}

/// A question waiting for the person: the site, and the origin string WebKit gave (the engine is
/// answered with exactly that).
struct Ask {
    site: Origin,
    raw: String,
}

/// Permissions, the toasts that are up, and the questions waiting for the person.
pub struct Notifications {
    permissions: Permissions,
    registry: Registry,
    toasts: Option<Rc<dyn Toasts>>,
    asks: Vec<Ask>,
}

impl Notifications {
    /// `toasts` is `None` in a window with no shell to show them: nothing is then allowed to
    /// show (a permission may still be given and kept).
    pub fn new(permissions: Permissions, toasts: Option<Rc<dyn Toasts>>) -> Notifications {
        Notifications {
            permissions,
            registry: Registry::default(),
            toasts,
            asks: Vec::new(),
        }
    }

    /// The decisions (shared with the UI process, which tells WebKit at start).
    pub fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    /// The site the bar is asking about, if any.
    pub fn asking(&self) -> Option<&Origin> {
        self.asks.first().map(|ask| &ask.site)
    }

    /// A page called `Notification.requestPermission()` (`origin` is as WebKit writes it, the
    /// UI process taking it from the page's process). `page_url` is the page on screen and
    /// `gesture` whether a person's input started the request.
    ///
    /// Only the page on screen may ask, and only for its own origin: the engine already
    /// requires a top-level, same-origin request, but the page's process is not trusted to say
    /// whose it is, so a bar never names a site that is not the one the person is looking at.
    pub fn request(&mut self, engine: &dyn Engine, origin: &str, gesture: bool, page_url: &str) {
        let Some(site) = Origin::from_url(origin) else {
            // `data:`, `about:`, a file: nothing to allow, and nothing to remember.
            deny(origin, "origin");
            engine.notify_permission(origin, NotifyAnswer::Dismiss);
            return;
        };
        let key = site.key();
        if !site.owns(page_url) {
            deny(&key, "page");
            engine.notify_permission(origin, NotifyAnswer::Dismiss);
            return;
        }
        if !gesture {
            deny(&key, "gesture");
            engine.notify_permission(origin, NotifyAnswer::Dismiss);
            return;
        }
        match self.permissions.decision(&key) {
            Some(Decision::Allow) => engine.notify_permission(origin, NotifyAnswer::Allow),
            Some(Decision::Block) => engine.notify_permission(origin, NotifyAnswer::Block),
            None if self.asks.iter().any(|ask| ask.site == site) => {}
            None if self.asks.len() >= MAX_ASKS => {
                deny(&key, "busy");
                engine.notify_permission(origin, NotifyAnswer::Dismiss);
            }
            None => {
                named(&format!("ASK:{}", marker_text(&key)));
                self.asks.push(Ask {
                    site,
                    raw: origin.to_owned(),
                });
            }
        }
    }

    /// The engine refused the waiting requests of `origin` itself (their page went away): the
    /// question is moot and needs no answer.
    pub fn request_gone(&mut self, origin: &str) {
        self.asks.retain(|ask| ask.raw != origin);
    }

    /// The person answered the bar for [`Notifications::asking`].
    pub fn answer(&mut self, engine: &dyn Engine, answer: BarAnswer, now_secs: u64) {
        if self.asks.is_empty() {
            return;
        }
        let Ask { site, raw } = self.asks.remove(0);
        let key = site.key();
        let (decision, engine_answer, word) = match answer {
            BarAnswer::Allow => (Some(Decision::Allow), NotifyAnswer::Allow, "ALLOW"),
            BarAnswer::Block => (Some(Decision::Block), NotifyAnswer::Block, "BLOCK"),
            BarAnswer::Later => (None, NotifyAnswer::Dismiss, "DISMISS"),
        };
        if let Some(decision) = decision {
            if let Err(e) = self.permissions.set(&site, decision, now_secs) {
                eprintln!("lazyweb: notification choice not saved: {e}");
            }
        }
        named(&format!("{word}:{}", marker_text(&key)));
        engine.notify_permission(&raw, engine_answer);
    }

    /// The page on screen is `committed_url`: a question about another site is the page that
    /// is gone's, and is dismissed (the site may ask again).
    pub fn page_moved(&mut self, engine: &dyn Engine, committed_url: &str) {
        let here = Origin::from_url(committed_url);
        let (keep, drop): (Vec<Ask>, Vec<Ask>) = std::mem::take(&mut self.asks)
            .into_iter()
            .partition(|ask| Some(&ask.site) == here.as_ref());
        self.asks = keep;
        for ask in drop {
            named(&format!("DISMISS:{}", marker_text(&ask.site.key())));
            engine.notify_permission(&ask.raw, NotifyAnswer::Dismiss);
        }
    }

    /// A page asked to show a notification. It is shown when its site is allowed and it is the
    /// site's page that is on screen (the page's process says whose it is; the UI process says
    /// which page it is, and a notification of another site, or of a frame of one, is refused),
    /// within the limits. Every refusal is answered as closed, so the page's object does not stay
    /// "showing" for as long as its document lives, and webd, which keeps the notifications it handed
    /// over until they end, is not left holding it.
    pub fn show(&mut self, engine: &dyn Engine, now_ms: u64, incoming: &Incoming<'_>) {
        let Some(site) = Origin::from_url(incoming.origin) else {
            deny(incoming.origin, "origin");
            engine.notify_closed(incoming.id);
            return;
        };
        let key = site.key();
        if self.permissions.decision(&key) != Some(Decision::Allow) {
            deny(&key, "not-allowed");
            engine.notify_closed(incoming.id);
            return;
        }
        let Some(toasts) = self.toasts.clone() else {
            deny(&key, "no-shell");
            engine.notify_closed(incoming.id);
            return;
        };
        for old in self.registry.expire(now_ms) {
            if let Some(toast) = old.toast {
                toasts.close(toast);
            }
            engine.notify_closed(old.id);
        }
        // The page that asked is the site's, or this is not the site's notification: a click on a
        // toast of a page that is gone opens that page and no other.
        if !site.owns(incoming.page_url) {
            deny(&key, "page");
            engine.notify_closed(incoming.id);
            return;
        }
        match self
            .registry
            .admit(now_ms, incoming.id, &key, incoming.page_url, incoming.tag)
        {
            Admit::Drop(why) => {
                named(&format!("DROP:{}:{}", marker_text(&key), why.word()));
                engine.notify_closed(incoming.id);
            }
            Admit::Post {
                replaces,
                displaced,
            } => {
                if let Some(old) = displaced {
                    // The page's earlier object with this tag is over.
                    engine.notify_closed(old);
                }
                toasts.post(
                    incoming.id,
                    Toast {
                        level: ToastLevel::Info,
                        title: origin::toast_title(&site),
                        body: origin::toast_body(incoming.title, incoming.body),
                        sticky: false,
                        replaces,
                    },
                );
            }
        }
    }

    /// The page closed notification `id` itself.
    pub fn cancel(&mut self, id: u64) {
        if let Some(gone) = self.registry.cancel(id) {
            if let (Some(toast), Some(toasts)) = (gone.toast, &self.toasts) {
                toasts.close(toast);
            }
        }
    }

    /// The page's object for `id` is gone; its toast stays.
    pub fn gone(&mut self, id: u64) {
        self.registry.gone(id);
    }

    /// A tab closed: the notifications whose ids `of_tab` accepts lose their page's object (their
    /// toasts stay, and a click on one opens the site, as for a page that is gone).
    pub fn tab_closed(&mut self, of_tab: impl Fn(u64) -> bool) {
        let ids: Vec<u64> = self
            .registry
            .all()
            .iter()
            .map(|n| n.id)
            .filter(|id| of_tab(*id))
            .collect();
        for id in ids {
            self.registry.gone(id);
        }
    }

    /// The page's process went away: every notification's object with it.
    pub fn page_crashed(&mut self) {
        self.registry.orphan_all();
    }

    /// Forgets what the person decided about `origin` (an `Origin::key`) and takes its live
    /// toasts down; whether there was a decision.
    pub fn forget(&mut self, engine: &dyn Engine, origin: &str) -> bool {
        let had = self.permissions.forget(origin).unwrap_or_else(|e| {
            eprintln!("lazyweb: notification choice not saved: {e}");
            true
        });
        let ids: Vec<u64> = self
            .registry
            .all()
            .iter()
            .filter(|n| n.origin == origin)
            .map(|n| n.id)
            .collect();
        for id in ids {
            self.cancel(id);
            engine.notify_closed(id);
        }
        // Asking about it again is the page's business.
        self.asks.retain(|ask| ask.site.key() != origin);
        engine.notify_forget(origin);
        named(&format!("FORGET:{}", marker_text(origin)));
        had
    }

    /// Applies what the shell reported; the clicks that need the window's help.
    pub fn news(&mut self, engine: &dyn Engine) -> Vec<Click> {
        let Some(toasts) = self.toasts.clone() else {
            return Vec::new();
        };
        let mut clicks = Vec::new();
        for news in toasts.drain() {
            match news {
                ShellNews::Posted { key, toast } => match self.registry.posted(key, toast) {
                    Some(stale) => toasts.close(stale),
                    None => {
                        if let Some(n) = self.registry.by_toast(toast) {
                            named(&format!("SHOW:{}:{}", marker_text(&n.origin), n.id));
                        }
                        engine.notify_shown(key);
                    }
                },
                ShellNews::Failed { key } => {
                    if let Some(n) = self.registry.failed(key) {
                        named(&format!("DENY:{}:shell", marker_text(&n.origin)));
                        engine.notify_closed(key);
                    }
                }
                ShellNews::Activated { toast } => {
                    let Some(n) = self.registry.by_toast(toast).cloned() else {
                        continue;
                    };
                    named(&format!("CLICK:{}", n.id));
                    if n.orphaned {
                        clicks.push(Click::Open(n.page_url));
                    } else {
                        engine.notify_clicked(n.id);
                        clicks.push(Click::Delivered);
                    }
                }
                ShellNews::Closed { toast, reason } => {
                    if let Some(n) = self.registry.toast_closed(toast) {
                        named(&format!("CLOSE:{}", n.id));
                        // What this browser did itself the page already knows.
                        if !n.orphaned && !matches!(reason, Reason::Sender | Reason::Replaced) {
                            engine.notify_closed(n.id);
                        }
                    }
                }
            }
        }
        clicks
    }

    /// How many notifications of `origin` are live.
    pub fn live_of(&self, origin: &str) -> usize {
        self.registry.live_of(origin)
    }
}

fn named(line: &str) {
    crate::named_marker(&format!("NOTIFY:{line}"));
}

fn deny(origin: &str, why: &str) {
    named(&format!("DENY:{}:{why}", marker_text(origin)));
}
