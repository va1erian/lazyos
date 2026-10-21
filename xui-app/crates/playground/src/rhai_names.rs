//! How Rhai spells an interface: the `sys::<alias>` module, a function per
//! method, enum constants and topic helpers. These are the rules
//! `tools/midlc/midlc_rhai.py` generates the API with; a test in `docs.rs`
//! checks every name here against the generated modules themselves.

use rhai_lazy::msg::api::{self, ApiModule, ApiTopic};
use rhai_lazy::msg::schema::{self, Interface, Method, Topic};

/// Methods only the kernel gate serves (`messengerd` does not proxy them), so
/// the generator leaves them out of the `sys` modules.
const GATE_ONLY: &[(&str, &str)] = &[("os.lazy.messenger.registry.v1", "Connect")];

/// Method names the service object keeps for itself (`svc.invoke(..)` is the
/// only way to reach an IDL method spelled like one).
const RESERVED_SUGAR: &[&str] = &[
    "invoke",
    "invoke_oneway",
    "call",
    "interface",
    "service",
    "methods",
    "type_of",
];

/// `os.lazy.confd.v1` -> `confd`; `os.lazy.net.nic.v1` -> `net_nic`
/// (`alias` in `tools/midlc/midlc_rhai.py`).
pub fn alias(interface_name: &str) -> String {
    let mut name = interface_name;
    // `\.v\d+$`
    if let Some((base, version)) = name.rsplit_once('.') {
        if version.len() > 1
            && version.starts_with('v')
            && version[1..].bytes().all(|b| b.is_ascii_digit())
        {
            name = base;
        }
    }
    let name = name.strip_prefix("os.lazy.").unwrap_or(name);
    let lowered = name.to_lowercase();
    let mut out = String::new();
    let mut pending = false;
    for ch in lowered.chars() {
        if ch.is_ascii_digit() || ch.is_ascii_lowercase() {
            if pending && !out.is_empty() {
                out.push('_');
            }
            pending = false;
            out.push(ch);
        } else {
            pending = true;
        }
    }
    out
}

/// The generated module of `interface`, when it has one (kernel ACL scopes
/// have none).
pub fn module(interface: &Interface) -> Option<&'static ApiModule> {
    api::module(&alias(interface.name)).filter(|m| m.interface == interface.name)
}

/// Whether a script can name the module: `sys::print` cannot be written,
/// because `print` is a Rhai keyword (the generator checks function names for
/// reserved words but not module aliases). Such an interface is reached with
/// `msg::connect`.
pub fn module_usable(interface: &Interface) -> bool {
    module(interface).is_some_and(|m| !crate::util::is_rhai_reserved(m.alias))
}

/// Whether a script can call `method` at all: it carries no kernel object
/// and the gate does not keep it to itself.
pub fn scriptable(interface: &Interface, method: &Method) -> bool {
    method.objects.is_empty()
        && !GATE_ONLY
            .iter()
            .any(|(i, m)| *i == interface.name && *m == method.name)
}

/// The `svc.<snake>(..)` sugar exists for `method` (it is not one of the
/// names the service object reserves).
pub fn has_sugar(method: &Method) -> bool {
    !RESERVED_SUGAR.contains(&schema::snake_case(method.name).as_str())
}

/// `sys::<alias>::<snake>`, when the interface has a module and the method
/// is generated.
pub fn sys_function(interface: &Interface, method: &Method) -> Option<String> {
    let module = module(interface).filter(|_| module_usable(interface))?;
    scriptable(interface, method)
        .then(|| format!("sys::{}::{}", module.alias, schema::snake_case(method.name)))
}

/// The helper stem the generator gave `topic` (`on_<stem>`), found by pattern
/// in the generated index.
pub fn topic_helper(interface: &Interface, topic: &Topic) -> Option<&'static ApiTopic> {
    module(interface)?
        .topics
        .iter()
        .find(|t| t.pattern == topic.pattern)
}

/// `Level` -> `LEVEL`, the constant holding every variant.
pub fn enum_const(enum_name: &str) -> String {
    schema::snake_case(enum_name).to_uppercase()
}

/// `Level`, `Warn` -> `LEVEL_WARN`.
pub fn variant_const(enum_name: &str, variant: &str) -> String {
    format!(
        "{}_{}",
        enum_const(enum_name),
        schema::snake_case(variant).to_uppercase()
    )
}

/// How many `+` and `#` segments a topic pattern has: the placeholders of its
/// `<stem>_topic(..)` helper.
pub fn wildcards(pattern: &str) -> usize {
    pattern
        .split('/')
        .filter(|s| *s == "+" || *s == "#")
        .count()
}

/// A concrete topic name made from `pattern` by replacing each wildcard with
/// `x` (a stand-in the user edits).
pub fn concrete_topic(pattern: &str) -> String {
    pattern
        .split('/')
        .map(|s| if s == "+" || s == "#" { "x" } else { s })
        .collect::<Vec<_>>()
        .join("/")
}
