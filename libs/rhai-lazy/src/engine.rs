//! Engine construction: limits, output routing, sandbox switch, the `os`
//! module and, when the host reaches a fabric, the `msg` module.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::Cell;

use rhai::module_resolvers::DummyModuleResolver;
use rhai::{Dynamic, Engine};

use crate::host::Host;
use crate::limits::Limits;
use crate::os;

/// Marker carried by the termination raised when standard output is closed
/// (`rhai -e '...' | head -1`): the script stops quietly instead of erroring
/// on every remaining `print`.
pub(crate) const OUTPUT_CLOSED: &str = "output closed";

/// How to build an engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct Config {
    pub limits: Limits,
    /// `--sandbox`: `eval` is disabled and `import` resolves nothing, so a
    /// script cannot compile code from a string or load other files itself.
    pub sandbox: bool,
}

/// Apply `limits`; a `0` size or operation limit means unlimited (Rhai's
/// convention), while depth limits are always enforced.
fn apply_limits(engine: &mut Engine, limits: &Limits) {
    engine
        .set_max_operations(limits.max_operations)
        .set_max_call_levels(limits.max_call_levels)
        .set_max_expr_depths(limits.max_expr_depth, limits.max_fn_expr_depth)
        .set_max_string_size(limits.max_string_size)
        .set_max_array_size(limits.max_array_size)
        .set_max_map_size(limits.max_map_size);
}

/// Route `print`/`debug` to the host and stop the script when stdout closes.
fn route_output<H: Host + 'static>(engine: &mut Engine, host: &Rc<H>) {
    let closed = Rc::new(Cell::new(false));
    let (h, flag) = (host.clone(), closed.clone());
    engine.on_print(move |text| {
        let mut line = String::with_capacity(text.len() + 1);
        line.push_str(text);
        line.push('\n');
        if h.write_out(&line).is_err() {
            flag.set(true);
        }
    });
    let h = host.clone();
    engine.on_debug(move |text, source, position| {
        let origin = source.map_or(String::new(), |s| format!("{s} "));
        h.write_err(&format!("{origin}{position} | {text}\n"));
    });
    // Checked on every operation: cheap, and the only way a native `print`
    // callback (which cannot return an error) can end the script.
    engine.on_progress(move |_| {
        closed
            .get()
            .then(|| Dynamic::from(String::from(OUTPUT_CLOSED)))
    });
}

/// Build a ready-to-use engine bound to `host`.
pub fn build_engine<H: Host + 'static>(host: Rc<H>, config: &Config) -> Engine {
    let mut engine = Engine::new();
    apply_limits(&mut engine, &config.limits);
    route_output(&mut engine, &host);
    if let Some(bus) = host.bus() {
        crate::msg::install(&mut engine, bus);
    }
    os::install(&mut engine, host, config.limits);
    if config.sandbox {
        engine.disable_symbol("eval");
        engine.set_module_resolver(DummyModuleResolver::new());
    }
    engine
}
