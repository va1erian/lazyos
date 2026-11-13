"""midlc Java backend: names and types.

Part of the Messenger IDL compiler; see `midlc_java.py`. Maps MIDL names to
Java ones (packages, classes, members, constants) and MIDL types to the Java
types LazyVM programs see (LazyVM's docs/msg.md, section 3), and refuses what
would not compile.
"""

from __future__ import annotations

import re

from midlc_model import Interface, MidlError, Param, Type
from midlc_objects import object_kind as object_kind_of

JAVA_KEYWORDS = {
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class", "const",
    "continue", "default", "do", "double", "else", "enum", "extends", "final", "finally", "float",
    "for", "goto", "if", "implements", "import", "instanceof", "int", "interface", "long", "native",
    "new", "package", "private", "protected", "public", "return", "short", "static", "strictfp",
    "super", "switch", "synchronized", "this", "throw", "throws", "transient", "try", "void",
    "volatile", "while", "true", "false", "null", "var", "async", "await",
}
# Methods a generated member may not overload with the same arity: `Object`'s,
# and the ones every generated client, listener remote or server already has.
TAKEN_ARITIES = {
    "getClass": {0}, "hashCode": {0}, "equals": {1}, "toString": {0}, "notify": {0},
    "notifyAll": {0}, "wait": {0, 1, 2}, "clone": {0}, "finalize": {0},
    "connect": {0, 1}, "within": {1}, "serve": {1, 2}, "close": {0}, "closed": {0},
    "endpoint": {0},
}
# Runtime classes the generated files import by name; a declared type may not
# take one of these names.
# (`java.util` types and `Client`, `Server` and `Topics` are always written in
# full, so an interface may be named like them.)
IMPORTED = {"CompletionStage", "Wire", "WireWriter", "WireReader", "Listeners", "TopicNames"}
# `java.lang` types the generated code names unqualified; a declared type or a
# client class may not shadow them.
JAVA_LANG = {
    "Object", "String", "Boolean", "Integer", "Long", "Double", "Void", "Enum", "Override",
    "SuppressWarnings", "FunctionalInterface", "NullPointerException", "Exception",
}
# Locals and lambda parameters of generated methods, which a parameter may not shadow.
LOCALS = {"w", "r", "v", "a", "c", "s", "l", "f", "e", "target", "method", "raw", "end", "client"}
# Scalars: the Java type, its box, the zero value and the WireWriter/WireReader method.
SCALARS = {
    "Bool": ("boolean", "Boolean", "false", "bool"),
    "I32": ("int", "Integer", "0", "i32"),
    "I64": ("long", "Long", "0L", "i64"),
    "U32": ("long", "Long", "0L", "u32"),
    "U64": ("long", "Long", "0L", "u64"),
    "F64": ("double", "Double", "0.0", "f64"),
}


def package(interface_name: str) -> str:
    """The Java package of an interface: its own dotted name."""
    for part in interface_name.split("."):
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", part) or part in JAVA_KEYWORDS:
            raise MidlError(f"{interface_name}: {part!r} cannot be a Java package segment")
    return interface_name


def base_name(interface_name: str) -> str:
    """`os.lazy.echo.v1` -> `Echo`: the segment before the version, in UpperCamelCase."""
    parts = interface_name.split(".")
    last = parts[-2] if re.fullmatch(r"v[0-9]+", parts[-1]) and len(parts) > 1 else parts[-1]
    return upper_camel(last)


def upper_camel(text: str) -> str:
    return "".join(p[:1].upper() + p[1:] for p in re.split(r"[_\-]+", text) if p)


def lower_camel(text: str) -> str:
    """`replaces_id` -> `replacesId`, `SetZone` -> `setZone`."""
    camel = upper_camel(text) if "_" in text or "-" in text else text
    return camel[:1].lower() + camel[1:]


def ident(name: str) -> str:
    """A Java identifier: a keyword gets a trailing `_`."""
    return name + "_" if name in JAVA_KEYWORDS else name


def member(name: str, arity: int) -> str:
    """A method name that clashes with no inherited or generated member."""
    java = ident(lower_camel(name))
    return java + "_" if arity in TAKEN_ARITIES.get(java, ()) else java


def constant(name: str) -> str:
    """`NotFound` -> `NOT_FOUND`, `system_health` -> `SYSTEM_HEALTH`."""
    return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", name).replace("-", "_").upper()


def java_string(text: str) -> str:
    """`text` as a Java string literal (ASCII only, so any source encoding reads it)."""
    out = []
    for ch in text:
        if ch in "\\\"":
            out.append("\\" + ch)
        elif 0x20 <= ord(ch) < 0x7F:
            out.append(ch)
        elif ord(ch) > 0xFFFF:
            code = ord(ch) - 0x10000
            out.append(f"\\u{0xD800 + (code >> 10):04x}\\u{0xDC00 + (code & 0x3FF):04x}")
        else:
            out.append(f"\\u{ord(ch):04x}")
    return '"' + "".join(out) + '"'


def javadoc(doc: str, indent: str) -> list[str]:
    """A doc comment as Javadoc lines, or nothing."""
    if not doc.strip():
        return []
    lines = [line.replace("*/", "*&#47;").replace("@", "{@literal @}") for line in doc.strip().splitlines()]
    return [f"{indent}/**"] + [f"{indent} * {line}".rstrip() for line in lines] + [f"{indent} */"]


class Names:
    """Java names of one interface, with the others of the compile for channels."""

    def __init__(self, interface: Interface, interfaces: dict[str, Interface]):
        self.interface = interface
        self.interfaces = interfaces
        self.package = package(interface.name)
        self.base = base_name(interface.name)
        self.codec = f"{self.base}Codec"
        self.structs = {s.name for s in interface.structs}
        self.enums = {e.name for e in interface.enums}

    def listener_of(self, target: str) -> str:
        """The listener type a `Channel<target>` field is."""
        return f"{package(target)}.{base_name(target)}Listener"

    def codec_of(self, target: str) -> str:
        return f"{package(target)}.{base_name(target)}Codec"

    def java_type(self, ty: Type, boxed: bool = False) -> str:
        kind = object_kind_of(ty)
        if kind == "rings":
            raise MidlError(f"{self.interface.name}: the Java backend has no Ring type")
        if kind == "channel":
            return self.listener_of(ty.args[0].name)
        if kind == "buffer":
            return "lazy.msg.Buffer"
        if ty.enum or ty.name in self.enums:
            return ty.name
        if ty.name in SCALARS:
            return SCALARS[ty.name][1 if boxed else 0]
        if ty.name == "String":
            return "String"
        if ty.name == "Bytes":
            return "byte[]"
        if ty.name == "Array":
            return f"java.util.List<{self.java_type(ty.args[0], True)}>"
        if ty.name == "Option":
            return self.java_type(ty.args[0], True)
        if ty.name in self.structs:
            return ty.name
        raise MidlError(f"{self.interface.name}: no Java type for {ty}")

    def zero(self, ty: Type) -> str:
        """The value of an absent field (the Rust `Default`)."""
        kind = object_kind_of(ty)
        if kind is not None or ty.name == "Option":
            return "null"
        if ty.enum or ty.name in self.enums:
            return f"{ty.name}.values()[0]"
        if ty.name in SCALARS:
            return SCALARS[ty.name][2]
        if ty.name == "String":
            return '""'
        if ty.name == "Bytes":
            return "new byte[0]"
        if ty.name == "Array":
            return "new java.util.ArrayList<>()"
        return f"new {ty.name}()"

    def field(self, param: Param) -> str:
        return ident(lower_camel(param.name))

    def setter(self, param: Param) -> str:
        """A record's fluent setter: the field's name unless that would override `Object`'s."""
        name = self.field(param)
        return name + "_" if 1 in TAKEN_ARITIES.get(name, ()) else name

    def param(self, param: Param) -> str:
        """A method parameter: a field name that keeps clear of the generated locals."""
        name = self.field(param)
        return name + "_" if name in LOCALS or re.fullmatch(r"[mxoe][0-9]+", name) else name


def has_ring(ty: Type, interface: Interface, seen: frozenset = frozenset()) -> bool:
    """Whether a value of `ty` holds a `Ring`, here or in a struct inside it."""
    if object_kind_of(ty) == "rings":
        return True
    if any(has_ring(a, interface, seen) for a in ty.args):
        return True
    struct = next((s for s in interface.structs if s.name == ty.name), None)
    if struct is None or struct.name in seen:
        return False
    return any(has_ring(f.ty, interface, seen | {struct.name}) for f in struct.fields)
