//! The MOD player sample (`samples/modplayer`) run for real on the host: the
//! project is loaded as the player loads it, its window runs on xui's
//! offscreen backend with the `modplay` extension installed (a silent clock
//! stands in for the mixer), and the test clicks through it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lazyrad_os::platform::{Home, LazyOsPlatform};
use lazyrad_os::tracker::sink::{ClockSink, Sink};
use lazyrad_os::tracker::{self, song::decode_base64, Output};
use lazyrad_runtime::extensions;
use lazyrad_runtime::platform::Platform;
use lazyrad_runtime::{FormRuntime, Msg};
use rhai::{Engine, Scope};
use xui_canvas::OffscreenBackend;
use xui_core::app::{run_app, Ui};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_form::{LiveForm, Value};

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/modplayer")
}

fn click(control: &str) -> Msg {
    event(control, "Click", Vec::new())
}

fn event(control: &str, event: &str, args: Vec<Value>) -> Msg {
    Msg::Event {
        form: "main_form".to_owned(),
        control: control.to_owned(),
        event: event.to_owned(),
        args,
    }
}

/// Debug-build Rhai uses large stack frames: a handler a few script calls
/// deep needs more than a test thread's 2 MiB (a release build does not).
const STACK_BYTES: usize = 64 << 20;

/// What a run left behind: the window, and every handler that ran without
/// error (`control_event`; a failing handler shows a dialog instead).
struct Outcome {
    form: Rc<LiveForm<Msg>>,
    handled: Vec<String>,
}

/// Run `messages` and hand the outcome to `check`, on a thread of its own.
/// Every event among `messages` must have run its handler without error.
fn run(
    messages: impl FnOnce() -> Vec<Msg> + Send + 'static,
    check: impl FnOnce(&LiveForm<Msg>) + Send + 'static,
) {
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let messages = messages();
            let expected = events(&messages);
            let outcome = run_with(Output::silent(), messages);
            for event in expected {
                assert!(
                    outcome.handled.contains(&event),
                    "{event} failed: {:?}",
                    outcome.handled
                );
            }
            check(&outcome.form)
        })
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

/// The handler names the events among `messages` should run.
fn events(messages: &[Msg]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Msg::Event { control, event, .. } => Some(format!("{control}_{event}")),
            _ => None,
        })
        .collect()
}

/// A silent output whose clock jumps `step_ms` every time a sink reads it,
/// so a test can play a whole song in a few hundred polls.
fn fast_forward(step_ms: u64) -> Output {
    Output {
        open: Box::new(move |rate| {
            let now = std::cell::Cell::new(0);
            let clock = Box::new(move || {
                now.set(now.get() + step_ms);
                now.get()
            });
            Ok(Box::new(ClockSink::new(rate, clock)) as Box<dyn Sink>)
        }),
        available: Box::new(|| true),
    }
}

fn run_with(output: Output, messages: Vec<Msg>) -> Outcome {
    extensions::clear();
    tracker::install_with(output, None);
    let runtime = FormRuntime::load(sample()).expect("the sample loads");
    let handled = Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = Rc::clone(&handled);
    runtime.set_handler_observer(Rc::new(move |_form: &str, control: &str, event: &str| {
        log.borrow_mut().push(format!("{control}_{event}"));
    }));
    let backend = Rc::new(OffscreenBackend::new());
    let seen = Rc::new(std::cell::RefCell::new(None));
    let slot = Rc::clone(&seen);
    let spec = PlatformSpec::new("modplayer").size(Dip(724.0), Dip(458.0));
    run_app(backend as Rc<dyn Backend>, spec, move |ui: &mut Ui<Msg>| {
        let app = runtime
            .build_app(ui, "main_form")
            .expect("main_form builds");
        for message in messages.clone() {
            ui.emit(message);
        }
        *slot.borrow_mut() = Some(app.root_form().expect("live").clone());
        app
    })
    .expect("the event loop runs");
    extensions::clear();
    let form = seen.borrow_mut().take().expect("built");
    let handled = handled.borrow().clone();
    Outcome { form, handled }
}

fn text(form: &LiveForm<Msg>, control: &str, property: &str) -> String {
    match form.get(control, property) {
        Some(Value::Text(text)) => text,
        other => panic!("{control}.{property}: {other:?}"),
    }
}

fn items(form: &LiveForm<Msg>, control: &str) -> Vec<String> {
    match form.get(control, "items") {
        Some(Value::List(items)) => items,
        other => panic!("{control}.items: {other:?}"),
    }
}

#[test]
fn loading_shows_the_built_in_song() {
    run(Vec::new, |form| {
        assert_eq!(text(form, "title_label", "text"), "Song: LazyOS Groove");
        assert_eq!(
            text(form, "info_label", "text"),
            "7 orders, 5 patterns, 6 instruments"
        );
        assert_eq!(items(form, "songs_list")[0], "LazyOS Groove (built in)");
        let instruments = items(form, "samples_list");
        assert!(
            instruments[0].starts_with("01  lead (pulse 25%)  (32 bytes"),
            "{instruments:?}"
        );
        assert!(
            instruments.iter().any(|row| row == "08  made with LazyRAD"),
            "{instruments:?}"
        );
        let rows = items(form, "pattern_list");
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[3], "", "above the first row");
        assert!(
            rows[4].starts_with("00 | --- .. F06 | A-1 02 ... | C-2 03 ..."),
            "{rows:?}"
        );
        assert_eq!(form.get("pattern_list", "selected"), Some(Value::Int(4)));
        assert!(text(form, "status_label", "text").starts_with("No sound card"));
    });
}

#[test]
fn play_pause_and_the_mix_controls_drive_the_deck() {
    run(
        || {
            vec![
                click("play_button"),
                Msg::Poll,
                Msg::Poll,
                event("separation_slider", "Change", vec![Value::Float(30.0)]),
                event("ch2_button", "Toggle", vec![Value::Bool(false)]),
                event("interpolate_check", "Toggle", vec![Value::Bool(false)]),
                Msg::Poll,
            ]
        },
        |form| {
            assert_eq!(
                text(form, "status_label", "text"),
                "Playing LazyOS Groove (built in)."
            );
            assert_eq!(text(form, "separation_value", "text"), "30%");
            assert!(text(form, "position_label", "text").starts_with("Order 1/7  Pattern 0  Row "));
            assert!(text(form, "time_label", "text").contains("125 BPM"));
        },
    );
}

#[test]
fn pause_and_stop_report_themselves() {
    run(
        || vec![click("play_button"), Msg::Poll, click("pause_button")],
        |form| {
            assert_eq!(text(form, "status_label", "text"), "Paused.");
        },
    );
    run(
        || {
            vec![
                click("play_button"),
                click("pause_button"),
                click("play_button"),
                Msg::Poll,
                click("next_button"),
                click("stop_button"),
            ]
        },
        |form| assert_eq!(text(form, "status_label", "text"), "Stopped."),
    );
}

#[test]
fn the_song_plays_to_the_end() {
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(|| {
            let mut messages = vec![click("play_button")];
            messages.extend((0..3000).map(|_| Msg::Poll));
            let outcome = run_with(fast_forward(25), messages);
            assert_eq!(
                text(&outcome.form, "status_label", "text"),
                "Finished (53 s)."
            );
        })
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

#[test]
fn the_cells_module_formats_like_a_tracker() {
    let engine = Engine::new();
    let source = std::fs::read_to_string(sample().join("cells.rhai")).unwrap();
    let ast = engine.compile(source).unwrap();
    let call = |name: &str, args: Vec<rhai::Dynamic>| -> String {
        engine
            .call_fn::<rhai::Dynamic>(&mut Scope::new(), &ast, name, args)
            .unwrap()
            .to_string()
    };
    assert_eq!(call("note_name", vec![428.into()]), "C-2");
    assert_eq!(call("note_name", vec![113.into()]), "B-3");
    assert_eq!(call("note_name", vec![430.into()]), "C-2", "nearest");
    assert_eq!(call("note_name", vec![0.into()]), "---");
    assert_eq!(call("hex", vec![0x0F.into(), 2.into()]), "0F");
    let cell = engine
        .eval::<rhai::Dynamic>("#{ period: 254, sample: 18, effect: 10, param: 15 }")
        .unwrap();
    assert_eq!(call("cell_text", vec![cell]), "A-2 12 A0F");
    let empty = engine
        .eval::<rhai::Dynamic>("#{ period: 0, sample: 0, effect: 0, param: 0 }")
        .unwrap();
    assert_eq!(call("cell_text", vec![empty]), "--- .. ...");
}

#[test]
fn the_built_in_song_is_current_and_valid() {
    let engine = Engine::new();
    let source = std::fs::read_to_string(sample().join("demo_song.rhai")).unwrap();
    let ast = engine.compile(source).unwrap();
    let data: String = engine.call_fn(&mut Scope::new(), &ast, "data", ()).unwrap();
    let module = modplay::Module::parse(&decode_base64(&data).unwrap()).unwrap();
    assert_eq!(module.orders, [0, 1, 2, 3, 4, 2, 3]);
    assert_eq!(&module.title[..13], b"LazyOS Groove");
}

#[test]
fn the_packaged_sample_may_use_the_mixer() {
    let scripts: Vec<String> = ["main_form.rhai", "cells.rhai", "demo_song.rhai"]
        .iter()
        .map(|name| std::fs::read_to_string(sample().join(name)).unwrap())
        .collect();
    let scripts: Vec<&str> = scripts.iter().map(String::as_str).collect();
    let found = LazyOsPlatform::ide(Home::from_var(Some(OsStr::new("/home/user")))).script_permissions(&scripts);
    assert_eq!(found.interfaces, ["os.lazy.audio.v1"]);
    assert!(found.topics.is_empty());
}
