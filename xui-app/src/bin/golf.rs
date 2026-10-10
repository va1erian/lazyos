//! `xui-golf`: LazyGolf, a procedurally generated golf course drawn like a
//! mid-90s golf game, to fly over with the keyboard and mouse. The
//! generator, the renderer and the window live in `crates/golf` (portable:
//! its `golf` example runs the same app on a desktop); this file only
//! launches it.
//!
//! Serial evidence: `GOLF:UP:PASS` after the first frame,
//! `GOLF:READY:<seed>:par=<par>:ms=<ms>:<name>` once the course is built,
//! `GOLF:FPS:<fps>:scale=<n>:<w>x<h>:work=<ms>` once a second while frames
//! are drawn, `GOLF:BENCH:frames=<n>:min=<fps>:avg=<fps>` after the `B`
//! flyover, and `GOLF:QUIT:PASS` on Escape (or the window close button).
//! A first argument that is a number picks the course seed.

use xui_app::launch;
use xui_golf::game::Report;
use xui_golf::{GolfApp, Msg, WINDOW};

fn main() {
    let seed = std::env::args()
        .skip(1)
        .find_map(|arg| arg.parse::<u64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_secs() % 100_000)
        });
    launch::run("GOLF", "LazyGolf", WINDOW, move |ui, backend| {
        backend.on_first_frame(|| println!("GOLF:UP:PASS"));
        // Resizable down to 320 x 200 and up to the screen (maximizable):
        // the view re-renders at the new size.
        backend.set_size_hints(320, 200, 0, 0);
        GolfApp::build(ui, seed, |msg| match msg {
            Msg::Report(Report::Ready {
                name,
                seed,
                par,
                millis,
            }) => {
                println!("GOLF:READY:{seed}:par={par}:ms={millis}:{name}");
            }
            Msg::Report(Report::Fps {
                fps,
                scale,
                width,
                height,
                work_ms,
            }) => {
                println!("GOLF:FPS:{fps}:scale={scale}:{width}x{height}:work={work_ms:.1}");
            }
            Msg::Report(Report::Bench {
                frames,
                min_fps,
                avg_fps,
            }) => {
                println!("GOLF:BENCH:frames={frames}:min={min_fps}:avg={avg_fps:.1}");
            }
            Msg::Quit => println!("GOLF:QUIT:PASS"),
        })
        .inspect_err(|error| println!("GOLF:BUILD:FAIL:{error}"))
    })
}
