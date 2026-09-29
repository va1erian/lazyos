#!/usr/bin/env python3
"""midlc - the Messenger IDL compiler (issue #90).

Reads `.midl` interface definitions (docs/messenger.md section 11) and emits:

  * Rust wire helpers (encode/decode over the `libmessenger` parcel codec),
  * a Markdown reference per interface (signatures, types),
  * a machine-readable manifest (method ids and the interface hash).

Usage:
    python tools/midlc/midlc.py --out libs/generated/src/lib.rs idl/echo.midl
    python tools/midlc/midlc.py --check --out libs/generated/src/lib.rs idl/echo.midl
    python tools/midlc/midlc.py --manifest build/manifest.json idl/echo.midl

`--check` regenerates in memory and fails if the committed file differs, so
generated code cannot silently drift; CI runs it on every pull request.

Grammar (small on purpose):

    interface os.lazy.echo.v1 {
        /// Doc comment for the method.
        method Echo(text: String, count: U32) -> (reply: String);
        method Notify(event: Event) -> () oneway;    // oneway methods return ()
        struct Event { topic: String, at: U64 }
        enum Level { Info, Warn, Error }
    }

Method ids are stable: an explicit `= 7` wins, otherwise a deterministic hash of
the method name is used; adding methods never renumbers existing ones.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

SCALARS = {
    "Bool": ("bool", "bool"),
    "I32": ("i32", "i32"),
    "I64": ("i64", "i64"),
    "U32": ("u32", "u32"),
    "U64": ("u64", "u64"),
    "F64": ("f64", "f64"),
}
BUILTINS = set(SCALARS) | {"String", "Bytes", "Handle", "Buffer", "Array", "Option"}


class MidlError(Exception):
    """A parse or codegen failure with a friendly, located message."""

    def __init__(self, message: str, line: int = 0):
        self.line = line
        super().__init__(f"line {line}: {message}" if line else message)


@dataclass
class Type:
    name: str
    args: list["Type"] = field(default_factory=list)

    def __str__(self) -> str:
        return f"{self.name}<" + ", ".join(str(a) for a in self.args) + ">" if self.args else self.name


@dataclass
class Param:
    name: str
    ty: Type


@dataclass
class Method:
    name: str
    params: list[Param]
    returns: list[Param]
    method_id: int
    oneway: bool = False
    doc: str = ""


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
class Interface:
    name: str
    docs: str = ""
    methods: list[Method] = field(default_factory=list)
    structs: list[Struct] = field(default_factory=list)
    enums: list[Enum] = field(default_factory=list)

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


# ---------------------------------------------------------------------------
# Lexer / parser
# ---------------------------------------------------------------------------

TOKEN = re.compile(
    r"(?P<doc>///[^\n]*)"
    r"|(?P<comment>//[^\n]*)"
    r"|(?P<number>\d+)"
    r"|(?P<arrow>->)"
    r"|(?P<ident>[A-Za-z_][A-Za-z0-9_.]*)"
    r"|(?P<punct>[{}()<>:,;=])"
)


@dataclass
class Token:
    kind: str
    text: str
    line: int


def lex(text: str) -> list[Token]:
    tokens: list[Token] = []
    for match in TOKEN.finditer(text):
        line = text.count("\n", 0, match.start()) + 1
        kind = match.lastgroup
        assert kind is not None
        if kind in ("comment",):
            continue
        tokens.append(Token(kind, match.group(), line))
    return tokens


class Parser:
    def __init__(self, tokens: list[Token]):
        self.tokens = tokens
        self.pos = 0
        self.pending_doc = ""

    def peek(self) -> Token | None:
        return self.tokens[self.pos] if self.pos < len(self.tokens) else None

    def next(self) -> Token:
        token = self.peek()
        if token is None:
            raise MidlError("unexpected end of input")
        self.pos += 1
        if token.kind == "doc":
            # Consecutive `///` lines are one doc comment; append (not
            # replace) so a multi-line comment keeps every line, in order.
            line = token.text.lstrip("/").strip()
            self.pending_doc = f"{self.pending_doc}\n{line}" if self.pending_doc else line
            return self.next()
        return token

    def expect(self, text: str) -> Token:
        token = self.next()
        if token.text != text:
            raise MidlError(f"expected {text!r}, found {token.text!r}", token.line)
        return token

    def take_doc(self) -> str:
        doc, self.pending_doc = self.pending_doc, ""
        return doc

    def parse_type(self) -> Type:
        token = self.next()
        if token.kind != "ident":
            raise MidlError(f"expected a type, found {token.text!r}", token.line)
        args: list[Type] = []
        if self.peek() and self.peek().text == "<":
            self.next()
            while True:
                args.append(self.parse_type())
                if self.peek() and self.peek().text == ",":
                    self.next()
                    continue
                break
            self.expect(">")
        return Type(token.text, args)

    def parse_params(self) -> list[Param]:
        self.expect("(")
        params: list[Param] = []
        if self.peek() and self.peek().text == ")":
            self.next()
            return params
        while True:
            name = self.next()
            if name.kind != "ident":
                raise MidlError(f"expected a parameter name, found {name.text!r}", name.line)
            self.expect(":")
            params.append(Param(name.text, self.parse_type()))
            if self.peek() and self.peek().text == ",":
                self.next()
                continue
            break
        self.expect(")")
        return params

    def parse_method(self) -> Method:
        doc = self.take_doc()
        name = self.next()
        if name.kind != "ident":
            raise MidlError(f"expected a method name, found {name.text!r}", name.line)
        params = self.parse_params()
        self.expect("->")
        returns = self.parse_params()
        method_id = fnv1a32(name.text)
        oneway = False
        while self.peek() and self.peek().text != ";":
            token = self.next()
            if token.text == "=":
                number = self.next()
                if number.kind != "number":
                    raise MidlError("expected a method id after '='", number.line)
                method_id = int(number.text)
            elif token.text == "oneway":
                oneway = True
            elif token.text == "sync":
                oneway = False
            else:
                raise MidlError(f"unexpected {token.text!r} in method", token.line)
        self.expect(";")
        return Method(name.text, params, returns, method_id, oneway, doc)

    def parse_struct(self) -> Struct:
        doc = self.take_doc()
        name = self.next()
        if name.kind != "ident":
            raise MidlError(f"expected a struct name, found {name.text!r}", name.line)
        self.expect("{")
        fields: list[Param] = []
        while self.peek() and self.peek().text != "}":
            field_name = self.next()
            if field_name.kind != "ident":
                raise MidlError(f"expected a field name, found {field_name.text!r}", field_name.line)
            self.expect(":")
            fields.append(Param(field_name.text, self.parse_type()))
            if self.peek() and self.peek().text == ",":
                self.next()
        self.expect("}")
        return Struct(name.text, fields, doc)

    def parse_enum(self) -> Enum:
        self.take_doc()
        name = self.next()
        if name.kind != "ident":
            raise MidlError(f"expected an enum name, found {name.text!r}", name.line)
        self.expect("{")
        variants: list[str] = []
        while self.peek() and self.peek().text != "}":
            variant = self.next()
            if variant.kind != "ident":
                raise MidlError(f"expected a variant, found {variant.text!r}", variant.line)
            variants.append(variant.text)
            if self.peek() and self.peek().text == ",":
                self.next()
        self.expect("}")
        return Enum(name.text, variants)

    def parse_interface(self) -> Interface:
        self.expect("interface")
        name = self.next()
        if name.kind != "ident" or not re.fullmatch(r"[a-z0-9_.]+\.v\d+", name.text):
            raise MidlError(f"interface {name.text!r} must be reverse-DNS with a .vN suffix", name.line)
        interface = Interface(name.text, self.take_doc())
        self.expect("{")
        while self.peek() and self.peek().text != "}":
            keyword = self.next()
            if keyword.text == "method":
                interface.methods.append(self.parse_method())
            elif keyword.text == "struct":
                interface.structs.append(self.parse_struct())
            elif keyword.text == "enum":
                interface.enums.append(self.parse_enum())
            else:
                raise MidlError(f"unexpected {keyword.text!r}", keyword.line)
        self.expect("}")
        validate(interface)
        return interface


def validate(interface: Interface) -> None:
    named = {s.name for s in interface.structs} | {e.name for e in interface.enums}
    ids: dict[int, str] = {}
    # snake_case identifier -> the declared name(s) it came from, so distinct
    # names that fold to the same generated `encode_*`/`decode_*` function
    # (e.g. structs `Foo` and `foo`) are rejected here rather than surfacing
    # as a confusing duplicate-definition error out of the generated Rust.
    codec_names: dict[str, str] = {}

    def claim(identifier: str, origin: str) -> None:
        if identifier in codec_names and codec_names[identifier] != origin:
            raise MidlError(
                f"{origin!r} and {codec_names[identifier]!r} both generate the "
                f"codec name {identifier!r}; rename one"
            )
        codec_names[identifier] = origin

    for struct in interface.structs:
        claim(snake_case(struct.name), struct.name)
    for method in interface.methods:
        if method.method_id in ids:
            raise MidlError(
                f"method id {method.method_id} is used by {ids[method.method_id]!r} and {method.name!r}"
            )
        ids[method.method_id] = method.name
        if method.oneway and method.returns:
            raise MidlError(f"oneway method {method.name!r} cannot return values")
        if method.params:
            claim(f"{snake_case(method.name)}_args", f"{method.name} (args)")
        if method.returns:
            claim(f"{snake_case(method.name)}_reply", f"{method.name} (reply)")
        for param in method.params + method.returns:
            check_type(param.ty, named)
    for struct in interface.structs:
        for f in struct.fields:
            check_type(f.ty, named)


def check_type(ty: Type, named: set[str]) -> None:
    if ty.name in {"Array", "Option"} and len(ty.args) != 1:
        raise MidlError(f"{ty.name} takes exactly one type parameter")
    if ty.name not in BUILTINS and ty.name not in named:
        raise MidlError(f"unknown type {ty.name!r}")
    for arg in ty.args:
        check_type(arg, named)


# ---------------------------------------------------------------------------
# Rust code generation
# ---------------------------------------------------------------------------

RUST_TYPE = {
    "String": "alloc::string::String",
    "Bytes": "alloc::vec::Vec<u8>",
    "Handle": "u64",
    "Buffer": "libmessenger::BufferDesc",
}


def snake_case(name: str) -> str:
    """PascalCase IDL name -> snake_case Rust identifier, for function names
    built from a type/method name (the type itself stays PascalCase)."""
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def rust_type(ty: Type) -> str:
    if ty.name in SCALARS:
        return SCALARS[ty.name][0]
    if ty.name in RUST_TYPE:
        return RUST_TYPE[ty.name]
    if ty.name == "Array":
        return f"alloc::vec::Vec<{rust_type(ty.args[0])}>"
    if ty.name == "Option":
        # `Option` lives in `core`, not `alloc`; the generated module only
        # imports `alloc::vec::Vec`.
        return f"core::option::Option<{rust_type(ty.args[0])}>"
    return ty.name  # named struct


def deref(value: str) -> str:
    """Copy expression for a Copy scalar `value`: strip a literal leading `&`
    (an owning field access, e.g. `&value.at` -> `value.at`) rather than
    writing `*&value.at`, which is what a plain `*{value}` would produce."""
    return value[1:] if value.startswith("&") else f"*{value}"


def encode_lines(ty: Type, *, id: int, value: str, indent: str, target: str = "target") -> list[str]:
    """Lines writing `value` into encoder `target` as field `id`.

    `target` is parameterised because an `Array`/`Option` encodes its element
    into a fresh `nested` encoder: writing the element into the outer encoder
    would emit a field at the wrong depth (and, for a struct's first field,
    collide with an earlier sibling id)."""
    if ty.name in SCALARS:
        return [f"{indent}{target}.{SCALARS[ty.name][1]}({id}, {deref(value)})?;"]
    if ty.name == "String":
        return [f"{indent}{target}.string({id}, {value})?;"]
    if ty.name == "Bytes":
        return [f"{indent}{target}.bytes({id}, {value})?;"]
    if ty.name == "Handle":
        return [f"{indent}{target}.handle({id}, {deref(value)})?;"]
    if ty.name == "Buffer":
        return [f"{indent}{target}.buffer({id}, {value})?;"]
    if ty.name == "Array":
        inner = ty.args[0]
        lines = [f"{indent}let mut nested = Encoder::new();", f"{indent}for item in {value} {{"]
        lines += encode_lines(inner, id=1, value="item", indent=indent + "    ", target="nested")
        lines += [f"{indent}}}", f"{indent}{target}.array({id}, &nested)?;"]
        return lines
    if ty.name == "Option":
        inner = ty.args[0]
        lines = [f"{indent}match {value} {{", f"{indent}    Some(item) => {{"]
        lines += [f"{indent}        let mut nested = Encoder::new();"]
        lines += encode_lines(inner, id=1, value="item", indent=indent + "        ", target="nested")
        lines += [f"{indent}        {target}.option({id}, Some(&nested))?;", f"{indent}    }}"]
        lines += [f"{indent}    None => {{", f"{indent}        {target}.option({id}, None)?;", f"{indent}    }}"]
        lines += [f"{indent}}}"]
        return lines
    # Named struct: encode as a nested record.
    return [f"{indent}{target}.raw(Kind::Struct, {id}, &encode_{snake_case(ty.name)}({value})?)?;"]


DECODE_EXPR = {
    "Bool": "field.as_bool()?",
    "I32": "field.as_i32()?",
    "I64": "field.as_i64()?",
    "U32": "field.as_u32()?",
    "U64": "field.as_u64()?",
    "F64": "field.as_f64()?",
    "String": "field.as_str()?.into()",
    "Bytes": "field.as_bytes().to_vec()",
    "Handle": "field.as_handle()?",
    "Buffer": "field.as_buffer()?",
}

ITEM_EXPR = {
    "Bool": "item.as_bool()?",
    "I32": "item.as_i32()?",
    "I64": "item.as_i64()?",
    "U32": "item.as_u32()?",
    "U64": "item.as_u64()?",
    "F64": "item.as_f64()?",
    "String": "item.as_str()?.into()",
    "Bytes": "item.as_bytes().to_vec()",
    "Handle": "item.as_handle()?",
    "Buffer": "item.as_buffer()?",
}


def decode_block(ty: Type, *, target: str, indent: str) -> list[str]:
    """Lines that fill `target` from the current `field`."""
    if ty.name in DECODE_EXPR:
        return [f"{indent}{target} = {DECODE_EXPR[ty.name]};"]
    if ty.name == "Array":
        inner = ty.args[0]
        lines = [
            f"{indent}let mut nested = field.nested(0)?;",
            f"{indent}while let Some(item) = nested.next()? {{",
        ]
        if inner.name in ITEM_EXPR:
            lines.append(f"{indent}    {target}.push({ITEM_EXPR[inner.name]});")
        else:
            lines.append(f"{indent}    {target}.push(decode_{snake_case(inner.name)}(item.payload)?);")
        lines.append(f"{indent}}}")
        return lines
    if ty.name == "Option":
        inner = ty.args[0]
        expr = ITEM_EXPR.get(inner.name, f"decode_{snake_case(inner.name)}(item.payload)?")
        return [
            f"{indent}if field.payload.is_empty() {{",
            f"{indent}    {target} = None;",
            f"{indent}}} else {{",
            f"{indent}    let mut nested = field.nested(0)?;",
            f"{indent}    let item = nested.next()?.ok_or(Error::BadValue)?;",
            f"{indent}    {target} = Some({expr});",
            f"{indent}}}",
        ]
    return [f"{indent}{target} = decode_{snake_case(ty.name)}(field.payload)?;"]


def emit_field_dispatch(fields: list[Param], target_prefix: str, indent: str) -> list[str]:
    """Decode loop body dispatching on `field.id`: one field is an `if` (a
    `match` against a single value plus a wildcard arm is just an equality
    check), more than one is a real `match`."""
    if len(fields) == 1:
        f = fields[0]
        lines = [f"{indent}if field.id == 1 {{"]
        lines += decode_block(f.ty, target=f"{target_prefix}.{f.name}", indent=indent + "    ")
        lines.append(f"{indent}}}")
        return lines
    lines = [f"{indent}match field.id {{"]
    for index, f in enumerate(fields, start=1):
        lines.append(f"{indent}    {index} => {{")
        lines += decode_block(f.ty, target=f"{target_prefix}.{f.name}", indent=indent + "        ")
        lines.append(f"{indent}    }}")
    lines.append(f"{indent}    _ => {{}}")
    lines.append(f"{indent}}}")
    return lines


def emit_doc_lines(doc: str, indent: str) -> list[str]:
    """One `///` line per line of a (possibly multi-line) doc comment."""
    return [f"{indent}/// {line}" for line in doc.splitlines()] if doc else []


def emit_struct(name: str, fields: list[Param], doc: str) -> str:
    lines = []
    lines += emit_doc_lines(doc, indent="    ")
    lines.append("    #[derive(Clone, Debug, Default, PartialEq)]")
    lines.append(f"    pub struct {name} {{")
    for f in fields:
        lines.append(f"        pub {f.name}: {rust_type(f.ty)},")
    lines.append("    }")
    lines.append("")
    lines.append(f"    pub fn encode_{snake_case(name)}(value: &{name}) -> Result<Vec<u8>, Error> {{")
    lines.append("        let mut target = Encoder::new();")
    for index, f in enumerate(fields, start=1):
        lines += encode_lines(f.ty, id=index, value=f"&value.{f.name}", indent="        ")
    lines.append("        Ok(target.finish())")
    lines.append("    }")
    lines.append("")
    lines.append(f"    pub fn decode_{snake_case(name)}(body: &[u8]) -> Result<{name}, Error> {{")
    lines.append(f"        let mut out = {name}::default();")
    lines.append("        let mut decoder = Decoder::new(body);")
    lines.append("        while let Some(field) = decoder.next()? {")
    lines += emit_field_dispatch(fields, "out", indent="            ")
    lines.append("        }")
    lines.append("        Ok(out)")
    lines.append("    }")
    return "\n".join(lines)


def emit_message(method_name: str, kind: str, params: list[Param]) -> str:
    struct_name = f"{method_name}{kind.capitalize()}"
    lines = ["    #[derive(Clone, Debug, Default, PartialEq)]"]
    lines.append(f"    pub struct {struct_name} {{")
    for p in params:
        lines.append(f"        pub {p.name}: {rust_type(p.ty)},")
    lines.append("    }")
    lines.append("")
    lines.append(f"    pub fn encode_{snake_case(method_name)}_{kind}(value: &{struct_name}) -> Result<Vec<u8>, Error> {{")
    lines.append("        let mut target = Encoder::new();")
    for index, p in enumerate(params, start=1):
        lines += encode_lines(p.ty, id=index, value=f"&value.{p.name}", indent="        ")
    lines.append("        Ok(target.finish())")
    lines.append("    }")
    lines.append("")
    lines.append(f"    pub fn decode_{snake_case(method_name)}_{kind}(body: &[u8]) -> Result<{struct_name}, Error> {{")
    lines.append(f"        let mut out = {struct_name}::default();")
    lines.append("        let mut decoder = Decoder::new(body);")
    lines.append("        while let Some(field) = decoder.next()? {")
    lines += emit_field_dispatch(params, "out", indent="            ")
    lines.append("        }")
    lines.append("        Ok(out)")
    lines.append("    }")
    return "\n".join(lines)


def emit_rust(interface: Interface) -> str:
    lines = [
        f"/// `{interface.name}` (interface id `{interface.id:#x}`).",
        f"pub mod {interface.module} {{",
        "    use alloc::vec::Vec;",
        "    use libmessenger::{Decoder, Encoder, Error, Kind};",
        "",
        "    /// The interface id: the FNV-1a hash of the `.vN` interface name.",
        f"    pub const INTERFACE_ID: u64 = {interface.id:#x};",
        "",
    ]
    for struct in interface.structs:
        lines += emit_struct(struct.name, struct.fields, struct.doc).splitlines()
        lines.append("")
    for method in interface.methods:
        lines.append(f"    /// `{method.name}` method id.")
        lines.append(f"    pub const METHOD_{method.name.upper()}: u32 = {method.method_id};")
    lines.append("")
    for method in interface.methods:
        lines += emit_doc_lines(method.doc, indent="    ")
        if method.params:
            lines += emit_message(method.name, "args", method.params).splitlines()
            lines.append("")
        if method.returns:
            lines += emit_message(method.name, "reply", method.returns).splitlines()
            lines.append("")
    if lines[-1] == "":
        lines.pop()
    lines.append("}")
    return "\n".join(lines)


def emit_markdown(interface: Interface) -> str:
    lines = [f"# `{interface.name}`", "", f"Interface id: `{interface.id:#x}`", ""]
    if interface.docs:
        lines += [interface.docs, ""]
    lines += ["## Methods", "", "| Method | Id | Kind | Signature |", "|---|---|---|---|"]
    for m in interface.methods:
        args = ", ".join(f"{p.name}: {p.ty}" for p in m.params)
        rets = ", ".join(f"{p.name}: {p.ty}" for p in m.returns)
        lines.append(f"| {m.name} | {m.method_id} | {'oneway' if m.oneway else 'sync'} | `({args}) -> ({rets})` |")
    for struct in interface.structs:
        lines += ["", f"## struct `{struct.name}`", ""]
        lines += [f"- `{f.name}: {f.ty}`" for f in struct.fields]
    for enum in interface.enums:
        lines += ["", f"## enum `{enum.name}`", "", "- " + ", ".join(enum.variants)]
    return "\n".join(lines) + "\n"


def emit_manifest(interface: Interface) -> dict:
    return {
        "interface": interface.name,
        "interface_id": f"{interface.id:#x}",
        "methods": [{"name": m.name, "id": m.method_id, "oneway": m.oneway} for m in interface.methods],
    }


HEADER = (
    "// @generated by tools/midlc/midlc.py - do not edit.\n"
    "// Regenerate with:\n"
    "//   python tools/midlc/midlc.py --out libs/generated/src/lib.rs idl/*.midl\n"
    "#![cfg_attr(not(test), no_std)]\n"
    "extern crate alloc;\n\n"
)


def generate(inputs: list[Path]) -> tuple[str, list[dict], dict[str, str]]:
    interfaces = []
    for path in inputs:
        interfaces.append(Parser(lex(path.read_text(encoding="utf-8"))).parse_interface())
    rust = HEADER + "\n\n".join(emit_rust(i) for i in interfaces) + "\n"
    return rust, [emit_manifest(i) for i in interfaces], {i.name: emit_markdown(i) for i in interfaces}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path, help=".midl files")
    parser.add_argument("--out", type=Path, help="write generated Rust here")
    parser.add_argument("--manifest", type=Path, help="write the JSON manifest here")
    parser.add_argument("--docs", type=Path, help="write Markdown docs into this directory")
    parser.add_argument("--check", action="store_true", help="fail if --out would change")
    args = parser.parse_args()

    try:
        rust, manifests, docs = generate(args.inputs)
    except MidlError as error:
        print(f"midlc: {error}", file=sys.stderr)
        return 1

    if args.check:
        if not args.out or not args.out.is_file():
            print(f"midlc: --check needs an existing --out file ({args.out})", file=sys.stderr)
            return 1
        if args.out.read_text(encoding="utf-8") != rust:
            print(
                f"midlc: {args.out} is out of date; regenerate with:\n"
                f"  python tools/midlc/midlc.py --out {args.out} {' '.join(str(p) for p in args.inputs)}",
                file=sys.stderr,
            )
            return 1
        print(f"midlc: {args.out} is up to date ({len(manifests)} interface(s))")
        return 0

    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(rust, encoding="utf-8")
        print(f"midlc: wrote {args.out}")
    if args.manifest:
        args.manifest.parent.mkdir(parents=True, exist_ok=True)
        args.manifest.write_text(json.dumps(manifests, indent=2) + "\n", encoding="utf-8")
        print(f"midlc: wrote {args.manifest}")
    if args.docs:
        args.docs.mkdir(parents=True, exist_ok=True)
        for name, text in docs.items():
            (args.docs / f"{name}.md").write_text(text, encoding="utf-8")
        print(f"midlc: wrote {len(docs)} doc page(s) to {args.docs}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
