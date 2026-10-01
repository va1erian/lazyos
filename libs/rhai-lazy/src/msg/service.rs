//! `Service`: a connected Messenger interface as a Rhai value.
//!
//! `msg::connect("os.lazy.confd.v1")` resolves the service once and returns a
//! [`Service`]. Every method of its interface is then callable two ways:
//!
//! * generically: `svc.invoke("Get", ["sys/theme"])` (positional array),
//!   `svc.invoke("Set", #{ path: "x", value: v })` (named map), `svc.invoke("Info")`;
//! * as sugar: `svc.get("sys/theme")`, `svc.info()`; the `snake_case` method
//!   name with positional arguments, or a single map of named arguments.
//!
//! A reply with one value returns that value, several return a map, none
//! return `()`. One-way methods return `()` once the message is queued.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::fmt;

use rhai::{Dynamic, EvalAltResult, Map, Position};

use super::bus::{errno_name, Bus, BusError};
use super::codec;
use super::schema::{Interface, Method};

pub(crate) type Fallible<T> = Result<T, alloc::boxed::Box<EvalAltResult>>;

/// Stand-in for error text when the method lookup itself is what failed.
const NO_METHOD: Method = Method {
    name: "?",
    id: 0,
    oneway: false,
    doc: "",
    params: &[],
    returns: &[],
};

/// Default time a call waits for its reply.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

pub(crate) fn script_error(message: impl Into<String>) -> alloc::boxed::Box<EvalAltResult> {
    EvalAltResult::ErrorRuntime(Dynamic::from(message.into()), Position::NONE).into()
}

/// The process's view of the fabric: the bus plus the resolved endpoints,
/// which must never be closed (see [`Bus::resolve`]).
pub struct Fabric {
    bus: Rc<dyn Bus>,
    endpoints: RefCell<Vec<(String, u64)>>,
    timeout_ms: Cell<u64>,
    /// What `msg::run` waits on (`msg::on`, `msg::serve`).
    pub(crate) sources: RefCell<Vec<super::runloop::Source>>,
    /// Set by `msg::stop()`; cleared when `msg::run` starts.
    pub(crate) stop: Cell<bool>,
}

impl Fabric {
    pub fn new(bus: Rc<dyn Bus>) -> Self {
        Self {
            bus,
            endpoints: RefCell::new(Vec::new()),
            timeout_ms: Cell::new(DEFAULT_TIMEOUT_MS),
            sources: RefCell::new(Vec::new()),
            stop: Cell::new(false),
        }
    }

    pub fn bus(&self) -> &dyn Bus {
        &*self.bus
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.get()
    }

    pub fn set_timeout_ms(&self, ms: u64) {
        self.timeout_ms.set(ms);
    }

    /// The cached endpoint for `name`, resolving it on first use.
    pub fn endpoint(&self, name: &str) -> Result<u64, BusError> {
        if let Some((_, handle)) = self.endpoints.borrow().iter().find(|(n, _)| n == name) {
            return Ok(*handle);
        }
        let handle = self.bus.resolve(name)?;
        self.endpoints.borrow_mut().push((name.to_string(), handle));
        Ok(handle)
    }

    /// Drop a dead endpoint so the next call resolves a restarted service.
    fn forget(&self, name: &str) {
        self.endpoints.borrow_mut().retain(|(n, _)| n != name);
    }

    /// Run `op` on `name`'s endpoint; a dead peer is re-resolved once.
    fn with_endpoint<T>(
        &self,
        name: &str,
        op: impl Fn(u64) -> Result<T, BusError>,
    ) -> Result<T, BusError> {
        match op(self.endpoint(name)?) {
            Err(error) if error.is_dead_peer() => {
                self.forget(name);
                op(self.endpoint(name)?)
            }
            other => other,
        }
    }
}

/// A connected interface. Cheap to clone; endpoints are cached in the fabric.
#[derive(Clone)]
pub struct Service {
    pub(crate) interface: &'static Interface,
    pub(crate) name: String,
    pub(crate) fabric: Rc<Fabric>,
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Service({} at {})", self.interface.name, self.name)
    }
}

impl Service {
    /// Resolve `interface`'s service: `service` when given, else the name
    /// without `.vN`, then the full interface name.
    pub fn connect(fabric: Rc<Fabric>, interface: &str, service: Option<&str>) -> Fallible<Self> {
        let iface = super::schema::interface(interface).ok_or_else(|| {
            script_error(format!(
                "msg::connect: unknown interface `{interface}` (see msg::interfaces())"
            ))
        })?;
        let candidates: Vec<&str> = match service {
            Some(name) => alloc::vec![name],
            None if iface.default_service() != iface.name => {
                alloc::vec![iface.default_service(), iface.name]
            }
            None => alloc::vec![iface.name],
        };
        let mut last = None;
        for name in &candidates {
            match fabric.endpoint(name) {
                Ok(_) => {
                    return Ok(Self {
                        interface: iface,
                        name: (*name).to_string(),
                        fabric,
                    })
                }
                Err(error) => last = Some(error),
            }
        }
        let reason = last.map_or(String::new(), |e| e.message);
        Err(script_error(format!(
            "msg::connect: no service for {interface} (tried {}): {reason}",
            candidates.join(", ")
        )))
    }

    fn method(&self, name: &str) -> Fallible<&'static Method> {
        self.interface.method(name).ok_or_else(|| {
            let known: Vec<&str> = self.interface.methods.iter().map(|m| m.name).collect();
            script_error(format!(
                "{} has no method `{name}` (methods: {})",
                self.interface.name,
                known.join(", ")
            ))
        })
    }

    fn fail(&self, method: &Method, detail: impl fmt::Display) -> alloc::boxed::Box<EvalAltResult> {
        script_error(format!("{}.{}: {detail}", self.interface.name, method.name))
    }

    /// Encode `args`: a map is named arguments, an array positional ones,
    /// `()` means none. A single non-map, non-array value is the one argument.
    fn encode(&self, method: &Method, args: Dynamic) -> Fallible<Vec<u8>> {
        let result = if args.is_unit() {
            codec::encode_positional(self.interface, method.params, &[])
        } else if let Some(map) = args.read_lock::<Map>() {
            if method.params.len() == 1 && !map.keys().any(|k| k == method.params[0].name) {
                // A struct passed as the one argument.
                codec::encode_positional(
                    self.interface,
                    method.params,
                    core::slice::from_ref(&args),
                )
            } else {
                codec::encode_named(self.interface, method.params, &map, 0)
            }
        } else if let Some(items) = args.read_lock::<rhai::Array>() {
            codec::encode_positional(self.interface, method.params, &items)
        } else {
            codec::encode_positional(self.interface, method.params, core::slice::from_ref(&args))
        };
        result.map_err(|e| self.fail(method, e))
    }

    /// Call `method` with already-shaped `args` (see [`Service::encode`]).
    pub fn invoke(&self, method: &str, args: Dynamic) -> Fallible<Dynamic> {
        let timeout = self.fabric.timeout_ms();
        self.invoke_within(method, args, timeout)?.ok_or_else(|| {
            self.fail(
                self.method(method).unwrap_or(&NO_METHOD),
                BusError::errno(-110),
            )
        })
    }

    /// [`Service::invoke`] with an explicit timeout; `Ok(None)` when the
    /// reply did not arrive in time (a topic pull that found no event).
    pub fn invoke_within(
        &self,
        method: &str,
        args: Dynamic,
        timeout_ms: u64,
    ) -> Fallible<Option<Dynamic>> {
        let method = self.method(method)?;
        let body = self.encode(method, args)?;
        let fabric = &self.fabric;
        if method.oneway {
            fabric
                .with_endpoint(&self.name, |ep| {
                    fabric.bus().send(ep, self.interface.id, method.id, &body)
                })
                .map_err(|e| self.fail(method, e))?;
            return Ok(Some(Dynamic::UNIT));
        }
        let reply = match fabric.with_endpoint(&self.name, |ep| {
            fabric
                .bus()
                .call(ep, self.interface.id, method.id, &body, timeout_ms)
        }) {
            Ok(reply) => reply,
            Err(error) if error.is_timeout() => return Ok(None),
            Err(error) => return Err(self.fail(method, error)),
        };
        if let Some((code, message)) = codec::reply_error(&reply) {
            let name = errno_name(code.into()).map_or("error", |(name, _)| name);
            return Err(self.fail(method, format!("{message} ({name}, code {code})")));
        }
        let mut map = codec::decode_named(self.interface, method.returns, &reply, 0)
            .map_err(|e| self.fail(method, format!("bad reply: {e}")))?;
        Ok(Some(match method.returns {
            [] => Dynamic::UNIT,
            [only] => map.remove(only.name).unwrap_or(Dynamic::UNIT),
            _ => Dynamic::from_map(map),
        }))
    }

    /// Like [`Service::invoke`] but refuses a method that expects a reply,
    /// so a script cannot silently drop one.
    pub fn invoke_oneway(&self, method: &str, args: Dynamic) -> Fallible<Dynamic> {
        let def = self.method(method)?;
        if !def.oneway {
            return Err(self.fail(def, "not a one-way method; use invoke()"));
        }
        self.invoke(method, args)
    }

    /// `Echo(text: String, count: U32) -> (reply: String)` lines for `help`.
    pub fn signatures(&self) -> Vec<String> {
        self.interface.methods.iter().map(signature).collect()
    }
}

/// A method's IDL signature, for `describe` and error messages.
pub fn signature(method: &Method) -> String {
    let list = |fields: &[super::schema::Field]| -> String {
        fields
            .iter()
            .map(|f| format!("{}: {}", f.name, type_name(f.ty)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let tail = if method.oneway { " oneway" } else { "" };
    format!(
        "{}({}) -> ({}){tail}",
        method.name,
        list(method.params),
        list(method.returns)
    )
}

fn type_name(ty: super::schema::Ty) -> String {
    use super::schema::Ty;
    match ty {
        Ty::Array(inner) => format!("Array<{}>", type_name(*inner)),
        Ty::Option(inner) => format!("Option<{}>", type_name(*inner)),
        Ty::Struct(name) | Ty::Enum(name) => name.to_string(),
        other => format!("{other:?}"),
    }
}
