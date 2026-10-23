//! LazyWeb+ on LazyOS: the program `xui-lazywebplus` (docs/lazywebplus.md). It runs the window as a
//! compositor client whose page is one full view of `webd` (docs/webd-plan.md P4), driven by
//! `xui-webkit-remote`'s `RemoteEngine` over `xui_app::platform::webd::WebdTransport`.
//!
//! Everything runs on the process's first thread, the window's: its Messenger handles (the view, the
//! shell's notifications) are that thread's own. Three helper threads hand work back to it
//! ([`main_thread`] and `xui_webkit::relay`): the toasts' worker, the downloads' notifier and the ad
//! blocker's worker, which compiles its lists into webd through the window's engine.
//!
//! Serial evidence (channel `WEBPLUS`): `UP:PASS` at the first frame, `WAIT:<errno>` while the
//! compositor is not there, `QUIT:PASS` when the window closed and nothing it started is left (the
//! list updater's `curl`) and the window's markers (`app.rs`).

use std::cell::RefCell;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use xui_app::backend::LazyOSBackend;
use xui_app::platform::confd_store::ConfdStore;
use xui_app::platform::messenger::Service;
use xui_app::platform::webd::WebdTransport;
use xui_app::platform::{launcher, notify};
use xui_app::trace::{self, Channel};
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_settings::store::{ConfigStore, Value as StoreValue};
use xui_webkit::abi::first_address;
use xui_webkit::engine::{Engine, NotifyAnswer};
use xui_webkit::events::Event;
use xui_webkit::gesture;
use xui_webkit::mailbox::Mailbox;
use xui_webkit::relay::{self, Relay};
use xui_webkit_remote::{browser_config, Navigation, Options, RemoteEngine, SlotPlan};

use crate::app::{Browser, Host, Msg, Setup, APP_NAME};
use crate::blocker::worker::{Cmd as BlockerCmd, Exit, Runner, Worker};
use crate::blocker::{Blocker, Persist, Settings, Volatile};
use crate::handoff::Launcher;
use crate::internal::Internal;
use crate::notify::shell::{Event as ShellEvent, Shell, ShellWorker};
use crate::notify::{Permissions, Reason, Toasts};
use crate::policy::{self, Decision as Verdict};
use crate::prefs::Prefs;
use crate::transfers::{Toast, ToastLevel, Toaster, Transfers};
use crate::visits::Visits;
use crate::{DOWNLOADS_DIR, HISTORY_FILE, NOTIFY_FILE, SYSTEM_NAME};

mod blocker;
mod hosts;
mod main_thread;
mod prefs;
mod shell;
use blocker::{settings_store, stop_children, CurlRunner};
use hosts::{LazyLauncher, LazyToaster};
use main_thread::{on_main, on_main_detached};
use shell::LazyShell;

/// How long after a real click or key a hand-off to another program is still the person's (a slow
/// guest takes a moment from the click to the navigation).
const HANDOFF_WINDOW: Duration = Duration::from_millis(3000);
/// The window size asked of the compositor: short enough that the status bar clears the taskbar of
/// a 720p desktop.
const WINDOW: (i32, i32) = (1000, 540);
/// How long a closing window waits for the ad blocker's settings to reach `confd`.
const SETTINGS_FLUSH: Duration = Duration::from_secs(3);
/// How long to wait for the compositor before giving up.
const CONNECT_WAIT: Duration = Duration::from_secs(900);

/// The user's folders: the app's own (history, notification choices, the blocker's lists, the
/// "page data in memory" switch) and the Downloads folder webd writes.
struct Folders {
    profile: Option<PathBuf>,
    downloads: PathBuf,
}

fn folders() -> Folders {
    let home = std::env::var("HOME").ok().filter(|h| h.starts_with('/'));
    let profile = home.as_ref().and_then(|home| {
        let dir = PathBuf::from(fhs::app_data_dir(home, SYSTEM_NAME));
        std::fs::create_dir_all(&dir).ok().map(|_| dir)
    });
    let downloads = match &home {
        Some(home) => PathBuf::from(home).join(DOWNLOADS_DIR),
        None => PathBuf::from(fhs::mount::TMP),
    };
    Folders { profile, downloads }
}

/// The compositor client, waiting for the compositor: an app that opens at login starts while the
/// core apps are still being installed (LazyShell retries the same way).
fn connect() -> Option<Rc<LazyOSBackend>> {
    let started = Instant::now();
    let mut said = false;
    loop {
        match LazyOSBackend::connect() {
            Ok(backend) => return Some(Rc::new(backend)),
            Err(code) if started.elapsed() < CONNECT_WAIT => {
                if !said {
                    trace::line!(Channel::Webplus, "WAIT:{code}");
                    said = true;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(code) => {
                trace::fail!(Channel::Webplus, "BIND", "{code}");
                return None;
            }
        }
    }
}

/// The window's policy for a navigation webd held (another scheme, one of the browser's commands):
/// what the in-process policy decided on WebKit's thread (`policy.rs`), with the window's own lock
/// on real input for a hand-off to another program.
fn navigator(internal: Internal) -> Box<dyn Fn(&Navigation) -> Option<Event>> {
    Box::new(move |held: &Navigation| {
        let own = internal.name_for(&held.committed_url).is_some();
        match policy::decide(&held.url, held.kind, held.gesture, own) {
            // webd loads the web itself; it never asks about it.
            Verdict::Allow => None,
            Verdict::Handoff(url) => Some(Event::Launch {
                url,
                by_user: gesture::take_recent(HANDOFF_WINDOW),
            }),
            Verdict::Command(url) => Some(Event::Command { url }),
            Verdict::Block(url) => Some(Event::Launch {
                url,
                by_user: false,
            }),
        }
    })
}

/// The person's kept notification choices, for webd (which keeps them only while it runs).
fn decisions(permissions: &Permissions) -> Vec<(String, NotifyAnswer)> {
    (0..)
        .map_while(|at| permissions.at(at))
        .map(|(site, allowed)| {
            let answer = if allowed {
                NotifyAnswer::Allow
            } else {
                NotifyAnswer::Block
            };
            (site, answer)
        })
        .collect()
}

/// The history file, in the app's folder; in memory without one.
fn visits(profile: Option<&PathBuf>) -> Visits {
    match profile {
        // Written on the disk thread: the window's thread does no file I/O after a load.
        Some(dir) => Visits::open(dir.join(HISTORY_FILE)).in_background(),
        None => Visits::in_memory(),
    }
}

mod run;

/// Runs the browser until its window is closed; the process exit status.
pub fn main() -> i32 {
    run::run()
}
