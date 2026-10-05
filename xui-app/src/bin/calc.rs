//! `xui-calc`: the Calculator, a pocket calculator with a keypad and the
//! keyboard. The engine and the window live in `crates/calc`; this file only
//! launches them.
//!
//! Serial evidence: `CALC:UP:PASS` after the first frame, `CALC:MSG:<msg>`
//! for every message (a session's `until` confirms a click landed),
//! `CALC:RESULT:<display>` after each `=`, and `CALC:QUIT:PASS` on `q` (or
//! the window close button).

use xui_app::launch;
use xui_calc::{CalcApp, Msg, Report, WINDOW};

fn main() {
    launch::run("CALC", "Calculator", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("CALC:UP:PASS"));
        CalcApp::build(ui, |report| match report {
            Report::Msg(msg) => {
                println!("CALC:MSG:{msg:?}");
                if *msg == Msg::Quit {
                    println!("CALC:QUIT:PASS");
                }
            }
            Report::Result(shown) => println!("CALC:RESULT:{shown}"),
        })
        .inspect_err(|error| println!("CALC:BUILD:FAIL:{error}"))
    })
}
