//! `sysmon`: a windowed dashboard over the native system-stats snapshot
//! (syscall 14, issue #144).
//!
//! Three tabs laid out without coordinates. Overview is for anyone: how busy
//! the processor is (now and over the last minute), memory split into named,
//! coloured shares (programs, the system, the disk cache, free) and the
//! busiest programs. Services (issue #489) lists the services `init`
//! supervises with the health `healthd` retains for each. Advanced keeps the
//! kernel's own counters (frames, slab, kernel heap) and the full task table.
//! The Help button (or F1) opens the app's guide, `README.md` in its package
//! docs, in the Docs app through `mimed`. A status bar carries uptime and
//! refresh counts. A one-second timer refreshes; `o`/`s`/`a` (or a click on a
//! tab) switch tabs, the tables scroll with the keyboard and the wheel, `r`
//! refreshes immediately, `c` toggles the compact view (three meters) by
//! asking the compositor for a size, and `q` quits.
//!
//! Serial evidence: `SYSMON:UP:PASS` after the first frame (or
//! `SYSMON:UP:FAIL:<errno>` when the snapshot is unreadable),
//! `SYSMON:REFRESH:PASS` on `r`, `SYSMON:VIEW:<overview|services|advanced>`
//! on a tab switch, `SYSMON:HELP:PASS` once `mimed` launched the guide (or
//! `SYSMON:HELP:FAIL:<reason>`), `SYSMON:SERVICES:PASS services=<n> ok=<n> degraded=<n> down=<n>`
//! the first time the Services tab shows both sources after a switch to it
//! (`SYSMON:SERVICES:NONE:init=<errno> healthd=<errno>` once when it
//! cannot yet), `SYSMON:SIZE:<w>x<h>` after every resize, `SYSMON:QUIT:PASS`
//! on `q` (or the window close button), and a `SYSMON:DATA:...` line with the
//! headline counters.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::platform::launcher;
use xui_app::services::{self, Services};
use xui_app::sysinfo::{self, Snapshot};
use xui_app::{compact, hidpi, launch};
use xui_core::Key;
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, WidgetId};

#[path = "sysmon/advanced.rs"]
mod advanced;
#[path = "sysmon/load.rs"]
mod load;
#[path = "sysmon/overview.rs"]
mod overview;
#[path = "sysmon/paint.rs"]
mod paint;
#[cfg(test)]
#[path = "sysmon/tests.rs"]
mod tests;
#[path = "sysmon/view.rs"]
mod view;

use load::Load;
use view::Widgets;

/// The window size when a compositor lays the app out (issue #215); as the
/// display owner it fills the screen instead.
const WINDOW: (i32, i32) = (860, 600);
/// How often the snapshot refreshes.
const REFRESH_MILLIS: u32 = 1000;

/// The app's guide, shipped in its package's `docs/` (`docs/packages.md`).
fn help_path() -> String {
    format!("{}/os.lazy.sysmon/README.md", fhs::docs::DOCS_APPS)
}

/// The three tabs, in their order on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum View {
    Overview,
    Services,
    Advanced,
}

impl View {
    pub(crate) fn from_index(index: usize) -> View {
        match index {
            1 => View::Services,
            2 => View::Advanced,
            _ => View::Overview,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// The word the `SYSMON:VIEW:<name>` marker carries.
    fn marker(self) -> &'static str {
        match self {
            View::Overview => "overview",
            View::Services => "services",
            View::Advanced => "advanced",
        }
    }
}

/// One application message.
#[derive(Clone)]
pub(crate) enum Msg {
    Tick,
    Refresh,
    Quit,
    ToggleCompact,
    Resized,
    Show(View),
    Help,
}

/// What the services marker has said since the last switch to the tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reported {
    Nothing,
    Missing,
    Complete,
}

struct Sysmon {
    widgets: Widgets,
    load: Load,
    backend: Rc<LazyOSBackend>,
    view: View,
    refreshes: u64,
    reported: Reported,
    /// The last full-size window, restored when leaving compact mode.
    full_size: (i32, i32),
}

impl Sysmon {
    /// Reads the snapshot (and, while the Services tab is shown, `init` and
    /// `healthd`) and shows it. The snapshot is read whichever tab is shown:
    /// the compact meters and the status bar use it.
    fn reload(&mut self, ui: &Ui<Msg>) -> Option<i64> {
        if self.view == View::Services {
            let services = services::fetch();
            self.widgets.show_services(ui, &services);
            self.report_services(&services);
        }
        match sysinfo::snapshot() {
            Ok(snapshot) => {
                let (cpu, programs) = self.load.update(&snapshot);
                self.widgets.show_snapshot(&snapshot, cpu, &programs);
                self.widgets
                    .show_status(snapshot.ticks, self.refreshes, None);
                None
            }
            Err(code) => {
                self.widgets.show_unavailable(code);
                self.widgets.show_status(0, self.refreshes, Some(code));
                Some(code)
            }
        }
    }

    /// Says once per switch whether the Services tab has both sources.
    fn report_services(&mut self, view: &Services) {
        if view.complete() {
            if self.reported != Reported::Complete {
                let (ok, degraded, down) = view.counts();
                println!(
                    "SYSMON:SERVICES:PASS services={} ok={ok} degraded={degraded} down={down}",
                    view.rows.len()
                );
                self.reported = Reported::Complete;
            }
        } else if self.reported == Reported::Nothing {
            println!(
                "SYSMON:SERVICES:NONE:init={} healthd={}",
                view.init_error.unwrap_or(0),
                view.health_error.unwrap_or(0)
            );
            self.reported = Reported::Missing;
        }
    }
}

impl App for Sysmon {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick | Msg::Refresh => {
                self.refreshes += 1;
                let error = self.reload(ui);
                if matches!(msg, Msg::Refresh) {
                    println!("SYSMON:REFRESH:PASS");
                }
                if let Some(code) = error {
                    println!("SYSMON:SNAPSHOT:FAIL:{code}");
                }
            }
            Msg::Show(view) => {
                if self.view != view {
                    self.view = view;
                    self.reported = Reported::Nothing;
                    self.widgets.tabs.get().select(view.index());
                    self.reload(ui);
                }
                println!("SYSMON:VIEW:{}", view.marker());
            }
            Msg::Help => match launcher::open_path(&help_path()) {
                Ok(()) => println!("SYSMON:HELP:PASS"),
                Err(error) => {
                    println!("SYSMON:HELP:FAIL:{error}");
                    self.widgets
                        .status
                        .get()
                        .set_text(3, &format!("Could not open the guide: {error}"));
                }
            },
            Msg::Quit => {
                println!("SYSMON:QUIT:PASS");
                ui.quit();
            }
            Msg::ToggleCompact => {
                let rect = hidpi::design_rect(ui);
                let (w, h) = compact::toggle_target(
                    (rect.width(), rect.height()),
                    self.full_size,
                    (WINDOW.0 as u32, WINDOW.1 as u32),
                );
                self.backend.request_size(w, h);
            }
            Msg::Resized => {
                let design = hidpi::design_rect(ui);
                let small = compact::is_compact(design.width(), design.height());
                if !small {
                    self.full_size = (design.width(), design.height());
                }
                self.widgets.set_compact(ui, small);
                let rect = ui.client_rect();
                println!("SYSMON:SIZE:{}x{}", rect.width(), rect.height());
            }
        }
        // Longer values change the labels' natural sizes.
        ui.relayout();
    }
}

/// The keys the dashboard answers wherever the focus is.
fn shortcut(key: Key) -> Option<Msg> {
    Some(match key {
        Key::R => Msg::Refresh,
        Key::Q => Msg::Quit,
        Key::C => Msg::ToggleCompact,
        Key::O => Msg::Show(View::Overview),
        Key::S => Msg::Show(View::Services),
        Key::A => Msg::Show(View::Advanced),
        Key::F1 => Msg::Help,
        _ => return None,
    })
}

/// The first-frame evidence for the snapshot read at start-up.
fn evidence(snapshot: &Result<Snapshot, i64>) -> String {
    match snapshot {
        Ok(s) => format!(
            "SYSMON:UP:PASS\nSYSMON:DATA:tasks={} frames_free={} frames_live={} slab_live={} heap_used={} cache_frames={}",
            s.tasks_live, s.frames_free, s.frames_live, s.slab_live, s.heap_used, s.cache_frames
        ),
        Err(code) => format!("SYSMON:UP:FAIL:{code}"),
    }
}

fn main() {
    launch::run("SYSMON", "System Monitor", WINDOW, |ui, backend| {
        // Resizable, down to the compact view, so `RequestSize` is accepted.
        backend.set_size_hints(compact::MIN_SIZE.0, compact::MIN_SIZE.1, 0, 0);
        let first = evidence(&sysinfo::snapshot());
        backend.on_first_frame(move || println!("{first}"));

        let widgets = Widgets::new();
        ui.root(widgets.layout())?;
        ui.on_key(|key, _| shortcut(key));
        ui.on_close(|| Some(Msg::Quit));
        ui.register_events(WidgetId::NONE, |event| {
            matches!(event, Event::Resize { .. }).then_some(Msg::Resized)
        });
        ui.every(REFRESH_MILLIS, Msg::Tick);

        let mut app = Sysmon {
            widgets,
            load: Load::default(),
            backend: Rc::clone(backend),
            view: View::Overview,
            refreshes: 0,
            reported: Reported::Nothing,
            full_size: WINDOW,
        };
        app.reload(ui);
        let design = hidpi::design_rect(ui);
        app.widgets
            .set_compact(ui, compact::is_compact(design.width(), design.height()));
        Ok(app)
    })
}
