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
//! Playing: `GOLF:SHOT:<hole>:<stroke>:<club>:power=<p>` at impact,
//! `GOLF:LAND:<hole>:<stroke>:<material>:carry=<m>` at the first touch,
//! `GOLF:PENALTY:<hole>:<stroke>:<kind>` and
//! `GOLF:HOLED:<hole>:<strokes>:par=<par>`, or `GOLF:PICKUP:` with the same
//! fields when the hole is given up at the stroke limit.
//! A first argument that is a number picks the course seed.

use xui_app::trace::{self, Channel};
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
    xui_app::app(Channel::Golf, "LazyGolf")
        .size(WINDOW.0, WINDOW.1)
        .min_size(320, 200)
        .run(move |ui, _| {
            // Resizable down to 320 x 200 and up to the screen (maximizable):
            // the view re-renders at the new size.
            GolfApp::build(ui, seed, |msg| match msg {
                Msg::Report(Report::Ready {
                    name,
                    seed,
                    par,
                    millis,
                }) => {
                    trace::line!(Channel::Golf, "READY:{seed}:par={par}:ms={millis}:{name}");
                }
                Msg::Report(Report::Fps {
                    fps,
                    scale,
                    width,
                    height,
                    work_ms,
                }) => {
                    trace::line!(
                        Channel::Golf,
                        "FPS:{fps}:scale={scale}:{width}x{height}:work={work_ms:.1}"
                    );
                }
                Msg::Report(Report::Bench {
                    frames,
                    min_fps,
                    avg_fps,
                }) => {
                    trace::line!(
                        Channel::Golf,
                        "BENCH:frames={frames}:min={min_fps}:avg={avg_fps:.1}"
                    );
                }
                Msg::Report(Report::Shot {
                    hole,
                    stroke,
                    club,
                    power,
                }) => {
                    trace::line!(
                        Channel::Golf,
                        "SHOT:{hole}:{stroke}:{club}:power={power:.2}"
                    );
                }
                Msg::Report(Report::Landed {
                    hole,
                    stroke,
                    material,
                    carry,
                }) => {
                    trace::line!(
                        Channel::Golf,
                        "LAND:{hole}:{stroke}:{material}:carry={carry:.0}"
                    );
                }
                Msg::Report(Report::Penalty { hole, stroke, kind }) => {
                    trace::line!(Channel::Golf, "PENALTY:{hole}:{stroke}:{kind}");
                }
                Msg::Report(Report::Holed { hole, strokes, par }) => {
                    trace::line!(Channel::Golf, "HOLED:{hole}:{strokes}:par={par}");
                }
                Msg::Report(Report::PickedUp { hole, strokes, par }) => {
                    trace::line!(Channel::Golf, "PICKUP:{hole}:{strokes}:par={par}");
                }
                // The builder reports `QUIT` when the window is gone.
                Msg::Quit | Msg::Gen(_) => {}
            })
            .inspect_err(|error| trace::fail!(Channel::Golf, "BUILD", "{error}"))
        })
}
