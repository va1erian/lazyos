//! What every desktop app gets from the builder instead of re-deriving it:
//! [`AppContext`] (`docs/client-api-plan.md` 4.2).
//!
//! ```ignore
//! xui_app::app(Channel::Pdf, "PDF Viewer")
//!     .run(|ui, cx| PdfApp::build(ui, cx.host()))
//! ```
//!
//! The context holds the connected backend, the app's channel and its
//! settings id, and gives out:
//!
//! * [`AppContext::settings`]: a typed store over `confd` at
//!   `user/<uid>/<settings id>/..`, loaded once and saved off the UI thread;
//! * [`AppContext::recent`]: the app's recent files, kept through settings;
//! * [`AppContext::open_dialog`] and [`AppContext::save_dialog`]: the dialog
//!   presets (the error dialog is `xui_host::error::ErrorDialog`);
//! * [`AppContext::host`]: the context as the [`AppHost`] the portable app
//!   crates take (file system, start folder, launcher, atomic write, and the
//!   evidence seam that turns a crate's report into a `trace` line).

mod kv;

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{FileDialog, FileSystem, StdFileSystem};
use xui_explorer::platform::Launcher;
use xui_host::dialogs::{self, Filter};
use xui_host::evidence;
use xui_host::recent::Recent;
use xui_host::settings::{Factory, KeyValue, MemKv, Saver, Settings};
use xui_host::{AppHost, Report};

pub use kv::user_key;
use kv::ConfdKv;

use crate::backend::LazyOSBackend;
use crate::launch::Setup;
use crate::platform::confd_store::ConfdStore;
use crate::platform::launcher::LazyLauncher;
use crate::platform::{dirs as picker_dirs, storage};
use crate::trace::{self, Channel};

/// The shared services of one running app.
pub struct AppContext {
    backend: Rc<LazyOSBackend>,
    channel: Channel,
    settings_id: String,
    recent: OnceCell<Rc<Recent>>,
}

impl AppContext {
    pub(crate) fn new(backend: Rc<LazyOSBackend>, setup: &Setup) -> AppContext {
        // The evidence of the crates that have no host in reach (`settings`,
        // `confd-editor`): one line, one `trace` line on the app's channel.
        let channel = setup.channel;
        evidence::install(move |line| trace::line!(channel, "{line}"));
        AppContext {
            backend,
            channel: setup.channel,
            settings_id: setup.channel.prefix().to_ascii_lowercase(),
            recent: OnceCell::new(),
        }
    }

    /// The connected display, for the app-specific requests (`request_size`,
    /// drag hooks, a first-frame line that carries data).
    pub fn backend(&self) -> &Rc<LazyOSBackend> {
        &self.backend
    }

    /// The channel the app traces on.
    pub fn channel(&self) -> Channel {
        self.channel
    }

    /// The context as the host a portable app crate takes.
    pub fn host(&self) -> Rc<dyn AppHost> {
        Rc::new(Host {
            channel: self.channel,
            app: self.settings_id.clone(),
        })
    }

    /// The app's typed settings: one `confd` key per field under
    /// `user/<uid>/<settings id>/`, read now and saved on a worker thread.
    /// `confd` not yet running (an app that starts at login before it) gives
    /// settings that live in memory, so start-up never waits for it.
    pub fn settings<T: Serialize + DeserializeOwned + Default>(&self) -> Settings<T> {
        let (kv, saver) = backing();
        Settings::open(kv.as_ref(), &self.settings_id, saver)
    }

    /// The files the app opened or saved last, newest first, at most
    /// [`xui_host::recent::MAX`]. Read from `confd` on the first call.
    pub fn recent(&self) -> Rc<Recent> {
        Rc::clone(self.recent.get_or_init(|| {
            let (kv, saver) = backing();
            Rc::new(Recent::open(kv.as_ref(), &self.settings_id, saver))
        }))
    }

    /// An Open dialog (file system, start folder, "All files" last, refuses a
    /// path that does not exist). It opens in the folder of the newest recent
    /// file when the app has asked for [`AppContext::recent`] and that
    /// folder still exists.
    pub fn open_dialog<M: 'static>(
        &self,
        ui: &Ui<M>,
        title: &str,
        filters: &[Filter<'_>],
        on_cancel: impl Fn() -> Option<M> + 'static,
    ) -> Result<FileDialog<M>> {
        let dialog = dialogs::open_dialog(ui, &*self.host(), title, filters, on_cancel)?;
        Ok(match self.recent_dir() {
            Some(dir) => dialog.initial_dir(dir),
            None => dialog,
        })
    }

    /// A Save dialog over the same preset; `filters` is used as given.
    pub fn save_dialog<M: 'static>(
        &self,
        ui: &Ui<M>,
        title: &str,
        filters: &[Filter<'_>],
        on_cancel: impl Fn() -> Option<M> + 'static,
    ) -> Result<FileDialog<M>> {
        dialogs::save_dialog(ui, &*self.host(), title, filters, on_cancel)
    }

    fn recent_dir(&self) -> Option<PathBuf> {
        let dir = self.recent.get()?.last_dir()?;
        dir.is_dir().then_some(dir)
    }
}

/// `confd` with saves on a worker thread (a fresh client there); when `confd`
/// is not registered yet, a store in memory, so an app never waits for it at
/// start-up.
fn backing() -> (Box<dyn KeyValue>, Saver) {
    if ConfdStore::new().registered() {
        let factory: Factory = Arc::new(|| Box::new(ConfdKv));
        (Box::new(ConfdKv), Saver::Worker(factory))
    } else {
        let memory = Rc::new(MemKv::anonymous());
        (Box::new(MemKv::anonymous()), Saver::Inline(memory))
    }
}

/// The app's [`AppHost`]: what the portable crates ask of their platform.
struct Host {
    channel: Channel,
    app: String,
}

impl Report for Host {
    fn report(&self, line: &str) {
        trace::line!(self.channel, "{line}");
    }
}

impl AppHost for Host {
    fn file_system(&self) -> Rc<dyn FileSystem> {
        Rc::new(StdFileSystem)
    }

    fn start_dir(&self) -> PathBuf {
        picker_dirs::default_dir()
    }

    /// `/tmp/<app>-<pid>`: private to this run.
    fn temp_dir(&self) -> PathBuf {
        PathBuf::from(fhs::mount::TMP).join(format!("{}-{}", self.app, std::process::id()))
    }

    fn launch(&self, path: &Path) -> std::result::Result<(), String> {
        LazyLauncher::new()
            .open(path)
            .map_err(|error| error.to_string())
    }

    fn read_only_roots(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(fhs::system::SYSTEM)]
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> std::result::Result<(), String> {
        storage::write_atomic(path, bytes)
    }
}
