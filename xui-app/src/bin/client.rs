//! `xui-client`: the xui Counter + Edit running as a `xuid` compositor client
//! (issue #168).
//!
//! Unlike the M0-M2 milestones (`counter`, `sysmon`, `fabricmon`), this binary
//! never binds the display grant: `LazyOSBackend::new_client` resolves `xuid`,
//! the app creates a surface with `os.lazy.display.v1`, attaches a shared
//! pixel buffer, commits damage rectangles, and receives pointer/key/close
//! events. The compositor provides the window chrome, drag, taskbar, minimize
//! and close.
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
//! Serial evidence: `XUIAPP:CLIENT:PASS` after the first commit.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::themed::run_themed;
use xui_core::app::{App, Ui};
use xui_core::backend::PlatformSpec;
use xui_core::{Button, Dip, Edit, HasText, Label, Rect};

/// Surface size in pixels; the compositor places and decorates it.
const W: i32 = 560;
const H: i32 = 360;

/// One application message.
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
    _edit: Edit<Msg>,
    status: Label<Msg>,
    count: i32,
    /// Whether the keyboard-focus evidence line was already printed.
    key_pass: bool,
    // Kept alive for the lifetime of the app, so the node and its mappers stay
    // registered.
    _hint: Label<Msg>,
    _button: Button<Msg>,
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
                self.status.set_text(&format!("{} clicks", self.count));
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

fn main() {
    let backend = match LazyOSBackend::new_client() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("XUIAPP:CLIENT:FAIL:{code}");
            std::process::exit(1);
        }
    };
    backend.on_first_frame(|| println!("XUIAPP:CLIENT:PASS"));

    let spec = PlatformSpec::new("xui-client").size(Dip(W as f32), Dip(H as f32));
    let outcome = run_themed(&backend, spec, |ui| {
        let hint = Label::new(
            ui,
            Rect::new(16, 8, 544, 32),
            "click the field, then type - click the button, then space",
        )
        .expect("hint");
        let edit = Edit::new(ui, Rect::new(16, 40, 420, 76), "")
            .expect("edit")
            .on_change(|value| Some(Msg::Edit(value.to_string())));
        let button = Button::new(ui, Rect::new(16, 96, 200, 132), "Click me")
            .expect("button")
            .on_click(|| Some(Msg::Bump));
        let status = Label::new(ui, Rect::new(16, 152, 544, 200), "0 clicks").expect("status");
        ui.on_close(|| Some(Msg::Close));
        ClientApp {
            _edit: edit,
            status,
            count: 0,
            key_pass: false,
            _hint: hint,
            _button: button,
        }
    });

    backend.unbind();
    match outcome {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            println!("XUIAPP:RUN:FAIL:{error}");
            std::process::exit(1);
        }
    }
}
