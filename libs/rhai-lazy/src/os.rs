//! The `os` module: the Rust-registered functions a script uses to reach the
//! process (issue #319).
//!
//! Every function is registered twice, as `os::name(..)` and as a bare
//! global `name(..)`, so `echo hi | rhai -e 'print(stdin_text())'` and
//! `os::read("f")` both work. All fallible operations return a catchable Rhai
//! runtime error (never a panic); `exit(n)` is the one deliberate exception:
//! it ends the script with a *termination*, which `try`/`catch` cannot catch.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use core::fmt::Display;

use rhai::{
    Array, Dynamic, Engine, EvalAltResult, FuncRegistration, ImmutableString, Map, Module,
    Position, INT,
};

use crate::host::{DirEntry, Host};
use crate::limits::Limits;

type Fallible<T> = Result<T, Box<EvalAltResult>>;

/// Hard ceiling on `ls` results when the array limit is disabled (`0`).
const LS_HARD_MAX: usize = 1 << 20;

fn fail(op: &str, detail: impl Display) -> Box<EvalAltResult> {
    let message: String = format!("os::{op}: {detail}");
    EvalAltResult::ErrorRuntime(message.into(), Position::NONE).into()
}

/// The termination value `exit(n)` raises, read back by [`crate::outcome::classify`].
pub(crate) fn exit_termination(code: INT) -> Box<EvalAltResult> {
    EvalAltResult::ErrorTerminated(Dynamic::from(code), Position::NONE).into()
}

fn to_map(entry: DirEntry) -> Dynamic {
    let mut map = Map::new();
    map.insert("name".into(), Dynamic::from(entry.name));
    // Sizes above i64::MAX cannot exist on any supported filesystem; saturate
    // rather than wrap so a bogus host value never turns negative.
    map.insert(
        "size".into(),
        Dynamic::from(INT::try_from(entry.size).unwrap_or(INT::MAX)),
    );
    map.insert(
        "kind".into(),
        Dynamic::from(entry.kind.as_str().to_string()),
    );
    Dynamic::from(map)
}

/// Read `path` as text: bounded, and rejected (not lossily converted) when it
/// is not UTF-8, so a script never silently works on mangled data.
fn read_text(host: &dyn Host, op: &str, path: &str, max: usize) -> Fallible<String> {
    let bytes = host
        .read_file(path, max)
        .map_err(|e| fail(op, format!("{path}: {e}")))?;
    String::from_utf8(bytes).map_err(|_| fail(op, format!("{path}: not valid UTF-8")))
}

/// Register one host-backed function. Impure and volatile, so Rhai's
/// optimizer never folds a call with constant arguments away at compile time.
macro_rules! register {
    ($module:expr, $name:literal, $func:expr) => {
        FuncRegistration::new($name)
            .in_global_namespace()
            .with_purity(false)
            .with_volatility(true)
            .set_into_module($module, $func);
    };
}

/// Build the `os` module bound to `host`.
pub fn module<H: Host + 'static>(host: Rc<H>, limits: Limits) -> Module {
    let mut m = Module::new();
    let max_io = limits.max_io_bytes;
    let max_entries = match limits.max_array_size {
        0 => LS_HARD_MAX,
        n => n.min(LS_HARD_MAX),
    };

    let h = host.clone();
    register!(&mut m, "args", move || -> Array {
        h.args().into_iter().map(Dynamic::from).collect()
    });
    let h = host.clone();
    register!(&mut m, "env", move |key: ImmutableString| -> Dynamic {
        h.env_var(&key).map_or(Dynamic::UNIT, Dynamic::from)
    });
    let h = host.clone();
    register!(&mut m, "env", move || -> Map {
        h.env_vars()
            .into_iter()
            .map(|(k, v)| (k.into(), Dynamic::from(v)))
            .collect()
    });
    register!(&mut m, "exit", |code: INT| -> Fallible<()> {
        Err(exit_termination(code))
    });
    register!(&mut m, "exit", || -> Fallible<()> {
        Err(exit_termination(0))
    });
    let h = host.clone();
    register!(&mut m, "clock", move || -> rhai::FLOAT {
        h.now_secs() as rhai::FLOAT
    });
    let h = host.clone();
    let max_sleep = limits.max_sleep_ms;
    register!(&mut m, "sleep", move |ms: INT| -> Fallible<()> {
        let ms = u64::try_from(ms).map_err(|_| fail("sleep", "negative duration"))?;
        if ms > max_sleep {
            return Err(fail(
                "sleep",
                format!("{ms} ms is longer than the {max_sleep} ms limit"),
            ));
        }
        h.sleep_ms(ms);
        Ok(())
    });
    let h = host.clone();
    register!(
        &mut m,
        "read",
        move |path: ImmutableString| -> Fallible<String> { read_text(&*h, "read", &path, max_io) }
    );
    let h = host.clone();
    register!(&mut m, "write", move |path: ImmutableString,
                                     text: ImmutableString|
          -> Fallible<()> {
        h.write_file(&path, text.as_bytes())
            .map_err(|e| fail("write", format!("{path}: {e}")))
    });
    let h = host.clone();
    register!(
        &mut m,
        "ls",
        move |path: ImmutableString| -> Fallible<Array> {
            let mut entries = h
                .list_dir(&path, max_entries)
                .map_err(|e| fail("ls", format!("{path}: {e}")))?;
            if entries.len() > max_entries {
                return Err(fail(
                    "ls",
                    format!("{path}: more than {max_entries} entries"),
                ));
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries.into_iter().map(to_map).collect())
        }
    );
    let h = host;
    register!(&mut m, "stdin_text", move || -> Fallible<String> {
        let bytes = h.read_stdin(max_io).map_err(|e| fail("stdin_text", e))?;
        String::from_utf8(bytes)
            .map_err(|_| fail("stdin_text", "standard input is not valid UTF-8"))
    });
    m
}

/// Install the module both as `os::…` and as global functions.
pub fn install<H: Host + 'static>(engine: &mut Engine, host: Rc<H>, limits: Limits) {
    let module: rhai::Shared<Module> = module(host, limits).into();
    engine.register_global_module(module.clone());
    engine.register_static_module("os", module);
}
