//! The first steps: view A, the rule list, navigation and history, and the cookies of the two
//! profiles (views B and C).

use super::{
    browser_config, browser_plain_config, own_config, Browse, Marker, Stage, Which, LIST, RETRY_MS,
    RULES,
};
use crate::event::Error;
use crate::full::{blocking_status_name, Scope};
use crate::transport::ScopedTransport;

/// How long webd may take to be there and make view A (its first web process starts).
pub(super) const CREATE_MS: u64 = 120_000;
/// The first page that loads over the network (the network process starts).
pub(super) const FIRST_LOAD_MS: u64 = 120_000;
/// Any other step.
pub(super) const STEP_MS: u64 = 60_000;
/// After the index page loaded, how long its `Blocked` count may take.
const BLOCKED_MS: u64 = 15_000;

impl<T: ScopedTransport> Browse<T> {
    pub(super) fn stage_view(&mut self, now: u64, out: &mut Vec<Marker>) {
        if now < self.retry_at {
            return;
        }
        if self.a.is_none() {
            let view = self.make_view(Scope::Browser, browser_config());
            self.a = Some(view);
        }
        let (w, h) = self.size;
        let Some(a) = self.a.as_mut() else {
            return;
        };
        match a.create(w, h) {
            Ok(()) => {
                let id = a.view_id().unwrap_or(0);
                self.pass("VIEW", format!("id={id} scope=browser w={w} h={h}"), out);
                self.next(Stage::Blocklist, now);
            }
            Err(Error::NotRunning) if self.elapsed(now) < CREATE_MS => {
                self.retry_at = now + RETRY_MS;
            }
            Err(error) => {
                // Nothing else can run without view A.
                self.fail("VIEW", error.to_string(), out);
                self.next(Stage::Done, now);
            }
        }
    }

    pub(super) fn stage_blocklist(&mut self, now: u64, out: &mut Vec<Marker>) {
        if self.phase == 0 {
            self.seen.lists.clear();
            let compiled = match self.a.as_mut() {
                Some(a) => a.blocking_compile(LIST, RULES.as_bytes()),
                None => Err(Error::NoView),
            };
            if let Err(error) = compiled {
                self.fail("BLOCKLIST", format!("compile refused: {error}"), out);
                return self.next(Stage::Index, now);
            }
            self.phase = 1;
            return;
        }
        let status = self
            .seen
            .lists
            .iter()
            .find(|(list, _)| list == LIST)
            .map(|(_, status)| *status);
        match status {
            Some(status) if blocking_status_name(status) == "ready" => {
                self.pass("BLOCKLIST", format!("list={LIST} status=ready"), out);
            }
            Some(status) => {
                let name = blocking_status_name(status);
                self.fail("BLOCKLIST", format!("list={LIST} status={name}"), out);
            }
            None if self.elapsed(now) > STEP_MS => {
                self.fail("BLOCKLIST", "no BlockingList event".into(), out);
            }
            None => return,
        }
        self.next(Stage::Index, now);
    }

    pub(super) fn stage_index(&mut self, now: u64, out: &mut Vec<Marker>) {
        let url = self.url("/");
        if self.phase == 0 {
            // `Blocked` may come while the page loads: what the step looks for starts here.
            self.seen = Default::default();
            self.phase = 1;
            if let Err(error) = self.load(Which::A, &url) {
                self.nav_failed = true;
                self.fail("NAV", format!("load {url}: {error}"), out);
                self.next(Stage::Blocked, now);
            }
            return;
        }
        if self.loaded(Which::A, None) {
            let (title, committed) = (self.title(Which::A), self.committed(Which::A));
            if title == "Index" && committed == url {
                self.index_url = committed;
            } else {
                self.nav_failed = true;
                self.fail("NAV", format!("index title={title} url={committed}"), out);
            }
            self.next(Stage::Blocked, now);
        } else if self.elapsed(now) > FIRST_LOAD_MS {
            self.nav_failed = true;
            let title = self.title(Which::A);
            self.fail("NAV", format!("timeout loading {url} title={title}"), out);
            self.next(Stage::Blocked, now);
        }
    }

    pub(super) fn stage_blocked(&mut self, now: u64, out: &mut Vec<Marker>) {
        let count = self.seen.blocked;
        if count >= 1 {
            self.pass("BLOCKED", format!("count={count}"), out);
        } else if self.elapsed(now) > BLOCKED_MS {
            self.fail("BLOCKED", "count=0".into(), out);
        } else {
            return;
        }
        self.next(Stage::Unblocked, now);
    }

    /// Reload the index page without the blocker (loading it first if it is not on show).
    pub(super) fn stage_unblocked(&mut self, now: u64, out: &mut Vec<Marker>) {
        let index = self.url("/");
        let asked = match self.phase {
            0 if self.committed(Which::A) != index => {
                self.phase = 1;
                self.load(Which::A, &index)
            }
            0 => {
                self.phase = 2;
                self.reload_a()
            }
            1 if self.loaded(Which::A, None) => {
                self.phase = 2;
                self.reload_a()
            }
            2 if self.loaded(Which::A, None) => {
                let committed = self.committed(Which::A);
                self.pass("UNBLOCKED", format!("url={committed}"), out);
                return self.next(Stage::Page2, now);
            }
            _ => Ok(()),
        };
        if let Err(error) = asked {
            self.fail("UNBLOCKED", error.to_string(), out);
            self.next(Stage::Page2, now);
        } else if self.elapsed(now) > STEP_MS {
            self.fail("UNBLOCKED", "timeout".into(), out);
            self.next(Stage::Page2, now);
        }
    }

    fn reload_a(&mut self) -> Result<(), Error> {
        self.note_page(Which::A);
        self.a.as_mut().ok_or(Error::NoView)?.reload(true)
    }

    pub(super) fn stage_page2(&mut self, now: u64, out: &mut Vec<Marker>) {
        let url = self.url("/page2");
        if self.phase == 0 {
            self.phase = 1;
            if let Err(error) = self.load(Which::A, &url) {
                self.fail("NAV", format!("load {url}: {error}"), out);
                self.next(Stage::Back, now);
            }
            return;
        }
        if self.loaded(Which::A, None) {
            let (title, committed) = (self.title(Which::A), self.committed(Which::A));
            if title != "Page Two" || committed != url {
                self.fail("NAV", format!("page2 title={title} url={committed}"), out);
            } else if !self.nav_failed {
                let index = self.index_url.clone();
                self.pass("NAV", format!("index={index} page2={committed}"), out);
            }
            self.next(Stage::Back, now);
        } else if self.elapsed(now) > STEP_MS {
            self.fail("NAV", format!("timeout loading {url}"), out);
            self.next(Stage::Back, now);
        }
    }

    /// `BACK` to the index page (which can then go forward), or `FORWARD` to page two.
    pub(super) fn stage_history(&mut self, now: u64, out: &mut Vec<Marker>, back: bool) {
        let (step, title_wanted, url, next) = match back {
            true => ("BACK", "Index", self.url("/"), Stage::Forward),
            false => ("FORWARD", "Page Two", self.url("/page2"), Stage::CookieA),
        };
        if self.phase == 0 {
            self.phase = 1;
            self.note_page(Which::A);
            let asked = match self.a.as_mut() {
                Some(a) if back => a.back(),
                Some(a) => a.forward(),
                None => Err(Error::NoView),
            };
            if let Err(error) = asked {
                self.fail(step, error.to_string(), out);
                self.next(next, now);
            }
            return;
        }
        if self.loaded(Which::A, Some(&url)) {
            let (title, committed) = (self.title(Which::A), self.committed(Which::A));
            let can_forward = self.a.as_ref().is_some_and(|a| a.page().can_forward);
            let fine = title == title_wanted && committed == url && (!back || can_forward);
            match (fine, back) {
                (true, true) => {
                    self.pass(step, format!("url={committed} can_forward=true"), out);
                }
                (true, false) => self.pass(step, format!("url={committed}"), out),
                (false, _) => {
                    let why = format!("title={title} url={committed} can_forward={can_forward}");
                    self.fail(step, why, out);
                }
            }
            self.next(next, now);
        } else if self.elapsed(now) > STEP_MS {
            self.fail(step, "timeout".into(), out);
            self.next(next, now);
        }
    }

    pub(super) fn stage_cookie_a(&mut self, now: u64, out: &mut Vec<Marker>) {
        let url = self.url("/cookie");
        if self.phase == 0 {
            self.phase = 1;
            if let Err(error) = self.load(Which::A, &url) {
                self.fail("COOKIE_A", error.to_string(), out);
                self.next(Stage::CookieB, now);
            }
            return;
        }
        if self.loaded(Which::A, None) {
            let title = self.title(Which::A);
            match title.contains("webd=browser") {
                true => self.pass("COOKIE_A", format!("title={title}"), out),
                false => self.fail("COOKIE_A", format!("title={title}"), out),
            }
            self.next(Stage::CookieB, now);
        } else if self.elapsed(now) > STEP_MS {
            self.fail("COOKIE_A", "timeout".into(), out);
            self.next(Stage::CookieB, now);
        }
    }

    /// View B, the app's own profile: it must not see the browser's cookie, and keeps its own.
    pub(super) fn stage_cookie_b(&mut self, now: u64, out: &mut Vec<Marker>) {
        let (cookie, set) = (self.url("/cookie"), self.url("/setcookie-b"));
        let asked = match self.phase {
            0 => {
                self.phase = 1;
                self.open_other(Scope::Net, own_config(), &cookie)
            }
            1 if self.loaded(Which::Other, None) => {
                self.first_title = self.title(Which::Other);
                if self.first_title.contains("webd=browser") {
                    let why = format!(
                        "own profile sees the browser's cookie: {}",
                        self.first_title
                    );
                    self.fail("COOKIE_B", why, out);
                    return self.close_other(Stage::CookieA2, now);
                }
                self.phase = 2;
                self.load(Which::Other, &set)
            }
            2 if self.loaded(Which::Other, None) => {
                self.phase = 3;
                self.load(Which::Other, &cookie)
            }
            3 if self.loaded(Which::Other, None) => {
                let (first, second) = (self.first_title.clone(), self.title(Which::Other));
                let detail = format!("first={first} second={second}");
                match second.contains("webd=own") {
                    true => self.pass("COOKIE_B", detail, out),
                    false => self.fail("COOKIE_B", detail, out),
                }
                return self.close_other(Stage::CookieA2, now);
            }
            _ => Ok(()),
        };
        if let Err(error) = asked {
            self.fail("COOKIE_B", format!("view B: {error}"), out);
            self.close_other(Stage::CookieA2, now);
        } else if self.elapsed(now) > FIRST_LOAD_MS {
            self.fail("COOKIE_B", format!("timeout in phase {}", self.phase), out);
            self.close_other(Stage::CookieA2, now);
        }
    }

    /// View C, the browser profile again: it sees the browser's cookie and not B's.
    pub(super) fn stage_cookie_a2(&mut self, now: u64, out: &mut Vec<Marker>) {
        let cookie = self.url("/cookie");
        let asked = match self.phase {
            0 => {
                self.phase = 1;
                self.open_other(Scope::Browser, browser_plain_config(), &cookie)
            }
            1 if self.loaded(Which::Other, None) => {
                let title = self.title(Which::Other);
                let fine = title.contains("webd=browser") && !title.contains("webd=own");
                match fine {
                    true => self.pass("COOKIE_A2", format!("title={title}"), out),
                    false => self.fail("COOKIE_A2", format!("title={title}"), out),
                }
                return self.close_other(Stage::Download, now);
            }
            _ => Ok(()),
        };
        if let Err(error) = asked {
            self.fail("COOKIE_A2", format!("view C: {error}"), out);
            self.close_other(Stage::Download, now);
        } else if self.elapsed(now) > STEP_MS {
            self.fail("COOKIE_A2", "timeout".into(), out);
            self.close_other(Stage::Download, now);
        }
    }

    /// Make the other view through `scope` and load `url` in it.
    pub(super) fn open_other(
        &mut self,
        scope: Scope,
        config: crate::full::ViewConfig,
        url: &str,
    ) -> Result<(), Error> {
        let (w, h) = self.small_size();
        let mut view = self.make_view(scope, config);
        view.create(w, h)?;
        self.other = Some(view);
        self.load(Which::Other, url)
    }

    /// End the other view and go on.
    pub(super) fn close_other(&mut self, next: Stage, now: u64) {
        if let Some(mut view) = self.other.take() {
            view.destroy();
        }
        self.next(next, now);
    }
}
