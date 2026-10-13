//! What the platform and the caller give the window: the probe, the launcher, the history and
//! the downloads.

use std::rc::Rc;

use xui_core::backend::WidgetId;
use xui_core::geometry::Rect;

use crate::handoff::Launcher;
use crate::internal::Internal;
use crate::notify::{Permissions, Toasts};
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
        }
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
    pub host: Host,
}
