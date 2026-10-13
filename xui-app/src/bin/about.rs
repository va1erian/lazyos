//! `about`: About LazyOS, a window with a real-time software 3D renderer.
//! The rasteriser and the window live in `crates/about` (portable: its
//! `about` example runs the same app on a desktop); this file only launches
//! it.
//!
//! Serial evidence: `ABOUT:UP:PASS` after the first frame,
//! `ABOUT:FPS:<fps>:<w>x<h>:div=<n>:work=<ms>:tris=<n>` once a second,
//! `ABOUT:SHAPE:<name>`, `ABOUT:STYLE:<name>`, `ABOUT:PAGE:<n>` when the
//! shape, style or page changes, and `ABOUT:QUIT:PASS` on Escape or `Q` (or
//! the window close button).

use xui_about::{AboutApp, Info, Msg, Report, WINDOW};
use xui_app::launch;

fn main() {
    launch::run(xui_app::trace::Channel::About, "About LazyOS", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("ABOUT:UP:PASS"));
        // Resizable down to 320 x 240 and up to the screen: the view
        // re-renders at the new size.
        backend.set_size_hints(320, 240, 0, 0);
        let info = Info {
            version: env!("CARGO_PKG_VERSION").into(),
        };
        AboutApp::build(ui, info, |msg| match msg {
            Msg::Report(Report::Frame {
                fps,
                width,
                height,
                divisor,
                millis,
                triangles,
            }) => println!(
                "ABOUT:FPS:{fps}:{width}x{height}:div={divisor}:work={millis:.1}:tris={triangles}"
            ),
            Msg::Report(Report::Shape(name)) => println!("ABOUT:SHAPE:{name}"),
            Msg::Report(Report::Style(name)) => println!("ABOUT:STYLE:{name}"),
            Msg::Report(Report::Page(n)) => println!("ABOUT:PAGE:{n}"),
            Msg::Quit => println!("ABOUT:QUIT:PASS"),
        })
        .inspect_err(|error| println!("ABOUT:BUILD:FAIL:{error}"))
    })
}
