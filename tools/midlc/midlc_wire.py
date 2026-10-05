"""midlc wire: a reference encoder for MIDL values, independent of Rust.

Part of the Messenger IDL compiler; see `docs/midl.md` ("Wire encoding") for
the rules this implements. The conformance corpus (`idl/conformance/`) holds
the bytes this encoder produces for sample values; the generated Rust codecs
must produce the same bytes (`libs/generated/tests/conformance.rs`), so the
corpus pins the wire format for every implementation, not just one.

Values are JSON-shaped: `Bool` a bool, integers and `F64` numbers, `String` a
string, `Bytes` a hex string, `Array` a list, `Option` `null` or the value, a
struct an object with every field, an enum its variant name (or index).
"""

from __future__ import annotations

import struct

from midlc_model import Interface, MidlError, Param, Type

KIND = {
    "Bool": 1, "I32": 2, "I64": 3, "U32": 4, "U64": 5, "F64": 6, "String": 7,
    "Bytes": 8, "Array": 9, "Struct": 10, "Map": 11, "Option": 12, "Error": 13,
}
SCALAR_FORMAT = {"I32": "<i", "I64": "<q", "U32": "<I", "U64": "<Q", "F64": "<d"}
# The standard error's detail record (`midlc_errors.DETAIL_FIELDS`).
ERROR_DETAIL = {"domain": 1, "hint": 2, "docs": 3}


def tlv(kind: str, field_id: int, payload: bytes) -> bytes:
    """One field: `kind | id << 8` and the payload length, both u32 LE."""
    return struct.pack("<II", KIND[kind] | (field_id << 8), len(payload)) + payload


def enum_index(interface: Interface, ty: Type, value) -> int:
    variants = next(e.variants for e in interface.enums if e.name == ty.name)
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    if value not in variants:
        raise MidlError(f"{value!r} is not a variant of {ty.name}")
    return variants.index(value)


def encode_value(interface: Interface, ty: Type, value, field_id: int) -> bytes:
    """`value` of type `ty` as the field `field_id`."""
    name = "U32" if ty.enum else ty.name
    if ty.enum:
        value = enum_index(interface, ty, value)
    if name == "Bool":
        if not isinstance(value, bool):
            raise MidlError(f"expected a bool, found {value!r}")
        return tlv("Bool", field_id, bytes([int(value)]))
    if name in SCALAR_FORMAT:
        return tlv(name, field_id, struct.pack(SCALAR_FORMAT[name], value))
    if name == "String":
        return tlv("String", field_id, value.encode("utf-8"))
    if name == "Bytes":
        return tlv("Bytes", field_id, bytes.fromhex(value))
    if name == "Array":
        items = b"".join(encode_value(interface, ty.args[0], item, 1) for item in value)
        return tlv("Array", field_id, items)
    if name == "Option":
        inner = b"" if value is None else encode_value(interface, ty.args[0], value, 1)
        return tlv("Option", field_id, inner)
    record = next((s for s in interface.structs if s.name == name), None)
    if record is None:
        raise MidlError(f"cannot encode type {ty}")
    return tlv("Struct", field_id, encode_fields(interface, record.fields, value))


def encode_fields(interface: Interface, fields: list[Param], value: dict) -> bytes:
    """A record body: every field in declaration order, under its own id
    (the order the generated encoders write; a decoder accepts any order)."""
    missing = [f.name for f in fields if f.name not in value]
    extra = sorted(set(value) - {f.name for f in fields})
    if missing or extra:
        raise MidlError(f"record value: missing {missing}, unknown {extra}")
    return b"".join(encode_value(interface, f.ty, value[f.name], f.id) for f in fields)


def message_fields(interface: Interface, message: str) -> list[Param]:
    """The field list `message` names: a struct, or `Method.args`/`.reply`."""
    if "." in message:
        method_name, kind = message.split(".", 1)
        method = next((m for m in interface.methods if m.name == method_name), None)
        if method is None or kind not in ("args", "reply"):
            raise MidlError(f"unknown message {message!r}")
        return method.params if kind == "args" else method.returns
    record = next((s for s in interface.structs if s.name == message), None)
    if record is None:
        raise MidlError(f"unknown message {message!r}")
    return record.fields


def encode_message(interface: Interface, message: str, value: dict) -> bytes:
    return encode_fields(interface, message_fields(interface, message), value)


def encode_error(error_field: int, value: dict) -> bytes:
    """A reply body holding only the standard error: code, message, then (when
    any is given) a NUL and the domain/hint/docs detail record. An empty
    domain is the service's own errno space and is not written; a hint or
    docs id is written whenever present, even empty."""
    payload = struct.pack("<I", value["code"]) + value["message"].encode("utf-8")
    present = {"domain": bool(value.get("domain"))}
    present.update({key: value.get(key) is not None for key in ("hint", "docs")})
    detail = b"".join(
        tlv("String", ERROR_DETAIL[key], value[key].encode("utf-8"))
        for key in ("domain", "hint", "docs")
        if present[key]
    )
    if detail:
        payload += b"\0" + detail
    return tlv("Error", error_field, payload)
