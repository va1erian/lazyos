//! The `modplay` script surface end to end: a Rhai engine with the bindings,
//! a tracker on a manual clock, and the event source polled as a form's
//! window would poll it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lazyrad_runtime::extensions::EventSource;
use modplay::synth::{effect, note, square, ModBuilder};
use modplay::Module;
use rhai::{Dynamic, Engine, EvalAltResult, FnPtr, AST};

use super::events::Tracker;
use super::sink::ClockSink;
use super::{api, script_interfaces, Output};

/// Three orders of a two-voice tune with a named sample; about 23 s long.
pub(crate) fn demo_bytes() -> Vec<u8> {
    let mut builder = ModBuilder::new()
        .title("tracker test")
        .sample(1, square(32), 48, true)
        .name(1, "square")
        .orders(&[0, 1, 2]);
    for pattern in 0..3 {
        for row in (0..64).step_by(4) {
            builder = builder.note(pattern, row, 0, note(428 - 4 * row as u16, 1));
        }
        builder = builder.note(pattern, 0, 1, note(856, 1));
    }
    builder.note(2, 63, 3, effect(0xF, 6)).build()
}

pub(crate) fn demo_module() -> Rc<Module> {
    Rc::new(Module::parse(&demo_bytes()).unwrap())
}

fn base64(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = chunk
            .iter()
            .enumerate()
            .fold(0u32, |w, (i, &b)| w | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(DIGITS[(word >> (18 - 6 * i) & 63) as usize]));
        }
        out.push_str(&"=".repeat(3 - chunk.len()));
    }
    out
}

/// An engine for form `main` with `modplay`, a `note(text)` log, the demo
/// song as the global constant `DEMO`, and the tracker behind it.
struct Rig {
    engine: Engine,
    tracker: Rc<Tracker>,
    now: Rc<Cell<u64>>,
    log: Rc<RefCell<Vec<String>>>,
}

impl Rig {
    fn new() -> Rig {
        let now = Rc::new(Cell::new(0));
        let clock = |now: &Rc<Cell<u64>>| {
            let now = Rc::clone(now);
            Box::new(move || now.get()) as super::sink::Clock
        };
        let reports = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&reports);
        let tracker = Rc::new(Tracker::new(
            clock(&now),
            Some(Rc::new(move |stage: &str, detail: &str| {
                seen.borrow_mut().push(format!("{stage} {detail}"));
            })),
        ));
        let sink_clock = Rc::clone(&now);
        let output = Rc::new(Output {
            open: Box::new(move |rate| {
                let now = Rc::clone(&sink_clock);
                Ok(Box::new(ClockSink::new(rate, Box::new(move || now.get()))) as _)
            }),
            available: Box::new(|| false),
        });
        let mut engine = Engine::new();
        api::register(&mut engine, &tracker, &output, "main");
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&log);
        engine.register_fn("note", move |text: &str| {
            sink.borrow_mut().push(text.to_owned())
        });
        let _ = reports;
        Rig {
            engine,
            tracker,
            now,
            log,
        }
    }

    fn run(&self, script: &str) -> Result<AST, Box<EvalAltResult>> {
        let source = format!("const DEMO = \"{}\";\n{script}", base64(&demo_bytes()));
        let ast = self.engine.compile(source)?;
        self.engine.run_ast(&ast)?;
        Ok(ast)
    }

    /// Advance the clock by `ms` and poll as the window would.
    fn tick(&self, ast: &AST, ms: u64) -> Vec<String> {
        self.now.set(self.now.get() + ms);
        let mut call =
            |handler: &FnPtr, args: Vec<Dynamic>| handler.call::<Dynamic>(&self.engine, ast, args);
        self.tracker
            .poll("main", &mut call)
            .iter()
            .map(|e| e.to_string())
            .collect()
    }

    fn notes(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

#[test]
fn a_script_plays_a_decoded_song_and_hears_updates_and_the_end() {
    let rig = Rig::new();
    let ast = rig
        .run(
            r#"
            let song = modplay::decode(DEMO);
            note(`${song.title}|${song.length}|${song.samples[0].name}|${song.pattern_at(2)}`);
            let deck = modplay::play(song, #{ separation: 20, volume: 80 });
            note(`${deck.status}|${deck.volume}|${deck.separation}|${deck.sound}`);
            deck.on_update(|d| note(`update ${d.order}:${d.row}`));
            deck.on_end(|d| note(`end ${d.status}`));
            "#,
        )
        .unwrap();
    assert_eq!(
        rig.notes()[..2],
        ["tracker test|3|square|2", "playing|80|20|false"]
    );
    assert!(rig.tracker.active("main"));
    assert!(!rig.tracker.active("other"));
    for _ in 0..1000 {
        assert!(rig.tick(&ast, 40).is_empty());
        if !rig.tracker.active("main") {
            break;
        }
    }
    let notes = rig.notes();
    assert_eq!(
        notes.last().map(String::as_str),
        Some("end ended"),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n == "update 1:0"),
        "rows advance: {notes:?}"
    );
    let updates = notes.iter().filter(|n| n.starts_with("update")).count();
    assert!(updates > 50 && updates < 400, "throttled: {updates}");
}

#[test]
fn transport_controls_change_what_is_heard() {
    let rig = Rig::new();
    let ast = rig
        .run(
            r#"
            let deck = modplay::play(modplay::decode(DEMO), #{ loops: 0 });
            deck.pause();
            note(deck.status);
            deck.resume();
            deck.seek(2);
            deck.mute(0, true);
            note(`${deck.status} ${deck.muted(0)} ${deck.muted(1)} ${deck.muted(9)}`);
            deck.separation = 100;
            deck.interpolate = false;
            note(`${deck.separation} ${deck.interpolate}`);
            deck.on_update(|d| if d.order == 2 { note("at 2"); d.stop(); });
            "#,
        )
        .unwrap();
    let notes = rig.notes();
    assert_eq!(
        notes[..3],
        ["paused", "playing true false false", "100 false"]
    );
    for _ in 0..100 {
        rig.tick(&ast, 40);
    }
    assert!(rig.notes().contains(&"at 2".to_owned()));
    assert_eq!(rig.tracker.decks("main"), 0, "a stopped deck is dropped");
}

#[test]
fn bad_options_and_bad_songs_are_script_errors() {
    let rig = Rig::new();
    for (code, expected) in [
        (
            "modplay::play(modplay::decode(DEMO), #{ speed: 3 })",
            "unknown option",
        ),
        (
            "modplay::play(modplay::decode(DEMO), #{ volume: 101 })",
            "`volume`",
        ),
        (
            "modplay::play(modplay::decode(DEMO), #{ interpolate: 1 })",
            "true or false",
        ),
        ("modplay::decode(\"aGVsbG8=\")", "too short"),
        ("modplay::decode(\"!!!!\")", "base64"),
    ] {
        let error = rig.run(code).expect_err(code).to_string();
        assert!(error.contains(expected), "{code}: {error}");
    }
}

#[test]
fn a_failing_handler_is_reported_once_and_dropped() {
    let rig = Rig::new();
    let ast = rig
        .run(
            r#"
            let deck = modplay::play(modplay::decode(DEMO));
            deck.on_update(|d| throw "broken");
            "#,
        )
        .unwrap();
    let first = rig.tick(&ast, 40);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].contains("broken"));
    for _ in 0..20 {
        assert!(rig.tick(&ast, 40).is_empty());
    }
    assert!(rig.tracker.active("main"), "the music plays on");
}

#[test]
fn closing_the_window_stops_its_decks() {
    let rig = Rig::new();
    let ast = rig
        .run("let deck = modplay::play(modplay::decode(DEMO)); deck.on_update(|d| note(d.status));")
        .unwrap();
    rig.tick(&ast, 40);
    rig.tracker.release("main");
    assert!(!rig.tracker.active("main"));
    let before = rig.notes().len();
    rig.tick(&ast, 40);
    assert_eq!(rig.notes().len(), before, "nothing runs for a closed form");
}

#[test]
fn many_decks_come_and_go_without_piling_up() {
    // A soak: a playlist skipping tracks starts and stops decks over and
    // over, and ended decks the script let go of; nothing is left behind.
    let rig = Rig::new();
    let ast = rig
        .run(
            r#"
            fn skip() {
                let deck = modplay::play(modplay::decode(global::DEMO), #{ loops: 0 });
                deck.stop();
            }
            fn short() {
                let deck = modplay::play(modplay::decode(global::DEMO));
                deck.seek(2);
            }
            "#,
        )
        .unwrap();
    let call = |name: &str| {
        let _: Dynamic = rig
            .engine
            .call_fn(&mut rhai::Scope::new(), &ast, name, ())
            .unwrap();
    };
    for _ in 0..200 {
        call("skip");
        rig.tick(&ast, 40);
        assert_eq!(rig.tracker.decks("main"), 0);
    }
    for _ in 0..20 {
        call("short");
    }
    assert_eq!(rig.tracker.decks("main"), 20);
    for _ in 0..500 {
        rig.tick(&ast, 40);
    }
    assert_eq!(rig.tracker.decks("main"), 0, "played out and unreferenced");
    assert!(!rig.tracker.active("main"));
}

#[test]
fn only_scripts_that_play_need_the_audio_interface() {
    assert_eq!(
        script_interfaces(["fn a() { let d = modplay::play(s); }"]),
        ["os.lazy.audio.v1"]
    );
    assert!(script_interfaces(["fn a() { modplay::decode(x) }"]).is_empty());
    assert!(script_interfaces(Vec::<&str>::new()).is_empty());
}
