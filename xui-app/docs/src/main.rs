//! `xui-docs`: renders a Markdown file with litehtml.
//!
//! A `xuid` desktop client (or the display owner in a headless session). With no
//! path argument it shows a built-in welcome page. litehtml lays the page out on a
//! worker thread; this file supplies the LazyOS platform and the serial
//! markers the screenshot sessions wait on.
//!
//! Serial evidence: `DOCS:UP:PASS` after the first frame, `DOCS:RENDER:PASS`
//! once litehtml's first frame has been painted, `DOCS:LINK:<href>` on a link
//! click, `DOCS:OPEN:FAIL:<path>` when the file cannot be read.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_core::app::{run_app, App, Ui};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_docs::page;
use xui_litehtml::{HtmlView, HtmlViewEvent};

/// Window size a compositor lays the page out at.
const WINDOW: (i32, i32) = (900, 640);

/// Shown when no file is named on the command line.
const SAMPLE: &str = include_str!("welcome.md");

enum Msg {
    /// litehtml finished a layout pass on its worker thread.
    Frame,
    /// Polls for the first painted frame, to print the render marker once.
    Tick,
    Link(String),
    Copy(String),
}

struct Docs {
    view: HtmlView<Msg>,
    reported: bool,
}

impl App for Docs {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        match msg {
            Msg::Frame => self.view.invalidate(),
            Msg::Tick => {
                if !self.reported && self.view.is_ready() {
                    self.reported = true;
                    println!("DOCS:RENDER:PASS");
                }
            }
            Msg::Link(href) => println!("DOCS:LINK:{href}"),
            Msg::Copy(text) => println!("DOCS:COPY:{} bytes", text.len()),
        }
    }
}

/// The Markdown source: the file named on the command line, else the sample.
fn source() -> Result<String, String> {
    let Some(path) = xui_app::platform::argv::file_arg(std::env::args_os()) else {
        return Ok(SAMPLE.to_string());
    };
    std::fs::read_to_string(&path).map_err(|_| path.display().to_string())
}

fn main() -> std::process::ExitCode {
    xui_app::font::register_docs();
    let markdown = match source() {
        Ok(text) => text,
        Err(path) => {
            println!("DOCS:OPEN:FAIL:{path}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let html = page(&markdown);
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("DOCS:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("DOCS:UP:PASS"));

    let spec = PlatformSpec::new("Docs").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        let view = HtmlView::new(
            ui,
            ui.client_rect(),
            html,
            || Msg::Frame,
            |event| match event {
                HtmlViewEvent::LinkClicked(href) => Some(Msg::Link(href)),
                HtmlViewEvent::CopyRequested(text) => Some(Msg::Copy(text)),
            },
        )
        .expect("the HTML view was created");
        let tick = ui.set_timer(100);
        ui.on_timer(move |id| (id == tick).then_some(Msg::Tick));
        Docs {
            view,
            reported: false,
        }
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("DOCS:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
