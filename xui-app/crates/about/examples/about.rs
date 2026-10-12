//! About LazyOS as a desktop window on Windows, Linux or macOS, through
//! xui's winit backend: the same app the LazyOS binary launches.
//!
//! ```text
//! cargo run --release -p xui-about --example about --features desktop
//! ```

use std::rc::Rc;

use xui_about::{AboutApp, Info, Msg, WINDOW};
use xui_canvas::WinitBackend;

fn main() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
    let outcome = xui_core::app("About LazyOS")
        .size(WINDOW.0, WINDOW.1)
        .backend(Rc::new(WinitBackend::new()))
        .run(|ui| {
            let info = Info {
                version: env!("CARGO_PKG_VERSION").into(),
            };
            AboutApp::build(ui, info, |msg| {
                if let Msg::Report(report) = msg {
                    println!("{report:?}");
                }
            })
        });
    if let Err(error) = outcome {
        eprintln!("about: {error}");
        std::process::exit(1);
    }
}
