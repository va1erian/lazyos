//! LazyGolf as a desktop window on Windows, Linux or macOS, through xui's
//! winit backend: the same app the LazyOS binary launches.
//!
//! ```text
//! cargo run --release -p xui-golf --example golf --features desktop [seed]
//! ```

use std::rc::Rc;

use xui_canvas::WinitBackend;
use xui_golf::{GolfApp, Msg, WINDOW};

fn main() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
    let seed = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let outcome = xui_core::app("LazyGolf")
        .size(WINDOW.0, WINDOW.1)
        .backend(Rc::new(WinitBackend::new()))
        .run(|ui| {
            GolfApp::build(ui, seed, |msg| {
                if let Msg::Report(report) = msg {
                    println!("{report:?}");
                }
            })
        });
    if let Err(error) = outcome {
        eprintln!("golf: {error}");
        std::process::exit(1);
    }
}
