//! The method table: every method `dbgd` answers, who may call it, and the
//! parameters it takes. Dispatch looks a request up here and
//! [`validate`] checks its `params` before any handler runs, so a handler
//! sees only well-typed, in-range values.

use alloc::string::String;

use crate::json::Value;

/// Who may call a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Before authentication too (`auth`).
    Open,
    /// An authenticated session: read-only inspection.
    Read,
    /// An authenticated session that opened control (`control.begin`) on a
    /// box whose `lazyos.cfg` allows it (`diag.dbg.control=1`): methods that
    /// change the machine (docs/dbgd-plan.md, v2).
    Control,
}

/// The type and bounds of one parameter.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Uint { min: u64, max: u64 },
    Str { max: usize },
    Bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug)]
pub struct Method {
    pub name: &'static str,
    pub access: Access,
    pub summary: &'static str,
    pub params: &'static [Param],
}

const fn uint(name: &'static str, min: u64, max: u64) -> Param {
    Param {
        name,
        kind: Kind::Uint { min, max },
    }
}

const fn text(name: &'static str, max: usize) -> Param {
    Param {
        name,
        kind: Kind::Str { max },
    }
}

/// Most lines one `log.tail` returns.
pub const MAX_TAIL_LINES: u64 = 2000;
/// Most bytes one `fs.read` returns.
pub const MAX_READ: u64 = 32 * 1024;
/// Most bytes of PCI configuration space.
pub const PCI_CONFIG_BYTES: u64 = 4096;

/// Most bytes of a service binary one `service.upload` carries, before
/// base64: its base64 is exactly the JSON reader's longest string
/// ([`crate::json::MAX_STRING`]).
pub const UPLOAD_CHUNK: u64 = 6 * 1024;
/// Base64 characters of one full chunk.
pub const UPLOAD_CHUNK_B64: usize = (UPLOAD_CHUNK as usize).div_ceil(3) * 4;

macro_rules! control {
    ($name:expr, $summary:expr, [$($param:expr),*]) => {
        Method { name: $name, access: Access::Control, summary: $summary, params: &[$($param),*] }
    };
}

macro_rules! read {
    ($name:expr, $summary:expr, [$($param:expr),*]) => {
        Method { name: $name, access: Access::Read, summary: $summary, params: &[$($param),*] }
    };
}

/// Every method, in the order `methods` lists them.
pub const METHODS: &[Method] = &[
    Method {
        name: "auth",
        access: Access::Open,
        summary: "prove knowledge of the key: {mac}",
        params: &[text("mac", 64)],
    },
    read!("ping", "echo the uptime", []),
    read!("methods", "this table", []),
    read!(
        "log.tail",
        "the newest log lines: \"kernel\" (default), \"programs\" (services' own output) or a logd source",
        [uint("lines", 1, MAX_TAIL_LINES), text("source", 64)]
    ),
    read!(
        "log.sources",
        "the logd journals that can be tailed",
        []
    ),
    read!(
        "log.follow",
        "stream new kernel log lines as notifications until log.unfollow",
        [uint("lines", 0, MAX_TAIL_LINES), text("source", 64)]
    ),
    read!("log.unfollow", "stop the log stream", []),
    read!("tasks.list", "the scheduler's task table", []),
    read!("sysinfo", "the system statistics snapshot", []),
    read!("mem.stats", "frame, heap and slab counters", []),
    read!("fabric.stats", "Messenger fabric counters", []),
    read!(
        "msg.registry",
        "Messenger: the registered service names, owners and interfaces",
        []
    ),
    read!(
        "msg.services",
        "Messenger: init's supervised services with state and health",
        []
    ),
    read!(
        "msg.topics",
        "Messenger: the topics the broker has seen, subscribers, retained",
        []
    ),
    read!(
        "msg.topic",
        "Messenger: the retained value of one exact topic: {topic}",
        [text("topic", 128)]
    ),
    read!("devices.list", "the PCI inventory: owner, class, rights", []),
    read!("drivers.list", "devd's view: match, driver, state", []),
    read!(
        "usb.dump",
        "usbd's last controller and device snapshot",
        []
    ),
    read!(
        "fs.read",
        "read a file under an allowlisted root: {path, offset, len}",
        [
            text("path", 256),
            uint("offset", 0, u64::MAX >> 1),
            uint("len", 1, MAX_READ)
        ]
    ),
    read!(
        "hwreport",
        "the HW:* verdict lines of the boot log, as fields",
        []
    ),
    // The control tier (v2): refused unless `diag.dbg.control=1` and the
    // session called `control.begin`.
    Method {
        name: "control.begin",
        access: Access::Read,
        summary: "open control for this session (needs diag.dbg.control=1): {confirm: \"control\"}",
        params: &[text("confirm", 16)],
    },
    control!(
        "service.restart",
        "restart a supervised service as it is: {name}",
        [text("name", crate::control::MAX_NAME)]
    ),
    control!(
        "service.upload",
        "stage a service binary in chunks: {name, offset, total, data (base64)}",
        [
            text("name", crate::control::MAX_NAME),
            uint("offset", 0, crate::control::MAX_BINARY),
            uint("total", 1, crate::control::MAX_BINARY),
            text("data", UPLOAD_CHUNK_B64)
        ]
    ),
    control!(
        "service.reload",
        "run the staged binary; init rolls back unless it still runs after trial_ms: {name, sha256, trial_ms}",
        [
            text("name", crate::control::MAX_NAME),
            text("sha256", 64),
            uint("trial_ms", crate::control::TRIAL_MIN_MS, crate::control::TRIAL_MAX_MS)
        ]
    ),
    control!(
        "service.revert",
        "go back to the image's binary of a reloaded service: {name}",
        [text("name", crate::control::MAX_NAME)]
    ),
    control!(
        "app.upload",
        "stage an .lzp package in chunks: {offset, total, data (base64)}",
        [
            uint("offset", 0, crate::control::MAX_PACKAGE),
            uint("total", 1, crate::control::MAX_PACKAGE),
            text("data", UPLOAD_CHUNK_B64)
        ]
    ),
    control!(
        "app.install",
        "install the staged package through pkgd (core apps too) and relaunch its running instances: {sha256, relaunch}",
        [text("sha256", 64), Param { name: "relaunch", kind: Kind::Bool }]
    ),
    control!(
        "app.relaunch",
        "stop every running instance of an app and start it again in the same session: {app}",
        [text("app", crate::control::MAX_APP_ID)]
    ),
    read!(
        "service.reloads",
        "the hot reloads since boot: trial, committed, rolled back or reverted",
        []
    ),
];

/// The method called `name`.
pub fn lookup(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|m| m.name == name)
}

/// Check `params` (an object) against `method`: every member must be a
/// declared parameter of the right type and within bounds. Parameters are
/// all optional; a handler supplies its own defaults.
pub fn validate(method: &Method, params: &Value) -> Result<(), String> {
    let Value::Object(members) = params else {
        return Err(String::from("params is an object"));
    };
    for (key, value) in members {
        let Some(param) = method.params.iter().find(|p| p.name == key) else {
            return Err(alloc::format!("{}: unknown parameter {key}", method.name));
        };
        let good = match (param.kind, value) {
            (Kind::Uint { min, max }, Value::Int(n)) => {
                *n >= 0 && (min..=max).contains(&(*n as u64))
            }
            (Kind::Str { max }, Value::Str(s)) => s.len() <= max,
            (Kind::Bool, Value::Bool(_)) => true,
            _ => false,
        };
        if !good {
            return Err(alloc::format!("{}: bad value for {key}", method.name));
        }
    }
    Ok(())
}
