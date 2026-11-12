//! The one way to start a desktop app: [`app`].
//!
//! ```ignore
//! fn main() -> ! {
//!     xui_app::app(Channel::Pdf, "PDF Viewer")
//!         .size(800, 600)
//!         .min_size(420, 300)
//!         .opens(Msg::OpenChosen)   // a path on argv
//!         .drops(Msg::Dropped)      // `text/uri-list` dropped on the window
//!         .run(|ui, cx| PdfApp::build(ui, cx.host()))
//! }
//! ```
//!
//! [`AppBuilder::run`] connects (display owner, or `xuid` client; issue
//! #215), opens the window in the desktop's theme (xui applies
//! `Backend::system_theme` itself; nothing here sets a theme), installs the
//! trace panic hook, reports `UP` on the first frame and `QUIT` at a clean
//! exit through [`crate::trace`], reports `BIND:FAIL` when no display can be
//! bound and `RUN:FAIL` when the window cannot be built, and always exits
//! through [`finish`]. A tray app is [`AppBuilder::resident`] or
//! [`AppBuilder::windowless`]; a host with its own loop (the LazyRAD IDE) is
//! [`AppBuilder::run_host`].

mod files;

use std::rc::Rc;
use std::time::{Duration, Instant};

use xui_core::app::{App, Ui};
use xui_core::backend::{Backend, PlatformSpec, Result};
use xui_core::units::Dip;

pub use files::{Files, NoFiles, WithFiles};

use crate::backend::LazyOSBackend;
use crate::context::AppContext;
use crate::resident::Resident;
use crate::trace::{self, Channel};

/// The argument `init`'s app registry passes to a desktop app.
pub const CLIENT_ARG: &str = "--client";

/// The window size an app asks for unless it says otherwise.
const DEFAULT_SIZE: (i32, i32) = (800, 600);

impl LazyOSBackend {
    /// Choose the mode at start-up.
    ///
    /// An explicit [`CLIENT_ARG`] always means client mode, so an app launched
    /// before `xuid` has bound the display cannot race it for the grant.
    /// Otherwise the app tries to own the display and falls back to client
    /// mode when the grant is already held (by `xuid`).
    pub fn connect() -> std::result::Result<LazyOSBackend, i64> {
        if std::env::args().any(|arg| arg == CLIENT_ARG) {
            return Self::new_client();
        }
        Self::new().or_else(|_| Self::new_client())
    }

    /// The window size an app should ask for, in design pixels (the app
    /// wraps it in `Dip`): the whole screen when it owns the display,
    /// `windowed` (its preferred size) when a compositor lays it out. A client
    /// surface's size is only known once its window opens.
    pub fn window_size(&self, windowed: (i32, i32)) -> (i32, i32) {
        if self.is_client() {
            windowed
        } else {
            let (width, height) = self.screen();
            let scale = self.scale() as i32;
            (width / scale, height / scale)
        }
    }
}

/// What every start shares, whatever it runs afterwards.
pub(crate) struct Setup {
    pub(crate) channel: Channel,
    pub(crate) title: String,
    pub(crate) windowed: (i32, i32),
    pub(crate) min: Option<(u32, u32)>,
    pub(crate) ask_before_close: bool,
    pub(crate) client_only: bool,
    pub(crate) display_wait: Duration,
    /// The `confd` subtree name under `user/<uid>/` (the channel's prefix in
    /// lower case unless the app says otherwise).
    pub(crate) settings_id: Option<String>,
    /// The package's system name, the folder under `<home>/.apps`.
    pub(crate) system_name: Option<String>,
}

impl Setup {
    /// The display, waiting for the compositor up to `display_wait`
    /// (`WAIT:<errno>` once): `BIND:FAIL` and exit 1 without one.
    pub(crate) fn connect(&self) -> Rc<LazyOSBackend> {
        let started = Instant::now();
        let mut said = false;
        loop {
            let connected = if self.client_only {
                LazyOSBackend::new_client()
            } else {
                LazyOSBackend::connect()
            };
            match connected {
                Ok(backend) => return Rc::new(backend),
                Err(code) if started.elapsed() < self.display_wait => {
                    if !said {
                        trace::line!(self.channel, "WAIT:{code}");
                        said = true;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                Err(code) => {
                    trace::fail!(self.channel, "BIND", "{code}");
                    std::process::exit(1);
                }
            }
        }
    }

    /// The panic hook, the display, size hints, the close policy and the
    /// first-frame `UP` marker.
    fn start(&self) -> Rc<LazyOSBackend> {
        trace::install_panic_hook(self.channel);
        let backend = self.connect();
        self.prepare(&backend);
        backend
    }

    /// Size hints, the close policy and `UP`, on a connected backend.
    pub(crate) fn prepare(&self, backend: &LazyOSBackend) {
        if let Some((width, height)) = self.min {
            backend.set_size_hints(width, height, 0, 0);
        }
        if self.ask_before_close {
            backend.ask_before_close();
        }
        let channel = self.channel;
        backend.on_first_frame(move || trace::pass!(channel, "UP"));
    }
}

/// Describes a desktop app; see [`app`].
pub struct AppBuilder<F = NoFiles> {
    pub(crate) setup: Setup,
    files: F,
    at_exit: Option<Box<dyn FnOnce() -> bool>>,
}

/// Starts describing the desktop app that reports as `channel`, with a window
/// titled `title`.
pub fn app(channel: Channel, title: &str) -> AppBuilder {
    AppBuilder {
        setup: Setup {
            channel,
            title: title.to_owned(),
            windowed: DEFAULT_SIZE,
            min: None,
            ask_before_close: false,
            client_only: false,
            display_wait: Duration::ZERO,
            settings_id: None,
            system_name: None,
        },
        files: NoFiles,
        at_exit: None,
    }
}

impl<F> AppBuilder<F> {
    /// The window's size in design pixels when a compositor lays it out (as
    /// display owner the app fills the screen).
    pub fn size(mut self, width: i32, height: i32) -> Self {
        self.setup.windowed = (width, height);
        self
    }

    /// The smallest content size, in design pixels, below which the app's
    /// layout clips. An app with no minimum has a fixed-size window.
    pub fn min_size(mut self, width: u32, height: u32) -> Self {
        self.setup.min = Some((width, height));
        self
    }

    /// Keep the window when the window manager closes it: the app still gets
    /// its close event and decides (the Terminal with a program running).
    pub fn ask_before_close(mut self) -> Self {
        self.setup.ask_before_close = true;
        self
    }

    /// Never try to own the display: always a `xuid` client (the compositor's
    /// own client test, started beside `xuid`).
    pub fn client_only(mut self) -> Self {
        self.setup.client_only = true;
        self
    }

    /// Wait up to `wait` for the compositor to appear instead of failing at
    /// once: an app that opens at login starts while the core apps are still
    /// being installed.
    pub fn wait_for_display(mut self, wait: Duration) -> Self {
        self.setup.display_wait = wait;
        self
    }

    /// The name of the app's `confd` subtree (`user/<uid>/<id>/..`, what
    /// `AppContext::settings` reads and writes) when it is not the channel's
    /// prefix in lower case: the volume applet keeps `audio`.
    pub fn settings_id(mut self, id: &str) -> Self {
        self.setup.settings_id = Some(id.to_owned());
        self
    }

    /// The app's package system name (`os.lazy.nes`): the folder
    /// `AppContext::dirs().app_data()` names under `<home>/.apps`.
    pub fn system_name(mut self, name: &str) -> Self {
        self.setup.system_name = Some(name.to_owned());
        self
    }

    /// Cleanup that runs once the window is closed and the display released,
    /// before `QUIT`. Returning `false` reports a failed exit (status 1, no
    /// `QUIT`): LazyWeb's children that outlived the window.
    pub fn at_exit(mut self, cleanup: impl FnOnce() -> bool + 'static) -> Self {
        self.at_exit = Some(Box::new(cleanup));
        self
    }

    /// A tray app with a window it opens on demand (`Resident::window`).
    pub fn resident(self) -> ResidentApp {
        ResidentApp {
            setup: self.setup,
            windowed: true,
        }
    }

    /// A tray app with no window at all, so it needs no display permission.
    pub fn windowless(self) -> ResidentApp {
        ResidentApp {
            setup: self.setup,
            windowed: false,
        }
    }

    /// For a host that runs its own event loop on the connected backend (the
    /// LazyRAD IDE, the shell): `host` gets the backend and the window's
    /// spec, and returns how the loop ended. Connecting, hints, `UP`, `QUIT`
    /// and the exit are the same as [`AppBuilder::run`].
    pub fn run_host<E, H>(self, host: H) -> !
    where
        E: std::fmt::Display,
        H: FnOnce(Rc<LazyOSBackend>, PlatformSpec) -> std::result::Result<(), E>,
    {
        let backend = self.setup.start();
        let (width, height) = backend.window_size(self.setup.windowed);
        let spec = PlatformSpec::new(self.setup.title.as_str())
            .size(Dip(width as f32), Dip(height as f32));
        let outcome = host(Rc::clone(&backend), spec);
        exit(&self.setup, &backend, outcome, self.at_exit)
    }

    /// Connects, opens the window, builds the app with `make` and runs it
    /// until it quits; then releases the display and exits the process, 0
    /// after a clean quit and 1 otherwise. `make` gets the [`AppContext`]:
    /// the shared services, and through `cx.backend()` the display for
    /// app-specific requests (`request_size`) and for the rare app whose
    /// first-frame line carries data (it replaces the plain `UP` with
    /// `backend.on_first_frame`).
    pub fn run<A, M>(self, make: M) -> !
    where
        A: App,
        F: Files<A::Msg>,
        M: FnOnce(&mut Ui<A::Msg>, &AppContext) -> Result<A>,
    {
        let AppBuilder {
            setup,
            files,
            at_exit,
        } = self;
        let backend = setup.start();
        let (width, height) = backend.window_size(setup.windowed);
        let window = Rc::clone(&backend);
        let cx = AppContext::new(Rc::clone(&backend), &setup);
        let outcome = xui_core::app(setup.title.as_str())
            .size(width, height)
            .backend(Rc::clone(&backend) as Rc<dyn Backend>)
            .run(move |ui| {
                files.attach(&window, ui);
                let app = make(ui, &cx)?;
                files.open_argv(ui);
                Ok(app)
            });
        exit(&setup, &backend, outcome, at_exit)
    }
}

/// Releases the display, runs the app's cleanup and exits: `RUN:FAIL`
/// (status 1) when the loop failed, `QUIT` (status 0) otherwise.
fn exit<E: std::fmt::Display>(
    setup: &Setup,
    backend: &LazyOSBackend,
    outcome: std::result::Result<(), E>,
    at_exit: Option<Box<dyn FnOnce() -> bool>>,
) -> ! {
    backend.unbind();
    let clean = at_exit.is_none_or(|cleanup| cleanup());
    let ran = outcome.is_ok();
    let status = finish(setup.channel, outcome);
    if status == 0 && ran && clean {
        trace::pass!(setup.channel, "QUIT");
    }
    std::process::exit(i32::from(status != 0 || !clean))
}

/// A tray app: its [`Resident`] runtime and nothing else; see
/// [`AppBuilder::resident`] and [`AppBuilder::windowless`].
pub struct ResidentApp {
    setup: Setup,
    windowed: bool,
}

impl ResidentApp {
    /// Connects (a windowless app only to `init` and the tray), runs `body`
    /// and, when it returns (the app was asked to quit), clears the tray item,
    /// releases the display and exits 0 after `QUIT`. `UP` is the app's to
    /// report: a windowed app's comes with its first window's first frame
    /// (`Resident::window`), a windowless app says when its item is up.
    pub fn run(self, body: impl FnOnce(&Rc<Resident>)) -> ! {
        let ResidentApp { setup, windowed } = self;
        trace::install_panic_hook(setup.channel);
        let backend = windowed.then(|| {
            let backend = setup.connect();
            if let Some((width, height)) = setup.min {
                backend.set_size_hints(width, height, 0, 0);
            }
            backend
        });
        let channel = setup.channel;
        let res = Resident::start(&setup, backend);
        body(&res);
        let _ = res.tray.borrow_mut().clear();
        if let Some(backend) = &res.backend {
            backend.unbind();
        }
        trace::pass!(channel, "QUIT");
        std::process::exit(0)
    }
}

/// The process exit status for an app's `run` outcome, reporting a failure as
/// `<channel>:RUN:FAIL:<error>`.
///
/// A window closed before its first attach (issue #498: the user, or monkey
/// input, dismissed it while it opened) is not a failure: it prints
/// `<channel>:RUN:CLOSED` and exits 0, so `init` does not mark the app
/// degraded. Every desktop app exits through it; a host with a loop of its
/// own (`emusic`) calls it directly.
pub fn finish<E: std::fmt::Display>(channel: Channel, outcome: std::result::Result<(), E>) -> u8 {
    match outcome {
        Ok(()) => 0,
        // The flag is process-wide, so the error must also be the close the
        // backend reported (its text), not an unrelated failure of the app.
        Err(error)
            if crate::client_window::closed_while_opening()
                && error
                    .to_string()
                    .contains(&crate::client_window::OpenError::Closed.to_string()) =>
        {
            trace::line!(channel, "RUN:CLOSED");
            0
        }
        Err(error) => {
            trace::fail!(channel, "RUN", "{error}");
            1
        }
    }
}
