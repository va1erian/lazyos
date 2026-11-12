"""midlc Rust server backend: a `Server` trait and a `dispatch` per interface.

Part of the Messenger IDL compiler; see `docs/client-api-plan.md` 3.3. The
server sits in the interface's module behind the cargo feature
`<interface>_server` (which implies the interface's own) and depends only on
`libmessenger::server`, so the native `user` runtime and the std `xui-app`
runtime share it. `dispatch` is the only place a request header is checked,
a body decoded and a reply (or the standard error field) framed: a daemon's
handler is the trait's method bodies and nothing else.
"""

from __future__ import annotations

from midlc_model import Interface, Method, MidlError, snake_case
from midlc_rhai import KERNEL_SCOPES
from midlc_rust import emit_doc_lines, rust_type
from midlc_rust_client import result_type
from midlc_split import feature_name

# A parameter may not take the name of the credentials argument.
LOCALS = frozenset({"cx", "self"})
RT = "libmessenger::server"
FAULT = f"{RT}::Fault"
CALLER = f"{RT}::Caller"
# Kernel-served interfaces (no daemon implements them).
KERNEL_SERVED = frozenset({"os.lazy.messenger.registry.v1"})


def has_server(interface: Interface) -> bool:
    """Whether `interface` gets a server: it has methods and a user-space
    program answers it. A kernel ACL scope or policy loader never receives a
    parcel, and the registry is served by the kernel."""
    return interface.name not in KERNEL_SCOPES and interface.name not in KERNEL_SERVED and bool(interface.methods)


def server_feature(interface: Interface) -> str:
    return f"{feature_name(interface)}_server"


def method_fn(method: Method) -> str:
    return snake_case(method.name)


def check_names(interface: Interface) -> None:
    """Refuse a name that would collide in the generated trait."""
    seen: dict[str, str] = {}
    for method in interface.methods:
        name = method_fn(method)
        if name in seen:
            raise MidlError(f"{interface.name}: methods {seen[name]} and {method.name} both become the server function `{name}`")
        seen[name] = method.name
        for param in method.params:
            if param.name in LOCALS:
                raise MidlError(f"{interface.name}.{method.name}: parameter `{param.name}` is a name the generated server uses")
    if any(s.name in ("Server", "Caller", "Fault") for s in interface.structs):
        raise MidlError(f"{interface.name}: a struct named Server, Caller or Fault collides with the generated server")


def header_id(method: Method) -> str:
    return f"METHOD_{method.name.upper()}_SCOPE" if method.scope is not None else "INTERFACE_ID"


def signature(method: Method) -> str:
    params = ["&mut self", f"cx: &{CALLER}"] + [f"{p.name}: {rust_type(p.ty)}" for p in method.params]
    ret = "" if method.oneway else f" -> Result<{result_type(method)}, {FAULT}>"
    return f"fn {method_fn(method)}({', '.join(params)}){ret}"


def default_body(method: Method) -> list[str]:
    names = ["cx"] + [p.name for p in method.params]
    lines = [f"let _ = ({', '.join(names)},);"]
    if not method.oneway:
        lines.append(f"Err({FAULT}::errno(libmessenger::client::Errno::NOSYS))")
    return lines


def emit_trait(interface: Interface, gate: str) -> list[str]:
    lines = [
        gate,
        f"/// What a program answering `{interface.name}` implements: one function per method, with the",
        "/// kernel-stamped [`Caller`](libmessenger::server::Caller) first and the decoded arguments after it.",
        "/// A method left out answers `ENOSYS`. [`dispatch`] does the framing, so a body only decides.",
        "#[allow(clippy::too_many_arguments)]",
        "pub trait Server {",
    ]
    for method in interface.methods:
        lines += emit_doc_lines(method.doc, "    ")
        if method.oneway:
            lines += ["    ///", "    /// `oneway`: it has no reply, so it returns nothing."]
        lines.append(f"    {signature(method)} {{")
        lines += [f"        {line}" for line in default_body(method)]
        lines += ["    }", ""]
    if lines[-1] == "":
        lines.pop()
    lines.append("}")
    return lines


def call_expr(method: Method) -> str:
    args = ", ".join(["&cx"] + [f"args.{p.name}" for p in method.params])
    return f"server.{method_fn(method)}({args})"


def encode_closure(method: Method) -> str:
    """The closure turning the handler's value into the reply body."""
    snake = snake_case(method.name)
    if not method.returns:
        return "|()| Ok(Vec::new())"
    if len(method.returns) == 1:
        field = method.returns[0].name
        init = "value" if field == "value" else f"{field}: value"
        return f"|value| encode_{snake}_reply(&{method.name}Reply {{ {init} }})"
    return f"|value| encode_{snake}_reply(&value)"


def emit_arm(method: Method) -> list[str]:
    snake = snake_case(method.name)
    guard = f"METHOD_{method.name.upper()} if id == {header_id(method)}"
    lines = [f"        {guard} => {{"]
    if method.params:
        decoder = f"decode_{snake}_args" if method.objects else f"|body, _| decode_{snake}_args(body)"
        lines += [
            f"            let Ok(args) = message.decode({decoder}) else {{",
            f"                return {RT}::bad_request(id, method);",
            "            };",
        ]
    if method.oneway:
        lines += [f"            {call_expr(method)};", f"            {RT}::reply(id, method, Vec::new())"]
    else:
        lines += [f"            {RT}::finish(id, method, {call_expr(method)}, {encode_closure(method)})"]
    lines.append("        }")
    return lines


def emit_dispatch(interface: Interface, gate: str) -> list[str]:
    ours = ["id == INTERFACE_ID"] + [
        f"id == METHOD_{m.name.upper()}_SCOPE" for m in interface.methods if m.scope is not None
    ]
    lines = [
        gate,
        "/// Answers one request of this interface through `server`: checks the header's interface (or",
        "/// the method's ACL scope id; another is `EINVAL`), decodes the arguments (a bad body is",
        "/// `EINVAL`), calls the handler, and frames its value as the reply or its [`Fault`](libmessenger::server::Fault)",
        "/// as the standard error field. A method this interface does not declare is `ENOSYS`.",
        "/// A `oneway` method yields an empty reply a runtime drops when the message was no call.",
        "/// Objects of the request are claimed by the decode and passed by value.",
        f"pub fn dispatch<S: Server>(server: &mut S, message: &impl {RT}::Incoming) -> libmessenger::Parcel {{",
        "    let id = message.interface_id();",
        "    let method = message.method();",
        "    let cx = message.caller();",
        "    match method {",
    ]
    for method in interface.methods:
        lines += emit_arm(method)
    lines += [
        f"        _ if {' || '.join(ours)} => {RT}::unknown_method(id, method),",
        f"        _ => {RT}::wrong_interface(id, method),",
        "    }",
        "}",
    ]
    return lines


def emit_service_error(interface: Interface, gate: str) -> list[str]:
    if not interface.errors:
        return []
    lines = [
        gate,
        "impl ServiceError {",
        "    /// The sentence the declared error is answered with: its doc line in the `.midl`.",
        "    pub const fn describe(self) -> &'static str {",
        "        match self {",
    ]
    for error in interface.errors:
        text = (error.doc.splitlines() or [error.name])[0].strip() or error.name
        lines.append(f'            ServiceError::{error.name} => "{text.replace(chr(34), chr(39))}",')
    lines += [
        "        }",
        "    }",
        "}",
        "",
        gate,
        f"impl From<ServiceError> for {FAULT} {{",
        "    fn from(error: ServiceError) -> Self {",
        f"        {FAULT}::new(error.code(), error.describe())",
        "    }",
        "}",
        "",
    ]
    return lines


def emit_server(interface: Interface) -> list[str]:
    """The `Server` trait and `dispatch` of `interface`, in the module's
    indentation, or nothing when the interface gets no server."""
    if not has_server(interface):
        return []
    check_names(interface)
    gate = f'#[cfg(feature = "{server_feature(interface)}")]'
    lines = emit_service_error(interface, gate) + emit_trait(interface, gate) + [""] + emit_dispatch(interface, gate)
    return [f"    {line}" if line else "" for line in lines]
