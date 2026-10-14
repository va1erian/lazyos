//! LazyWeb+'s ad blocker (issue #845): what the user chose, which pages it applies to, and the
//! state of the lists it blocks with.
//!
//! The engine blocks (WebKit content extensions, `webkit/port/app/blocking.cpp`); this is the
//! window's half:
//!
//! - [`Settings`]: the global switch, the sites the user excepted and the lists they turned
//!   off, kept by a [`Persist`] (`confd` keys under `user/<uid>/lazywebplus/blocker/` on LazyOS);
//! - [`Blocker::decide`]: the per-navigation decision the UI process asks for through
//!   `lzw_ui_blocking`, on the engine's thread, so it takes a lock and nothing else;
//! - [`lists`]: the filter lists, converted by `lazyadblock` and handed to the engine;
//! - [`update`] and [`worker`]: fetching the lists with `curl` under a label of their own, on
//!   a thread of their own, and keeping them current;
//! - [`page`]: `about:blocker`.
//!
//! **Why exceptions are a site and not a list rule.** The compiled lists cost seconds of CPU, so a
//! per-site switch never recompiles: the engine takes the lists off the page for a main-frame
//! navigation to an excepted site and puts them back for the next one.

pub mod lists;
pub mod page;
pub mod update;
pub mod worker;

use std::collections::BTreeSet;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use xui_webkit::abi::ListStatus;

pub use lists::{ListSpec, LISTS};
use worker::Cmd;

/// What the user chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The global switch.
    pub enabled: bool,
    /// Sites left alone, subdomains too (`example.com` covers `news.example.com`). Lower case,
    /// without `www.`.
    pub exceptions: BTreeSet<String>,
    /// Lists turned off, by id.
    pub lists_off: BTreeSet<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: true,
            exceptions: BTreeSet::new(),
            lists_off: BTreeSet::new(),
        }
    }
}

impl Settings {
    /// Settings from the stored texts (`enabled`: `true`/`false`; `exceptions` and `lists_off`:
    /// space separated). Anything that is not a valid site or list id is dropped, whoever
    /// wrote it.
    pub fn from_texts(enabled: Option<&str>, exceptions: &str, lists_off: &str) -> Settings {
        Settings {
            enabled: !matches!(enabled.map(str::trim), Some("false" | "0" | "off")),
            exceptions: exceptions
                .split_whitespace()
                .filter_map(normalize_site)
                .collect(),
            lists_off: lists_off
                .split_whitespace()
                .filter(|id| lists::spec(id).is_some())
                .map(str::to_owned)
                .collect(),
        }
    }

    /// The exceptions as the text stored.
    pub fn exceptions_text(&self) -> String {
        self.exceptions
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The disabled lists as the text stored.
    pub fn lists_off_text(&self) -> String {
        self.lists_off
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Where [`Settings`] are kept. Called from the window's thread and the engine's.
pub trait Persist: Send + Sync {
    fn load(&self) -> Settings;
    fn save(&self, settings: &Settings);
}

/// Keeps nothing: a session without `confd`, and the tests.
pub struct Volatile;

impl Persist for Volatile {
    fn load(&self) -> Settings {
        Settings::default()
    }
    fn save(&self, _settings: &Settings) {}
}

/// Where a list is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListState {
    /// Nothing fetched yet, and nothing to compile.
    Waiting,
    Fetching,
    /// Handed to the engine; its answer has not come.
    Compiling,
    Ready,
    /// The last step failed; the text says which.
    Failed(String),
    /// The engine has no store: no profile folder, or WebKit without content extensions.
    Unavailable,
}

/// One list as `about:blocker` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub licence: &'static str,
    pub enabled: bool,
    pub state: ListState,
    /// Rules in the lists the engine holds for it.
    pub rules: usize,
    /// Filters of the source that were not converted (element hiding included).
    pub dropped: usize,
    /// Seconds since the epoch of the last successful fetch.
    pub fetched: Option<u64>,
}

struct Inner {
    settings: Settings,
    /// A site the user asked to see unblocked once ("Reload without blocking"), until they leave it.
    once: Option<String>,
    blocked: u32,
    lists: Vec<ListInfo>,
    persist: Arc<dyn Persist>,
    /// The user changed a setting since this blocker was made: what `attach` finds stored no
    /// longer replaces them.
    touched: bool,
    commands: Option<Sender<Cmd>>,
    /// Told when a list's state changes on the worker's thread (the window redraws).
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The blocker's shared state: one clone for the window, one for the engine's thread (the policy
/// callback) and one for the worker.
#[derive(Clone)]
pub struct Blocker {
    inner: Arc<Mutex<Inner>>,
}

impl Blocker {
    /// A blocker whose settings `persist` keeps.
    pub fn new(persist: Arc<dyn Persist>) -> Blocker {
        let settings = persist.load();
        let lists = LISTS
            .iter()
            .map(|spec| ListInfo {
                id: spec.id,
                name: spec.name,
                url: spec.url,
                licence: spec.licence,
                enabled: !settings.lists_off.contains(spec.id),
                state: ListState::Waiting,
                rules: 0,
                dropped: 0,
                fetched: None,
            })
            .collect();
        Blocker {
            inner: Arc::new(Mutex::new(Inner {
                settings,
                once: None,
                blocked: 0,
                lists,
                persist,
                touched: false,
                commands: None,
                wake: None,
            })),
        }
    }

    /// A blocker that keeps nothing.
    pub fn volatile() -> Blocker {
        Blocker::new(Arc::new(Volatile))
    }

    /// Hands a blocker made without its store (settings at their defaults) the store that is ready
    /// now: from here on `persist` keeps the settings, and the ones it holds replace the defaults
    /// unless the user changed something meanwhile. Opening the store can wait for a service, so it
    /// is done on a thread of its own, never on WebKit's main thread. The window is woken.
    pub fn attach(&self, persist: Arc<dyn Persist>) {
        let stored = persist.load();
        let wake = {
            let mut guard = self.lock();
            guard.persist = persist;
            if !guard.touched {
                let Inner {
                    settings, lists, ..
                } = &mut *guard;
                *settings = stored;
                for list in lists.iter_mut() {
                    list.enabled = !settings.lists_off.contains(list.id);
                }
            }
            guard.wake.clone()
        };
        if let Some(wake) = wake {
            wake();
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the rule lists apply to a page whose main-frame address is `url`
    /// (`lzw_ui_blocking`). Only the web is blocked: the browser's own pages and `file:`
    /// are not. A web address whose host this does not name as a site (an IPv6 literal, a host
    /// the engine accepts and the site list cannot spell) is blocked like any other: no
    /// exception can match it, and a page must not be able to switch the blocker off by
    /// its address.
    pub fn decide(&self, url: &str) -> bool {
        if !is_web_address(url) {
            return false;
        }
        let Some(site) = site_of(url) else {
            return self.lock().settings.enabled;
        };
        let inner = self.lock();
        inner.settings.enabled
            && inner.once.as_deref() != Some(site.as_str())
            && !inner.settings.exceptions.iter().any(|e| covers(e, &site))
    }

    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    fn change(&self, change: impl FnOnce(&mut Settings)) {
        let (saved, persist) = {
            let mut guard = self.lock();
            let Inner {
                settings,
                lists,
                touched,
                ..
            } = &mut *guard;
            change(settings);
            for list in lists.iter_mut() {
                list.enabled = !settings.lists_off.contains(list.id);
            }
            *touched = true;
            (guard.settings.clone(), Arc::clone(&guard.persist))
        };
        // A save may be a call to a service: it runs with the lock released, which the engine's
        // policy callback (on its run loop) takes for every navigation.
        persist.save(&saved);
    }

    pub fn set_enabled(&self, on: bool) {
        self.change(|s| s.enabled = on);
    }

    /// Excepts `site` (or ends the exception). Returns the site as stored, or `None` for text
    /// that is not a site.
    pub fn set_site_excepted(&self, site: &str, excepted: bool) -> Option<String> {
        let site = normalize_site(site)?;
        let stored = site.clone();
        self.change(move |s| {
            if excepted {
                s.exceptions.insert(site);
            } else {
                // Ending the exception of a site ends the one that covers it, too: the user
                // asked for blocking there.
                s.exceptions.retain(|e| !covers(e, &site));
            }
        });
        Some(stored)
    }

    /// Whether `site` (a host, as [`site_of`] makes it) is under an exception.
    pub fn site_excepted(&self, site: &str) -> bool {
        self.lock()
            .settings
            .exceptions
            .iter()
            .any(|e| covers(e, site))
    }

    pub fn set_list_enabled(&self, id: &str, on: bool) {
        if lists::spec(id).is_none() {
            return;
        }
        self.change(|s| {
            if on {
                s.lists_off.remove(id);
            } else {
                s.lists_off.insert(id.to_owned());
            }
        });
        self.send(Cmd::ListsChanged);
    }

    /// The site shown unblocked once, if any.
    pub fn once(&self) -> Option<String> {
        self.lock().once.clone()
    }

    /// Shows `site` unblocked until the browser leaves it.
    pub fn skip_once(&self, site: &str) {
        self.lock().once = normalize_site(site);
    }

    /// Ends the one-shot unless the page on show is on that site.
    pub fn leave_unless(&self, site: Option<&str>) {
        let mut inner = self.lock();
        if inner.once.as_deref() != site {
            inner.once = None;
        }
    }

    /// Loads blocked on the page on show.
    pub fn blocked(&self) -> u32 {
        self.lock().blocked
    }

    pub fn set_blocked(&self, count: u32) {
        self.lock().blocked = count;
    }

    /// The lists as they stand.
    pub fn lists(&self) -> Vec<ListInfo> {
        self.lock().lists.clone()
    }

    /// Changes the state of list `id` (the worker's and the window's way in).
    pub fn update_list(&self, id: &str, change: impl FnOnce(&mut ListInfo)) {
        let wake = {
            let mut inner = self.lock();
            let Some(list) = inner.lists.iter_mut().find(|l| l.id == id) else {
                return;
            };
            change(list);
            inner.wake.clone()
        };
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Calls `wake` (from any thread) whenever a list changes state.
    pub fn on_change(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.lock().wake = Some(Arc::new(wake));
    }

    /// Whether any list is ready to block with.
    pub fn any_ready(&self) -> bool {
        self.lock()
            .lists
            .iter()
            .any(|l| l.enabled && l.state == ListState::Ready)
    }

    /// Connects the worker thread's command channel.
    pub fn connect(&self, commands: Sender<Cmd>) {
        self.lock().commands = Some(commands);
    }

    /// Sends the worker a command; nothing happens when there is no worker (a test, or a
    /// session with no home folder).
    pub fn send(&self, command: Cmd) {
        let sender = self.lock().commands.clone();
        if let Some(sender) = sender {
            let _ = sender.send(command);
        }
    }

    /// The engine's answer for a list part: handed to the worker, which owns the lists' files.
    pub fn list_result(&self, id: &str, status: ListStatus) {
        self.send(Cmd::ListResult {
            id: id.to_owned(),
            status,
        });
    }
}

/// The site of a web address: its host, lower case, without a `www.` prefix, port or
/// credentials. `None` for anything that is not an `http:` or `https:` address with a host.
pub fn site_of(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = strip_scheme(url, "http://").or_else(|| strip_scheme(url, "https://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    if host.starts_with('[') {
        return None;
    }
    let host = host.split(':').next().unwrap_or("");
    normalize_site(host)
}

/// Whether `url` is an `http:` or `https:` address, whatever its host.
fn is_web_address(url: &str) -> bool {
    let url = url.trim();
    strip_scheme(url, "http://")
        .or_else(|| strip_scheme(url, "https://"))
        .is_some()
}

fn strip_scheme<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
    let head = url.get(..scheme.len())?;
    head.eq_ignore_ascii_case(scheme)
        .then(|| &url[scheme.len()..])
}

/// A host as the blocker keeps sites; `None` when it is not one.
pub fn normalize_site(text: &str) -> Option<String> {
    let host = text.trim().trim_end_matches('.').to_ascii_lowercase();
    // `www.` goes only when a name with a suffix is left: `www.com` is not "com".
    let host = match host.strip_prefix("www.") {
        Some(rest) if rest.contains('.') => rest,
        _ => &host,
    };
    let valid = !host.is_empty()
        && host.len() <= 253
        && !host.starts_with(['.', '-'])
        && !host.contains("..")
        && host.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        });
    valid.then(|| host.to_owned())
}

/// Whether the exception `site` covers `candidate`: the site itself and its subdomains.
fn covers(site: &str, candidate: &str) -> bool {
    candidate == site
        || candidate
            .strip_suffix(site)
            .is_some_and(|head| head.ends_with('.'))
}

#[cfg(test)]
mod tests;
