//! `xui-client`: the xui Counter + Edit running as a `xuid` compositor client
//! (issue #168).
//!
//! Unlike the M0-M2 milestones (`counter`, `sysmon`, `fabricmon`), this binary
//! never binds the display grant: `LazyOSBackend::new_client` resolves `xuid`,
//! the app creates a surface with `os.lazy.display.v1`, attaches a shared
//! pixel buffer, commits damage rectangles, and receives pointer/key/close
//! events. The compositor provides the window chrome, drag, taskbar, minimize
//! and close. (So it does not start through `launch::run`, which tries the
//! grant first: the kernel boots `xuid` and this app side by side.)
//!
//! The window holds a text field and a Counter button, so the session proves
//! the keyboard-focus routing of issue #151:
//!
//! * clicking the field focuses it; typing then reaches it even after the
//!   pointer moves away (`XUIAPP:TEXT:…`, `XUIAPP:KEY:PASS`);
//! * clicking the Counter focuses it; Space/Enter activate it through the
//!   widget's own key handling (`XUIAPP:COUNTER:…`);
//! * `PageDown`/`PageUp` cycle the widget focus (`xuid` reserves `Tab`);
//! * the WM close button arrives as `WindowClose` (`XUIAPP:CLOSE:PASS`).
//!
//! Serial evidence: `XUIAPP:CLIENT:PASS` after the first commit,
//! `XUIAPP:CLIENT:FAIL:<errno>` without a compositor and
//! `XUIAPP:RUN:FAIL:<error>` when the window cannot be built.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_core::app::{App, Ui};
use xui_core::backend::Backend;
use xui_core::prelude::*;

/// Surface size in design pixels; the compositor places and decorates it.
const W: i32 = 560;
const H: i32 = 360;

/// One application message.
#[derive(Clone)]
enum Msg {
    /// The Counter was activated (mouse click or the focused button's key).
    Bump,
    /// The text field changed.
    Edit(String),
    /// The compositor asked the window to close.
    Close,
}

/// The demo app.
struct ClientApp {
    status: Handle<Label<Msg>>,
    count: i32,
    /// Whether the keyboard-focus evidence line was already printed.
    key_pass: bool,
}

impl ClientApp {
    /// Print the once-only marker that a key reached a focused widget.
    fn key_pass(&mut self) {
        if !self.key_pass {
            self.key_pass = true;
            println!("XUIAPP:KEY:PASS");
        }
    }
}

impl App for ClientApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Bump => {
                self.count += 1;
                self.status
                    .get()
                    .set_text(&format!("{} clicks", self.count));
                println!("XUIAPP:COUNTER:{}", self.count);
            }
            Msg::Edit(value) => {
                println!("XUIAPP:TEXT:{value}");
                self.key_pass();
            }
            Msg::Close => {
                println!("XUIAPP:CLOSE:PASS");
                ui.quit();
            }
        }
    }
}

/// The hint, the field, the Counter and its count, top to bottom. The sizes
/// keep the field and the button where the `xui_client` session clicks.
fn layout(status: &Handle<Label<Msg>>) -> Layout<Msg> {
    column()
        .padding(Insets::new(Dip(16.0), Dip(8.0), Dip(16.0), Dip(8.0)))
        .gap(20)
        .children((
            column().gap(8).children((
                label("click the field, then type - click the button, then space").fixed(24),
                edit().on_change(Msg::Edit).fixed(36).max_width(404),
            )),
            button("Click me")
                .on_click(Msg::Bump)
                .fixed(36)
                .max_width(184),
            label("0 clicks").bind(status).fixed(48),
        ))
}

fn main() {
    let backend = match LazyOSBackend::new_client() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("XUIAPP:CLIENT:FAIL:{code}");
            std::process::exit(1);
        }
    };
    backend.on_first_frame(|| println!("XUIAPP:CLIENT:PASS"));

    let outcome = xui_core::app("xui-client")
        .size(W, H)
        .backend(Rc::clone(&backend) as Rc<dyn Backend>)
        .run(|ui| {
            let status = Handle::new();
            ui.root(layout(&status))?;
            ui.on_close(|| Some(Msg::Close));
            Ok(ClientApp {
                status,
                count: 0,
                key_pass: false,
            })
        });

    backend.unbind();
    std::process::exit(i32::from(xui_app::launch::finish("XUIAPP", outcome)))
}
