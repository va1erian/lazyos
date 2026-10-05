//! The Counter milestone (M1 + M2): a `xui-core` app painted by `xui-canvas`
//! and presented through LazyOS's display grant, driven by real kernel input.
//!
//! M1 evidence: `XUIAPP:COUNTER:PASS` after the first frame is on screen.
//! M2 evidence: a left click on the button raises `Msg::Bump` and the app
//! prints `XUIAPP:INPUT:PASS` (and the new count) on the first click.
//!
//! As the display owner the button sits over the kernel's initial pointer
//! position (400, 300), so a headless click needs no pointer movement; a
//! client window centres it instead.

use xui_app::{hidpi, launch};
use xui_core::app::{App, Ui};
use xui_core::prelude::*;

/// The kernel seeds the pointer at (400, 300) when the display is bound.
const POINTER_SEED: (i32, i32) = (400, 300);

/// The window size when a compositor lays the app out (issue #215); as the
/// display owner it fills the screen instead.
const WINDOW: (i32, i32) = (400, 260);

/// The label's design size; the button's centre sits `BUTTON_DROP` below the
/// label's top.
const LABEL: (i32, i32) = (280, 60);
const BUTTON: (i32, i32) = (200, 64);
const GAP: i32 = 8;
const BUTTON_DROP: i32 = LABEL.1 + GAP + BUTTON.1 / 2;

#[derive(Clone)]
enum Msg {
    Bump,
    /// The compositor asked the window to close (client mode).
    Close,
}

struct Counter {
    label: Handle<Label<Msg>>,
    count: i32,
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
                self.label.get().set_text(&format!("{} clicks", self.count));
                println!("XUIAPP:CLICK:{}", self.count);
                if self.count == 1 {
                    println!("XUIAPP:INPUT:PASS");
                }
            }
        }
    }
}

/// The label over the button, centred on the window or, with `seed` (design
/// pixels), with the button centred on that point.
fn layout(
    label_handle: &Handle<Label<Msg>>,
    area: (i32, i32),
    seed: Option<(i32, i32)>,
) -> Layout<Msg> {
    let content = column().gap(GAP).align(Align::Center);
    let content = match seed {
        // The window fills the screen: inset the column so the button's
        // centre lands on the seed.
        Some((x, y)) => {
            let x = x.min(area.0 - 8).max(0);
            let y = y.min(area.1 - 8).max(0);
            content.padding(Insets::new(
                Dip(((2 * x - area.0).max(0)) as f32),
                Dip(((y - BUTTON_DROP).max(0)) as f32),
                Dip(((area.0 - 2 * x).max(0)) as f32),
                Dip(0.0),
            ))
        }
        None => content.justify(Align::Center),
    };
    content.children((
        row()
            .child(label("0 clicks").bind(label_handle).width(LABEL.0))
            .fixed(LABEL.1),
        row()
            .child(button("Click me").on_click(Msg::Bump).width(BUTTON.0))
            .fixed(BUTTON.1),
    ))
}

fn main() {
    launch::run("XUIAPP", "xui counter", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("XUIAPP:COUNTER:PASS"));
        let scale = backend.scale() as i32;
        let area = hidpi::design_rect(ui);
        let area = (area.width(), area.height());
        let seed = (!backend.is_client()).then(|| (POINTER_SEED.0 / scale, POINTER_SEED.1 / scale));

        let label = Handle::new();
        ui.root(layout(&label, area, seed))?;
        ui.on_close(|| Some(Msg::Close));
        Ok(Counter { label, count: 0 })
    })
}
