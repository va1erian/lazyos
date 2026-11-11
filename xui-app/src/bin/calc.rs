//! `xui-calc`: the Calculator, a pocket calculator with a keypad and the
//! keyboard. The engine and the window live in `crates/calc`; this file only
//! launches them.
//!
//! Serial evidence: `CALC:UP:PASS` after the first frame, `CALC:MSG:<msg>`
//! for every message (a session's `until` confirms a click landed),
//! `CALC:RESULT:<display>` after each `=`, and `CALC:QUIT:PASS` on `q` (or
//! the window close button).

use xui_app::trace::{self, Channel};
use xui_calc::{CalcApp, Report, WINDOW};

fn main() {
    xui_app::app(Channel::Calc, "Calculator")
        .size(WINDOW.0, WINDOW.1)
        .run(|ui, _| {
            CalcApp::build(ui, |report| match report {
                Report::Msg(msg) => trace::line!(Channel::Calc, "MSG:{msg:?}"),
                Report::Result(shown) => trace::line!(Channel::Calc, "RESULT:{shown}"),
            })
            .inspect_err(|error| trace::fail!(Channel::Calc, "BUILD", "{error}"))
        })
}
