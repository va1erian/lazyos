"""midlc Rust output for `service`, `errors`, `scope` and `deadline`.

Part of the Messenger IDL compiler; see `midlc_decls.py` for the syntax and
`docs/midl.md` for the rules. The constants and the error enum sit in the
interface's module (indented like the rest of `midlc_rust.emit_rust`); the
service table is crate-level, next to `DECLARED_TOPICS`.
"""

from __future__ import annotations

from midlc_model import Interface, Method
from midlc_schema import rust_str

# The generated enum is `ServiceError`, not `Error`: every interface module
# already imports `libmessenger::Error`, the codec's own error.
SERVICES_SUPPORT = '''\
/// Registry names declared with `service` (`docs/midl.md`).
#[rustfmt::skip]
pub mod services {
    /// One interface a declared service serves: its `.vN` name and id.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Served {
        pub name: &'static str,
        pub id: u64,
    }

    /// A `service` declaration: the registry `name` a daemon registers and
    /// the interfaces it serves under it. A family (`instanced`) registers
    /// one name per instance, `name` followed by the instance's own part, so
    /// `name` ends in `/`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ServiceDecl {
        pub name: &'static str,
        pub instanced: bool,
        pub interfaces: &'static [Served],
    }
}
'''


def emit_service_consts(interface: Interface) -> list[str]:
    """`SERVICE_NAME` / `SERVICE_INSTANCE_PREFIX`: the registry names a client
    of this interface resolves, so no caller spells one."""
    lines: list[str] = []
    if interface.service:
        lines += [
            "    /// The registry name this interface is served under (`service` in the `.midl`).",
            f"    pub const SERVICE_NAME: &str = {rust_str(interface.service.name)};",
        ]
    if interface.family:
        family = interface.family
        lines += [
            f"    /// The registry names of this interface's family are this prefix followed by",
            f"    /// the instance's `{family.placeholder}` (`service` in the `.midl`).",
            f"    pub const SERVICE_INSTANCE_PREFIX: &str = {rust_str(family.prefix)};",
        ]
    return lines + [""] if lines else []


def emit_method_attrs(method: Method) -> list[str]:
    """`METHOD_<X>_SCOPE` and `METHOD_<X>_DEADLINE_MS`, only for a method that
    declares them; a method without a scope travels on the interface id and
    one without a deadline has no IDL-stated bound."""
    name = method.name.upper()
    lines: list[str] = []
    if method.scope is not None:
        lines += [
            f"    /// The ACL scope interface `{method.name}` travels on (`fnv1a64({rust_str(method.scope)})`),",
            "    /// in the request header instead of [`INTERFACE_ID`].",
            f"    pub const METHOD_{name}_SCOPE: u64 = {method.scope_id:#x};",
        ]
    if method.deadline_ms is not None:
        lines += [
            f"    /// How long a client waits for `{method.name}`'s reply, in milliseconds.",
            f"    pub const METHOD_{name}_DEADLINE_MS: u32 = {method.deadline_ms};",
        ]
    return lines


def emit_errors(interface: Interface) -> list[str]:
    """`ServiceError`: the `errors` block as an enum with its wire codes."""
    if not interface.errors:
        return []
    lines = [
        "    /// The service errors of this interface (`errors` in the `.midl`). A failed",
        "    /// call carries one of these codes in the standard error field.",
        "    #[repr(u32)]",
        "    #[derive(Clone, Copy, Debug, PartialEq, Eq)]",
        "    pub enum ServiceError {",
    ]
    for error in interface.errors:
        lines += [f"        /// {line}" for line in error.doc.splitlines()]
        lines.append(f"        {error.name} = {error.code},")
    lines += [
        "    }",
        "",
        "    impl ServiceError {",
        "        /// The code the error field carries.",
        "        pub const fn code(self) -> u32 {",
        "            self as u32",
        "        }",
        "",
        "        /// The error a received code names, or `None` for a code this interface does not declare.",
        "        pub const fn from_code(code: u32) -> Option<ServiceError> {",
        "            match code {",
    ]
    for error in interface.errors:
        lines.append(f"                {error.code} => Some(ServiceError::{error.name}),")
    lines += [
        "                _ => None,",
        "            }",
        "        }",
        "",
        "        /// The declared name, for logs and tables.",
        "        pub const fn name(self) -> &'static str {",
        "            match self {",
    ]
    for error in interface.errors:
        lines.append(f"                ServiceError::{error.name} => {rust_str(error.name)},")
    lines += [
        "            }",
        "        }",
        "    }",
        "",
        "    /// Every declared error, in declaration order.",
        "    pub const SERVICE_ERRORS: &[ServiceError] = &[",
    ]
    lines += [f"        ServiceError::{error.name}," for error in interface.errors]
    lines += ["    ];", ""]
    return lines


def emit_service_table(interfaces: list[Interface]) -> str:
    """The crate-level table of every `service` declaration, and the lookup of
    the service that serves an interface id (a singleton before a family)."""
    seen: dict[str, object] = {}
    for interface in interfaces:
        for service in interface.services:
            seen.setdefault(service.name, service)
    lines = [
        "/// Every service declared across the compiled `.midl` files, in file order.",
        "#[rustfmt::skip]",
        "pub static DECLARED_SERVICES: &[services::ServiceDecl] = &[",
    ]
    by_name = {i.name: i for i in interfaces}
    for service in seen.values():
        served = ", ".join(
            f"services::Served {{ name: {rust_str(n)}, id: {by_name[n].id:#x} }}" for n in service.interfaces
        )
        lines += [
            "    services::ServiceDecl {",
            f"        name: {rust_str(service.prefix)},",
            f"        instanced: {str(service.instanced).lower()},",
            f"        interfaces: &[{served}],",
            "    },",
        ]
    lines += [
        "];",
        "",
        "/// The declared service that serves `interface` (its `INTERFACE_ID`): the",
        "/// singleton when there is one, else the family.",
        "#[rustfmt::skip]",
        "pub fn declared_service(interface: u64) -> Option<&'static services::ServiceDecl> {",
        "    let serves = |s: &&services::ServiceDecl| s.interfaces.iter().any(|i| i.id == interface);",
        "    DECLARED_SERVICES",
        "        .iter()",
        "        .filter(serves)",
        "        .find(|s| !s.instanced)",
        "        .or_else(|| DECLARED_SERVICES.iter().find(serves))",
        "}",
    ]
    return "\n".join(lines) + "\n"
