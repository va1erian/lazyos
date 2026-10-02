"""midlc parser: recursive descent over the token stream plus validation.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI and
grammar. Produces the `midlc_model.Interface` tree, rejecting malformed input
with a located `MidlError` before any code is generated.
"""

from __future__ import annotations

import re

from midlc_lexer import Token
from midlc_model import (
    BODY_FORBIDDEN,
    BUILTINS,
    Enum,
    Interface,
    Method,
    MidlError,
    Param,
    Struct,
    Topic,
    Type,
    fnv1a32,
    snake_case,
)
from midlc_topics import make_topic
from midlc_rings import make_ring, validate_rings
from midlc_transfers import claim_names, make_transfers


# ---------------------------------------------------------------------------
# Lexer / parser
# ---------------------------------------------------------------------------

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
        transfers = []
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
            elif token.text == "transfers":
                if transfers:
                    raise MidlError(f"method {name.text!r} has two `transfers` clauses", token.line)
                transfers = make_transfers(self.parse_params(), token.line)
            else:
                raise MidlError(f"unexpected {token.text!r} in method", token.line)
        self.expect(";")
        return Method(name.text, params, returns, method_id, oneway, doc, transfers)

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

    def parse_topic(self) -> Topic:
        doc = self.take_doc()
        name = self.next()
        if name.kind != "string":
            raise MidlError(f"expected a quoted topic name, found {name.text!r}", name.line)
        self.expect(":")
        payload = self.next()
        if payload.kind != "ident":
            raise MidlError(f"expected a payload type, found {payload.text!r}", payload.line)
        qos = "latest"
        retained = False
        while self.peek() and self.peek().text != ";":
            token = self.next()
            if token.text == "retained":
                retained = True
            elif token.text == "qos":
                self.expect("=")
                value = self.next()
                if value.kind != "ident":
                    raise MidlError("expected a qos value after 'qos='", value.line)
                qos = value.text
            else:
                raise MidlError(f"unexpected {token.text!r} in topic", token.line)
        self.expect(";")
        # The pattern literal is quoted with no escapes; the lexer's string
        # token keeps the quotes, so strip them.
        return make_topic(name.text[1:-1], payload.text, qos, retained, doc, name.line)

    def parse_ring(self):
        doc = self.take_doc()
        name = self.next()
        if name.kind != "ident":
            raise MidlError(f"expected a ring name, found {name.text!r}", name.line)
        self.expect(":")
        layout = self.next()
        if layout.kind != "ident":
            raise MidlError(f"expected a ring layout, found {layout.text!r}", layout.line)
        options: dict[str, str] = {}
        while self.peek() and self.peek().text != ";":
            key = self.next()
            self.expect("=")
            value = self.next()
            if key.kind != "ident" or value.kind != "ident":
                raise MidlError(f"expected key=value in ring {name.text!r}", key.line)
            if key.text in options:
                raise MidlError(f"ring {name.text!r}: {key.text!r} given twice", key.line)
            options[key.text] = value.text
        self.expect(";")
        return make_ring(name.text, layout.text, options, doc, name.line)

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

    def parse_interfaces(self) -> list[Interface]:
        """Every interface in the file (one or more, in source order)."""
        interfaces = [self.parse_interface()]
        while any(t.kind != "doc" for t in self.tokens[self.pos :]):
            interfaces.append(self.parse_interface())
        return interfaces

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
            elif keyword.text == "topic":
                interface.topics.append(self.parse_topic())
            elif keyword.text == "ring":
                interface.rings.append(self.parse_ring())
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
        # The generated codec names are `encode_<snake>`/`decode_<snake>`, so
        # claim those: a topic whose suffix folds to a struct's name must be
        # rejected here rather than collide in the generated Rust.
        claim(f"encode_{snake_case(struct.name)}", struct.name)
        claim(f"decode_{snake_case(struct.name)}", struct.name)
    # Enum variants become `{ENUM}_{VARIANT}` constants; two enums whose
    # folded names meet (`Qos`/`LevelHigh` vs `QosLevel`/`High`) would emit a
    # duplicate `const`, so reject that here.
    for enum in interface.enums:
        for variant in enum.variants:
            claim(
                f"{snake_case(enum.name)}_{snake_case(variant)}".upper(),
                f"{enum.name}::{variant}",
            )
    for method in interface.methods:
        if method.method_id in ids:
            raise MidlError(
                f"method id {method.method_id} is used by {ids[method.method_id]!r} and {method.name!r}"
            )
        ids[method.method_id] = method.name
        if method.oneway and method.returns:
            raise MidlError(f"oneway method {method.name!r} cannot return values")
        claim_names(method, claim)
        if method.params:
            claim(f"encode_{snake_case(method.name)}_args", f"{method.name} (args)")
            claim(f"decode_{snake_case(method.name)}_args", f"{method.name} (args)")
        if method.returns:
            claim(f"encode_{snake_case(method.name)}_reply", f"{method.name} (reply)")
            claim(f"decode_{snake_case(method.name)}_reply", f"{method.name} (reply)")
        for param in method.params + method.returns:
            check_type(param.ty, named)
    for struct in interface.structs:
        for f in struct.fields:
            check_type(f.ty, named)
    validate_topics(interface, named, claim)
    validate_rings(interface, claim)


def validate_topics(interface: Interface, named: set[str], claim) -> None:
    """Every topic must name a payload type of this interface, be unique, and
    not collide with another codec name."""
    patterns: dict[str, str] = {}
    suffixes: dict[str, str] = {}
    for topic in interface.topics:
        origin = f"topic {topic.name!r}"
        if topic.payload not in named:
            raise MidlError(
                f"{origin} payload {topic.payload!r} is not a struct or enum of {interface.name!r}"
            )
        if topic.name in patterns:
            raise MidlError(f"topic {topic.name!r} is declared twice")
        if topic.suffix in suffixes:
            raise MidlError(
                f"topics {topic.name!r} and {suffixes[topic.suffix]!r} share "
                f"the generated name {topic.suffix!r}; rename one"
            )
        patterns[topic.name] = origin
        suffixes[topic.suffix] = topic.name
        for identifier in (
            f"encode_{topic.suffix}",
            f"decode_{topic.suffix}",
            f"name_{topic.suffix}",
            f"publish_{topic.suffix}",
            f"subscribe_{topic.suffix}",
        ):
            claim(identifier, origin)


def check_type(ty: Type, named: set[str]) -> None:
    if ty.name in BODY_FORBIDDEN:
        raise MidlError(
            f"{ty.name} cannot travel in a message body (a handle number means "
            "nothing in the receiver's table); declare it in the method's "
            "`transfers (...)` clause"
        )
    if ty.name in {"Array", "Option"} and len(ty.args) != 1:
        raise MidlError(f"{ty.name} takes exactly one type parameter")
    if ty.name not in BUILTINS and ty.name not in named:
        raise MidlError(f"unknown type {ty.name!r}")
    for arg in ty.args:
        check_type(arg, named)
