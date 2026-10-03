//! The generated Rhai API: one `sys::<alias>` module per Messenger interface.
//!
//! `midlc --rhai-api libs/rhai-lazy/api idl/*.midl` writes a Rhai source
//! module per interface (`api/<alias>.rhai`) and the table below
//! (`api/index.rs`). Each function is one call into `msg`, so a script gets
//! named, documented functions without any wire code here:
//!
//! ```rhai
//! let theme = sys::confd::get("sys/ui/theme");
//! sys::confd::on_changed("sys/#", |event| print(event.payload.path));
//! ```
//!
//! The modules are compiled once per [`Fabric`] and shared by every engine
//! installed over it (the LazyRAD player builds one engine per form). A
//! generated function calls `msg::*` when it runs, so it uses the calling
//! engine's `msg`, and therefore that engine's owner (`super::events`).

use alloc::format;
use alloc::string::String;

use rhai::{Engine, Module, Scope, Shared};

use super::service::Fabric;

/// The namespace every generated module lives under.
pub const NAMESPACE: &str = "sys";

/// One generated module.
#[derive(Debug, Clone, Copy)]
pub struct ApiModule {
    /// `confd`: the module is `sys::confd`.
    pub alias: &'static str,
    /// The interface its functions call.
    pub interface: &'static str,
    /// The generated Rhai source.
    pub source: &'static str,
    /// The topic helpers it defines.
    pub topics: &'static [ApiTopic],
}

/// One topic's helpers (`on_<helper>`, `subscribe_<helper>`,
/// `publish_<helper>`) and the filter they use.
#[derive(Debug, Clone, Copy)]
pub struct ApiTopic {
    pub helper: &'static str,
    pub pattern: &'static str,
}

mod index {
    use super::{ApiModule, ApiTopic};
    include!("../../api/index.rs");
}

/// Every generated module.
pub fn modules() -> &'static [ApiModule] {
    index::MODULES
}

/// The module `sys::<alias>`.
pub fn module(alias: &str) -> Option<&'static ApiModule> {
    modules().iter().find(|m| m.alias == alias)
}

/// Compile every module into one `sys` namespace with `engine`.
pub fn build(engine: &Engine) -> Result<Module, String> {
    let mut sys = Module::new();
    for api in modules() {
        let ast = engine
            .compile(api.source)
            .map_err(|e| format!("sys::{}: {e}", api.alias))?;
        let module = Module::eval_ast_as_new(Scope::new(), &ast, engine)
            .map_err(|e| format!("sys::{}: {e}", api.alias))?;
        sys.set_sub_module(api.alias, module);
    }
    Ok(sys)
}

/// Register `sys::*` on `engine`, compiling the modules on first use of
/// `fabric` and reusing them for every later engine.
pub fn install(engine: &mut Engine, fabric: &Fabric) -> Result<(), String> {
    let sys = fabric.api_namespace(|| build(engine).map(Shared::new))?;
    engine.register_static_module(NAMESPACE, sys);
    Ok(())
}
