//! Registry commands: `list` and `resolve <name>`.

use alloc::format;
use user::messenger::registry;
use user::sys;

use super::commands::report;

/// `list`: print every registered name with its owner, interfaces and lease.
pub(crate) fn print_registry() {
    match registry::list() {
        Ok(entries) if entries.is_empty() => {
            sys::write_str("registry: no names registered\n");
        }
        Ok(entries) => {
            sys::write_str(&format!("registry: {} name(s)\n", entries.len()));
            for entry in &entries {
                sys::write_str(&format!(
                    "  {}  owner {}  object 0x{:x}\n",
                    entry.name, entry.owner_slot, entry.object_id
                ));
                if entry.lease_remaining == 0 {
                    sys::write_str("    lease permanent\n");
                } else {
                    sys::write_str(&format!("    lease {} ticks\n", entry.lease_remaining));
                }
                for interface in &entry.interfaces {
                    sys::write_str(&format!("    iface 0x{interface:016x}\n"));
                }
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `resolve <name>`: ask the kernel for a handle to the service endpoint and
/// print it. The handle stays open: closing an endpoint closes that *side* of
/// the channel for every holder (the bootstrap listener among them), so a
/// browsing tool must not close what it resolved. The handle dies with the
/// task.
pub(crate) fn resolve(name: &str) {
    match registry::resolve(name) {
        Ok(endpoint) => sys::write_str(&format!(
            "resolved {} -> handle {}\n",
            name,
            endpoint.handle()
        )),
        Err(error) => report(error.message()),
    }
}
