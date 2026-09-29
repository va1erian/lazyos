"""midlc parser: recursive descent over the token stream plus validation.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI and
grammar. Produces the `midlc_model.Interface` tree, rejecting malformed input
with a located `MidlError` before any code is generated.
"""

from __future__ import annotations

import re

from midlc_lexer import Token
from midlc_model import (
    BUILTINS,
    Enum,
    Interface,
    Method,
    MidlError,
    Param,
    Struct,
    Type,
    fnv1a32,
    snake_case,
)


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
