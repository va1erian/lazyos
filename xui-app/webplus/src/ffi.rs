//! LazyWebPlus's half of the C ABI (`webkit/port/app/lazyos_ui.h`) and its LazyOS platform glue.
//!
//! `xui-webkit`'s `ffi` has the calls every window shares (the frame, the page state, the link
//! under the pointer, the engine's functions). The ones that decide something belong to the
//! window and are here: which features the UI process installs, the profile directory,
//! the navigation policy, where a download is written, and `lzw_ui_main`, which runs the window.
//! They run on WebKit's run loop, so none may block: the policy is a pure function and a
//! download's destination is a `create_dir_all` and a few `exists` calls.

use std::ffi::{c_char, c_int, CString};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use xui_app::platform::{launcher, notify};
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_webkit::abi::{byte_count, copy_c_string, features, first_address, NavKind, Verdict};
use xui_webkit::events::Event;
use xui_webkit::ffi::{arguments, connect, text, FfiEngine};
use xui_webkit::gesture;
use xui_webkit::mailbox;

use crate::app::{Browser, Host, Setup, APP_NAME};
use crate::downloads::Saver;
use crate::handoff::Launcher;
use crate::internal::Internal;
use crate::policy::{self, Decision};
use crate::transfers::{Toast, ToastLevel, Toaster, Transfers};
use crate::visits::Visits;
use crate::{DOWNLOADS_DIR, HISTORY_FILE, SYSTEM_NAME};

/// How long after a real click or key a hand-off to another program is still the person's (a slow guest
/// takes a moment from the click to the navigation).
const HANDOFF_WINDOW: Duration = Duration::from_millis(3000);

/// The window size asked of the compositor: short enough that the status bar clears the taskbar of
/// a 720p desktop.
const WINDOW: (i32, i32) = (1000, 540);

/// What the three threads of the process share: made once, by whichever call comes first (the UI
/// process asks for the profile directory before it starts the window).
struct Shared {
    home: Option<String>,
    saver: Arc<Saver>,
    internal: Internal,
    profile: Option<CString>,
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
        Shared {
            home,
            saver: Arc::new(Saver::new(folder)),
            internal: Internal::default(),
            profile,
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
        notify::post(&notice).map_err(|code| format!("errno {code}"))
    }

    fn close(&mut self, id: u64) {
        let _ = notify::close(id);
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
        let setup = Setup {
            url: first,
            visits: visits(shared),
            transfers: Transfers::new(Arc::clone(&shared.saver), Some(Box::new(LazyToaster))),
            internal: shared.internal.clone(),
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
