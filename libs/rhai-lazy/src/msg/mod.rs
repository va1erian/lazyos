//! The `msg` module: Messenger from Rhai (step R3 of `docs/rhai-plan.md`).
//!
//! Every interface in `idl/` is scriptable with no hand-written glue: `midlc
//! --schema` compiles the IDL into a data table ([`idl`]), and the generic
//! codec ([`codec`]) turns Rhai values into parcel bodies and back from it.
//!
//! ```rhai
//! let confd = msg::connect("os.lazy.confd.v1");
//! print(confd.info().store_dir);              // method sugar
//! confd.invoke("Get", ["sys/ui/theme"]);        // generic, positional
//! print(msg::describe("os.lazy.echo.v1"));     // signatures and docs
//! ```
//!
//! The module talks to the fabric only through [`Bus`], so it runs against an
//! in-memory fabric in host tests and against the real `int 0x80` gate
//! ([`gate`], feature `lazyos`) in the `rhai` command and the LazyRAD player.
//! Scripts run with the process's own credentials: the kernel and the services
//! enforce access, and a refusal comes back as a catchable error carrying the
//! service's friendly text.

pub mod bus;
pub mod codec;
#[cfg(all(feature = "lazyos", target_arch = "x86_64"))]
pub mod gate;
mod idl;
pub mod runloop;
pub mod schema;
pub mod service;
pub mod topics;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::TypeId;

use rhai::{
    Array, Dynamic, Engine, FnPtr, FuncRegistration, ImmutableString, Map, Module,
    NativeCallContext, INT,
};

pub use bus::{Bus, BusError, Incoming};
pub use service::{Fabric, Service};
pub use topics::Subscription;

use service::{script_error, signature, Fallible};

/// Method names the service object keeps for itself; an IDL method spelled
/// like one is still reachable through `svc.invoke("Name", ...)`.
const RESERVED: &[&str] = &[
    "invoke",
    "invoke_oneway",
    "call",
    "interface",
    "service",
    "methods",
    "type_of",
];

/// Register one function in the `msg::` namespace. Impure and volatile, so
/// the optimizer never folds a fabric call away.
macro_rules! register {
    ($module:expr, $name:literal, $func:expr) => {
        FuncRegistration::new($name)
            .with_purity(false)
            .with_volatility(true)
            .set_into_module($module, $func);
    };
}

/// A readable reference for one interface: its doc, methods and topics.
pub fn describe(name: &str) -> Option<String> {
    let iface = schema::interface(name)?;
    let mut text = format!("{} (service {})\n", iface.name, iface.default_service());
    if !iface.doc.is_empty() {
        text += &format!("{}\n", iface.doc);
    }
    for method in iface.methods {
        text += &format!("  {}\n", signature(method));
        if let Some(first) = method.doc.lines().next() {
            text += &format!("      {first}\n");
        }
    }
    for topic in iface.topics {
        text += &format!("  topic {} : {}\n", topic.pattern, topic.payload);
    }
    Some(text)
}

/// The `msg::` namespace module.
fn namespace(fabric: &Rc<Fabric>) -> Module {
    let mut m = Module::new();
    register!(&mut m, "interfaces", || -> Array {
        schema::interfaces()
            .iter()
            .map(|i| Dynamic::from(String::from(i.name)))
            .collect()
    });
    let f = fabric.clone();
    register!(&mut m, "services", move || -> Fallible<Array> {
        let mut names = f
            .bus()
            .names()
            .map_err(|e| script_error(format!("msg::services: {e}")))?;
        names.sort();
        Ok(names.into_iter().map(Dynamic::from).collect())
    });
    register!(
        &mut m,
        "describe",
        |name: ImmutableString| -> Fallible<String> {
            describe(&name)
                .ok_or_else(|| script_error(format!("msg::describe: unknown interface `{name}`")))
        }
    );
    let f = fabric.clone();
    register!(
        &mut m,
        "connect",
        move |iface: ImmutableString| -> Fallible<Service> {
            Service::connect(f.clone(), &iface, None)
        }
    );
    let f = fabric.clone();
    register!(&mut m, "connect", move |iface: ImmutableString,
                                       service: ImmutableString|
          -> Fallible<Service> {
        Service::connect(f.clone(), &iface, Some(&service))
    });
    let f = fabric.clone();
    register!(&mut m, "timeout", move || -> INT { f.timeout_ms() as INT });
    let f = fabric.clone();
    register!(&mut m, "set_timeout", move |ms: INT| -> Fallible<()> {
        let ms = u64::try_from(ms).map_err(|_| script_error("msg::set_timeout: negative"))?;
        f.set_timeout_ms(ms);
        Ok(())
    });
    register_topics(&mut m, fabric);
    register_loop(&mut m, fabric);
    m
}

/// `msg::publish`, `msg::subscribe`.
fn register_topics(m: &mut Module, fabric: &Rc<Fabric>) {
    let f = fabric.clone();
    register!(
        m,
        "subscribe",
        move |filter: ImmutableString| -> Fallible<Subscription> {
            topics::subscribe(&f, &filter, &Map::new())
        }
    );
    let f = fabric.clone();
    register!(m, "subscribe", move |filter: ImmutableString,
                                    opts: Map|
          -> Fallible<Subscription> {
        topics::subscribe(&f, &filter, &opts)
    });
    let f = fabric.clone();
    register!(m, "publish", move |topic: ImmutableString,
                                  value: Dynamic|
          -> Fallible<INT> {
        topics::publish(&f, &topic, &value, None)
    });
    let f = fabric.clone();
    register!(m, "publish", move |topic: ImmutableString,
                                  value: Dynamic,
                                  retained: bool|
          -> Fallible<INT> {
        topics::publish(&f, &topic, &value, Some(retained))
    });
}

/// `msg::on`, `msg::serve`, `msg::run`, `msg::stop`.
fn register_loop(m: &mut Module, fabric: &Rc<Fabric>) {
    let f = fabric.clone();
    register!(m, "on", move |filter: ImmutableString,
                             handler: FnPtr|
          -> Fallible<Subscription> {
        runloop::on(&f, &filter, &Map::new(), handler)
    });
    let f = fabric.clone();
    register!(m, "on", move |filter: ImmutableString,
                             opts: Map,
                             handler: FnPtr|
          -> Fallible<Subscription> {
        runloop::on(&f, &filter, &opts, handler)
    });
    let f = fabric.clone();
    register!(m, "serve", move |name: ImmutableString,
                                interface: ImmutableString,
                                handlers: Map|
          -> Fallible<()> {
        runloop::serve(&f, &name, &interface, handlers)
    });
    let f = fabric.clone();
    register!(m, "serve", move |interface: ImmutableString,
                                handlers: Map|
          -> Fallible<()> {
        let name =
            schema::interface(&interface).map_or(interface.as_str(), |i| i.default_service());
        runloop::serve(&f, name, &interface, handlers)
    });
    let f = fabric.clone();
    register!(m, "run", move |ctx: NativeCallContext| -> Fallible<INT> {
        runloop::run(&ctx, &f, None)
    });
    let f = fabric.clone();
    register!(m, "run", move |ctx: NativeCallContext,
                              ms: INT|
          -> Fallible<INT> {
        let ms = u64::try_from(ms).map_err(|_| script_error("msg::run: negative duration"))?;
        runloop::run(&ctx, &f, Some(ms))
    });
    let f = fabric.clone();
    register!(m, "stop", move || f.stop.set(true));
}

/// `sub.next([ms])`, `sub.ack(seq)`, `sub.close()` and the getters.
fn register_subscription_type(engine: &mut Engine, fabric: &Rc<Fabric>) {
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
            s.next(ms)
        });
    let f = fabric.clone();
    engine.register_fn("next", move |s: &mut Subscription| s.next(f.timeout_ms()));
}

/// `svc.invoke(..)`, `svc.invoke_oneway(..)`, the getters and printing.
fn register_service_type(engine: &mut Engine) {
    engine
        .register_type_with_name::<Service>("Service")
        .register_get("interface", |s: &mut Service| {
            String::from(s.interface.name)
        })
        .register_get("service", |s: &mut Service| s.name.clone())
        .register_get("methods", |s: &mut Service| -> Array {
            s.interface
                .methods
                .iter()
                .map(|m| Dynamic::from(String::from(m.name)))
                .collect()
        })
        .register_fn("to_string", |s: &mut Service| format!("{s:?}"))
        .register_fn("to_debug", |s: &mut Service| format!("{s:?}"));
    engine.register_fn("invoke", |s: &mut Service, method: ImmutableString| {
        s.invoke(&method, Dynamic::UNIT)
    });
    engine.register_fn(
        "invoke",
        |s: &mut Service, method: ImmutableString, args: Dynamic| s.invoke(&method, args),
    );
    engine.register_fn(
        "invoke_oneway",
        |s: &mut Service, method: ImmutableString| s.invoke_oneway(&method, Dynamic::UNIT),
    );
    engine.register_fn(
        "invoke_oneway",
        |s: &mut Service, method: ImmutableString, args: Dynamic| s.invoke_oneway(&method, args),
    );
}

/// One `svc.<snake_name>(args...)` per method name and arity in the schema.
/// The receiver's interface picks the method at run time, so a name shared
/// by several interfaces (`ping`) is one function.
fn register_sugar(engine: &mut Engine) {
    let mut seen: Vec<(String, usize)> = Vec::new();
    for iface in schema::interfaces() {
        for method in iface.methods {
            let name = schema::snake_case(method.name);
            if RESERVED.contains(&name.as_str()) {
                continue;
            }
            // Positional, plus the single-map form for named arguments.
            for arity in [method.params.len(), 1] {
                if seen.iter().any(|(n, a)| *n == name && *a == arity) {
                    continue;
                }
                seen.push((name.clone(), arity));
                let mut types = alloc::vec![TypeId::of::<Service>()];
                types.extend(core::iter::repeat_n(TypeId::of::<Dynamic>(), arity));
                let method_name = name.clone();
                engine.register_raw_fn(name.clone(), types, move |_ctx, args| {
                    let service = args[0].clone_cast::<Service>();
                    let rest: Array = args[1..].iter().map(|a| (**a).clone()).collect();
                    let shaped = match rest.as_slice() {
                        [single] if single.is::<Map>() => single.clone(),
                        _ => Dynamic::from_array(rest),
                    };
                    service.invoke(&method_name, shaped)
                });
            }
        }
    }
}

/// Install `msg::*` and the `Service` type on `engine`, bound to `bus`.
pub fn install(engine: &mut Engine, bus: Rc<dyn Bus>) -> Rc<Fabric> {
    let fabric = Rc::new(Fabric::new(bus));
    engine.register_static_module("msg", namespace(&fabric).into());
    register_service_type(engine);
    register_subscription_type(engine, &fabric);
    register_sugar(engine);
    fabric
}

/// Interface names, for completion and `help`.
pub fn interface_names() -> Vec<String> {
    schema::interfaces()
        .iter()
        .map(|i| i.name.to_string())
        .collect()
}
