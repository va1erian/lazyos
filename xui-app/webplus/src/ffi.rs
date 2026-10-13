//! LazyWebPlus's half of the C ABI (`webkit/port/app/lazyos_ui.h`) and its LazyOS platform glue.
//!
//! `xui-webkit`'s `ffi` has the calls every window shares (the frame, the page state, the link
//! under the pointer, the engine's functions). The ones that decide something belong to the
//! window and are here: which features the UI process installs, the profile directory,
//! the navigation policy, where a download is written, and `lzw_ui_main`, which runs the window.
//! They run on WebKit's run loop, so none may block: the policy is a pure function and a
//! download's destination is a `create_dir_all` and a few `exists` calls.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use xui_app::platform::{launcher, notify};
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_webkit::abi::{byte_count, copy_c_string, features, first_address, NavKind, Verdict};
use xui_webkit::events::Event;
use xui_webkit::ffi::{arguments, connect, text, FfiEngine};
use xui_webkit::gesture;
use xui_webkit::mailbox;

use crate::app::{Browser, Host, Msg, Setup, APP_NAME};
use crate::downloads::Saver;
use crate::handoff::Launcher;
use crate::internal::Internal;
use crate::notify::shell::{Event as ShellEvent, Shell, ShellWorker};
use crate::notify::{Permissions, Reason, Toasts};
use crate::policy::{self, Decision};
use crate::transfers::{Toast, ToastLevel, Toaster, Transfers};
use crate::visits::Visits;
use crate::{DOWNLOADS_DIR, HISTORY_FILE, NOTIFY_FILE, SYSTEM_NAME};

/// How long after a real click or key a hand-off to another program is still the person's (a slow guest
/// takes a moment from the click to the navigation).
const HANDOFF_WINDOW: Duration = Duration::from_millis(3000);

/// The window size asked of the compositor: short enough that the status bar clears the taskbar of
/// a 720p desktop.
const WINDOW: (i32, i32) = (1000, 540);

extern "C" {
    /// `lazyos_ui.h`: runs `task(context)` once on WebKit's main thread and returns.
    fn lzw_engine_run_on_main(task: extern "C" fn(*mut c_void), context: *mut c_void);
}

/// How long a call into the main thread is waited for. The main thread is WebKit's run loop, which
/// only ever waits for the shell's answer (bounded) inside what is sent to it.
const MAIN_WAIT: Duration = Duration::from_secs(3);

/// Runs `job` on WebKit's main thread and returns what it returned; `None` when the thread did not
/// get to it in [`MAIN_WAIT`] (the job then still runs, its answer unread).
///
/// The shell's notification service names an app by the task id of the sender, and a message from
/// a thread other than the process's first carries the thread's id: the window's thread, and a
/// worker's, are refused as "not an app". The first thread is WebKit's run loop, so the calls the
/// window makes of that service are made there.
fn on_main<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    extern "C" fn run(context: *mut c_void) {
        // SAFETY: `context` is the pointer `Box::into_raw` made in `on_main`, handed over once.
        let job = unsafe { Box::from_raw(context.cast::<Box<dyn FnOnce() + Send>>()) };
        job();
    }
    let (answer, reply) = mpsc::channel();
    let job: Box<dyn FnOnce() + Send> = Box::new(move || {
        let _ = answer.send(job());
    });
    let context = Box::into_raw(Box::new(job)).cast::<c_void>();
    // SAFETY: `run` takes the pointer back exactly once; the engine calls it once, on its thread.
    unsafe { lzw_engine_run_on_main(run, context) };
    reply.recv_timeout(MAIN_WAIT).ok()
}

/// What the three threads of the process share: made once, by whichever call comes first (the UI
/// process asks for the profile directory before it starts the window).
struct Shared {
    home: Option<String>,
    saver: Arc<Saver>,
    internal: Internal,
    profile: Option<CString>,
    /// The sites allowed to notify: the UI process reads them when WebKit asks, before the window
    /// exists, and the window changes them.
    permissions: Permissions,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| {
        let home = std::env::var("HOME").ok().filter(|h| h.starts_with('/'));
        let folder = match &home {
            Some(home) => PathBuf::from(home).join(DOWNLOADS_DIR),
            None => PathBuf::from(fhs::mount::TMP),
        };
        // The folder exists before the NetworkProcess, which may only write inside it, saves a file there.
        let _ = std::fs::create_dir_all(&folder);
        let profile = home.as_ref().and_then(|home| {
            let dir = fhs::app_data_dir(home, SYSTEM_NAME);
            std::fs::create_dir_all(&dir).ok()?;
            CString::new(dir).ok()
        });
        let permissions = match &home {
            Some(home) => Permissions::open(
                PathBuf::from(fhs::app_data_dir(home, SYSTEM_NAME)).join(NOTIFY_FILE),
            ),
            None => Permissions::in_memory(),
        };
        Shared {
            home,
            saver: Arc::new(Saver::new(folder)),
            internal: Internal::default(),
            profile,
            permissions,
        }
    })
}

/// Everything LazyWebPlus asks the UI process for.
#[no_mangle]
pub extern "C" fn lzw_ui_features() -> c_int {
    features::ALL
}

/// The prefix of the UI process's markers.
#[no_mangle]
pub extern "C" fn lzw_ui_marker_prefix() -> *const c_char {
    c"WEBPLUS".as_ptr()
}

/// Where the engine keeps cookies and site data (`$HOME/.apps/os.lazy.lazywebplus`), or null
/// when there is no home: then nothing is persisted.
#[no_mangle]
pub extern "C" fn lzw_ui_profile_dir() -> *const c_char {
    shared()
        .profile
        .as_ref()
        .map_or(std::ptr::null(), |dir| dir.as_ptr())
}

/// The navigation policy (`policy.rs`): `url` is about to load; `kind` and `user_gesture` say how
/// it began and `committed_url` what is on screen.
///
/// # Safety
/// `url` and `committed_url` must be null or NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_navigate(
    url: *const c_char,
    kind: c_int,
    user_gesture: c_int,
    committed_url: *const c_char,
) -> c_int {
    // SAFETY: by this function's contract.
    let (url, committed) = unsafe { (text(url), text(committed_url)) };
    let from_own_page = shared().internal.name_for(&committed).is_some();
    let decision = policy::decide(
        &url,
        NavKind::from_code(kind),
        user_gesture != 0,
        from_own_page,
    );
    let queue = |event| mailbox::global().post_event(event);
    let verdict = match decision {
        Decision::Allow => Verdict::Allow,
        // Starting another program needs the engine's gesture flag *and* real input the window
        // forwarded a moment ago, used once (`xui_webkit::gesture`).
        Decision::Handoff(url) if !gesture::take_recent(HANDOFF_WINDOW) => {
            queue(Event::Launch {
                url,
                by_user: false,
            });
            Verdict::Deny
        }
        Decision::Handoff(url) => {
            queue(Event::Launch { url, by_user: true });
            Verdict::Handoff
        }
        Decision::Command(url) => {
            queue(Event::Command { url });
            Verdict::Handoff
        }
        Decision::Block(url) => {
            queue(Event::Launch {
                url,
                by_user: false,
            });
            Verdict::Deny
        }
    };
    verdict.code()
}

/// A download began: chooses its file in the Downloads folder, writes the path the engine saves
/// to (`<name>.part`) into `path` (`path_size` bytes), and tells the window. Returns 0, or
/// non-zero to refuse the download (no room for the path, or the folder cannot be made).
///
/// # Safety
/// `url` and `suggested_name` must be null or NUL-terminated strings; `path` must be valid for
/// `path_size` bytes.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_download_start(
    id: u64,
    url: *const c_char,
    suggested_name: *const c_char,
    total: i64,
    path: *mut c_char,
    path_size: usize,
) -> c_int {
    if path.is_null() || path_size == 0 {
        return 1;
    }
    // SAFETY: by this function's contract.
    let (url, suggested) = unsafe { (text(url), text(suggested_name)) };
    // The manifest lets the network process write the Downloads folder of a home only.
    if shared().home.is_none() {
        return 1;
    }
    let saver = &shared().saver;
    let Ok(destination) = saver.reserve(id, &suggested) else {
        return 1;
    };
    let part = destination.part.to_string_lossy().into_owned();
    // SAFETY: `path` is valid for `path_size` bytes, by this function's contract.
    let out = unsafe { std::slice::from_raw_parts_mut(path.cast::<u8>(), path_size) };
    if !copy_c_string(&part, out) {
        let _ = saver.finish(id, false);
        return 1;
    }
    let name = destination
        .path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    mailbox::global().post_event(Event::DownloadStarted {
        id,
        url,
        name,
        total: byte_count(total),
        path: part,
    });
    0
}

/// The decision at `index` about notifications, which WebKit asks for when it starts a page's
/// process (and the window changes later through `lzw_engine_notify_permission`): writes the
/// origin as WebKit spells it into `origin` (`origin_size` bytes) and returns 1 for allowed and 0
/// for blocked, -1 past the end, -2 for a row that does not fit (the caller skips it).
///
/// # Safety
/// `origin` must be valid for `origin_size` bytes.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_notify_permission_at(
    index: usize,
    origin: *mut c_char,
    origin_size: usize,
) -> c_int {
    let Some((site, allowed)) = shared().permissions.at(index) else {
        return -1;
    };
    if origin.is_null() || origin_size == 0 {
        return -2;
    }
    // SAFETY: `origin` is valid for `origin_size` bytes, by this function's contract.
    let out = unsafe { std::slice::from_raw_parts_mut(origin.cast::<u8>(), origin_size) };
    if !copy_c_string(&site, out) {
        return -2;
    }
    c_int::from(allowed)
}

/// `mimed`, for the links and files the browser hands on.
struct LazyLauncher;

impl Launcher for LazyLauncher {
    fn open_url(&self, url: &str) -> Result<(), String> {
        launcher::open_url(url).map_err(|e| e.to_string())
    }
    fn open_path(&self, path: &str) -> Result<(), String> {
        launcher::open_path(path).map_err(|e| e.to_string())
    }
}

/// `os.lazy.notify`, for the download toasts.
struct LazyToaster;

impl Toaster for LazyToaster {
    fn post(&mut self, toast: &Toast) -> Result<u64, String> {
        let level = match toast.level {
            ToastLevel::Info => notify::Level::Info,
            ToastLevel::Success => notify::Level::Success,
            ToastLevel::Error => notify::Level::Error,
        };
        let notice = notify::Notice {
            icon: toast.sticky.then(|| String::from("download")),
            timeout_ms: toast.sticky.then_some(0),
            replaces: toast.replaces,
            ..notify::Notice::new(level, &toast.title, &toast.body)
        };
        // Made from the main thread: the shell refuses any other as "not an app" (`on_main`).
        on_main(move || notify::post(&notice))
            .ok_or_else(|| String::from("the browser's main thread did not answer"))?
            .map_err(|code| format!("errno {code}"))
    }

    fn close(&mut self, id: u64) {
        let _ = on_main(move || notify::close(id));
    }
}

/// The shell's event channel for web notifications. Messenger handles belong to the thread that
/// made them: this lives on WebKit's main thread (`on_main`), which is also the thread the shell
/// accepts the app's calls from.
struct Listening {
    listener: Option<notify::Listener>,
    retry_at: Option<Instant>,
}

thread_local! {
    static LISTENING: RefCell<Listening> = const { RefCell::new(Listening { listener: None, retry_at: None }) };
}

impl Listening {
    /// Asks for the event channel when there is none (before the first toast, so no event is
    /// missed; again, a little later, when the shell restarted).
    fn listen(&mut self) {
        if self.listener.is_some() || self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        match notify::listen() {
            Ok(listener) => {
                self.listener = Some(listener);
                self.retry_at = None;
            }
            Err(_) => self.retry_at = Some(Instant::now() + Duration::from_secs(2)),
        }
    }

    fn poll(&mut self) -> Vec<ShellEvent> {
        self.listen();
        let Some(listener) = self.listener.as_mut() else {
            return Vec::new();
        };
        match listener.poll() {
            Ok(events) => events
                .into_iter()
                .filter_map(|event| match event {
                    notify::Event::Activated { id } => Some(ShellEvent::Activated { toast: id }),
                    notify::Event::Closed { id, reason } => Some(ShellEvent::Closed {
                        toast: id,
                        reason: match reason {
                            notify::CloseReason::User => Reason::User,
                            notify::CloseReason::Timeout => Reason::Timeout,
                            notify::CloseReason::Replaced => Reason::Replaced,
                            notify::CloseReason::Sender => Reason::Sender,
                            notify::CloseReason::Restart => Reason::Restart,
                            notify::CloseReason::Muted => Reason::Muted,
                            notify::CloseReason::Other => Reason::Other,
                        },
                    }),
                    // A web notification offers no buttons of its own.
                    notify::Event::Action { .. } => None,
                })
                .collect(),
            Err(_) => {
                // The shell's end is gone (it restarted): ask again.
                self.listener = None;
                Vec::new()
            }
        }
    }
}

/// `os.lazy.notify`, for web notifications: posts them, and listens to what becomes of them. Every
/// call is made on WebKit's main thread (`on_main`); this value is only the worker's handle.
struct LazyShell;

impl Shell for LazyShell {
    fn post(&mut self, toast: &Toast) -> Result<u64, String> {
        let notice = notify::Notice {
            replaces: toast.replaces,
            ..notify::Notice::new(notify::Level::Info, &toast.title, &toast.body)
        };
        on_main(move || {
            LISTENING.with(|state| state.borrow_mut().listen());
            notify::post(&notice)
        })
        .ok_or_else(|| String::from("the browser's main thread did not answer"))?
        .map_err(|code| format!("errno {code}"))
    }

    fn close(&mut self, toast: u64) {
        let _ = on_main(move || notify::close(toast));
    }

    fn poll(&mut self) -> Vec<ShellEvent> {
        on_main(|| LISTENING.with(|state| state.borrow_mut().poll())).unwrap_or_default()
    }
}

/// The history file, in the app's folder in the home; in memory without one.
fn visits(shared: &Shared) -> Visits {
    match &shared.home {
        Some(home) => {
            Visits::open(PathBuf::from(fhs::app_data_dir(home, SYSTEM_NAME)).join(HISTORY_FILE))
        }
        None => Visits::in_memory(),
    }
}

/// Runs the window until it is closed. Returns the process exit status.
///
/// # Safety
/// `argv` must point at `argc` valid NUL-terminated strings (or be null).
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_main(argc: c_int, argv: *const *const c_char) -> c_int {
    // SAFETY: forwarded from this function's contract.
    let args = unsafe { arguments(argc, argv) };
    let first = first_address(&args);
    let shared = shared();
    let Some(backend) = connect("WEBPLUS") else {
        return 1;
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.set_size_hints(420, 300, 0, 0);
    backend.on_first_frame(|| println!("WEBPLUS:UP:PASS"));
    let theme = backend.desktop_theme();
    let spec = PlatformSpec::new(APP_NAME).size(Dip(width as f32), Dip(height as f32));
    let focus_backend = Rc::clone(&backend);
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        // The shell's toasts for web notifications, on a thread of their own that wakes this window.
        let proxy = ui.proxy();
        let toasts = ShellWorker::spawn(LazyShell, move || {
            let _ = proxy.send(Msg::Shell);
        })
        .map(|worker| Rc::new(worker) as Rc<dyn Toasts>);
        let setup = Setup {
            url: first,
            visits: visits(shared),
            transfers: Transfers::new(Arc::clone(&shared.saver), Some(Box::new(LazyToaster))),
            internal: shared.internal.clone(),
            permissions: shared.permissions.clone(),
            toasts,
            host: Host {
                traces: {
                    crate::allow_named_markers(xui_app::probe::enabled());
                    xui_app::probe::enabled()
                },
                focused: Rc::new(move || focus_backend.focused()),
                launcher: Rc::new(LazyLauncher),
                probe: Box::new(|window, name, rect| {
                    xui_app::probe::widget(
                        window,
                        name,
                        rect.left,
                        rect.top,
                        rect.width(),
                        rect.height(),
                    );
                }),
            },
        };
        Browser::build(ui, Rc::new(FfiEngine), Arc::clone(mailbox::global()), setup)
            .expect("the LazyWebPlus window's widgets built")
    });
    backend.unbind();
    c_int::from(xui_app::launch::finish(
        xui_app::trace::Channel::Webplus,
        outcome,
    ))
}
