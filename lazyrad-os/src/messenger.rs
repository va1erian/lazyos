//! Messenger for LazyRAD scripts: `msg` and the generated `sys::*` modules
//! (`docs/rhai/msg.md`, `libs/rhai-lazy/api/README.md`).
//!
//! The player registers them as a LazyRAD script extension, so every form
//! script can call system services, subscribe to topics and serve methods:
//!
//! ```rhai
//! fn form_load() {
//!     theme_label.text = sys::confd::get("sys/ui/theme").str_value;
//!     sys::confd::on_changed("sys/ui/#", |e| theme_label.text = e.payload.path);
//! }
//! ```
//!
//! Every engine the runtime builds (one per form) shares one [`Fabric`], so a
//! service is resolved once per process and the generated modules are
//! compiled once. Each engine is bound to its form, and [`MessengerEvents`]
//! is the event source the form's window polls: handlers run on the UI thread
//! between window messages, and a closed window's subscriptions and served
//! names are released (`docs/lazyrad-messenger-plan.md` §2.2).
//!
//! Scripts call with the player's own credentials. An installed app gets the
//! interfaces and topics its manifest declares, which Make LazyOS App derives
//! from the scripts ([`crate::platform`], `rhai_lazy::msg::permissions`).

use std::rc::Rc;

use lazyrad_runtime::extensions::{self, EventSource, ScriptCall};
use rhai::EvalAltResult;
use rhai_lazy::msg::{self, Bus, Fabric};

/// How often a form with live subscriptions or services polls them. Short
/// enough that a change feels immediate, long enough that an idle form with a
/// subscription costs little.
pub const POLL_INTERVAL_MS: u32 = 50;

/// Told after a poll ran Messenger handlers without error (the player's
/// `LRPLAY:MSGEVENT:PASS` marker).
pub type HandledObserver = Rc<dyn Fn()>;

/// The fabric as a LazyRAD event source.
pub struct MessengerEvents {
    fabric: Rc<Fabric>,
    observer: Option<HandledObserver>,
}

impl MessengerEvents {
    pub fn new(fabric: Rc<Fabric>, observer: Option<HandledObserver>) -> MessengerEvents {
        MessengerEvents { fabric, observer }
    }
}

impl EventSource for MessengerEvents {
    fn interval_ms(&self) -> u32 {
        POLL_INTERVAL_MS
    }

    fn active(&self, form: &str) -> bool {
        self.fabric.active(form)
    }

    fn poll(&self, form: &str, call: &mut ScriptCall<'_>) -> Vec<Box<EvalAltResult>> {
        let pumped = self.fabric.pump(form, call);
        if pumped.handled > 0 && pumped.errors.is_empty() {
            if let Some(observer) = &self.observer {
                observer();
            }
        }
        if pumped.suppressed > 0 {
            eprintln!(
                "lrplay: {form}: {} more handler error(s) like the one shown",
                pumped.suppressed
            );
        }
        pumped.errors
    }

    fn release(&self, form: &str) {
        self.fabric.release(form);
    }
}

/// Installs `msg` and `sys::*` into every LazyRAD engine built on this
/// thread, over `bus`, and registers the event source that delivers their
/// events (`observer` is told when handlers ran). Returns the shared fabric.
pub fn install_over(bus: Rc<dyn Bus>, observer: Option<HandledObserver>) -> Rc<Fabric> {
    let fabric = Rc::new(Fabric::new(bus));
    let for_engines = Rc::clone(&fabric);
    extensions::add_scoped(move |engine, scope| {
        if let Err(error) = msg::install_hosted(engine, &for_engines, scope.form) {
            eprintln!("lrplay: the sys::* modules are unavailable: {error}");
        }
    });
    extensions::add_event_source(Rc::new(MessengerEvents::new(Rc::clone(&fabric), observer)));
    fabric
}

/// Installs Messenger when this process runs on LazyOS; `false` elsewhere (a
/// host build), where scripts simply have no `msg` or `sys` modules.
pub fn install(observer: Option<HandledObserver>) -> bool {
    match msg::gate::Gate::detect() {
        Some(gate) => {
            install_over(Rc::new(gate), observer);
            true
        }
        None => false,
    }
}
