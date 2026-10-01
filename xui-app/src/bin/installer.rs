//! `xui-installer`: the user-facing half of the `.lzp` package system
//! (`docs/packages.md`, phase 5).
//!
//! One window, three screens: the installed list (with a `Remove` button per
//! row and an "open a package" field), the consent screen that shows a
//! package's permissions grouped by risk, and the remove confirmation. A
//! package opened by `mimed`/Files arrives as a path argument and goes straight
//! to consent.
//!
//! The state machine and the text hygiene live in the `xui_app::installer`
//! module and are unit-tested there; this file owns the widgets, the `pkgd`
//! calls and the serial evidence:
//!
//! ```text
//! INSTALLER:UP:PASS
//! INSTALLER:LIST:PASS count=<n>            INSTALLER:LIST:FAIL <reason>
//! INSTALLER:INSPECT:PASS <system_name>     INSTALLER:INSPECT:FAIL <reason>
//! INSTALLER:CONSENT:SHOWN perms=<n> problems=<n>
//! INSTALLER:INSTALL:PASS <system_name>     INSTALLER:INSTALL:FAIL <reason>
//! INSTALLER:REMOVE:PASS <system_name>      INSTALLER:REMOVE:FAIL <reason>
//! ```

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
#[path = "installer/list_screen.rs"]
mod list_screen;
#[path = "installer/msg.rs"]
mod msg;
#[path = "installer/simple.rs"]
mod simple;
#[path = "installer/view.rs"]
mod view;

use msg::Msg;
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
}

impl Installer {
    /// Builds the app, performing the initial `List` (or `Inspect` when the app
    /// was launched with a package path).
    fn build(ui: &mut Ui<Msg>, start: Option<std::path::PathBuf>) -> Result<Installer, String> {
        let mut model = Model::new();
        match start {
            Some(path) => inspect(&mut model, &path.to_string_lossy()),
            None => reload(&mut model),
        }
        install_hooks(ui);
        let view = build_view(ui, &model)?;
        let shown = model.screen;
        Ok(Installer {
            model,
            _view: view,
            shown,
            install_timer: None,
        })
    }

    /// Replaces the widgets with a view for the model's current screen.
    fn rebuild(&mut self, ui: &mut Ui<Msg>) {
        match build_view(ui, &self.model) {
            Ok(view) => {
                self._view = view;
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
            Msg::PathChanged(text) => {
                // Kept in the model but not rebuilt: rebuilding would drop the
                // field's focus mid-typing.
                self.model.set_path(&text);
            }
            Msg::Inspect => {
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
            Msg::Install => {
                let installable = self.model.screen == Screen::Consent
                    && self.install_timer.is_none()
                    && self
                        .model
                        .inspected
                        .as_ref()
                        .is_some_and(|package| package.problems.is_empty());
                if installable {
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
                    self.model.remove_asked(app);
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
                // `q` quits from the list; the path field needs `q` only when
                // the user has started typing (an absolute path starts with
                // `/`, so the first character is never `q`).
                if self.model.screen == Screen::List && self.model.path_input.is_empty() {
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
fn install_hooks(ui: &Ui<Msg>) {
    ui.on_close(|| Some(Msg::Quit));
    ui.on_key(|key, _modifiers| match key {
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
    let spec = PlatformSpec::new("Installer").size(Dip(width as f32), Dip(height as f32));
    let outcome =
        run_app(
            Rc::clone(&backend) as Rc<dyn Backend>,
            spec,
            move |ui| match Installer::build(ui, start) {
                Ok(app) => app,
                Err(error) => {
                    println!("INSTALLER:BUILD:FAIL:{error}");
                    std::process::exit(1);
                }
            },
        );
    backend.unbind();
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            println!("INSTALLER:RUN:FAIL:{error}");
            ExitCode::FAILURE
        }
    }
}
