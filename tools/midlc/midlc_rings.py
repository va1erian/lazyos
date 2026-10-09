"""midlc rings: shared-memory rings declared in `.midl`.

Part of the Messenger IDL compiler; see `midlc.py` for the CLI and
`docs/midl.md` for the grammar. Bulk data (frames, samples) never travels in a
message body: it goes through a single-producer/single-consumer ring in a
shared buffer, and messages only wake the peer or move a position. A `ring`
declaration states the whole contract, which used to live in prose:

    /// Frames the card received, driver to client.
    ring Rx : frames producer=server doorbell=Notify;
    /// Frames to send, client to driver.
    ring Tx : frames producer=client doorbell=Kick;

    method AttachRing(slots: U32, rings: Ring<Rx, Tx>, notify: Channel<os.lazy.net.nic.v1>) -> (ring: U32);

* `frames`: fixed slots with the indices and an `armed` flag in a header page
  (`libs/framering`). The producer sends the `oneway` `doorbell` method when
  the consumer armed the ring, so a burst costs one message.
* `stream`: a byte ring whose position travels in calls: the producer reports
  how far it wrote with the `advance` method (audio's `Commit`).

`producer` is `client` (the side that sends the buffer) or `server`. A
`Ring<A, B>` parameter is one shared buffer (a `Buffer` object with a layout)
holding the listed rings back to back, all the same size.
"""

from __future__ import annotations

from midlc_model import Interface, Method, MidlError, Ring, snake_case

LAYOUTS = ("frames", "stream")
SIDES = ("client", "server")
LAYOUT_RUST = {"frames": "rings::Layout::Frames", "stream": "rings::Layout::Stream"}
SIDE_RUST = {"client": "rings::Side::Client", "server": "rings::Side::Server"}


def make_ring(name: str, layout: str, options: dict[str, str], doc: str, line: int) -> Ring:
    """Check one `ring` declaration's own fields (methods are checked later,
    against the whole interface)."""
    if layout not in LAYOUTS:
        raise MidlError(f"ring {name!r}: layout {layout!r} is not one of {', '.join(LAYOUTS)}", line)
    unknown = set(options) - {"producer", "doorbell", "advance"}
    if unknown:
        raise MidlError(f"ring {name!r}: unknown option {sorted(unknown)[0]!r}", line)
    producer = options.get("producer")
    if producer not in SIDES:
        raise MidlError(f"ring {name!r}: needs producer=client or producer=server", line)
    doorbell, advance = options.get("doorbell"), options.get("advance")
    if layout == "frames" and (doorbell is None or advance is not None):
        raise MidlError(f"ring {name!r}: a frames ring names a doorbell= method and no advance=", line)
    if layout == "stream" and (advance is None or doorbell is not None):
        raise MidlError(f"ring {name!r}: a stream ring names an advance= method and no doorbell=", line)
    return Ring(name, layout, producer, doorbell, advance, doc, line)


def validate_rings(interface: Interface, claim) -> None:
    """Rings are unique, name real methods, are carried by a request, and a
    server-produced frames ring has a channel to ring its doorbell on."""
    methods = {m.name: m for m in interface.methods}
    rings = {}
    for ring in interface.rings:
        if ring.name in rings:
            raise MidlError(f"ring {ring.name!r} is declared twice", ring.line)
        if snake_case(ring.name) == "total":
            raise MidlError("a ring may not be named 'Total' (the layout's size field)", ring.line)
        rings[ring.name] = ring
        claim(f"RING_{snake_case(ring.name).upper()}", f"ring {ring.name}")
        if ring.doorbell is not None:
            target = methods.get(ring.doorbell)
            if target is None or not target.oneway:
                raise MidlError(f"ring {ring.name!r}: doorbell {ring.doorbell!r} is not a oneway method", ring.line)
        if ring.advance is not None and ring.advance not in methods:
            raise MidlError(f"ring {ring.name!r}: advance {ring.advance!r} is not a method", ring.line)
    carried: set[str] = set()
    for method in interface.methods:
        ring_objects = [o for o in method.objects if o.kind == "rings"]
        if len(ring_objects) > 1:
            raise MidlError(f"{method.name}: a request carries at most one Ring<...> buffer")
        for obj in ring_objects:
            check_ring_object(interface, method, obj.rings, rings)
            carried.update(obj.rings)
            claim(f"{snake_case(method.name)}_rings".upper(), f"{method.name} (rings)")
            claim(f"{method.name}Rings", f"{method.name} (rings)")
            claim(f"{snake_case(method.name)}_rings", f"{method.name} (rings)")
    for ring in interface.rings:
        if ring.name not in carried:
            raise MidlError(f"ring {ring.name!r} is declared but no method carries it", ring.line)


def check_ring_object(interface: Interface, method: Method, names: list[str], rings: dict) -> None:
    origin = f"{method.name} object Ring<{', '.join(names)}>"
    if len(set(names)) != len(names):
        raise MidlError(f"{origin}: a ring is listed twice")
    for name in names:
        ring = rings.get(name)
        if ring is None:
            raise MidlError(f"{origin}: {name!r} is not a ring of {interface.name!r}")
        # The client sends the buffer, so it can only wake a server-side
        # consumer over its own connection; a server producer needs a channel
        # back to the client, carrying this interface's doorbell method.
        if ring.layout == "frames" and ring.producer == "server":
            if not any(o.kind == "channel" and o.interface == interface.name for o in method.objects):
                raise MidlError(
                    f"{origin}: ring {name!r} is produced by the server, so the request "
                    f"must also carry a Channel<{interface.name}> for its doorbell"
                )


# ---------------------------------------------------------------------------
# Rust
# ---------------------------------------------------------------------------

RING_SUPPORT = '''\
/// Shared-memory rings declared in `.midl` (`ring` and `Ring<...>` parameters).
#[rustfmt::skip]
pub mod rings {
    /// How a ring is laid out and how its position moves.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Layout {
        /// Fixed slots with the indices and an `armed` flag in a header page
        /// (`libs/framering`); the producer rings a `oneway` doorbell.
        Frames,
        /// A byte ring whose position travels in calls (an `advance` method).
        Stream,
    }

    /// Which side of the request writes the ring.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Side {
        /// The side that sends the buffer.
        Client,
        /// The side that receives it.
        Server,
    }

    /// One declared ring.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct RingDecl {
        pub name: &'static str,
        pub layout: Layout,
        pub producer: Side,
        /// Method id of the `oneway` wake-up (`Frames`).
        pub doorbell: Option<u32>,
        /// Method id that moves the position (`Stream`).
        pub advance: Option<u32>,
    }

    /// Byte offset of ring `index` in a buffer of rings `ring_bytes` long,
    /// back to back; `None` on overflow.
    pub fn offset(index: u64, ring_bytes: u64) -> Option<u64> {
        index.checked_mul(ring_bytes)
    }
}
'''


def emit_ring_decls(interface: Interface) -> list[str]:
    lines: list[str] = []
    for ring in interface.rings:
        doorbell = f"Some(METHOD_{ring.doorbell.upper()})" if ring.doorbell else "None"
        advance = f"Some(METHOD_{ring.advance.upper()})" if ring.advance else "None"
        lines += [f"    /// {line}" for line in ring.doc.splitlines()] if ring.doc else []
        lines.append(f"    pub const RING_{snake_case(ring.name).upper()}: rings::RingDecl = rings::RingDecl {{")
        lines.append(f'        name: "{ring.name}",')
        lines.append(f"        layout: {LAYOUT_RUST[ring.layout]},")
        lines.append(f"        producer: {SIDE_RUST[ring.producer]},")
        lines.append(f"        doorbell: {doorbell},")
        lines.append(f"        advance: {advance},")
        lines.append("    };")
        lines.append("")
    return lines


def emit_method_rings(method: Method) -> list[str]:
    """For a `Ring<...>` parameter: the rings in order and their offsets."""
    transfer = next((o for o in method.objects if o.kind == "rings"), None)
    if transfer is None:
        return []
    snake = snake_case(method.name)
    struct = f"{method.name}Rings"
    lines = [
        f"    /// The rings of `{method.name}`'s `{transfer.name}` buffer, in order.",
        f"    pub const {snake.upper()}_RINGS: [rings::RingDecl; {len(transfer.rings)}] = ["
        + ", ".join(f"RING_{snake_case(r).upper()}" for r in transfer.rings)
        + "];",
        "",
        f"    /// Where each ring of `{method.name}`'s buffer starts, and its total size.",
        "    #[derive(Clone, Copy, Debug, PartialEq, Eq)]",
        f"    pub struct {struct} {{",
    ]
    for ring in transfer.rings:
        lines.append(f"        pub {snake_case(ring)}: u64,")
    lines.append("        pub total: u64,")
    lines.append("    }")
    lines.append("")
    lines.append(f"    /// The layout of `{method.name}`'s buffer for rings `ring_bytes` long; `None` on overflow.")
    lines.append(f"    pub fn {snake}_rings(ring_bytes: u64) -> Option<{struct}> {{")
    lines.append(f"        Some({struct} {{")
    for index, ring in enumerate(transfer.rings):
        lines.append(f"            {snake_case(ring)}: rings::offset({index}, ring_bytes)?,")
    lines.append(f"            total: rings::offset({len(transfer.rings)}, ring_bytes)?,")
    lines.append("        })")
    lines.append("    }")
    return lines


def emit_markdown_rings(interface: Interface) -> list[str]:
    if not interface.rings:
        return []
    lines = ["", "## Rings", "", "| Ring | Layout | Producer | Doorbell / advance | |", "|---|---|---|---|---|"]
    for ring in interface.rings:
        moves = f"doorbell `{ring.doorbell}`" if ring.doorbell else f"advance `{ring.advance}`"
        doc = ring.doc.replace("\n", " ")
        lines.append(f"| `{ring.name}` | {ring.layout} | {ring.producer} | {moves} | {doc} |")
    return lines


def manifest_rings(interface: Interface) -> list[dict]:
    return [
        {
            "name": ring.name,
            "layout": ring.layout,
            "producer": ring.producer,
            **({"doorbell": ring.doorbell} if ring.doorbell else {"advance": ring.advance}),
        }
        for ring in interface.rings
    ]
