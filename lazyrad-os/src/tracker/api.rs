//! The script surface: the `modplay` module and the `Song` and `Deck` types.
//!
//! ```rhai
//! let song = modplay::load("music/tune.mod");   // or modplay::decode(base64)
//! let deck = modplay::play(song, #{ loops: 0, separation: 50 });
//! deck.on_update(|d| row_label.text = `${d.order}:${d.row}`);
//! deck.on_end(|d| status_label.text = "done");
//! ```
//!
//! Every function documents itself for LazyRAD's completion list; the
//! reference is `docs/lazyrad-modplay.md`.

use std::rc::Rc;

use lazyrad_runtime::platform;
use modplay::{CHANNELS, ROWS_PER_PATTERN};
use rhai::{Array, Dynamic, Engine, EvalAltResult, FnPtr, Map, Module};

use super::deck::{Deck, Settings, State};
use super::events::{DeckHandle, Entry, Tracker};
use super::song::{self, Song};
use super::Output;

type ScriptResult<T> = Result<T, Box<EvalAltResult>>;

fn error<T>(message: impl Into<String>) -> ScriptResult<T> {
    Err(message.into().into())
}

/// Registers `modplay::*`, `Song` and `Deck` on an engine scripting `form`.
pub fn register(engine: &mut Engine, tracker: &Rc<Tracker>, output: &Rc<Output>, form: &str) {
    engine.register_static_module("modplay", module(tracker, output, form).into());
    register_song(engine);
    register_deck(engine);
}

fn module(tracker: &Rc<Tracker>, output: &Rc<Output>, form: &str) -> Module {
    let mut module = Module::new();
    module.set_native_fn("load", |path: &str| -> ScriptResult<Song> {
        song::load(&platform::current().fs_policy(), path).or_else(error)
    });
    module.set_native_fn("decode", |text: &str| -> ScriptResult<Song> {
        let bytes = song::decode_base64(text).or_else(error)?;
        Song::parse(&bytes).or_else(error)
    });
    let available = Rc::clone(output);
    module.set_native_fn("sound_available", move || -> ScriptResult<bool> {
        Ok((available.available)())
    });
    for with_options in [false, true] {
        let tracker = Rc::clone(tracker);
        let output = Rc::clone(output);
        let form = form.to_owned();
        if with_options {
            module.set_native_fn("play", move |song: Song, options: Map| {
                play(&tracker, &output, &form, &song, &options)
            });
        } else {
            module.set_native_fn("play", move |song: Song| {
                play(&tracker, &output, &form, &song, &Map::new())
            });
        }
    }
    module
}

/// Open a sink and start `song` on a new deck owned by `form`.
fn play(
    tracker: &Tracker,
    output: &Output,
    form: &str,
    song: &Song,
    options: &Map,
) -> ScriptResult<DeckHandle> {
    let (settings, rate) = settings(options)?;
    let sink = (output.open)(rate).or_else(error)?;
    let detail = format!(
        "title=\"{}\" audio={} rate={}",
        song.title(),
        u8::from(sink.is_audio()),
        sink.rate()
    );
    let deck = Deck::new(Rc::clone(&song.0), sink, settings);
    tracker.report("MODPLAY", &detail);
    Ok(tracker.add(Entry::new(deck, form)))
}

/// The deck settings and mixing rate an options map asks for.
fn settings(options: &Map) -> ScriptResult<(Settings, u32)> {
    let mut settings = Settings::default();
    let mut rate = super::sink::DEFAULT_RATE;
    for (key, value) in options {
        let int = |low: i64, high: i64| -> ScriptResult<i64> {
            match value.as_int() {
                Ok(n) if (low..=high).contains(&n) => Ok(n),
                _ => error(format!(
                    "modplay::play: `{key}` must be an integer {low}..={high}"
                )),
            }
        };
        match key.as_str() {
            "loops" => {
                let loops = int(0, 1000)?;
                settings.options.loops = (loops > 0).then_some(loops as u32);
            }
            "separation" => settings.options.separation = int(0, 100)? as u8,
            "volume" => settings.volume = int(0, 100)? as u8,
            "rate" => rate = int(8_000, 48_000)? as u32,
            "interpolate" => {
                settings.options.interpolate = value
                    .as_bool()
                    .or_else(|_| error("modplay::play: `interpolate` must be true or false"))?;
            }
            other => {
                return error(format!(
                    "modplay::play: unknown option `{other}` (loops, separation, volume, rate, interpolate)"
                ))
            }
        }
    }
    Ok((settings, rate))
}

fn register_song(engine: &mut Engine) {
    engine.register_type_with_name::<Song>("Song");
    engine.register_get("title", |song: &mut Song| song.title());
    engine.register_get("length", |song: &mut Song| song.0.orders.len() as i64);
    engine.register_get("patterns", |song: &mut Song| song.0.pattern_count() as i64);
    engine.register_get("restart", |song: &mut Song| i64::from(song.0.restart));
    engine.register_get("channels", |_: &mut Song| CHANNELS as i64);
    engine.register_get("rows", |_: &mut Song| ROWS_PER_PATTERN as i64);
    engine.register_get("orders", |song: &mut Song| -> Array {
        song.0
            .orders
            .iter()
            .map(|&p| Dynamic::from(i64::from(p)))
            .collect()
    });
    engine.register_get("samples", |song: &mut Song| song.samples());
    engine.register_fn("pattern_at", |song: &mut Song, order: i64| {
        song.pattern_at(order)
    });
    engine.register_fn("row", |song: &mut Song, order: i64, row: i64| {
        song.row(order, row)
    });
    engine.register_fn(
        "cell",
        |song: &mut Song, order: i64, row: i64, channel: i64| song.cell(order, row, channel),
    );
    engine.register_fn("to_string", |song: &mut Song| {
        format!("Song(\"{}\")", song.title())
    });
}

fn status(state: State) -> &'static str {
    match state {
        State::Playing => "playing",
        State::Paused => "paused",
        State::Ended => "ended",
        State::Stopped => "stopped",
    }
}

fn register_deck(engine: &mut Engine) {
    engine.register_type_with_name::<DeckHandle>("Deck");
    let get = |handle: &DeckHandle, read: &dyn Fn(&Deck) -> Dynamic| read(&handle.0.borrow().deck);
    engine.register_get("status", move |d: &mut DeckHandle| {
        status(d.0.borrow().deck.state()).to_owned()
    });
    engine.register_get("playing", |d: &mut DeckHandle| {
        d.0.borrow().deck.state() == State::Playing
    });
    engine.register_get("order", move |d: &mut DeckHandle| {
        get(d, &|deck| (deck.heard().order as i64).into())
    });
    engine.register_get("row", move |d: &mut DeckHandle| {
        get(d, &|deck| (deck.heard().row as i64).into())
    });
    engine.register_get("pattern", move |d: &mut DeckHandle| {
        get(d, &|deck| (deck.heard().pattern as i64).into())
    });
    engine.register_get("speed", move |d: &mut DeckHandle| {
        get(d, &|deck| i64::from(deck.heard().speed).into())
    });
    engine.register_get("tempo", move |d: &mut DeckHandle| {
        get(d, &|deck| i64::from(deck.heard().tempo).into())
    });
    engine.register_get("levels", move |d: &mut DeckHandle| {
        get(d, &|deck| {
            let levels: Array = deck
                .heard()
                .levels
                .iter()
                .map(|&l| Dynamic::from(i64::from(l)))
                .collect();
            levels.into()
        })
    });
    engine.register_get("elapsed_ms", move |d: &mut DeckHandle| {
        get(d, &|deck| (deck.elapsed_ms() as i64).into())
    });
    engine.register_get("sound", move |d: &mut DeckHandle| {
        get(d, &|deck| deck.has_audio().into())
    });
    engine.register_get("song", |d: &mut DeckHandle| {
        Song(Rc::clone(d.0.borrow().deck.module()))
    });
    engine.register_get("volume", move |d: &mut DeckHandle| {
        get(d, &|deck| i64::from(deck.volume()).into())
    });
    engine.register_set(
        "volume",
        |d: &mut DeckHandle, percent: i64| -> ScriptResult<()> {
            let percent = percent.clamp(0, 100) as u8;
            d.0.borrow_mut().deck.set_volume(percent).or_else(error)
        },
    );
    engine.register_get("separation", move |d: &mut DeckHandle| {
        get(d, &|deck| {
            i64::from(deck.player().options().separation).into()
        })
    });
    engine.register_set("separation", |d: &mut DeckHandle, percent: i64| {
        let percent = percent.clamp(0, 100) as u8;
        d.0.borrow_mut().deck.player_mut().set_separation(percent);
    });
    engine.register_get("interpolate", move |d: &mut DeckHandle| {
        get(d, &|deck| deck.player().options().interpolate.into())
    });
    engine.register_set("interpolate", |d: &mut DeckHandle, on: bool| {
        d.0.borrow_mut().deck.player_mut().set_interpolate(on);
    });
    register_deck_methods(engine);
}

fn register_deck_methods(engine: &mut Engine) {
    engine.register_fn("pause", |d: &mut DeckHandle| -> ScriptResult<()> {
        d.0.borrow_mut().deck.pause().or_else(error)
    });
    engine.register_fn("resume", |d: &mut DeckHandle| -> ScriptResult<()> {
        d.0.borrow_mut().deck.resume().or_else(error)
    });
    engine.register_fn("stop", |d: &mut DeckHandle| d.0.borrow_mut().deck.stop());
    engine.register_fn("seek", |d: &mut DeckHandle, order: i64| {
        let mut entry = d.0.borrow_mut();
        entry.deck.seek(order.max(0) as usize);
        entry.end_delivered = false;
    });
    engine.register_fn("mute", |d: &mut DeckHandle, channel: i64, muted: bool| {
        if let Ok(channel) = usize::try_from(channel) {
            d.0.borrow_mut().deck.player_mut().set_muted(channel, muted);
        }
    });
    engine.register_fn("muted", |d: &mut DeckHandle, channel: i64| {
        usize::try_from(channel).is_ok_and(|c| d.0.borrow().deck.player().is_muted(c))
    });
    engine.register_fn("on_update", |d: &mut DeckHandle, handler: FnPtr| {
        d.0.borrow_mut().on_update = Some(handler);
    });
    engine.register_fn("on_end", |d: &mut DeckHandle, handler: FnPtr| {
        d.0.borrow_mut().on_end = Some(handler);
    });
    engine.register_fn("to_string", |d: &mut DeckHandle| {
        let entry = d.0.borrow();
        let heard = entry.deck.heard();
        format!(
            "Deck({} {:02}:{:02})",
            status(entry.deck.state()),
            heard.order,
            heard.row
        )
    });
}
