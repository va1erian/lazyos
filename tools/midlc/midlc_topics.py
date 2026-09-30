"""midlc topic patterns: parsing, validation and codegen helpers.

Part of the Messenger IDL compiler (issue #307); see `docs/midl.md` for the
grammar. A topic declaration names a filter pattern (`+` one segment, a
trailing `#` the rest) and a payload type declared in the same interface.
Placeholders (`{session}` / `{path...}`) are sugar for `+` / `#` that also
name the generated Rust parameter.

The rules mirror the broker and the kernel ACL gate
(`user/src/bin/messengerd/filter.rs`, `kernel/src/ipc/topics.rs`): at most
[`MAX_SEGMENTS`] segments, at most [`MAX_NAME_BYTES`] bytes, literal segments
limited to the broker's byte set, and `#` only last. Rejecting a bad pattern
here means the generated helpers can never build a name the broker refuses.
"""

from __future__ import annotations

import re

from midlc_model import MidlError, Topic, TopicParam

#: Longest topic/filter name, mirroring the broker and kernel ACL gate.
MAX_NAME_BYTES = 128
#: Deepest topic/filter, mirroring the broker and kernel ACL gate.
MAX_SEGMENTS = 8

#: The `Qos` values a declaration may name, in `topics.midl` order.
QOS_VALUES = ("latest", "buffered", "conflate", "reliable")

_QOS_RUST = {
    "latest": "topics::QOS_LATEST",
    "buffered": "topics::QOS_BUFFERED",
    "conflate": "topics::QOS_CONFLATE",
    "reliable": "topics::QOS_RELIABLE",
}

#: `{name}` is one segment; `{name...}` is the trailing remainder.
_PLACEHOLDER = re.compile(r"^\{([A-Za-z_][A-Za-z0-9_]*)(\.\.\.)?\}$")
#: Placeholder names become Rust parameter names, so a keyword cannot be used.
_RUST_KEYWORDS = frozenset(
    """as break const continue crate dyn else enum extern false fn for if impl in
    let loop match mod move mut pub ref return self Self static struct super
    trait true type unsafe use where while async await abstract become box do
    final macro override priv try typeof unsized virtual yield""".split()
)
#: The broker's literal byte set: alphanumerics plus `_ - .`.
_LITERAL = re.compile(r"^[A-Za-z0-9_.-]+$")


def qos_rust_expr(qos: str) -> str:
    """The generated `topics::QOS_*` constant for a declared `qos`."""
    return _QOS_RUST[qos]


def parse_pattern(raw: str, line: int = 0) -> tuple[str, list[TopicParam]]:
    """Normalize a topic pattern and list its wildcards in order.

    Returns `(normalized, params)` where `normalized` replaces every
    placeholder with `+`/`#`. Raises [`MidlError`] for any pattern the broker
    would refuse or that cannot generate well-formed Rust."""
    if not raw:
        raise MidlError("topic name must not be empty", line)
    if len(raw.encode("utf-8")) > MAX_NAME_BYTES:
        raise MidlError(f"topic name exceeds {MAX_NAME_BYTES} bytes", line)

    parts = raw.split("/")
    if len(parts) > MAX_SEGMENTS:
        raise MidlError(f"topic has more than {MAX_SEGMENTS} segments", line)

    normalized: list[str] = []
    params: list[TopicParam] = []
    names: dict[str, int] = {}
    for index, segment in enumerate(parts):
        if not segment:
            raise MidlError("topic segment must not be empty", line)
        if segment == "+":
            normalized.append("+")
            params.append(TopicParam(index, "+"))
            continue
        if segment == "#":
            if index + 1 != len(parts):
                raise MidlError("'#' may only be the last topic segment", line)
            normalized.append("#")
            params.append(TopicParam(index, "#"))
            continue
        placeholder = _PLACEHOLDER.match(segment)
        if placeholder:
            kind = "#" if placeholder.group(2) else "+"
            if kind == "#" and index + 1 != len(parts):
                raise MidlError("a trailing placeholder must be the last segment", line)
            name = placeholder.group(1)
            if name in _RUST_KEYWORDS:
                raise MidlError(f"placeholder {name!r} is a Rust keyword", line)
            if name in names:
                raise MidlError(f"placeholder {name!r} is used twice", line)
            names[name] = index
            normalized.append(kind)
            params.append(TopicParam(index, kind, name))
            continue
        if any(char in segment for char in "+#{}"):
            raise MidlError(f"topic segment {segment!r} mixes a wildcard into a literal", line)
        if not _LITERAL.match(segment):
            raise MidlError(f"topic segment {segment!r} uses characters the broker refuses", line)
        normalized.append(segment)

    # A bare `+` gets an automatic `wildcard<index>` name, so a placeholder
    # actually called `wildcard1` could collide with it; reject that here
    # rather than emit a Rust function with two same-named parameters.
    rust_names: dict[str, int] = {}
    for param in params:
        if param.rust_name in rust_names:
            raise MidlError(
                f"wildcard parameters fold to the same Rust name {param.rust_name!r}",
                line,
            )
        rust_names[param.rust_name] = param.index

    return "/".join(normalized), params


def make_topic(
    raw: str,
    payload: str,
    qos: str,
    retained: bool,
    doc: str,
    line: int = 0,
) -> Topic:
    """Build a validated [`Topic`] from one `topic` declaration."""
    name, params = parse_pattern(raw, line)
    if qos not in QOS_VALUES:
        raise MidlError(
            f"unknown qos {qos!r}; expected one of {', '.join(QOS_VALUES)}", line
        )
    if not any(segment not in ("+", "#") for segment in name.split("/")):
        raise MidlError("topic needs at least one literal segment", line)
    return Topic(name, raw, payload, qos, retained, params, doc)
