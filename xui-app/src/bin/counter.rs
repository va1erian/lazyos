//! The Counter milestone (M1 + M2): a `xui-core` app painted by `xui-canvas`
//! and presented through LazyOS's display grant, driven by real kernel input.
//!
//! M1 evidence: `XUIAPP:COUNTER:PASS` after the first frame is on screen.
//! M2 evidence: a left click on the button raises `Msg::Bump` and the app
//! prints `XUIAPP:INPUT:PASS` (and the new count) on the first click.
//!
//! The button sits over the kernel's initial pointer position (400, 300), so a
//! headless click needs no pointer movement.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::{Button, Dip, HasText, Label, Rect};

/// The kernel seeds the pointer at (400, 300) when the display is bound.
const POINTER_SEED: (i32, i32) = (400, 300);

/// The window size when a compositor lays the app out (issue #215); as the
/// display owner it fills the screen instead.
const WINDOW: (i32, i32) = (400, 260);

enum Msg {
    Bump,
    /// The compositor asked the window to close (client mode).
    Close,
}

struct Counter {
    label: Label<Msg>,
    count: i32,
    // Kept alive for the lifetime of the app, so the node and its click mapper
    // stay registered.
    _button: Button<Msg>,
}

impl App for Counter {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Close => {
                println!("XUIAPP:CLOSE:PASS");
                ui.quit();
            }
            Msg::Bump => {
                self.count += 1;
                self.label.set_text(&format!("{} clicks", self.count));
                println!("XUIAPP:CLICK:{}", self.count);
                if self.count == 1 {
                    println!("XUIAPP:INPUT:PASS");
                }
            }
        }
    }
}

fn main() {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("XUIAPP:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    let is_client = backend.is_client();
    backend.on_first_frame(|| println!("XUIAPP:COUNTER:PASS"));

    let spec = PlatformSpec::new("xui counter").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        let area = ui.client_rect();
        // Owner mode centres on the kernel's pointer seed so a headless click
        // needs no movement; a client window centres on its own area.
        let seed = if is_client {
            (area.width() / 2, area.height() / 2)
        } else {
            POINTER_SEED
        };
        let cx = seed.0.min((area.width() - 8).max(0));
        let cy = seed.1.min((area.height() - 8).max(0));
        let label = Label::new(
            ui,
            Rect::new(cx - 140, cy - 100, cx + 140, cy - 40),
            "0 clicks",
        )
        .expect("label");
        let button = Button::new(
            ui,
            Rect::new(cx - 100, cy - 32, cx + 100, cy + 32),
            "Click me",
        )
        .expect("button")
        .on_click(|| Some(Msg::Bump));
        ui.on_close(|| Some(Msg::Close));
        Counter {
            label,
            count: 0,
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
