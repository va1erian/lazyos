//! The `msg::` functions that subscribe, publish, serve and wait, and the
//! `Subscription` type.
//!
//! They are bound to a [`Binding`]: the fabric, the owner sources are filed
//! under (a LazyRAD form's name, empty for the `rhai` command) and whether a
//! host drives events (`super::events`). In a hosted engine `msg::run()`
//! refuses, because the window already delivers events and blocking would
//! stop it painting.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;

use rhai::{Dynamic, Engine, FnPtr, FuncRegistration, ImmutableString, Map, Module, NativeCallContext, INT};

use super::bus::Wait;
use super::runloop;
use super::schema;
use super::service::{script_error, Fabric, Fallible};
use super::topics::{self, Subscription};

/// Who an engine's `msg` acts for.
#[derive(Clone)]
pub(crate) struct Binding {
    pub fabric: Rc<Fabric>,
    pub owner: Rc<str>,
    /// Events arrive through a host's pump, not `msg::run`.
    pub hosted: bool,
}

/// `msg::publish`, `msg::subscribe`.
pub(crate) fn register_topics(m: &mut Module, binding: &Binding) {
    let f = binding.fabric.clone();
    register!(
        m,
        "subscribe",
        move |filter: ImmutableString| -> Fallible<Subscription> {
            topics::subscribe(&f, &filter, &Map::new())
        }
    );
    let f = binding.fabric.clone();
    register!(m, "subscribe", move |filter: ImmutableString,
                                    opts: Map|
          -> Fallible<Subscription> {
        topics::subscribe(&f, &filter, &opts)
    });
    let f = binding.fabric.clone();
    register!(m, "publish", move |topic: ImmutableString,
                                  value: Dynamic|
          -> Fallible<INT> {
        topics::publish(&f, &topic, &value, None)
    });
    let f = binding.fabric.clone();
    register!(m, "publish", move |topic: ImmutableString,
                                  value: Dynamic,
                                  retained: bool|
          -> Fallible<INT> {
        topics::publish(&f, &topic, &value, Some(retained))
    });
}

/// `msg::on`, `msg::serve`, `msg::run`, `msg::stop`.
pub(crate) fn register_loop(m: &mut Module, binding: &Binding) {
    let b = binding.clone();
    register!(m, "on", move |filter: ImmutableString,
                             handler: FnPtr|
          -> Fallible<Subscription> {
        runloop::on(&b.fabric, &b.owner, &filter, &Map::new(), handler)
    });
    let b = binding.clone();
    register!(m, "on", move |filter: ImmutableString,
                             opts: Map,
                             handler: FnPtr|
          -> Fallible<Subscription> {
        runloop::on(&b.fabric, &b.owner, &filter, &opts, handler)
    });
    let b = binding.clone();
    register!(m, "serve", move |name: ImmutableString,
                                interface: ImmutableString,
                                handlers: Map|
          -> Fallible<()> {
        runloop::serve(&b.fabric, &b.owner, &name, &interface, handlers)
    });
    let b = binding.clone();
    register!(m, "serve", move |interface: ImmutableString,
                                handlers: Map|
          -> Fallible<()> {
        let name =
            schema::interface(&interface).map_or(interface.as_str(), |i| i.default_service());
        runloop::serve(&b.fabric, &b.owner, name, &interface, handlers)
    });
    let b = binding.clone();
    register!(m, "run", move |ctx: NativeCallContext| -> Fallible<INT> {
        run(&ctx, &b, None)
    });
    let b = binding.clone();
    register!(m, "run", move |ctx: NativeCallContext,
                              ms: INT|
          -> Fallible<INT> {
        let ms = u64::try_from(ms).map_err(|_| script_error("msg::run: negative duration"))?;
        run(&ctx, &b, Some(ms))
    });
    let f = binding.fabric.clone();
    register!(m, "stop", move || f.stop.set(true));
}

fn run(ctx: &NativeCallContext, binding: &Binding, budget_ms: Option<u64>) -> Fallible<INT> {
    if binding.hosted {
        return Err(script_error(
            "msg::run: not needed here; msg::on and msg::serve handlers already run \
             while the window is open",
        ));
    }
    runloop::run(ctx, &binding.fabric, &binding.owner, budget_ms)
}

/// `sub.next([ms])`, `sub.ack(seq)`, `sub.close()` and the getters.
pub(crate) fn register_subscription_type(engine: &mut Engine, fabric: &Rc<Fabric>) {
    engine
        .register_type_with_name::<Subscription>("Subscription")
        .register_get("filter", |s: &mut Subscription| s.filter.clone())
        .register_get("id", |s: &mut Subscription| s.id)
        .register_fn("to_string", |s: &mut Subscription| format!("{s:?}"))
        .register_fn("to_debug", |s: &mut Subscription| format!("{s:?}"))
        .register_fn("ack", |s: &mut Subscription, seq: INT| s.ack(seq))
        .register_fn("close", |s: &mut Subscription| s.close())
        .register_fn("next", |s: &mut Subscription, ms: INT| {
            let ms = u64::try_from(ms).map_err(|_| script_error("next: negative timeout"))?;
            s.next(Wait::from_timeout_ms(ms))
        });
    let f = fabric.clone();
    engine.register_fn("next", move |s: &mut Subscription| {
        s.next(Wait::from_timeout_ms(f.timeout_ms()))
    });
}

/// The owner text for an engine bound to `owner`.
pub(crate) fn owner(owner: &str) -> Rc<str> {
    Rc::from(String::from(owner))
}
