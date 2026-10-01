//! `sysmon`: a windowed dashboard over the native system-stats snapshot
//! (syscall 14, issue #144).
//!
//! The whole window is one owner-drawn node: a header and two tabs. Overview
//! has three memory gauges (frames, slab, kernel heap), the task table (pid,
//! state, class, CPU ticks, name) and a footer with uptime; Services (issue
//! #489) lists the services `init` supervises with the health `healthd`
//! retains for each. A one-second `ui` timer refreshes whichever tab is shown;
//! `o`/`s` (or a click on a tab) switch tabs, `r` refreshes immediately and `q`
//! quits. Text uses the bundled Droid Sans through the backend's font.
//!
//! Serial evidence: `SYSMON:UP:PASS` after the first frame (or
//! `SYSMON:UP:FAIL:<errno>` when the snapshot is unreadable),
//! `SYSMON:REFRESH:PASS` on `r`, `SYSMON:VIEW:<overview|services>` on a tab
//! switch, `SYSMON:SERVICES:PASS services=<n> ok=<n> degraded=<n> down=<n>`
//! the first time the Services tab shows both sources after a switch to it
//! (`SYSMON:SERVICES:NONE:init=<errno> healthd=<errno>` once when it
//! cannot yet), `SYSMON:SIZE:<w>x<h>` after every resize (`c` or
//! the chip toggles the compact view by asking the compositor for a size), `SYSMON:QUIT:PASS` on `q` (or the window close button), and a
//! `SYSMON:DATA:...` line with the headline counters.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::compact;
use xui_app::services::{self, Services};
use xui_app::sysinfo::{self, Snapshot};
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, Event, NodeKind, NodeSpec, PlatformSpec, WidgetId};
use xui_core::{Control, MouseButton};

#[path = "sysmon/compact.rs"]
mod compact_view;
#[path = "sysmon/render.rs"]
mod render;
#[path = "sysmon/services_view.rs"]
mod services_view;

use render::paint;
use services_view::View;

/// The window size when a compositor lays the app out (issue #215); as the
/// display owner it fills the screen instead.
const WINDOW: (i32, i32) = (860, 600);

/// The memory-card height when the window is tall enough: four value lines.
const CARD_H: i32 = 164;
/// The smallest the cards shrink to before their value lines would clip.
const CARD_MIN_H: i32 = 158;
/// Horizontal gap between the three memory cards.
const CARD_GAP: i32 = 16;
/// Vertical gap between the memory cards and the task table.
const TABLE_GAP: i32 = 20;
/// Height reserved for the footer line.
const FOOTER_H: i32 = 26;

/// How often the snapshot refreshes.
const REFRESH_MILLIS: u32 = 1000;

/// One application message.
enum Msg {
    /// The one-second timer fired.
    Tick,
    /// The user pressed `r`.
    Refresh,
    /// The user pressed `q`.
    Quit,
    /// The user pressed `c` or clicked the chip.
    ToggleCompact,
    /// The compositor resized the window.
    Resized,
    /// The user picked a tab (key or click).
    Show(View),
}

/// What the services marker has said since the last switch to the tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reported {
    Nothing,
    Missing,
    Complete,
}

/// The decoded snapshot and the last error, shared with the painter.
struct State {
    snapshot: Option<Snapshot>,
    error: Option<i64>,
    refreshes: u64,
    /// The tab shown.
    view: View,
    /// The last services refresh; only read while the Services tab is shown.
    services: Option<Services>,
    reported: Reported,
}

impl State {
    /// Read the snapshot now.
    fn load() -> State {
        let mut state = State {
            snapshot: None,
            error: None,
            refreshes: 0,
            view: View::Overview,
            services: None,
            reported: Reported::Nothing,
        };
        state.reload();
        state
    }

    /// Read the snapshot (kept on failure, with the errno, so the page can
    /// show it) and, while the Services tab is shown, call `init` and
    /// `healthd`. The snapshot is always read: the compact view shows it
    /// whichever tab is selected.
    fn reload(&mut self) {
        if self.view == View::Services {
            self.services = Some(services::fetch());
            self.report_services();
        }
        match sysinfo::snapshot() {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(code) => self.error = Some(code),
        }
    }
}

impl State {
    /// Say once per switch whether the Services tab has both sources.
    fn report_services(&mut self) {
        let Some(view) = &self.services else {
            return;
        };
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

/// The dashboard app: one owner-drawn node plus its state.
struct Sysmon {
    state: Rc<RefCell<State>>,
    root: Control<Msg>,
    backend: Rc<LazyOSBackend>,
    /// The last full-size window, restored when leaving compact mode.
    full_size: (i32, i32),
}

impl App for Sysmon {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick | Msg::Refresh => {
                let mut state = self.state.borrow_mut();
                state.reload();
                state.refreshes += 1;
                let error = state.error;
                drop(state);
                ui.invalidate(self.root.id());
                if matches!(msg, Msg::Refresh) {
                    println!("SYSMON:REFRESH:PASS");
                }
                if let Some(code) = error {
                    println!("SYSMON:SNAPSHOT:FAIL:{code}");
                }
            }
            Msg::Show(view) => {
                let mut state = self.state.borrow_mut();
                if state.view != view {
                    state.view = view;
                    state.reported = Reported::Nothing;
                    state.reload();
                }
                drop(state);
                ui.invalidate(self.root.id());
                println!("SYSMON:VIEW:{}", view.marker());
            }
            Msg::Quit => {
                println!("SYSMON:QUIT:PASS");
                ui.quit();
            }
            Msg::ToggleCompact => {
                let rect = ui.client_rect();
                let current = (rect.width(), rect.height());
                let (w, h) = compact::toggle_target(
                    current,
                    self.full_size,
                    (WINDOW.0 as u32, WINDOW.1 as u32),
                );
                self.backend.request_size(w, h);
            }
            Msg::Resized => {
                // Follow the new client area; remember the last full size.
                let rect = ui.client_rect();
                if !compact::is_compact(rect.width(), rect.height()) {
                    self.full_size = (rect.width(), rect.height());
                }
                ui.apply_moves(&[(self.root.id(), rect)]);
                ui.invalidate(self.root.id());
                println!("SYSMON:SIZE:{}x{}", rect.width(), rect.height());
            }
        }
    }
}

fn main() {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("SYSMON:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    let state = Rc::new(RefCell::new(State::load()));
    // Resizable, down to the compact view, so `RequestSize` is accepted.
    backend.set_size_hints(compact::MIN_SIZE.0, compact::MIN_SIZE.1, 0, 0);

    {
        let state = Rc::clone(&state);
        backend.on_first_frame(move || {
            let state = state.borrow();
            match state.snapshot {
                Some(snapshot) => {
                    println!("SYSMON:UP:PASS");
                    println!(
                        "SYSMON:DATA:tasks={} frames_free={} frames_live={} slab_live={} heap_used={}",
                        snapshot.tasks_live,
                        snapshot.frames_free,
                        snapshot.frames_live,
                        snapshot.slab_live,
                        snapshot.heap_used
                    );
                }
                None => println!("SYSMON:UP:FAIL:{}", state.error.unwrap_or(-22)),
            }
        });
    }

    let spec =
        PlatformSpec::new("sysmon").size(xui_core::Dip(width as f32), xui_core::Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let state = Rc::clone(&state);
            root.set_painter(Rc::new(move |canvas| paint(canvas, &state.borrow())));
        }
        let ui_probe = ui.clone();
        root.on_events(move |event| match event {
            Event::Char('r') => Some(Msg::Refresh),
            Event::Char('q') => Some(Msg::Quit),
            Event::Char('c') => Some(Msg::ToggleCompact),
            Event::Char('o') => Some(Msg::Show(View::Overview)),
            Event::Char('s') => Some(Msg::Show(View::Services)),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } if compact::hit_chip(ui_probe.client_rect(), *x, *y) => Some(Msg::ToggleCompact),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => {
                let rect = ui_probe.client_rect();
                if compact::is_compact(rect.width(), rect.height()) {
                    None
                } else {
                    services_view::hit_tab(rect, *x, *y).map(Msg::Show)
                }
            }
            _ => None,
        });
        // The window-level resize maps to a message so the re-flow runs in
        // `update`, outside the event dispatch (as Paint does).
        ui.register_events(WidgetId::NONE, |event| match event {
            Event::Resize { .. } => Some(Msg::Resized),
            _ => None,
        });
        root.focus();
        ui.on_timer(|_| Some(Msg::Tick));
        ui.on_close(|| Some(Msg::Quit));
        ui.set_timer(REFRESH_MILLIS);
        Sysmon {
            state,
            root,
            backend: Rc::clone(&backend),
            full_size: WINDOW,
        }
    });

    backend.unbind();
    match outcome {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            println!("SYSMON:RUN:FAIL:{error}");
            std::process::exit(1);
        }
    }
}
