//! `traydemo` (`os.lazy.traydemo`): the tray sample app (docs/tray-plan.md
//! stages T1-T3). A resident app (`xui_app::resident`): it puts one item on
//! LazyShell's tray, keeps running with no window, closes its window to the
//! tray, opens it again from the icon or a second launch, and quits when
//! `init` asks.
//!
//! Keys (and the buttons) in its window: `L`, `P`, `B` switch the item's
//! picture (a Lucide outline, full-colour pixels at 1x and 2x, a Lucide name
//! that does not exist, which falls back to the package icon), `A` toggles
//! `Attention`, `C` clears the item (the tray then shows the default item of
//! a resident app), `Q` quits. Closing the window keeps the app in the tray.
//!
//! Test hooks for the lifecycle harness, read at start: the number of
//! milliseconds in `/tmp/traydemo-delay` delays its `Watch` (a slow start),
//! and `/tmp/traydemo-ignore-quit` makes it ignore `Quit` (so `init` kills
//! it when the grace ends).
//!
//! Serial evidence: `TRAYDEMO:UP:PASS` after the first frame,
//! `TRAYDEMO:WATCH:PASS`, `TRAYDEMO:TRAY:SET:PASS`, `TRAYDEMO:ICON:<kind>`,
//! `TRAYDEMO:ACTIVATE:PASS n=<clicks>`, `TRAYDEMO:SECONDARY:PASS`,
//! `TRAYDEMO:SCROLL:<delta>`, `TRAYDEMO:MENU:<id>:<checked>`,
//! `TRAYDEMO:CLOSED:TRAY`, `TRAYDEMO:REOPEN:PASS via=<reopen|activate>`,
//! `TRAYDEMO:CLEAR:PASS`, `TRAYDEMO:QUIT:IGNORED` and `TRAYDEMO:QUIT:PASS`.

#[path = "traydemo/demo.rs"]
mod demo;

use std::cell::RefCell;
use std::rc::Rc;

use demo::{Demo, Flow, Kind, Shared};
use xui_app::resident::Resident;
use xui_core::prelude::*;

/// The window size when a compositor lays the app out.
const WINDOW: (i32, i32) = (460, 220);
/// How often the window polls the tray and lifecycle channels (ms).
const POLL_MILLIS: u32 = 50;
/// The harness's hooks.
const DELAY_FILE: &str = "/tmp/traydemo-delay";
const IGNORE_QUIT_FILE: &str = "/tmp/traydemo-ignore-quit";
/// The longest start-up delay the hook may ask for (ms).
const MAX_DELAY_MS: u64 = 10_000;

#[derive(Clone, Copy)]
enum Msg {
    Tick,
    Icon(Kind),
    Attention,
    Clear,
    /// The window's close button: back to the tray.
    Close,
    Quit,
}

/// The window's app: a view of the shared [`Demo`].
struct Window {
    demo: Shared,
    status: Handle<Label<Msg>>,
    shown: String,
}

impl Window {
    fn refresh(&mut self) {
        let text = self.demo.borrow().status.clone();
        if text != self.shown {
            self.status.get().set_text(&text);
            self.shown = text;
        }
    }
}

impl App for Window {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                let wakes = self.demo.borrow().res.poll();
                for wake in wakes {
                    if self.demo.borrow_mut().wake(wake, true) == Flow::Quit {
                        self.demo.borrow_mut().quitting = true;
                        ui.quit();
                        return;
                    }
                }
            }
            Msg::Icon(kind) => self.demo.borrow_mut().set_icon(kind),
            Msg::Attention => self.demo.borrow_mut().toggle_attention(),
            Msg::Clear => self.demo.borrow_mut().clear_item(),
            Msg::Close => {
                println!("TRAYDEMO:CLOSED:TRAY");
                ui.quit();
                return;
            }
            Msg::Quit => {
                self.demo.borrow_mut().quitting = true;
                ui.quit();
                return;
            }
        }
        self.refresh();
    }
}

/// Build the window's view of `demo`.
fn build(demo: Shared, ui: &mut Ui<Msg>) -> xui_core::backend::Result<Window> {
    let shown = demo.borrow().status.clone();
    let app = Window {
        status: Handle::default(),
        shown,
        demo,
    };
    ui.root(
        column().padding(16).gap(8).children((
            label("Tray Demo").title(),
            label(&app.shown).bind(&app.status).fill(1),
            row().gap(8).children((
                button("Lucide (L)").on_click(Msg::Icon(Kind::Lucide)),
                button("Pixels (P)").on_click(Msg::Icon(Kind::Pixels)),
                button("Bad icon (B)").on_click(Msg::Icon(Kind::Bad)),
                button("Attention (A)").on_click(Msg::Attention),
                button("Clear (C)").on_click(Msg::Clear),
            )),
        )),
    )?;
    ui.on_key(|key, _| match key {
        Key::L => Some(Msg::Icon(Kind::Lucide)),
        Key::P => Some(Msg::Icon(Kind::Pixels)),
        Key::B => Some(Msg::Icon(Kind::Bad)),
        Key::A => Some(Msg::Attention),
        Key::C => Some(Msg::Clear),
        Key::Q => Some(Msg::Quit),
        _ => None,
    });
    ui.on_close(|| Some(Msg::Close));
    ui.on_timer(|_| Some(Msg::Tick));
    ui.set_timer(POLL_MILLIS);
    Ok(app)
}

/// The start-up delay the harness asked for, if any.
fn start_delay() -> u64 {
    std::fs::read_to_string(DELAY_FILE)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map_or(0, |ms| ms.min(MAX_DELAY_MS))
}

/// Run the window until it closes (to the tray) or the app quits; `true`
/// when the app should quit.
fn show_window(demo: &Shared, first: bool) -> bool {
    let res = Rc::clone(&demo.borrow().res);
    let shared = Rc::clone(demo);
    let outcome = res.window("Tray Demo", WINDOW, move |ui| {
        if first {
            res_first_frame(&shared);
        }
        build(shared, ui)
    });
    if let Err(error) = outcome {
        println!("TRAYDEMO:RUN:FAIL:{error}");
        return true;
    }
    demo.borrow().quitting
}

/// `TRAYDEMO:UP:PASS` once the first window has a frame.
fn res_first_frame(demo: &Shared) {
    demo.borrow()
        .res
        .backend
        .on_first_frame(|| println!("TRAYDEMO:UP:PASS"));
}

fn main() {
    let delay = start_delay();
    if delay > 0 {
        println!("TRAYDEMO:DELAY ms={delay}");
        std::thread::sleep(std::time::Duration::from_millis(delay));
    }
    let res = match Resident::connect("TRAYDEMO") {
        Ok(res) => res,
        Err(code) => {
            println!("TRAYDEMO:BIND:FAIL:{code}");
            std::process::exit(1);
        }
    };
    let demo: Shared = Rc::new(RefCell::new(Demo {
        res,
        clicks: 0,
        attention: false,
        notify: false,
        icon: Kind::Lucide,
        status: String::from("Click the icon in the taskbar tray."),
        ignore_quit: std::path::Path::new(IGNORE_QUIT_FILE).exists(),
        quitting: false,
    }));
    demo.borrow_mut().set_item();
    let mut quit = show_window(&demo, true);
    while !quit {
        let wakes = demo.borrow().res.idle(500);
        for wake in wakes {
            match demo.borrow_mut().wake(wake, false) {
                Flow::Quit => quit = true,
                Flow::Show => {}
                Flow::Stay => continue,
            }
            if !quit {
                quit = show_window(&demo, false);
            }
            break;
        }
    }
    let _ = demo.borrow().res.tray.borrow_mut().clear();
    println!("TRAYDEMO:QUIT:PASS");
    demo.borrow().res.backend.unbind();
    std::process::exit(0);
}
