//! The decks a form opened, kept playing by the form's window.
//!
//! [`Tracker`] is a LazyRAD [`EventSource`]: while a form has a deck playing,
//! its window polls; each poll tops the deck's sink up and moves what the
//! form sees to the frame being heard, then runs the script's `on_update`
//! handler (at most every [`UPDATE_MS`]) and, once a song has played out, its
//! `on_end` handler. Closing the window stops the form's decks.
//!
//! Handlers never run while a deck is borrowed: they may read the deck,
//! change it, or start another one.

use std::cell::RefCell;
use std::rc::Rc;

use lazyrad_runtime::extensions::{EventSource, ScriptCall};
use rhai::{Dynamic, EvalAltResult, FnPtr};

use super::deck::{Deck, Snapshot, State};
use super::sink::Clock;

/// How often a playing form polls. The sink buffers far longer than this,
/// so the interval only sets how smooth the meters look.
pub const POLL_INTERVAL_MS: u32 = 40;

/// The shortest gap between two `on_update` calls for one deck, unless the
/// deck changed state in between.
pub const UPDATE_MS: u64 = 80;

/// Told about deck life events: `(stage, detail)` for the serial markers.
pub type Reporter = Rc<dyn Fn(&str, &str)>;

/// Which of a deck's handlers a call is for.
#[derive(Clone, Copy)]
enum Handler {
    Update,
    End,
}

/// One deck and the script's view of it.
pub struct Entry {
    pub deck: Deck,
    pub form: String,
    pub on_update: Option<FnPtr>,
    pub on_end: Option<FnPtr>,
    /// What the last `on_update` was given, and when (tracker clock).
    delivered: Option<(Snapshot, State, u64)>,
    /// `on_end` ran for the current play (a `seek` starts a new one).
    pub end_delivered: bool,
}

impl Entry {
    pub fn new(deck: Deck, form: &str) -> Entry {
        Entry {
            deck,
            form: form.to_owned(),
            on_update: None,
            on_end: None,
            delivered: None,
            end_delivered: false,
        }
    }
}

/// A script's handle on a deck (`Deck` in Rhai).
#[derive(Clone)]
pub struct DeckHandle(pub Rc<RefCell<Entry>>);

/// Every deck opened on this thread.
pub struct Tracker {
    decks: RefCell<Vec<Rc<RefCell<Entry>>>>,
    clock: Clock,
    report: Option<Reporter>,
}

impl Tracker {
    pub fn new(clock: Clock, report: Option<Reporter>) -> Tracker {
        Tracker {
            decks: RefCell::new(Vec::new()),
            clock,
            report,
        }
    }

    /// Start keeping `entry` playing; returns the script's handle.
    pub fn add(&self, entry: Entry) -> DeckHandle {
        let handle = Rc::new(RefCell::new(entry));
        self.decks.borrow_mut().push(Rc::clone(&handle));
        DeckHandle(handle)
    }

    /// How many decks `form` has open (playing, paused, or ended and still
    /// held by the script).
    pub fn decks(&self, form: &str) -> usize {
        self.of(form).len()
    }

    pub fn report(&self, stage: &str, detail: &str) {
        if let Some(report) = &self.report {
            report(stage, detail);
        }
    }

    fn of(&self, form: &str) -> Vec<Rc<RefCell<Entry>>> {
        self.decks
            .borrow()
            .iter()
            .filter(|entry| entry.borrow().form == form)
            .cloned()
            .collect()
    }

    /// Pump one deck; returns the handlers due, in the order to run them,
    /// and an audio failure to show.
    fn step(
        &self,
        entry: &Rc<RefCell<Entry>>,
        now: u64,
    ) -> (Vec<(Handler, FnPtr)>, Option<String>) {
        let mut entry = entry.borrow_mut();
        let failure = entry.deck.pump().err();
        let state = entry.deck.state();
        let heard = entry.deck.heard();
        let mut due = Vec::new();
        let changed = match entry.delivered {
            None => true,
            Some((snapshot, was, at)) => {
                was != state || (snapshot != heard && now.saturating_sub(at) >= UPDATE_MS)
            }
        };
        if changed {
            due.extend(entry.on_update.clone().map(|f| (Handler::Update, f)));
            entry.delivered = Some((heard, state, now));
        }
        if state == State::Ended && !entry.end_delivered {
            entry.end_delivered = true;
            due.extend(entry.on_end.clone().map(|f| (Handler::End, f)));
            let detail = format!("elapsed_ms={}", entry.deck.elapsed_ms());
            drop(entry);
            self.report("MODEND", &detail);
        }
        (due, failure)
    }
}

impl EventSource for Tracker {
    fn interval_ms(&self) -> u32 {
        POLL_INTERVAL_MS
    }

    fn active(&self, form: &str) -> bool {
        self.of(form).iter().any(|entry| {
            let entry = entry.borrow();
            // An ended deck's `on_end` runs in the poll that notices the
            // end, so only a playing deck needs the timer.
            entry.deck.state() == State::Playing
        })
    }

    fn poll(&self, form: &str, call: &mut ScriptCall<'_>) -> Vec<Box<EvalAltResult>> {
        let now = (self.clock)();
        let mut errors = Vec::new();
        for entry in self.of(form) {
            let (due, failure) = self.step(&entry, now);
            if let Some(failure) = failure {
                self.report("MODPLAY:FAIL", &failure);
                errors.push(failure.into());
            }
            for (kind, handler) in due {
                let deck = Dynamic::from(DeckHandle(Rc::clone(&entry)));
                if let Err(error) = call(&handler, vec![deck]) {
                    // A broken handler would fail on every poll: report it
                    // once and drop it.
                    let mut borrowed = entry.borrow_mut();
                    match kind {
                        Handler::Update => borrowed.on_update = None,
                        Handler::End => borrowed.on_end = None,
                    }
                    errors.push(error);
                }
            }
        }
        // A stopped deck is done. An ended one stays while the script holds
        // it, since `seek` plays it again.
        self.decks
            .borrow_mut()
            .retain(|entry| match entry.borrow().deck.state() {
                State::Playing | State::Paused => true,
                State::Ended => Rc::strong_count(entry) > 1,
                State::Stopped => false,
            });
        errors
    }

    fn release(&self, form: &str) {
        for entry in self.of(form) {
            entry.borrow_mut().deck.stop();
        }
        self.decks
            .borrow_mut()
            .retain(|entry| entry.borrow().form != form);
    }
}
