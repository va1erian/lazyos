"""midlc transfers: what a request carries outside its TLV body.

Part of the Messenger IDL compiler; see `midlc.py` for the CLI and
`docs/midl.md` for the grammar. A parcel moves kernel objects in two vectors
beside the body, `handles` and `buffers`, where the kernel rewrites each one
into the receiver's table. A handle number written into the body would mean
nothing to the receiver, so the body never carries one; instead a method
declares its out-of-band objects:

    method CreateSurface(width: U32) -> (surface: U64) = 1
        transfers (events: Channel<os.lazy.display.v1>);

`Channel<I>` is one end of a channel pair; whoever receives it sends `I`'s
`oneway` methods on it (the event endpoint pattern). `Buffer` is a shared
buffer. Each kind fills its vector in declaration order.
"""

from __future__ import annotations

from midlc_model import Interface, Method, MidlError, Param, Transfer, snake_case

# The kernel installs every transferred object but tells the receiver only the
# first handle and the first buffer of a delivery (`Message::first_handle`,
# `first_buffer`), so a request can usefully carry one of each.
MAX_PER_KIND = 1

KIND_OF = {"Channel": "channel", "Buffer": "buffer", "Ring": "rings"}
# A `Ring<...>` transfer is a shared buffer whose layout the IDL declares, so
# it takes the request's buffer slot.
VECTOR_OF = {"channel": "handles", "buffer": "buffers", "rings": "buffers"}


def make_transfers(params: list[Param], line: int) -> list[Transfer]:
    """Check a parsed `transfers (...)` list and assign each object its slot."""
    transfers: list[Transfer] = []
    seen: set[str] = set()
    counts = {"handles": 0, "buffers": 0}
    for param in params:
        if param.explicit:
            raise MidlError(f"transfer {param.name!r}: a transfer has a slot, not a field id", line)
        kind = KIND_OF.get(param.ty.name)
        if kind is None:
            raise MidlError(
                f"transfer {param.name!r} has type {str(param.ty)!r}; "
                "a transfer is `Channel<interface>`, `Buffer` or `Ring<ring, ...>`",
                line,
            )
        if kind == "channel" and len(param.ty.args) != 1:
            raise MidlError(f"transfer {param.name!r}: Channel takes exactly one interface name", line)
        if kind == "buffer" and param.ty.args:
            raise MidlError(f"transfer {param.name!r}: Buffer takes no type parameter", line)
        if kind == "rings" and (not param.ty.args or any(a.args for a in param.ty.args)):
            raise MidlError(f"transfer {param.name!r}: Ring lists one or more declared ring names", line)
        if param.name in seen:
            raise MidlError(f"transfer {param.name!r} is declared twice", line)
        vector = VECTOR_OF[kind]
        if counts[vector] == MAX_PER_KIND:
            noun = "channel" if vector == "handles" else "buffer (a `Ring` is one)"
            raise MidlError(
                f"transfer {param.name!r}: a request carries at most one {noun} "
                f"(the kernel surfaces only the first of the parcel's {vector})",
                line,
            )
        interface = param.ty.args[0].name if kind == "channel" else None
        rings = [a.name for a in param.ty.args] if kind == "rings" else []
        transfers.append(Transfer(param.name, kind, counts[vector], interface, rings))
        seen.add(param.name)
        counts[vector] += 1
    if not transfers:
        raise MidlError("an empty `transfers ()` clause; drop it instead", line)
    return transfers


def claim_names(method: Method, claim) -> None:
    """Reserve the generated identifiers of `method`'s transfers."""
    if method.transfers:
        claim(f"{method.name}Transfers", f"{method.name} (transfers)")
        claim(f"encode_{snake_case(method.name)}_transfers", f"{method.name} (transfers)")
        claim(f"{snake_case(method.name)}_transfers".upper(), f"{method.name} (transfers)")


def check_channel_targets(interfaces: list[Interface]) -> None:
    """Every `Channel<I>` names a compiled interface that has something to
    send: at least one `oneway` method. Runs over the whole compilation, since
    a channel may carry another file's interface."""
    by_name = {interface.name: interface for interface in interfaces}
    for interface in interfaces:
        for method in interface.methods:
            for transfer in method.transfers:
                if transfer.kind != "channel":
                    continue
                origin = f"{interface.name}.{method.name} transfer {transfer.name!r}"
                target = by_name.get(transfer.interface or "")
                if target is None:
                    raise MidlError(f"{origin}: unknown interface {transfer.interface!r}")
                if not any(m.oneway for m in target.methods):
                    raise MidlError(
                        f"{origin}: {transfer.interface!r} has no oneway method to send on the channel"
                    )


def describe(transfer: Transfer) -> str:
    """`handles[0]`: a channel ... -- one line of prose for docs and comments."""
    slot = f"`{VECTOR_OF[transfer.kind]}[{transfer.index}]`"
    if transfer.kind == "channel":
        return f"{slot}, a channel the receiver sends `{transfer.interface}` on"
    if transfer.kind == "rings":
        names = ", ".join(f"`{name}`" for name in transfer.rings)
        return f"{slot}, a shared buffer holding the rings {names} back to back"
    return f"{slot}, a shared buffer"


def signature(method: Method) -> str:
    """` transfers (events: Channel<...>)`, or `""`, for signatures in docs."""
    if not method.transfers:
        return ""
    return " transfers (" + ", ".join(f"{t.name}: {type_text(t)}" for t in method.transfers) + ")"


def type_text(transfer: Transfer) -> str:
    """The transfer's MIDL type as written: `Channel<I>`, `Buffer`, `Ring<A, B>`."""
    if transfer.kind == "channel":
        return f"Channel<{transfer.interface}>"
    if transfer.kind == "rings":
        return "Ring<" + ", ".join(transfer.rings) + ">"
    return "Buffer"


# ---------------------------------------------------------------------------
# Rust
# ---------------------------------------------------------------------------

TRANSFER_SUPPORT = '''\
/// Out-of-band objects a request carries in the parcel's `handles` and
/// `buffers` vectors, as declared by `transfers (...)` clauses in `.midl`.
#[rustfmt::skip]
pub mod transfers {
    /// How many handles and shared buffers a request declares.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Transfers {
        pub handles: u8,
        pub buffers: u8,
    }

    impl Transfers {
        /// A request that declares no transfers.
        pub const NONE: Self = Self { handles: 0, buffers: 0 };

        /// Whether a delivery that installed `handles` handles and `buffers`
        /// shared buffers carries exactly what was declared. Servers check
        /// this before dispatch, so an undeclared object is refused (and
        /// closed) instead of leaking into their handle table.
        pub fn matches(self, handles: u64, buffers: u64) -> bool {
            handles == u64::from(self.handles) && buffers == u64::from(self.buffers)
        }
    }
}
'''


def emit_method_transfers(method: Method) -> list[str]:
    """The typed sender struct, its encoder and the declared counts."""
    if not method.transfers:
        return []
    name = f"{method.name}Transfers"
    upper = f"{snake_case(method.name)}_transfers".upper()
    handles = [t for t in method.transfers if t.kind == "channel"]
    buffers = [t for t in method.transfers if t.kind != "channel"]
    lines = [
        f"    /// What a `{method.name}` request carries outside its body.",
        f"    pub const {upper}: transfers::Transfers = transfers::Transfers {{ "
        f"handles: {len(handles)}, buffers: {len(buffers)} }};",
        "",
        f"    /// The objects a `{method.name}` request transfers, by name.",
        "    #[derive(Clone, Debug, Default, PartialEq)]",
        f"    pub struct {name} {{",
    ]
    for transfer in method.transfers:
        rust = "u64" if transfer.kind == "channel" else "libmessenger::BufferDesc"
        lines.append(f"        /// {describe(transfer)}.")
        lines.append(f"        pub {transfer.name}: {rust},")
    lines.append("    }")
    lines.append("")
    lines.append(f"    /// The parcel's `handles` and `buffers` for a `{method.name}` request.")
    lines.append(
        f"    pub fn encode_{snake_case(method.name)}_transfers(value: &{name}) "
        "-> (Vec<u64>, Vec<libmessenger::BufferDesc>) {"
    )
    lines.append(f"        ({vec_expr(handles)}, {vec_expr(buffers)})")
    lines.append("    }")
    return lines


def vec_expr(transfers: list[Transfer]) -> str:
    if not transfers:
        return "Vec::new()"
    return "alloc::vec![" + ", ".join(f"value.{t.name}" for t in transfers) + "]"


def emit_request_transfers(interface: Interface) -> list[str]:
    """`request_transfers(method)`: the declared counts of any method id, so a
    server can check a delivery generically before it dispatches."""
    lines = [
        "    /// The transfers the request `method` declares; `NONE` for a method",
        "    /// that declares none or an unknown method id.",
        "    pub fn request_transfers(method: u32) -> transfers::Transfers {",
    ]
    if not any(m.transfers for m in interface.methods):
        lines +=["        let _ = method;", "        transfers::Transfers::NONE", "    }"]
        return lines
    lines.append("        match method {")
    for m in interface.methods:
        if m.transfers:
            constant = f"{snake_case(m.name)}_transfers".upper()
            lines.append(f"            METHOD_{m.name.upper()} => {constant},")
    lines += ["            _ => transfers::Transfers::NONE,", "        }", "    }"]
    return lines
