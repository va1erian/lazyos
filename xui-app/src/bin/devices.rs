//! `devices`: who holds which device, and the rules that confine the drivers
//! (issue #481).
//!
//! One owner-drawn page with three sections: every device with its class, PCI
//! ids, owner and the rights it was granted; the driver class rules the kernel
//! installed at boot, grouped per driver; and the claims the kernel refused
//! (shown to `CAP_AUDIT_READ` holders, otherwise the page says so). Read-only
//! on purpose: the rules are compiled into the kernel, so there is nothing to
//! edit. A two-second timer refreshes; `r` refreshes now and `q` quits.
//!
//! Serial evidence: `DEVICES:UP:PASS devices=<n> rules=<n> denials=<n|-errno>`
//! after the first frame (`DEVICES:UP:FAIL:<errno>` when the inventory is
//! unreadable), `DEVICES:REFRESH:PASS` on `r`, `DEVICES:QUIT:PASS` on `q`.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::devinfo::DevView;
use xui_app::themed::run_themed;
use xui_core::app::{App, Ui};
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec, WidgetId};
use xui_core::Control;

#[path = "devices/render.rs"]
mod render;

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (900, 640);
/// How often the view refreshes.
const REFRESH_MILLIS: u32 = 2000;

enum Msg {
    Tick,
    Refresh,
    Quit,
    Resized,
}

/// The last read and how many refreshes ran, shared with the painter.
struct State {
    view: DevView,
    refreshes: u64,
}

struct Devices {
    state: Rc<RefCell<State>>,
    root: Control<Msg>,
}

impl App for Devices {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick | Msg::Refresh => {
                let mut state = self.state.borrow_mut();
                state.view = DevView::read();
                state.refreshes += 1;
                drop(state);
                ui.invalidate(self.root.id());
                if matches!(msg, Msg::Refresh) {
                    println!("DEVICES:REFRESH:PASS");
                }
            }
            Msg::Quit => {
                println!("DEVICES:QUIT:PASS");
                ui.quit();
            }
            Msg::Resized => {
                let rect = ui.client_rect();
                ui.apply_moves(&[(self.root.id(), rect)]);
                ui.invalidate(self.root.id());
            }
        }
    }
}

/// The `DEVICES:UP` evidence line for the first frame.
fn up_line(view: &DevView) -> String {
    let devices = match &view.devices {
        Ok(devices) => devices.len(),
        Err(code) => return format!("DEVICES:UP:FAIL:{code}"),
    };
    let rules = match &view.rules {
        Ok(Some(rules)) => rules.len().to_string(),
        Ok(None) => String::from("none"),
        Err(code) => format!("-{code}"),
    };
    let denials = match &view.denials {
        Ok(denials) => denials.len().to_string(),
        Err(code) => format!("-{code}"),
    };
    format!("DEVICES:UP:PASS devices={devices} rules={rules} denials={denials}")
}

fn main() {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("DEVICES:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    let state = Rc::new(RefCell::new(State {
        view: DevView::read(),
        refreshes: 0,
    }));
    backend.set_size_hints(560, 360, 0, 0);
    {
        let state = Rc::clone(&state);
        backend.on_first_frame(move || println!("{}", up_line(&state.borrow().view)));
    }

    let spec = PlatformSpec::new("Devices")
        .size(xui_core::Dip(width as f32), xui_core::Dip(height as f32));
    let outcome = run_themed(&backend, spec, |ui| {
        let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
            .expect("root node");
        {
            let state = Rc::clone(&state);
            let theme = ui.theme_handle();
            root.set_painter(Rc::new(move |canvas| {
                render::paint(canvas, theme.get(), &state.borrow())
            }));
        }
        root.on_events(|event| match event {
            Event::Char('r') => Some(Msg::Refresh),
            Event::Char('q') => Some(Msg::Quit),
            _ => None,
        });
        ui.register_events(WidgetId::NONE, |event| match event {
            Event::Resize { .. } => Some(Msg::Resized),
            _ => None,
        });
        root.focus();
        ui.on_timer(|_| Some(Msg::Tick));
        ui.on_close(|| Some(Msg::Quit));
        ui.set_timer(REFRESH_MILLIS);
        Devices { state, root }
    });

    backend.unbind();
    match outcome {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            println!("DEVICES:RUN:FAIL:{error}");
            std::process::exit(1);
        }
    }
}
