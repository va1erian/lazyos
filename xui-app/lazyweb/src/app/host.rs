//! What the platform and the caller give the window: the probe, the launcher, the history and
//! the downloads.

use std::rc::Rc;
use std::sync::Arc;

use xui_core::backend::WidgetId;
use xui_core::geometry::Rect;
use xui_webkit::engine::{Engine, Recorder};
use xui_webkit::mailbox::Mailbox;

use crate::blocker::Blocker;
use crate::handoff::Launcher;
use crate::internal::Internal;
use crate::notify::{Permissions, Toasts};
use crate::prefs::Prefs;
use crate::tabs::TabId;
use crate::transfers::Transfers;
use crate::visits::Visits;

/// Tells a session script where a named control is: (window title, name, rectangle in the
/// window's content, physical pixels).
pub type Probe = Box<dyn Fn(&str, &str, Rect)>;

/// What the platform adds to the window.
pub struct Host {
    /// Print the markers that name pages (`LOAD`, `TITLE`, `NAV`, ...): the UI probe's.
    pub traces: bool,
    /// The widget holding the keyboard focus (Enter belongs to the address field only while it
    /// has it).
    pub focused: Rc<dyn Fn() -> Option<WidgetId>>,
    /// Starts another program for a URL.
    pub launcher: Rc<dyn Launcher>,
    /// The probe (a normal image has none: nothing is printed).
    pub probe: Probe,
    /// The profile folder (`$HOME/.apps/<SYSTEM_NAME>`), where the File menu records whether the
    /// page's data is kept in memory (`memory.rs`); `None` without a home.
    pub profile: Option<std::path::PathBuf>,
    /// Where the tabs' engines come from (webd's views), and how their news is read.
    pub engines: Rc<dyn Engines>,
    /// Runs what other threads queued for the window's thread (the blocker's engine calls, the
    /// shell's notification calls).
    pub jobs: Rc<dyn Fn()>,
    /// The person's choices from `confd` (search template, session restore).
    pub prefs: Prefs,
}

impl Host {
    /// A host with nothing to add: no traces, no probe, nothing launched.
    pub fn none() -> Host {
        struct NoLauncher;
        impl Launcher for NoLauncher {
            fn open_url(&self, _url: &str) -> std::result::Result<(), String> {
                Err(String::from("no launcher"))
            }
            fn open_path(&self, _path: &str) -> std::result::Result<(), String> {
                Err(String::from("no launcher"))
            }
        }
        Host {
            traces: false,
            focused: Rc::new(|| None),
            launcher: Rc::new(NoLauncher),
            probe: Box::new(|_, _, _| {}),
            profile: None,
            engines: Rc::new(Inert),
            jobs: Rc::new(|| {}),
            prefs: Prefs::default(),
        }
    }
}

/// A tab's engine and the mailbox it posts into.
pub struct Fresh {
    pub engine: Rc<dyn Engine>,
    pub mailbox: Arc<Mailbox>,
}

/// Where each tab's engine comes from: the platform makes one view of webd per tab and reads the
/// news of all of them.
pub trait Engines {
    /// The engine of the new tab `tab`. `process_key` is the host its first address is on (empty
    /// for a page of the browser's own): tabs with one key share a web process in webd.
    fn make(&self, tab: TabId, process_key: &str) -> Fresh;
    /// From now on `tab`'s news wakes the window's loop by itself; the others are read on the beat.
    fn front(&self, tab: TabId);
    /// The window let go of `tab`'s engine.
    fn gone(&self, tab: TabId);
    /// Reads what every engine sent into its mailbox; true when one wants to be called again soon
    /// (a pointer move waits its turn, webd is being asked for a view again).
    fn pump(&self) -> bool;
}

/// Engines that do nothing: a recording engine per tab and no news (`Host::none`).
struct Inert;

impl Engines for Inert {
    fn make(&self, _tab: TabId, _process_key: &str) -> Fresh {
        Fresh {
            engine: Rc::new(Recorder::default()),
            mailbox: Arc::new(Mailbox::default()),
        }
    }
    fn front(&self, _tab: TabId) {}
    fn gone(&self, _tab: TabId) {}
    fn pump(&self) -> bool {
        false
    }
}

/// What the browser keeps beside its widgets.
pub struct Setup {
    pub url: Option<String>,
    pub visits: Visits,
    pub transfers: Transfers,
    /// The browser's own pages, shared with the navigation policy.
    pub internal: Internal,
    /// The sites the person allowed or blocked to notify (shared with the UI process).
    pub permissions: Permissions,
    /// The shell's toasts; `None` shows no notification.
    pub toasts: Option<Rc<dyn Toasts>>,
    /// The ad blocker's state, shared with the engine's policy callback and the worker.
    pub blocker: Blocker,
    pub host: Host,
}
