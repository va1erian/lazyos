//! `devices`: who holds which device, and the rules that confine the drivers
//! (issue #481).
//!
//! Standard xui widgets laid out without coordinates: three stacked group
//! boxes, each a heading over a list view — every device with its class, PCI
//! ids, owner and the rights it was granted; the driver class rules the kernel
//! installed at boot, grouped per driver; and the claims the kernel refused
//! (shown to `CAP_AUDIT_READ` holders, otherwise the section says so). A
//! status bar carries the refresh count. Read-only on purpose: the rules are
//! compiled into the kernel, so there is nothing to edit. A two-second timer
//! refreshes; `r` refreshes now and `q` quits.
//!
//! Serial evidence: `DEVICES:UP:PASS devices=<n> rules=<n> denials=<n|-errno>`
//! after the first frame (`DEVICES:UP:FAIL:<errno>` when the inventory is
//! unreadable), `DEVICES:REFRESH:PASS` on `r`, `DEVICES:QUIT:PASS` on `q` (or
//! the window close button).

use xui_app::devinfo::DevView;
use xui_app::trace::{self, Channel};
use xui_core::app::{App, Ui};
use xui_core::Key;

#[path = "devices/view.rs"]
mod view;

use view::{Table, Widgets};

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (900, 640);
/// How often the view refreshes.
const REFRESH_MILLIS: u32 = 2000;

/// One application message.
#[derive(Clone)]
pub(crate) enum Msg {
    Tick,
    Refresh,
    /// A header click on one of the tables.
    Sort(Table),
    Quit,
}

struct Devices {
    widgets: Widgets,
    refreshes: u64,
}

impl App for Devices {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick | Msg::Refresh => {
                self.refreshes += 1;
                self.widgets.show(&DevView::read(), self.refreshes);
                if matches!(msg, Msg::Refresh) {
                    println!("DEVICES:REFRESH:PASS");
                }
            }
            Msg::Sort(table) => self.widgets.resort(table),
            Msg::Quit => ui.quit(),
        }
        // Longer headings change the labels' natural sizes.
        ui.relayout();
    }
}

/// The keys the window answers wherever the focus is.
fn shortcut(key: Key) -> Option<Msg> {
    Some(match key {
        Key::R => Msg::Refresh,
        Key::Q => Msg::Quit,
        _ => return None,
    })
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
    format!("UP:PASS devices={devices} rules={rules} denials={denials}")
}

fn main() {
    xui_app::app(Channel::Devices, "Devices")
        .size(WINDOW.0, WINDOW.1)
        .min_size(560, 360)
        .run(|ui, backend| {
            let first = DevView::read();
            let evidence = up_line(&first);
            // The first-frame line carries what was read, so it is the app's.
            backend.on_first_frame(move || trace::line!(Channel::Devices, "{evidence}"));

            let widgets = Widgets::default();
            ui.root(widgets.layout())?;
            ui.on_key(|key, _| shortcut(key));
            ui.on_close(|| Some(Msg::Quit));
            ui.every(REFRESH_MILLIS, Msg::Tick);

            let app = Devices {
                widgets,
                refreshes: 0,
            };
            app.widgets.show(&first, app.refreshes);
            Ok(app)
        })
}
