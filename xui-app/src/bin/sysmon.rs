//! `sysmon`: a windowed dashboard over the native system-stats snapshot
//! (syscall 14, issue #144).
//!
//! The whole window is one owner-drawn node: a header and two tabs. Overview
//! has three memory gauges (frames, slab, kernel heap), the task table (pid,
//! state, class, CPU ticks, name) and a footer with uptime; Services (issue
//! #489) lists the services `init` supervises with the health `healthd`
//! retains for each. A one-second `ui` timer refreshes whichever tab is shown;
//! `o`/`s` (or a click on a tab) switch tabs, Up/Down, PageUp/PageDown,
//! Home/End and the wheel scroll the services table, `r` refreshes
//! immediately and `q` quits. Text uses the bundled Droid Sans through the backend's font.
//!
//! Serial evidence: `SYSMON:UP:PASS` after the first frame (or
//! `SYSMON:UP:FAIL:<errno>` when the snapshot is unreadable),
//! `SYSMON:REFRESH:PASS` on `r`, `SYSMON:VIEW:<overview|services>` on a tab
//! switch, `SYSMON:SERVICES:PASS services=<n> ok=<n> degraded=<n> down=<n>`
//! the first time the Services tab shows both sources after a switch to it
//! (`SYSMON:SERVICES:NONE:init=<errno> healthd=<errno>` once when it
//! cannot yet), `SYSMON:SCROLL:<first row>` when the services table moves,
//! `SYSMON:SIZE:<w>x<h>` after every resize (`c` or
//! the chip toggles the compact view by asking the compositor for a size), `SYSMON:QUIT:PASS` on `q` (or the window close button), and a
//! `SYSMON:DATA:...` line with the headline counters.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::compact;
use xui_app::services::{self, Services};
use xui_app::sysinfo::{self, Snapshot};
use xui_app::themed::run_themed;
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec, WidgetId};
use xui_core::message::Key;
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
/// Services-table rows one wheel notch scrolls.
const WHEEL_ROWS: i64 = 3;
/// The wheel delta of one notch.
const WHEEL_NOTCH: i64 = 120;

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
    /// The user scrolled the services table.
    Scroll(Step),
}

/// How far a scroll moves the services table.
#[derive(Clone, Copy)]
enum Step {
    Rows(i64),
    Pages(i64),
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
    /// The first services row shown.
    scroll: usize,
    /// How many services rows the last paint fitted, so a page scroll and
    /// the scroll bounds follow the window size.
    page: Cell<usize>,
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
            scroll: 0,
            page: Cell::new(1),
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
            // A refresh can shrink the table; keep the offset on a real row.
            self.scroll_by(Step::Rows(0));
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
    /// Move the services table, clamped to its rows.
    fn scroll_by(&mut self, step: Step) {
        let page = self.page.get().max(1);
        let delta = match step {
            Step::Rows(rows) => rows,
            Step::Pages(pages) => pages.saturating_mul(page as i64),
        };
        let len = self.services.as_ref().map_or(0, |view| view.rows.len());
        self.scroll = services::scroll(self.scroll, delta, len, page);
    }

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
            Msg::Scroll(step) => {
                let mut state = self.state.borrow_mut();
                if state.view != View::Services {
                    return;
                }
                let before = state.scroll;
                state.scroll_by(step);
                let after = state.scroll;
                drop(state);
                if after != before {
                    ui.invalidate(self.root.id());
                    println!("SYSMON:SCROLL:{after}");
                }
            }
            Msg::Quit => {
                println!("SYSMON:QUIT:PASS");
                ui.quit();
            }
            Msg::ToggleCompact => {
                let rect = xui_app::hidpi::design_rect(ui);
                let current = (rect.width(), rect.height());
                let (w, h) = compact::toggle_target(
                    current,
                    self.full_size,
                    (WINDOW.0 as u32, WINDOW.1 as u32),
                );
                self.backend.request_size(w, h);
            }
            Msg::Resized => {
                // Follow the new client area; remember the last full size
                // (in design pixels, like every size the dashboard reasons in).
                let rect = ui.client_rect();
                let design = xui_app::hidpi::design_rect(ui);
                if !compact::is_compact(design.width(), design.height()) {
                    self.full_size = (design.width(), design.height());
                }
                ui.apply_moves(&[(self.root.id(), rect)]);
                ui.invalidate(self.root.id());
                println!("SYSMON:SIZE:{}x{}", rect.width(), rect.height());
            }
        }
    }
}

/// The scroll a navigation key asks for.
fn scroll_key(key: Key) -> Option<Step> {
    match key {
        Key::UP => Some(Step::Rows(-1)),
        Key::DOWN => Some(Step::Rows(1)),
        Key::PAGE_UP => Some(Step::Pages(-1)),
        Key::PAGE_DOWN => Some(Step::Pages(1)),
        Key::HOME => Some(Step::Rows(i64::MIN)),
        Key::END => Some(Step::Rows(i64::MAX)),
        _ => None,
    }
}

/// The scroll a wheel delta asks for: a positive delta is away from the user,
/// which shows earlier rows. A partial notch still moves one row.
fn wheel_step(delta: i16) -> Step {
    let rows = if delta > 0 {
        -(i64::from(delta) * WHEEL_ROWS / WHEEL_NOTCH).max(1)
    } else if delta < 0 {
        (i64::from(-delta) * WHEEL_ROWS / WHEEL_NOTCH).max(1)
    } else {
        0
    };
    Step::Rows(rows)
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
    let outcome = run_themed(&backend, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let state = Rc::clone(&state);
            let theme = ui.theme_handle();
            root.set_painter(Rc::new(move |canvas| {
                paint(canvas, theme.get(), &state.borrow())
            }));
        }
        let ui_probe = ui.clone();
        root.on_events(move |event| match event {
            Event::Char('r') => Some(Msg::Refresh),
            Event::Char('q') => Some(Msg::Quit),
            Event::Char('c') => Some(Msg::ToggleCompact),
            Event::Char('o') => Some(Msg::Show(View::Overview)),
            Event::Char('s') => Some(Msg::Show(View::Services)),
            Event::KeyDown { key, .. } => scroll_key(*key).map(Msg::Scroll),
            Event::MouseWheel {
                delta,
                horizontal: false,
                ..
            } => Some(Msg::Scroll(wheel_step(*delta))),
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } if {
                let (x, y) = xui_app::hidpi::design_point(&ui_probe, *x, *y);
                compact::hit_chip(xui_app::hidpi::design_rect(&ui_probe), x, y)
            } =>
            {
                Some(Msg::ToggleCompact)
            }
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                ..
            } => {
                let rect = xui_app::hidpi::design_rect(&ui_probe);
                let (x, y) = xui_app::hidpi::design_point(&ui_probe, *x, *y);
                if compact::is_compact(rect.width(), rect.height()) {
                    None
                } else {
                    services_view::hit_tab(rect, x, y).map(Msg::Show)
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
