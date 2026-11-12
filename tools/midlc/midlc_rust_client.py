"""midlc Rust client backend: one typed `Client<T: Transport>` per interface.

Part of the Messenger IDL compiler; see `docs/client-api-plan.md` 3.2. The
client sits in the interface's module behind the cargo feature
`<interface>_client` (which implies the interface's own) and is a thin layer
over `libmessenger::client::Connection`: it flattens the arguments, builds
the request with the generated `encode_*_args`, picks the header id and the
deadline the IDL declares, and turns the reply into the declared result.
Nothing in it is hand-written protocol, so every caller of a method behaves
the same way.
"""

from __future__ import annotations

from midlc_model import SCALARS, Interface, Method, MidlError, Type, snake_case
from midlc_rhai import KERNEL_SCOPES
from midlc_rust import emit_doc_lines, rust_type, wire_name
from midlc_split import feature_name

# Names a method may not take: the constructors and the accessor of `Client`.
RESERVED = frozenset({"connect", "try_connect", "from_connection", "connection"})
# Names the generated bodies use for their own locals; a parameter may not
# take one (a `.midl` that does is refused rather than renamed, so the
# generated signature stays what the IDL says).
LOCALS = frozenset({"within", "wire_body", "wire_objects", "wire_reply", "reply"})

ERROR_ALIAS = "ClientError"
CONN = "libmessenger::client::Connection<T>"
TRANSPORT = "libmessenger::client::Transport"


def has_client(interface: Interface) -> bool:
    """Whether `interface` gets a client: it has methods and a registry name
    to reach it by. A kernel ACL scope or policy loader never receives a
    parcel (no service answers it), and an interface that no `service` serves
    (a callback channel) is reached through the channel its owner handed out,
    not through a name."""
    return interface.name not in KERNEL_SCOPES and bool(interface.methods) and bool(interface.services)


def client_feature(interface: Interface) -> str:
    return f"{feature_name(interface)}_client"


def method_fn(method: Method) -> str:
    """The Rust name of `method`'s function: `snake_case`, with `_call` after
    a name the constructors own (`Connect` -> `connect_call`)."""
    name = snake_case(method.name)
    return f"{name}_call" if name in RESERVED else name


def param_type(ty: Type) -> tuple[str, str]:
    """`(Rust parameter type, conversion template)` for an argument: the type
    a caller passes (borrowed where the generated struct owns a copy) and the
    expression (`{}` is the parameter) that makes the owned value the
    `XxxArgs` field holds. A value that holds a kernel object is taken by
    value and moved into the parcel."""
    if ty.objects or wire_name(ty) in SCALARS:
        return rust_type(ty), "{}"
    if ty.name == "String":
        return "&str", "{}.into()"
    if ty.name == "Bytes":
        return "&[u8]", "{}.to_vec()"
    if ty.name == "Array":
        return f"&[{rust_type(ty.args[0])}]", "{}.to_vec()"
    if ty.name == "Option":
        return option_param(ty)
    return f"&{ty.name}", "{}.clone()"


def option_param(ty: Type) -> tuple[str, str]:
    """[`param_type`] for `Option<T>`: the borrowed form of `T` inside."""
    inner = ty.args[0]
    if inner.objects or inner.name == "Option":
        return rust_type(ty), "{}"
    inner_type, _ = param_type(inner)
    wrapped = f"core::option::Option<{inner_type}>"
    if wire_name(inner) in SCALARS:
        return wrapped, "{}"
    if inner.name == "String":
        return wrapped, "{}.map(Into::into)"
    if inner.name in ("Bytes", "Array"):
        return wrapped, f"{{}}.map(<{inner_type[1:]}>::to_vec)"
    return wrapped, "{}.cloned()"


def check_names(interface: Interface) -> None:
    """Refuse a name that would collide in the generated client."""
    seen: dict[str, str] = {}
    for method in interface.methods:
        name = method_fn(method)
        for taken in (name, f"{name}_within"):
            if taken in seen:
                raise MidlError(
                    f"{interface.name}: methods {seen[taken]} and {method.name} both become the client function `{taken}`"
                )
            seen[taken] = method.name
        for param in method.params:
            if param.name in LOCALS:
                raise MidlError(f"{interface.name}.{method.name}: parameter `{param.name}` is a name the generated client uses")
    if any(s.name in ("Client", ERROR_ALIAS) for s in interface.structs):
        raise MidlError(f"{interface.name}: a struct named Client or {ERROR_ALIAS} collides with the generated client")


def check_policies(interface: Interface) -> None:
    """A client needs every two-way method's deadline from the IDL: refuse
    one that neither states `deadline=` nor gets one from a `default`."""
    for method in interface.methods:
        if not method.oneway and not method.policy.has_deadline:
            raise MidlError(
                f"{interface.name}.{method.name}: no deadline; state `deadline=<n>ms|<n>s|never` "
                "on the method or `default deadline=...;` in the interface"
            )


def deadline_expr(method: Method) -> str:
    """The deadline `method` waits: the constant its `.midl` resolves."""
    return f"METHOD_{method.name.upper()}_DEADLINE"


def retry_expr(method: Method) -> str:
    return f"METHOD_{method.name.upper()}_RETRY"


def result_type(method: Method) -> str:
    """`()` for an empty reply, the field's type for one field, else the
    `XxxReply` struct."""
    if not method.returns:
        return "()"
    if len(method.returns) == 1:
        return rust_type(method.returns[0].ty)
    return f"{method.name}Reply"


def request_lines(method: Method) -> list[str]:
    """Statements building `wire_body` and `wire_objects` from the parameters."""
    if not method.params:
        return ["let wire_body = Vec::new();", "let wire_objects = Vec::new();"]
    fields = []
    for param in method.params:
        _, template = param_type(param.ty)
        value = template.format(param.name)
        fields.append(param.name if value == param.name else f"{param.name}: {value}")
    snake = snake_case(method.name)
    literal = f"&{method.name}Args {{ {', '.join(fields)} }}"
    if method.objects:
        return [f"let (wire_body, wire_objects) = encode_{snake}_args({literal}).map_err({ERROR_ALIAS}::bad_request)?;"]
    return [
        f"let wire_body = encode_{snake}_args({literal}).map_err({ERROR_ALIAS}::bad_request)?;",
        "let wire_objects = Vec::new();",
    ]


def header_id(method: Method) -> str:
    return f"METHOD_{method.name.upper()}_SCOPE" if method.scope is not None else "INTERFACE_ID"




def reply_lines(method: Method) -> list[str]:
    if not method.returns:
        return ["Ok(())"]
    decode = f"decode_{snake_case(method.name)}_reply(&wire_reply.body).map_err({ERROR_ALIAS}::bad_reply)"
    if len(method.returns) == 1:
        return [f"let reply = {decode}?;", f"Ok(reply.{method.returns[0].name})"]
    return [decode]


def signature(method: Method, suffix: str, within: bool) -> str:
    params = ["&self"] + [f"{p.name}: {param_type(p.ty)[0]}" for p in method.params]
    if within:
        params.append("within: libmessenger::client::Deadline")
    return f"pub fn {method_fn(method)}{suffix}({', '.join(params)}) -> Result<{result_type(method)}, {ERROR_ALIAS}>"


def call_args(method: Method) -> str:
    return ", ".join(p.name for p in method.params)


def emit_oneway(method: Method, pad: str) -> list[str]:
    """A `oneway` method: it returns once the message is queued."""
    lines = [f"{pad}/// Sent one way: it returns once the message is queued, never waiting for the service."]
    lines += [f"{pad}{signature(method, '', False)} {{"]
    lines += [f"{pad}    {line}" for line in request_lines(method)]
    lines += [
        f"{pad}    self.conn",
        f"{pad}        .send({header_id(method)}, METHOD_{method.name.upper()}, wire_body, wire_objects)",
        f"{pad}}}",
        "",
    ]
    return lines


def emit_method(interface: Interface, method: Method) -> list[str]:
    """The function(s) of one method: a plain one with the declared deadline
    and its `_within` form taking the caller's; a `oneway` has only the first
    (it waits for nothing)."""
    pad = "        "
    lines: list[str] = emit_doc_lines(method.doc, pad)
    if method.oneway:
        return lines + emit_oneway(method, pad)
    policy = method.policy
    waits = "for ever" if policy.never else f"at most {policy.deadline_ms} ms"
    resend = (
        "resent once to a restarted service (`retry=safe`)"
        if policy.retry == "safe"
        else "resent to a restarted service only if the request never left (`retry=never`)"
    )
    lines += [
        f"{pad}///",
        f"{pad}/// Waits for the reply {waits} (`deadline` in the `.midl`); {resend}.",
    ]
    args = call_args(method)
    forward = f"{args}, {deadline_expr(method)}" if args else deadline_expr(method)
    lines += [
        f"{pad}{signature(method, '', False)} {{",
        f"{pad}    self.{method_fn(method)}_within({forward})",
        f"{pad}}}",
        "",
        f"{pad}/// [`Self::{method_fn(method)}`] with the caller's deadline instead of the IDL's.",
        f"{pad}{signature(method, '_within', True)} {{",
    ]
    lines += [f"{pad}    {line}" for line in request_lines(method)]
    bind = "let wire_reply = " if method.returns else ""
    lines += [
        f"{pad}    {bind}self",
        f"{pad}        .conn",
        f"{pad}        .call({header_id(method)}, METHOD_{method.name.upper()}, wire_body, wire_objects, within, {retry_expr(method)})",
    ]
    if interface.errors:
        lines += [f"{pad}        .map_err(|error| error.lift(ServiceError::from_code))?;"]
    else:
        lines += [f"{pad}        ?;"]
    lines += [f"{pad}    {line}" for line in reply_lines(method)]
    lines += [f"{pad}}}", ""]
    return lines


def emit_client(interface: Interface) -> list[str]:
    """The `ClientError` alias and the `Client` of `interface`, in the
    module's indentation, or nothing when the interface gets no client."""
    if not has_client(interface):
        return []
    check_names(interface)
    check_policies(interface)
    gate = f'    #[cfg(feature = "{client_feature(interface)}")]'
    service_error = "ServiceError" if interface.errors else "core::convert::Infallible"
    lines = [
        gate,
        "    /// What a call of the generated [`Client`] returns on failure: a transport or remote",
        "    /// failure, or one of this interface's declared errors.",
        f"    pub type {ERROR_ALIAS} = libmessenger::client::CallError<{service_error}>;",
        "",
        gate,
        f"    /// A client of `{interface.name}` over a [`{TRANSPORT}`]",
        "    /// (`docs/client-api-plan.md` 3.2): the registry name, the header ids, the deadlines and the",
        "    /// error mapping all come from the `.midl`, so no caller frames a request by hand.",
        f"    pub struct Client<T: {TRANSPORT}> {{",
        f"        conn: {CONN},",
        "    }",
        "",
        gate,
        "    #[allow(clippy::too_many_arguments)]",
        f"    impl<T: {TRANSPORT}> Client<T> {{",
    ]
    if interface.service:
        lines += [
            "        /// Connect to the service ([`SERVICE_NAME`]), waiting for it to register for the",
            "        /// transport's connect bound.",
            f"        pub fn connect(transport: T) -> Result<Self, {ERROR_ALIAS}> {{",
            "            let client = Self::from_connection(libmessenger::client::Connection::new(transport, SERVICE_NAME));",
            f"            client.conn.connect().map_err({ERROR_ALIAS}::Transport)?;",
            "            Ok(client)",
            "        }",
            "",
            "        /// [`Self::connect`] with one attempt, for a caller whose service being absent has",
            "        /// a cheap fallback.",
            f"        pub fn try_connect(transport: T) -> Result<Self, {ERROR_ALIAS}> {{",
            "            let client = Self::from_connection(libmessenger::client::Connection::new(transport, SERVICE_NAME));",
            f"            client.conn.try_connect().map_err({ERROR_ALIAS}::Transport)?;",
            "            Ok(client)",
            "        }",
            "",
        ]
    lines += [
        "        /// A client over an existing connection: one to a member of the interface's service",
        "        /// family (`SERVICE_INSTANCE_PREFIX` and the instance), or a test's [`libmessenger::client::Mock`].",
        f"        pub fn from_connection(conn: {CONN}) -> Self {{",
        "            Client { conn }",
        "        }",
        "",
        "        /// The connection behind this client.",
        f"        pub fn connection(&self) -> &{CONN} {{",
        "            &self.conn",
        "        }",
        "",
    ]
    for method in interface.methods:
        lines += emit_method(interface, method)
    if lines[-1] == "":
        lines.pop()
    lines.append("    }")
    return lines
