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
use xui_app::trace::{self, Channel};

fn main() {
    // Resizable down to 320 x 240 and up to the screen: the view re-renders
    // at the new size.
    xui_app::app(Channel::About, "About LazyOS")
        .size(WINDOW.0, WINDOW.1)
        .min_size(320, 240)
        .run(|ui, _| {
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
                }) => trace::line!(
                    Channel::About,
                    "FPS:{fps}:{width}x{height}:div={divisor}:work={millis:.1}:tris={triangles}"
                ),
                Msg::Report(Report::Shape(name)) => trace::line!(Channel::About, "SHAPE:{name}"),
                Msg::Report(Report::Style(name)) => trace::line!(Channel::About, "STYLE:{name}"),
                Msg::Report(Report::Page(n)) => trace::line!(Channel::About, "PAGE:{n}"),
                // The builder reports `QUIT` when the window is gone.
                Msg::Quit => {}
            })
            .inspect_err(|error| trace::fail!(Channel::About, "BUILD", "{error}"))
        })
}
