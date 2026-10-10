"""midlc objects: the kernel objects a request carries, as fields.

Part of the Messenger IDL compiler; see `midlc.py` for the CLI and
`docs/midl.md` for the grammar. A parcel carries its kernel objects in an
*object list* beside the TLV body (`docs/messenger-core-plan.md` 3.1), and an
object field in the body holds only an index into that list, which the
kernel resolves, checks and installs in the receiver's table. In MIDL an
object is a parameter type like any other:

    method CreateSurface(width: U32, events: Channel<os.lazy.display.v1>) -> (surface: U64) = 1;
    method AttachBuffer(surface: U64, pixels: Buffer) -> () = 2;

`Channel<I>` is one end of a channel pair; whoever receives it sends `I`'s
`oneway` methods on it (the event endpoint pattern). `Buffer` is a shared
buffer with a byte range; `Ring<A, B>` is a buffer with a ring layout
(`midlc_rings`). An object may sit inside a struct (nested structs too), but
never in a reply, a topic payload, an `Option<T>` or an `Array<T>`: a method's
object count is fixed, so the kernel gate compares one static kind list per
method and the generated decoder demands that each field's index be its
position in the declared order.
"""

from __future__ import annotations

from midlc_model import MAX_OBJECTS, Interface, Method, MidlError, ObjectRef, Param, Struct, Type, snake_case

KIND_OF = {"Channel": "channel", "Buffer": "buffer", "Ring": "rings"}
# The wire kind of each object (`libmessenger::ObjectKind`): a ring buffer is a buffer.
WIRE_KIND = {"channel": "Channel", "buffer": "Buffer", "rings": "Buffer"}


def object_kind(ty: Type) -> str | None:
    """`"channel"`, `"buffer"`, `"rings"`, or `None` for a value type."""
    return KIND_OF.get(ty.name)


def check_object_type(ty: Type, origin: str, line: int) -> None:
    """The arity of an object type as written."""
    kind = object_kind(ty)
    if kind == "channel" and len(ty.args) != 1:
        raise MidlError(f"{origin}: Channel takes exactly one interface name", line)
    if kind == "buffer" and ty.args:
        raise MidlError(f"{origin}: Buffer takes no type parameter", line)
    if kind == "rings" and (not ty.args or any(a.args for a in ty.args)):
        raise MidlError(f"{origin}: Ring lists one or more declared ring names", line)


def object_structs(structs: list[Struct]) -> set[str]:
    """The structs that hold an object somewhere inside, transitively."""
    holding: set[str] = set()
    changed = True
    while changed:
        changed = False
        for struct in structs:
            if struct.name in holding:
                continue
            if any(holds_object(f.ty, holding) for f in struct.fields):
                holding.add(struct.name)
                changed = True
    return holding


def holds_object(ty: Type, holding: set[str]) -> bool:
    """Whether a value of `ty` carries an object (itself, or a struct in
    `holding`, or one of those inside an `Option`/`Array`)."""
    return object_kind(ty) is not None or ty.name in holding or any(holds_object(a, holding) for a in ty.args)


def mark_objects(ty: Type, holding: set[str]) -> None:
    """Flag every node of `ty` that carries an object (`Type.objects`)."""
    ty.objects = holds_object(ty, holding)
    for arg in ty.args:
        mark_objects(arg, holding)


def walk_objects(params: list[Param], structs: dict[str, Struct], origin: str) -> list[ObjectRef]:
    """Every object the fields `params` carry, depth-first in declaration
    order, each with its index in the object list."""
    found: list[ObjectRef] = []

    def visit(fields: list[Param], path: list[str], stack: list[str]) -> None:
        for param in fields:
            here = path + [param.name]
            kind = object_kind(param.ty)
            if kind is not None:
                interface = param.ty.args[0].name if kind == "channel" else None
                rings = [a.name for a in param.ty.args] if kind == "rings" else []
                found.append(ObjectRef(param.name, kind, len(found), interface, rings, here))
            elif param.ty.name in structs:
                if param.ty.name in stack:
                    raise MidlError(f"{origin}: struct {param.ty.name!r} contains itself", param.line)
                visit(structs[param.ty.name].fields, here, stack + [param.ty.name])

    visit(params, [], [])
    if len(found) > MAX_OBJECTS:
        raise MidlError(f"{origin} carries {len(found)} objects; a request may carry at most {MAX_OBJECTS}")
    return found


def check_channel_targets(interfaces: list[Interface]) -> None:
    """Every `Channel<I>` names a compiled interface that has something to
    send: at least one `oneway` method. Runs over the whole compilation, since
    a channel may carry another file's interface."""
    by_name = {interface.name: interface for interface in interfaces}
    for interface in interfaces:
        for method in interface.methods:
            for obj in method.objects:
                if obj.kind != "channel":
                    continue
                origin = f"{interface.name}.{method.name} object {obj.dotted!r}"
                target = by_name.get(obj.interface or "")
                if target is None:
                    raise MidlError(f"{origin}: unknown interface {obj.interface!r}")
                if not any(m.oneway for m in target.methods):
                    raise MidlError(
                        f"{origin}: {obj.interface!r} has no oneway method to send on the channel"
                    )


def describe(obj: ObjectRef) -> str:
    """`objects[0]`, a channel ... -- one line of prose for docs and comments."""
    slot = f"`objects[{obj.index}]`"
    if obj.kind == "channel":
        return f"{slot}, a channel the receiver sends `{obj.interface}` on"
    if obj.kind == "rings":
        names = ", ".join(f"`{name}`" for name in obj.rings)
        return f"{slot}, a shared buffer holding the rings {names} back to back"
    return f"{slot}, a shared buffer"


def type_text(obj: ObjectRef) -> str:
    """The object's MIDL type as written: `Channel<I>`, `Buffer`, `Ring<A, B>`."""
    if obj.kind == "channel":
        return f"Channel<{obj.interface}>"
    if obj.kind == "rings":
        return "Ring<" + ", ".join(obj.rings) + ">"
    return "Buffer"


def kinds_expr(method: Method) -> str:
    """`&[objects::Kind::Channel, objects::Kind::Buffer]` for `method`."""
    if not method.objects:
        return "&[]"
    return "&[" + ", ".join(f"objects::Kind::{WIRE_KIND[o.kind]}" for o in method.objects) + "]"


# ---------------------------------------------------------------------------
# Rust
# ---------------------------------------------------------------------------

OBJECT_SUPPORT = '''\
/// The kernel objects a request carries in its parcel's object list, as
/// declared by `Channel<I>`, `Buffer` and `Ring<...>` parameters in `.midl`.
#[rustfmt::skip]
pub mod objects {
    /// The kind of one object-list entry (`libmessenger::ObjectKind`).
    pub use libmessenger::ObjectKind as Kind;

    /// One request that carries objects, for the kernel's table
    /// ([`crate::DECLARED_OBJECTS`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ObjectDecl {
        /// The interface id the request's parcel header carries.
        pub interface: u64,
        /// The method id the request's parcel header carries.
        pub method: u32,
        /// The declared kinds, in object-list order.
        pub kinds: &'static [Kind],
    }
}
'''


def emit_object_table(interfaces: list[Interface]) -> str:
    """The crate-level table of every request that carries objects, keyed by
    `(interface id, method id)`, and its lookup. The kernel refuses a request
    whose object list differs from its entry in length, kind or order; a
    request with no entry, including any request of an unknown interface,
    carries none."""
    lines = [
        "/// Every request that carries objects across the compiled `.midl` files,",
        "/// sorted by interface id then method id.",
        "#[rustfmt::skip]",
        "pub static DECLARED_OBJECTS: &[objects::ObjectDecl] = &[",
    ]
    rows = []
    for interface in interfaces:
        for method in interface.methods:
            if method.objects:
                rows.append((interface.id, method.method_id, interface.name, method))
    for interface_id, method_id, name, method in sorted(rows, key=lambda r: r[:2]):
        lines.append(f"    // {name}.{method.name}")
        lines.append("    objects::ObjectDecl {")
        lines.append(f"        interface: {interface_id:#x},")
        lines.append(f"        method: {method_id},")
        lines.append(f"        kinds: {kinds_expr(method)},")
        lines.append("    },")
    lines += [
        "];",
        "",
        "/// The object kinds the request `(interface, method)` declares, in order;",
        "/// empty when it declares none, including every method of an unknown",
        "/// interface.",
        "#[rustfmt::skip]",
        "pub fn declared_objects(interface: u64, method: u32) -> &'static [objects::Kind] {",
        "    DECLARED_OBJECTS",
        "        .iter()",
        "        .find(|decl| decl.interface == interface && decl.method == method)",
        "        .map_or(&[], |decl| decl.kinds)",
        "}",
    ]
    return "\n".join(lines) + "\n"


def emit_method_objects(method: Method) -> list[str]:
    """The declared kind list of one request, by name."""
    if not method.objects:
        return []
    upper = snake_case(method.name).upper()
    lines = [f"    /// The objects a `{method.name}` request carries, in object-list order:"]
    for obj in method.objects:
        lines.append(f"    /// `{obj.dotted}`, {describe(obj)}.")
    lines.append(f"    pub const {upper}_OBJECTS: &[objects::Kind] = {kinds_expr(method)};")
    return lines


def emit_declared_objects(interface: Interface) -> list[str]:
    """`DECLARED_OBJECTS`: `(method id, kinds)` for every request of the
    interface that carries objects."""
    lines = [
        "    /// Every request of this interface that carries objects: its method id",
        "    /// and the declared kinds, in object-list order.",
        "    pub const DECLARED_OBJECTS: &[(u32, &[objects::Kind])] = &[",
    ]
    for method in interface.methods:
        if method.objects:
            lines.append(f"        (METHOD_{method.name.upper()}, {snake_case(method.name).upper()}_OBJECTS),")
    lines.append("    ];")
    return lines
