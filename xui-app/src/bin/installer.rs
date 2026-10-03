//! `xui-installer`: the user-facing half of the `.lzp` package system
//! (`docs/packages.md`, phase 5).
//!
//! One window: the installed list (with a `Remove` button per row), the remove
//! confirmation, and a four-step install wizard started by "Install a
//! package…": **Choose** the `.lzp` (typed, or picked with the file picker),
//! **Review** what it is, consent to its **Permissions** (grouped by risk), then
//! **Install**. `Back` walks the steps; `Cancel`/`Esc` leaves the wizard. A
//! package opened by `mimed`/Files arrives as a path argument and starts at
//! Review.
//!
//! The state machine and the text hygiene live in the `xui_app::installer`
//! module and are unit-tested there; this file owns the widgets, the `pkgd`
//! calls and the serial evidence:
//!
//! ```text
//! INSTALLER:UP:PASS
//! INSTALLER:STEP:<screen>                  (LIST, CHOOSE, REVIEW, PERMISSIONS, ...)
//! INSTALLER:LIST:PASS count=<n>            INSTALLER:LIST:FAIL <reason>
//! INSTALLER:PICK:PASS <path>               INSTALLER:PICK:FAIL <reason>
//! INSTALLER:INSPECT:PASS <system_name>     INSTALLER:INSPECT:FAIL <reason>
//! INSTALLER:CONSENT:SHOWN perms=<n> problems=<n>
//! INSTALLER:INSTALL:PASS <system_name>     INSTALLER:INSTALL:FAIL <reason>
//! INSTALLER:REMOVE:PASS <system_name>      INSTALLER:REMOVE:FAIL <reason>
//! INSTALLER:REMOVE:REFUSED <system_name>   (a core app: no confirmation)
//! INSTALLER:DEVELOP:PASS <label> asked=<0|1>   INSTALLER:DEVELOP:DENIED
//! INSTALLER:DEVELOP:ASK perms=<n>           INSTALLER:DEVELOP:FAIL <reason>
//! ```
//!
//! Started with `--develop <path>` (the `develop` verb, `mimed` from an IDE),
//! it shows only the development consent instead ([`develop`]).

use std::cell::Cell;
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::installer::{clean, Model, Request, Screen};
use xui_app::platform::{argv, pkg};
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, Event, PlatformSpec, TimerId, WidgetId};
use xui_core::units::Dip;
use xui_core::Key;

#[path = "installer/consent.rs"]
mod consent;
#[path = "installer/develop.rs"]
mod develop;
#[path = "installer/list_screen.rs"]
mod list_screen;
#[path = "installer/msg.rs"]
mod msg;
#[path = "installer/picker.rs"]
mod picker;
#[path = "installer/simple.rs"]
mod simple;
#[path = "installer/view.rs"]
mod view;
#[path = "installer/wizard.rs"]
mod wizard;

use msg::Msg;
use picker::Picker;
use view::{build as build_view, View};

/// The window size (DIP) the app asks for.
const WINDOW: (i32, i32) = (640, 480);

/// The installer app: the pure model plus the widgets of its current screen.
struct Installer {
    model: Model,
    /// The widgets of the shown screen. Held so their nodes stay alive; the
    /// previous view is dropped (destroying its nodes) when a new one is built.
    _view: View,
    /// Which screen `_view` was built for, so a change rebuilds it.
    shown: Screen,
    /// The one-shot timer that runs a deferred install outside the click.
    install_timer: Option<TimerId>,
    /// The Choose step's file picker (window-lived, above every view).
    picker: Picker,
    /// A rebuild was wanted while the picker was open; it runs once it closes
    /// (a fresh view would otherwise be stacked over the open picker).
    stale: bool,
}

impl Installer {
    /// Builds the app, performing the initial `List`, then `Inspect` when the
    /// app was launched with a package path (the list tells the consent
    /// screen whether the package updates a built-in app).
    fn build(ui: &mut Ui<Msg>, start: Option<std::path::PathBuf>) -> Result<Installer, String> {
        let mut model = Model::new();
        reload(&mut model);
        if let Some(path) = start {
            inspect(&mut model, &path.to_string_lossy());
        }
        let picker = Picker::build(ui)?;
        install_hooks(ui, picker.gate());
        let view = build_view(ui, &model)?;
        let shown = model.screen;
        println!("INSTALLER:STEP:{}", shown.marker());
        Ok(Installer {
            model,
            _view: view,
            shown,
            install_timer: None,
            picker,
            stale: false,
        })
    }

    /// Replaces the widgets with a view for the model's current screen.
    fn rebuild(&mut self, ui: &mut Ui<Msg>) {
        if self.picker.is_open() {
            self.stale = true;
            return;
        }
        self.stale = false;
        match build_view(ui, &self.model) {
            Ok(view) => {
                self._view = view;
                if self.shown != self.model.screen {
                    println!("INSTALLER:STEP:{}", self.model.screen.marker());
                }
                self.shown = self.model.screen;
            }
            Err(error) => {
                println!("INSTALLER:VIEW:FAIL:{}", clean(&error));
                ui.quit();
            }
        }
    }
}

impl App for Installer {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        let mut dirty = false;
        match msg {
            Msg::Reload => {
                reload(&mut self.model);
                dirty = true;
            }
            Msg::StartInstall => {
                self.model.start_wizard();
                dirty = true;
            }
            Msg::PathChanged(text) => {
                // Kept in the model but not rebuilt: rebuilding would drop the
                // field's focus mid-typing.
                self.model.set_path(&text);
            }
            Msg::Browse => {
                if self.model.screen == Screen::Choose {
                    self.picker.show(&self.model.path_input);
                }
            }
            Msg::Picked(path) => {
                if self.model.screen == Screen::Choose {
                    picked(&mut self.model, &path);
                }
                dirty = true;
            }
            Msg::PickCancelled => dirty = self.stale,
            Msg::Inspect => {
                if self.model.screen == Screen::Choose {
                    let path = self.model.path_input.trim().to_owned();
                    if argv::is_acceptable(Path::new(&path)) {
                        inspect(&mut self.model, &path);
                    } else {
                        println!("INSTALLER:INSPECT:FAIL not an absolute path");
                        self.model
                            .inspect_failed("Enter an absolute path to a .lzp package.");
                    }
                    dirty = true;
                }
            }
            Msg::Next => dirty = self.model.advance(),
            Msg::Back => {
                self.model.back();
                dirty = true;
            }
            Msg::Install => {
                if self.model.can_install() && self.install_timer.is_none() {
                    self.model.install_started();
                    dirty = true;
                    // Defer the blocking call by a tick so the progress screen
                    // paints before it runs.
                    self.install_timer = Some(ui.set_timer(1));
                }
            }
            Msg::Tick(id) => {
                if self.install_timer == Some(id) {
                    self.install_timer = None;
                    ui.kill_timer(id);
                    if let Some(Request::Install(path)) = self.model.take_pending() {
                        match pkg::install(&path) {
                            Ok(app) => {
                                println!("INSTALLER:INSTALL:PASS {}", clean(&app.system_name));
                                self.model.install_ok(app);
                            }
                            Err(reason) => {
                                println!("INSTALLER:INSTALL:FAIL {}", clean(&reason));
                                self.model.install_failed(reason);
                            }
                        }
                        dirty = true;
                    }
                }
            }
            Msg::AskRemove(system_name) => {
                let target = self
                    .model
                    .packages
                    .iter()
                    .find(|app| app.system_name == system_name)
                    .cloned();
                if let Some(app) = target {
                    if !self.model.remove_asked(app) {
                        println!("INSTALLER:REMOVE:REFUSED {}", clean(&system_name));
                    }
                    dirty = true;
                }
            }
            Msg::ConfirmRemove => {
                if let Some(system_name) = self.model.pending_remove_name() {
                    match pkg::remove(&system_name) {
                        Ok(()) => {
                            println!("INSTALLER:REMOVE:PASS {}", clean(&system_name));
                            self.model.remove_ok(&system_name);
                        }
                        Err(reason) => {
                            println!("INSTALLER:REMOVE:FAIL {}", clean(&reason));
                            self.model.remove_failed(reason);
                        }
                    }
                    dirty = true;
                }
            }
            Msg::Cancel => {
                self.model.cancel();
                dirty = true;
            }
            Msg::Done => {
                self.model.done();
                dirty = true;
            }
            Msg::Escape => {
                if self.model.screen == Screen::List {
                    ui.quit();
                } else {
                    self.model.cancel();
                    dirty = true;
                }
            }
            Msg::KeyQ => {
                // `q` quits from the list only: the Choose step's path field
                // takes typed text.
                if self.model.screen == Screen::List {
                    ui.quit();
                }
            }
            Msg::Quit => ui.quit(),
            Msg::Resize => dirty = true,
        }
        if dirty || self.shown != self.model.screen {
            self.rebuild(ui);
        }
    }
}

/// Registers the window-level hooks: close, shortcuts, timer and resize.
/// `picker_open` keeps the shortcuts away from the file picker's own keys.
fn install_hooks(ui: &Ui<Msg>, picker_open: Rc<Cell<bool>>) {
    ui.on_close(|| Some(Msg::Quit));
    ui.on_key(move |key, _modifiers| match key {
        _ if picker_open.get() => None,
        Key::ESCAPE => Some(Msg::Escape),
        Key::Q => Some(Msg::KeyQ),
        _ => None,
    });
    ui.on_timer(|id| Some(Msg::Tick(id)));
    // A window-level resize maps to a message so the re-layout runs in
    // `update`, outside the event dispatch.
    ui.register_events(WidgetId::NONE, |event| match event {
        Event::Resize { .. } => Some(Msg::Resize),
        _ => None,
    });
}

/// Queries `pkgd.List` and reports the serial evidence.
fn reload(model: &mut Model) {
    match pkg::list() {
        Ok(apps) => {
            println!("INSTALLER:LIST:PASS count={}", apps.len());
            model.list_loaded(apps);
        }
        Err(reason) => {
            println!("INSTALLER:LIST:FAIL {}", clean(&reason));
            model.list_failed(reason);
        }
    }
}

/// Puts the picker's `path` in the Choose step's field, after the same check a
/// typed path gets, and reports the serial evidence.
fn picked(model: &mut Model, path: &Path) {
    let text = path.to_string_lossy();
    if argv::is_acceptable(path) {
        println!("INSTALLER:PICK:PASS {}", clean(&text));
        model.path_picked(&text);
    } else {
        println!("INSTALLER:PICK:FAIL not an absolute path");
        model.inspect_failed("The picker returned a path that cannot be used.");
    }
}

/// Inspects `path` and reports the serial evidence.
fn inspect(model: &mut Model, path: &str) {
    match pkg::inspect(path) {
        Ok(package) => {
            println!("INSTALLER:INSPECT:PASS {}", clean(&package.system_name));
            println!(
                "INSTALLER:CONSENT:SHOWN perms={} problems={}",
                package.permissions.len(),
                package.problems.len()
            );
            model.inspect_ok(path.to_owned(), package);
        }
        Err(reason) => {
            println!("INSTALLER:INSPECT:FAIL {}", clean(&reason));
            model.inspect_failed(reason);
        }
    }
}

fn main() -> ExitCode {
    if let Some(path) = develop::requested(std::env::args_os()) {
        return develop::main(path);
    }
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("INSTALLER:BIND:FAIL:{code}");
            return ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    // Resizable; the screens re-lay out to the client area. The minimum keeps
    // the consent screen's three stacked lists from overlapping.
    backend.set_size_hints(480, 360, 0, 0);
    backend.on_first_frame(|| println!("INSTALLER:UP:PASS"));

    let start = argv::file_arg(std::env::args_os());
    // Match the desktop's light/dark mode and accent (Settings).
    let theme = backend.desktop_theme();
    let spec = PlatformSpec::new("Installer").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        match Installer::build(ui, start) {
            Ok(app) => app,
            Err(error) => {
                println!("INSTALLER:BUILD:FAIL:{error}");
                std::process::exit(1);
            }
        }
    });
    backend.unbind();
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            println!("INSTALLER:RUN:FAIL:{error}");
            ExitCode::FAILURE
        }
    }
}
