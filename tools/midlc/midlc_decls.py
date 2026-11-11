"""midlc declarations that state protocol facts: `service`, `errors`, `scope`
and `deadline` (`docs/client-api-plan.md` 3.1, `docs/midl.md`).

Part of the Messenger IDL compiler; see `midlc.py` for the CLI. Each of these
replaces a family of hand-written constants (registry names, error codes, ACL
scope hashes, per-client timeouts) with one declaration in the interface that
owns the fact:

    service "os.lazy.confd" implements os.lazy.confd.v1;
    interface os.lazy.confd.v1 {
        errors { NotFound = 1001, BadPath = 1002 }
        method Get(path: String) -> (value: String) deadline=500ms;
        method Offer(...) -> (...) scope "os.lazy.clipboard.write.v1";
    }

The parsing functions take the `Parser` (they only use its token stream);
`attach_services` runs once every file is parsed, because a service may name
an interface of another file.
"""

from __future__ import annotations

import re

from midlc_model import ErrorCode, Interface, Method, MidlError, Service

INTERFACE_NAME = re.compile(r"[a-z0-9_.]+\.v\d+")
# A registry name, optionally a family: `os.lazy.net.nic/{ifname}`.
SERVICE_NAME = re.compile(r"[a-z0-9_.]+(/\{[a-z_][a-z0-9_]*\})?")
ERROR_NAME = re.compile(r"[A-Z][A-Za-z0-9]*")
# The generated Rust enum every `errors` block becomes; no struct may use it.
ERROR_TYPE = "ServiceError"
UNIT_MS = {"ms": 1, "s": 1000}
MAX_DEADLINE_MS = 0xFFFF_FFFF


def parse_service(parser) -> Service:
    """`service "<name>" implements <interface> {, <interface>} ;`"""
    doc = parser.take_doc()
    name = parser.next()
    if name.kind != "string":
        raise MidlError(f"expected a quoted registry name after 'service', found {name.text!r}", name.line)
    text = name.text[1:-1]
    if not SERVICE_NAME.fullmatch(text):
        raise MidlError(
            f"service name {text!r} must be lowercase dotted segments, optionally "
            "ending in /{placeholder} for a family",
            name.line,
        )
    parser.expect("implements")
    interfaces: list[str] = []
    while True:
        item = parser.next()
        if item.kind != "ident" or not INTERFACE_NAME.fullmatch(item.text):
            raise MidlError(f"expected an interface name (reverse-DNS, .vN), found {item.text!r}", item.line)
        if item.text in interfaces:
            raise MidlError(f"service {text!r} lists {item.text!r} twice", item.line)
        interfaces.append(item.text)
        if parser.peek() and parser.peek().text == ",":
            parser.next()
            continue
        break
    parser.expect(";")
    return Service(text, interfaces, doc, name.line)


def parse_errors(parser) -> list[ErrorCode]:
    """`errors { Name = CODE, ... }` (the keyword is already consumed)."""
    parser.take_doc()
    parser.expect("{")
    codes: list[ErrorCode] = []
    while parser.peek() and parser.peek().text != "}":
        name = parser.next()
        if name.kind != "ident":
            raise MidlError(f"expected an error name, found {name.text!r}", name.line)
        doc = parser.take_doc()
        parser.expect("=")
        number = parser.next()
        if number.kind != "number":
            raise MidlError(f"expected a code after '=' for error {name.text!r}", number.line)
        codes.append(ErrorCode(name.text, int(number.text), doc, name.line))
        if parser.peek() and parser.peek().text == ",":
            parser.next()
    parser.expect("}")
    return codes


def parse_scope(parser, method: Method, keyword) -> None:
    """`scope "<interface name>"` on a method."""
    if method.scope is not None:
        raise MidlError(f"method {method.name!r} declares a scope twice", keyword.line)
    name = parser.next()
    if name.kind != "string":
        raise MidlError(
            f"expected a quoted interface name after 'scope' (scope \"os.lazy.x.v1\"), found {name.text!r}",
            name.line,
        )
    text = name.text[1:-1]
    if not INTERFACE_NAME.fullmatch(text):
        raise MidlError(f"scope {text!r} must be reverse-DNS with a .vN suffix, like an interface name", name.line)
    method.scope = text


def parse_deadline(parser, method: Method, keyword) -> None:
    """`deadline=<N>ms` or `deadline=<N>s` on a method."""
    if method.deadline_ms is not None:
        raise MidlError(f"method {method.name!r} declares a deadline twice", keyword.line)
    parser.expect("=")
    number = parser.next()
    if number.kind != "number":
        raise MidlError("expected a number after 'deadline=' (deadline=500ms)", number.line)
    unit = parser.next()
    if unit.text not in UNIT_MS:
        raise MidlError(f"deadline {number.text} needs a unit: ms or s, found {unit.text!r}", unit.line)
    millis = int(number.text) * UNIT_MS[unit.text]
    if millis == 0:
        raise MidlError(f"method {method.name!r}: a deadline of 0 would fail every call", number.line)
    if millis > MAX_DEADLINE_MS:
        raise MidlError(f"method {method.name!r}: the deadline does not fit 32 bits of milliseconds", number.line)
    method.deadline_ms = millis


def validate_errors(interface: Interface) -> None:
    """Names and codes of the `errors` block are unique, and the codes are
    never 0 (the wire's "no error") or wider than the error field's `u32`."""
    codes: dict[int, str] = {}
    names: dict[str, int] = {}
    for error in interface.errors:
        if not ERROR_NAME.fullmatch(error.name):
            raise MidlError(f"error name {error.name!r} must be PascalCase (a Rust variant)", error.line)
        if error.code == 0:
            raise MidlError(f"error {error.name!r}: code 0 is reserved (it means no error)", error.line)
        if error.code > 0xFFFF_FFFF:
            raise MidlError(f"error {error.name!r}: code {error.code} does not fit a u32", error.line)
        if error.code in codes:
            raise MidlError(
                f"errors {codes[error.code]!r} and {error.name!r} both have code {error.code}", error.line
            )
        if error.name in names:
            raise MidlError(f"error {error.name!r} is declared twice", error.line)
        codes[error.code] = error.name
        names[error.name] = error.code
    if interface.errors:
        # A struct of this name would collide with the generated enum.
        if any(s.name == ERROR_TYPE for s in interface.structs):
            raise MidlError(f"struct {ERROR_TYPE!r} collides with the enum generated for `errors`")


def validate_method_attrs(method: Method) -> None:
    if method.oneway and method.deadline_ms is not None:
        raise MidlError(f"oneway method {method.name!r} has no reply to wait for, so it takes no deadline")


def attach_services(interfaces: list[Interface], services: list[Service]) -> None:
    """Link every service to the interfaces it implements (all files parsed).

    An interface may be served by one singleton name and one family at most,
    so `SERVICE_NAME` is never ambiguous; a service may serve any number of
    interfaces (`netd` serves the stack and the sockets on one endpoint)."""
    by_name = {i.name: i for i in interfaces}
    seen: dict[str, Service] = {}
    for service in services:
        if service.name in seen:
            raise MidlError(f"service {service.name!r} is declared twice", service.line)
        seen[service.name] = service
        for name in service.interfaces:
            interface = by_name.get(name)
            if interface is None:
                raise MidlError(f"service {service.name!r} implements unknown interface {name!r}", service.line)
            clash = interface.family if service.instanced else interface.service
            if clash is not None:
                raise MidlError(
                    f"interface {name!r} is served under both {clash.name!r} and {service.name!r}; "
                    "an interface has one service name and one family",
                    service.line,
                )
            interface.services.append(service)
