"""midlc declarations that state protocol facts: `service`, `errors`, `scope`
`deadline` and `retry` (`docs/client-api-plan.md` 3.1, `docs/midl.md`).

Part of the Messenger IDL compiler; see `midlc.py` for the CLI. Each of these
replaces a family of hand-written constants (registry names, error codes, ACL
scope hashes, per-client timeouts) with one declaration in the interface that
owns the fact:

    service "os.lazy.confd" implements os.lazy.confd.v1;
    interface os.lazy.confd.v1 {
        errors { NotFound = 1001, BadPath = 1002 }
        default deadline=2s retry=never;
        method Get(path: String) -> (value: String) deadline=500ms retry=safe;
        method Offer(...) -> (...) scope "os.lazy.clipboard.write.v1";
    }

The parsing functions take the `Parser` (they only use its token stream);
`attach_services` runs once every file is parsed, because a service may name
an interface of another file.
"""

from __future__ import annotations

import re

from midlc_model import CallPolicy, ErrorCode, Interface, Method, MidlError, Service

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


RETRY_VALUES = ("safe", "never")


def parse_deadline(parser, policy: CallPolicy, keyword, owner: str) -> None:
    """`deadline=<N>ms`, `deadline=<N>s` or `deadline=never` (`owner` names
    the method or the interface default for the messages)."""
    if policy.has_deadline:
        raise MidlError(f"{owner} declares a deadline twice", keyword.line)
    parser.expect("=")
    number = parser.next()
    if number.kind == "ident" and number.text == "never":
        policy.never = True
        return
    if number.kind != "number":
        raise MidlError("expected a number or 'never' after 'deadline=' (deadline=500ms)", number.line)
    unit = parser.next()
    if unit.text not in UNIT_MS:
        raise MidlError(f"deadline {number.text} needs a unit: ms or s, found {unit.text!r}", unit.line)
    millis = int(number.text) * UNIT_MS[unit.text]
    if millis == 0:
        raise MidlError(f"{owner}: a deadline of 0 would fail every call (deadline=never waits for ever)", number.line)
    if millis > MAX_DEADLINE_MS:
        raise MidlError(f"{owner}: the deadline does not fit 32 bits of milliseconds", number.line)
    policy.deadline_ms = millis


def parse_retry(parser, policy: CallPolicy, keyword, owner: str) -> None:
    """`retry=safe` or `retry=never`."""
    if policy.retry is not None:
        raise MidlError(f"{owner} declares a retry policy twice", keyword.line)
    parser.expect("=")
    value = parser.next()
    if value.text not in RETRY_VALUES:
        raise MidlError(f"retry must be one of {', '.join(RETRY_VALUES)}, found {value.text!r}", value.line)
    policy.retry = value.text


def parse_policy_attr(parser, policy: CallPolicy, keyword, owner: str) -> None:
    """The `deadline=` or `retry=` attribute `keyword` starts."""
    if keyword.text == "deadline":
        parse_deadline(parser, policy, keyword, owner)
    else:
        parse_retry(parser, policy, keyword, owner)


def parse_default(parser, interface: Interface, keyword) -> None:
    """`default deadline=<..> retry=<..>;` in an interface: the policy of
    every method that does not state its own (the keyword is consumed)."""
    parser.take_doc()
    if interface.default_policy is not None:
        raise MidlError(f"interface {interface.name!r} declares its default policy twice", keyword.line)
    policy = CallPolicy()
    while parser.peek() and parser.peek().text != ";":
        token = parser.next()
        if token.text not in ("deadline", "retry"):
            raise MidlError(f"unexpected {token.text!r} in `default` (deadline=.. or retry=..)", token.line)
        parse_policy_attr(parser, policy, token, "the interface default")
    parser.expect(";")
    if not policy.stated:
        raise MidlError("`default` states nothing: write deadline=.. and/or retry=..", keyword.line)
    interface.default_policy = policy


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
    if method.oneway and method.policy.has_deadline:
        raise MidlError(f"oneway method {method.name!r} has no reply to wait for, so it takes no deadline")
    if method.oneway and method.policy.retry is not None:
        raise MidlError(f"oneway method {method.name!r} is never retried (a send is refused before it is queued), so it takes no retry")


def apply_default_policy(interface: Interface) -> None:
    """Fold the interface's `default` line into every two-way method that
    does not state the attribute itself; a oneway method keeps none."""
    default = interface.default_policy
    if default is None:
        return
    for method in interface.methods:
        if method.oneway:
            continue
        policy = method.policy
        if not policy.has_deadline:
            policy.deadline_ms, policy.never = default.deadline_ms, default.never
        if policy.retry is None:
            policy.retry = default.retry


def validate_retry_objects(interface: Interface) -> None:
    """A `retry=safe` call is sent again after the peer died; the objects of
    the first send were moved or shared with the dead peer (a channel end is
    gone for good), so a request that carries objects cannot be resent."""
    for method in interface.methods:
        if method.objects and method.policy.retry == "safe":
            raise MidlError(
                f"method {method.name!r} carries objects, so it cannot be retry=safe "
                "(the objects of the first send are gone); use retry=never"
            )


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
