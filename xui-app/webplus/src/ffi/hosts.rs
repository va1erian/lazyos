//! The browser's hand-offs to the OS: `mimed` for links and files, `os.lazy.notify` for the
//! download toasts.

use super::*;

/// `mimed`, for the links and files the browser hands on.
pub(super) struct LazyLauncher;

impl Launcher for LazyLauncher {
    fn open_url(&self, url: &str) -> Result<(), String> {
        launcher::open_url(url).map_err(|e| e.to_string())
    }
    fn open_path(&self, path: &str) -> Result<(), String> {
        launcher::open_path(path).map_err(|e| e.to_string())
    }
}

/// `os.lazy.notify`, for the download toasts.
pub(super) struct LazyToaster;

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
