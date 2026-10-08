//! LazyRAD's brick breaker sample (`samples/brickbreaker`, copied from
//! va1erian/lazyrad's `examples/`) played on the host: the project loads with
//! the LazyOS platform's rules, its `Canvas` runs on xui's offscreen backend,
//! and a few seconds of play must raise no script error.

use std::path::{Path, PathBuf};

use lazyrad_runtime::testing::{run_on_large_stack, TestApp};
use xui_form::Value;

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/brickbreaker")
}

/// One frame of `dt` seconds.
fn frame(app: &mut TestApp) {
    app.event("game_canvas", "Frame", vec![Value::Float(1.0 / 60.0)]);
}

#[test]
fn the_brick_breaker_plays_without_a_script_error() {
    run_on_large_stack(|| {
        TestApp::run(sample(), |app| {
            // The title screen draws.
            for _ in 0..5 {
                frame(app);
            }
            // Space launches the ball; hold Left for a moment, then play on.
            app.event("game_canvas", "KeyDown", vec![Value::Text("space".to_owned())]);
            app.event("game_canvas", "KeyUp", vec![Value::Text("space".to_owned())]);
            app.event("game_canvas", "KeyDown", vec![Value::Text("left".to_owned())]);
            for _ in 0..30 {
                frame(app);
            }
            app.event("game_canvas", "KeyUp", vec![Value::Text("left".to_owned())]);
            // The mouse moves the paddle too.
            app.event(
                "game_canvas",
                "MouseMove",
                vec![Value::Float(400.0), Value::Float(480.0)],
            );
            for _ in 0..180 {
                frame(app);
            }
            // `TestApp` fails the test on any handler error; say so plainly.
            assert!(app.errors().is_empty(), "{:?}", app.errors());
            assert_eq!(
                app.get("game_canvas", "fps"),
                Some(Value::Int(60)),
                "the frame loop still runs (a failing frame would stop it)"
            );
        })
        .expect("the brick breaker loads");
    });
}
