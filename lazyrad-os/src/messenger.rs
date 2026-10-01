//! Messenger for LazyRAD scripts: the `msg` module (`docs/rhai/msg.md`).
//!
//! The player registers `rhai_lazy::msg` as a LazyRAD script extension, so
//! every form script can call system services, subscribe to topics and serve
//! methods, e.g. `status_label.text = msg::connect("os.lazy.confd.v1").info().store_dir;`.
//! Every engine the runtime builds (one per form) shares one [`Fabric`], so a
//! service is resolved once per process: resolved handles are never closed.
//!
//! Scripts call with the player's own credentials. A sandboxed app gets
//! exactly what its process may do; the services enforce access and a refusal
//! is a catchable script error.

use std::rc::Rc;

use rhai_lazy::msg::{self, Bus, Fabric};

/// Installs `msg` into every LazyRAD engine built on this thread, over `bus`.
pub fn install_over(bus: Rc<dyn Bus>) {
    let fabric = Rc::new(Fabric::new(bus));
    lazyrad_runtime::extensions::add(move |engine| msg::install_fabric(engine, &fabric));
}

/// Installs `msg` when this process runs on LazyOS; `false` elsewhere (a host
/// build), where scripts simply have no `msg` module.
pub fn install() -> bool {
    match msg::gate::Gate::detect() {
        Some(gate) => {
            install_over(Rc::new(gate));
            true
        }
        None => false,
    }
}
