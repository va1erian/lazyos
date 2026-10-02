//! The event loop: `msg::on(filter, |event| ...)`, `msg::serve(...)` and
//! `msg::run()`.
//!
//! Rhai is single-threaded, so a script that reacts to the fabric registers
//! *sources* (topic subscriptions with a handler, served endpoints with a
//! handler per method) and then hands control to `msg::run()`, which waits on
//! all of them and calls the handlers. A handler can end the loop with
//! `msg::stop()`; `msg::run(ms)` also ends after `ms` milliseconds.
//!
//! Serving: `msg::serve(name, interface, #{ Echo: |text, count| ... })`
//! registers `name` with the kernel and answers each call by running the
//! method's handler with the decoded arguments (in IDL order). The handler's
//! value is the reply (one return value, or a map for several). A handler that
//! throws answers with a structured error the caller sees as a catchable
//! error: the thrown text with `EIO`, or `#{ code: 13, message: "..." }` for a
//! specific errno. One-way methods run their handler and send nothing back.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libmessenger::Encoder;
use rhai::{Dynamic, EvalAltResult, FnPtr, Map, NativeCallContext, INT};

use super::bus::Incoming;
use super::codec;
use super::schema::{self, Interface, Method};
use super::service::{script_error, Fabric, Fallible};
use super::topics::Subscription;

/// Longest single wait when only one source is registered, so `run(ms)`
/// still notices its own deadline promptly.
const SLICE_MS: u64 = 250;
/// Per-source wait when several are registered: one PIT tick each.
const POLL_MS: u64 = 10;
/// The structured-error field id replies use (the services' convention).
const ERROR_FIELD: u16 = 15;
const EIO: u32 = 5;
const ENOSYS: u32 = 38;

/// Something `msg::run` waits on.
pub enum Source {
    Topic {
        subscription: Subscription,
        handler: FnPtr,
    },
    Service {
        name: String,
        endpoint: u64,
        interface: &'static Interface,
        handlers: Vec<(&'static Method, FnPtr)>,
    },
}

/// `msg::on(filter, handler)`: subscribe and remember the handler.
pub fn on(fabric: &Rc<Fabric>, filter: &str, opts: &Map, handler: FnPtr) -> Fallible<Subscription> {
    let subscription = super::topics::subscribe(fabric, filter, opts)?;
    fabric.sources.borrow_mut().push(Source::Topic {
        subscription: subscription.clone(),
        handler,
    });
    Ok(subscription)
}

/// `msg::serve(name, interface, handlers)`: register and remember.
pub fn serve(fabric: &Rc<Fabric>, name: &str, interface: &str, handlers: Map) -> Fallible<()> {
    let iface = schema::interface(interface)
        .ok_or_else(|| script_error(format!("msg::serve: unknown interface `{interface}`")))?;
    let mut table = Vec::new();
    for (key, value) in handlers {
        let method = iface.method(&key).ok_or_else(|| {
            script_error(format!("msg::serve: {} has no method `{key}`", iface.name))
        })?;
        let handler = value.try_cast::<FnPtr>().ok_or_else(|| {
            script_error(format!(
                "msg::serve: the handler for `{key}` is not a function"
            ))
        })?;
        table.push((method, handler));
    }
    let endpoint = fabric
        .bus()
        .register(name, &[iface.id])
        .map_err(|e| script_error(format!("msg::serve: cannot register `{name}`: {e}")))?;
    fabric.sources.borrow_mut().push(Source::Service {
        name: name.into(),
        endpoint,
        interface: iface,
        handlers: table,
    });
    Ok(())
}

/// What one pass over a source produced, taken out of the source list so no
/// borrow is held while a handler runs (a handler may call `msg::on` too).
enum Work {
    Event(Subscription, FnPtr, Dynamic),
    Request(
        &'static Interface,
        Option<(&'static Method, FnPtr)>,
        Incoming,
    ),
}

fn poll(fabric: &Fabric, index: usize, wait_ms: u64) -> Fallible<Option<Work>> {
    let sources = fabric.sources.borrow();
    let Some(source) = sources.get(index) else {
        return Ok(None);
    };
    match source {
        Source::Topic {
            subscription,
            handler,
        } => {
            let (subscription, handler) = (subscription.clone(), handler.clone());
            drop(sources);
            let event = subscription.next(wait_ms)?;
            Ok((!event.is_unit()).then(|| Work::Event(subscription, handler, event)))
        }
        Source::Service {
            name,
            endpoint,
            interface,
            handlers,
        } => {
            let incoming = fabric
                .bus()
                .recv(*endpoint, wait_ms)
                .map_err(|e| script_error(format!("msg::run: {name}: {e}")))?;
            Ok(incoming.map(|request| {
                let handler = interface
                    .method_by_id(request.method)
                    .and_then(|m| handlers.iter().find(|(h, _)| h.id == m.id).cloned());
                Work::Request(interface, handler, request)
            }))
        }
    }
}

/// The error a thrown value becomes on the wire: `(code, message)`.
fn thrown(error: &EvalAltResult) -> (u32, String) {
    // The throw happened inside the handler closure: look past the call frame.
    let error = error.unwrap_inner();
    if let EvalAltResult::ErrorRuntime(value, _) = error {
        if let Some(map) = value.read_lock::<Map>() {
            let code = map
                .get("code")
                .and_then(|c| c.as_int().ok())
                .unwrap_or(EIO as INT);
            let message = map
                .get("message")
                .map_or_else(String::new, |m| m.to_string());
            return (u32::try_from(code).unwrap_or(EIO), message);
        }
        return (EIO, value.to_string());
    }
    (EIO, error.to_string())
}

fn error_body(code: u32, message: &str) -> Vec<u8> {
    let mut body = Encoder::new();
    // A message too long for a body is cut rather than lost.
    let message: String = message.chars().take(512).collect();
    let _ = body.error(ERROR_FIELD, code, &message);
    body.finish()
}

/// Answer one request: decode, run the handler, encode the reply or error.
fn answer(
    ctx: &NativeCallContext,
    iface: &'static Interface,
    handler: Option<(&'static Method, FnPtr)>,
    request: &Incoming,
) -> Vec<u8> {
    let Some((method, handler)) = handler else {
        return error_body(ENOSYS, "this Rhai service does not handle that method");
    };
    let args = match codec::decode_named(iface, method.params, &request.body, 0) {
        Ok(mut map) => method
            .params
            .iter()
            .map(|p| map.remove(p.name).unwrap_or(Dynamic::UNIT))
            .collect::<Vec<_>>(),
        Err(e) => return error_body(22, &format!("bad arguments: {e}")),
    };
    let result: Result<Dynamic, _> = handler.call_within_context(ctx, args);
    let value = match result {
        Ok(value) => value,
        Err(error) => {
            let (code, message) = thrown(&error);
            return error_body(code, &message);
        }
    };
    let encoded = match method.returns {
        [] => Ok(Vec::new()),
        [_] => codec::encode_positional(iface, method.returns, core::slice::from_ref(&value)),
        _ => match value.read_lock::<Map>() {
            Some(map) => codec::encode_named(iface, method.returns, &map, 0),
            None => Err(format!(
                "the handler must return a map of {} values",
                method.returns.len()
            )),
        },
    };
    encoded.unwrap_or_else(|e| {
        error_body(
            EIO,
            &format!("{}: bad reply from the handler: {e}", method.name),
        )
    })
}

fn handle(ctx: &NativeCallContext, fabric: &Fabric, work: Work) -> Fallible<()> {
    match work {
        Work::Event(subscription, handler, event) => {
            let sequence = event
                .read_lock::<Map>()
                .and_then(|m| m.get("sequence").and_then(|s| s.as_int().ok()));
            // A topic handler's value has no reader.
            let _ = handler.call_within_context::<Dynamic>(ctx, (event,))?;
            if let (true, Some(sequence)) = (subscription.is_reliable(), sequence) {
                subscription.ack(sequence)?;
            }
            Ok(())
        }
        Work::Request(iface, handler, request) => {
            let body = answer(ctx, iface, handler, &request);
            if let Some(txn) = request.txn {
                fabric
                    .bus()
                    .reply(txn, request.interface, request.method, &body)
                    .map_err(|e| script_error(format!("msg::run: reply: {e}")))?;
            }
            Ok(())
        }
    }
}

/// `msg::run([ms])`: wait on every source and run handlers until `msg::stop()`
/// or, with `ms`, until that much time has passed. Returns the number of
/// events and requests handled.
pub fn run(ctx: &NativeCallContext, fabric: &Rc<Fabric>, budget_ms: Option<u64>) -> Fallible<INT> {
    let now_ms = || fabric.bus().clock_ms();
    if fabric.sources.borrow().is_empty() {
        return Err(script_error(
            "msg::run: nothing to wait for (use msg::on or msg::serve first)",
        ));
    }
    fabric.stop.set(false);
    let started = now_ms();
    let mut handled: INT = 0;
    loop {
        let count = fabric.sources.borrow().len();
        for index in 0..count {
            let remaining = budget_ms.map(|b| b.saturating_sub(now_ms().saturating_sub(started)));
            if fabric.stop.get() || remaining == Some(0) {
                return Ok(handled);
            }
            let slice = if count == 1 { SLICE_MS } else { POLL_MS };
            let wait = remaining.map_or(slice, |r| r.min(slice)).max(1);
            if let Some(work) = poll(fabric, index, wait)? {
                handle(ctx, fabric, work)?;
                handled += 1;
            }
        }
    }
}
