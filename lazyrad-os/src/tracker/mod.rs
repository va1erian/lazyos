//! ProTracker modules for LazyRAD scripts: the `modplay` module.
//!
//! A form script loads a `.mod` (or decodes one it carries), starts it on a
//! *deck* and drives a UI from the deck's position, levels and events; the
//! parsing and mixing are `libs/modplay`, the sound goes to the system mixer
//! through `libs/audioclient` (`xui_app::platform::audio`). The player
//! registers it like Messenger ([`crate::messenger`]): a script extension for
//! every form's engine and an event source the form's window polls, so a
//! playing deck keeps its sink full between window messages and calls the
//! script back on the UI thread. See `docs/lazyrad-modplay.md` and the
//! `samples/modplayer` project.
//!
//! * [`song`]: `Song`, a parsed module, from a file or from base64 text;
//! * [`deck`]: one song rendering ahead into a sink, and what is heard now;
//! * [`sink`]: where frames go, and a silent clock without a sound card;
//! * [`mixer`]: the sink on the system mixer, lossless across pauses;
//! * [`events`]: the decks per form, pumped and reported by the window;
//! * [`api`]: the Rhai surface.

pub mod api;
pub mod deck;
pub mod events;
pub mod mixer;
pub mod sink;
pub mod song;

#[cfg(test)]
pub(crate) mod tests;

use std::rc::Rc;

use lazyrad_runtime::extensions;

use events::{Reporter, Tracker};
use mixer::MixerSink;
use sink::{ClockSink, Sink};

/// The interface an app that plays sound talks to (`idl/audio.midl`).
pub const AUDIO_INTERFACE: &str = "os.lazy.audio.v1";

/// Opens a sink at (about) the given rate.
pub type OpenSink = Box<dyn Fn(u32) -> Result<Box<dyn Sink>, String>>;

/// Where decks send their frames.
pub struct Output {
    /// A sink at (about) the given rate.
    pub open: OpenSink,
    /// Whether a sound card is there (`modplay::sound_available()`).
    pub available: Box<dyn Fn() -> bool>,
}

impl Output {
    /// The system mixer when it runs, else a silent clock, so a song still
    /// plays (positions, events, the end) on an image without sound.
    pub fn lazyos() -> Output {
        Output {
            open: Box::new(|rate| match MixerSink::open(mixer::audiod(), rate)? {
                Some(sink) => Ok(Box::new(sink) as Box<dyn Sink>),
                None => Ok(Box::new(ClockSink::new(rate, sink::wall_clock()))),
            }),
            available: Box::new(|| xui_app::platform::audio::Audio::try_connect().is_some()),
        }
    }

    /// Always a silent clock: a host build, where there is no mixer to ask.
    pub fn silent() -> Output {
        Output {
            open: Box::new(|rate| Ok(Box::new(ClockSink::new(rate, sink::wall_clock())))),
            available: Box::new(|| false),
        }
    }
}

/// Installs `modplay` into every LazyRAD engine built on this thread and
/// registers the event source that keeps decks playing. `report` hears
/// `(stage, detail)` for the serial markers.
pub fn install_with(output: Output, report: Option<Reporter>) -> Rc<Tracker> {
    let tracker = Rc::new(Tracker::new(sink::wall_clock(), report));
    let output = Rc::new(output);
    let for_engines = Rc::clone(&tracker);
    extensions::add_scoped(move |engine, scope| {
        api::register(engine, &for_engines, &output, scope.form);
    });
    extensions::add_event_source(Rc::clone(&tracker) as Rc<dyn extensions::EventSource>);
    tracker
}

/// [`install_with`] the mixer on LazyOS, a silent clock elsewhere.
pub fn install(report: Option<Reporter>) -> Rc<Tracker> {
    let on_lazyos = rhai_lazy::msg::gate::Gate::detect().is_some();
    let output = if on_lazyos {
        Output::lazyos()
    } else {
        Output::silent()
    };
    install_with(output, report)
}

/// The interfaces scripts need to play sound: `os.lazy.audio.v1` when any
/// of them starts a deck. Used to derive a packaged app's permissions; a name
/// built at run time cannot be seen, which is why the call itself is the
/// marker.
pub fn script_interfaces<'a>(scripts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let plays = scripts
        .into_iter()
        .any(|script| script.contains("modplay::play"));
    if plays {
        vec![AUDIO_INTERFACE.to_owned()]
    } else {
        Vec::new()
    }
}
