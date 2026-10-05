"""midlc data model: interface types, hashing and name helpers.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI and
grammar. This module holds the plain-data model produced by the parser plus the
stable-hash functions the generated wire code depends on.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

SCALARS = {
    "Bool": ("bool", "bool"),
    "I32": ("i32", "i32"),
    "I64": ("i64", "i64"),
    "U32": ("u32", "u32"),
    "U64": ("u64", "u64"),
    "F64": ("f64", "f64"),
}
BUILTINS = set(SCALARS) | {"String", "Bytes", "Array", "Option"}
# Kernel objects: never a body field (a handle number means nothing in the
# receiver's table), only a method's `transfers (...)` clause.
TRANSFER_TYPES = {"Channel", "Buffer", "Ring"}
BODY_FORBIDDEN = TRANSFER_TYPES | {"Handle"}


class MidlError(Exception):
    """A parse or codegen failure with a friendly, located message."""

    def __init__(self, message: str, line: int = 0):
        self.line = line
        super().__init__(f"line {line}: {message}" if line else message)


@dataclass
class Type:
    name: str
    args: list["Type"] = field(default_factory=list)
    # Set by the parser when `name` is an enum of the interface: it travels
    # as a `U32` (the variant index) and is a `u32` in Rust.
    enum: bool = field(default=False, compare=False)

    def __str__(self) -> str:
        return f"{self.name}<" + ", ".join(str(a) for a in self.args) + ">" if self.args else self.name


# The reply field id every service uses for the standard structured error
# (`docs/midl.md`, "Errors"). Fifteen because every service already used it
# before it was standardized; a top-level reply field may not take it.
ERROR_FIELD = 15
MAX_FIELD_ID = 0xFFFF


@dataclass
class Param:
    """A struct field, method argument or reply field. `id` is the wire field
    id: the explicit `= N` when `explicit`, else the 1-based position (set by
    the parser's `assign_field_ids`). `end` is the source offset just past
    the type, where `--pin-field-ids` writes ` = N`."""

    name: str
    ty: Type
    id: int = 0
    explicit: bool = False
    line: int = 0
    end: int = 0


@dataclass
class Transfer:
    """One kernel object a request carries outside its TLV body
    (`transfers (...)`, `docs/midl.md`). `kind` is `"channel"` (a slot of the
    parcel's `handles`) or `"buffer"` (a slot of its `buffers`); `interface`
    is what the receiver of a channel sends on it; `index` is the slot."""

    name: str
    kind: str
    index: int
    interface: str | None = None
    # `"rings"` only: the declared rings the buffer holds, back to back.
    rings: list[str] = field(default_factory=list)


@dataclass
class Ring:
    """A declared single-producer/single-consumer ring (`ring` in `.midl`,
    `docs/midl.md`). `layout` is `"frames"` (fixed slots, indices in a shared
    header, `libs/framering`) or `"stream"` (a byte ring whose position travels
    in calls). `producer` is `"client"` (the side that transfers the buffer)
    or `"server"`. A `frames` ring names the `oneway` method its producer
    sends to wake the consumer (`doorbell`); a `stream` ring names the method
    that moves its position (`advance`)."""

    name: str
    layout: str
    producer: str
    doorbell: str | None = None
    advance: str | None = None
    doc: str = ""
    line: int = 0


@dataclass
class Method:
    name: str
    params: list[Param]
    returns: list[Param]
    method_id: int
    oneway: bool = False
    doc: str = ""
    transfers: list[Transfer] = field(default_factory=list)


@dataclass
class Struct:
    name: str
    fields: list[Param]
    doc: str = ""


@dataclass
class Enum:
    name: str
    variants: list[str]


@dataclass
class TopicParam:
    """One wildcard of a declared topic pattern, in left-to-right order.

    `kind` is `"+"` (exactly one segment) or `"#"` (the trailing segments).
    `name` is the placeholder name when the pattern wrote `{name}`, else
    `None` so the code generator spells an automatic identifier."""

    index: int
    kind: str
    name: str | None = None

    @property
    def rust_name(self) -> str:
        return self.name if self.name else f"wildcard{self.index}"


@dataclass
class Topic:
    """A declared topic: a filter pattern, its payload type, delivery policy
    and whether it is retained (`docs/midl.md`)."""

    name: str  # normalized pattern, `+`/`#` wildcards, no placeholders
    source: str  # the pattern as written, placeholders intact (docs)
    payload: str
    qos: str  # one of latest/buffered/conflate/reliable
    retained: bool
    params: list[TopicParam]
    doc: str = ""

    @property
    def segments(self) -> list[str]:
        return self.name.split("/")

    @property
    def suffix(self) -> str:
        """Rust identifier suffix: the literal pattern segments joined by `_`,
        with `.`/`-` folded to `_` so the generated function names are valid
        identifiers (`session/+/clipboard/changed` -> `session_clipboard_changed`)."""
        literal = "_".join(seg for seg in self.segments if seg not in ("+", "#"))
        return re.sub(r"[^0-9A-Za-z_]", "_", literal)

    @property
    def permissions(self) -> list[str]:
        return [f"publish:{self.name}", f"subscribe:{self.name}"]


@dataclass
class Interface:
    name: str
    docs: str = ""
    methods: list[Method] = field(default_factory=list)
    structs: list[Struct] = field(default_factory=list)
    enums: list[Enum] = field(default_factory=list)
    topics: list[Topic] = field(default_factory=list)
    rings: list[Ring] = field(default_factory=list)

    @property
    def id(self) -> int:
        return fnv1a64(self.name)

    @property
    def module(self) -> str:
        return re.sub(r"[^0-9A-Za-z]+", "_", self.name).strip("_").lower()


def fnv1a32(text: str) -> int:
    h = 0x811C9DC5
    for byte in text.encode():
        h = ((h ^ byte) * 0x01000193) & 0xFFFFFFFF
    return h & 0x7FFFFFFF


def fnv1a64(text: str) -> int:
    h = 0xCBF29CE484222325
    for byte in text.encode():
        h = ((h ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def snake_case(name: str) -> str:
    """PascalCase IDL name -> snake_case Rust identifier, for function names
    built from a type/method name (the type itself stays PascalCase)."""
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()
