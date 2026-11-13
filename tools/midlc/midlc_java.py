"""midlc Java backend (`--java DIR`): typed Messenger clients and servers for LazyVM.

Part of the Messenger IDL compiler. For each interface it writes, into the
Java package named like the interface, the types LazyVM's `lazy.msg` runtime
serves and calls (LazyVM's docs/msg.md, section 3):

* `<Base>`: the client (`connect()`, `within(ms)`, one method per call
  returning a `CompletionStage`) and `serve(...)`;
* `<Base>Server`: what a service implements;
* `<Base>Listener`: what a `Channel<I>` carries, both on the client that
  receives its events and on the service that sends them;
* `<Base>Error`/`<Base>Exception` for an `errors` block, `<Base>Topics` for
  topics, one class per struct and enum, `<M>Args`/`<M>Reply` records;
* `<Base>Codec`: the wire code those use, public for the runtime only.

Methods with a `Ring` parameter are left out: LazyVM has no ring runtime yet.
"""

from __future__ import annotations

from midlc_java_codec import (
    arg_names,
    decoded_args,
    emit_record,
    java_file,
    message_codecs,
    param_list,
    positional,
    reply_type,
    record_codec,
)
from midlc_java_decls import emit_error_enum, emit_exception, emit_topics, topic_codecs
from midlc_java_names import IMPORTED, JAVA_LANG, Names, constant, has_ring, java_string, javadoc, member
from midlc_model import Interface, Method, MidlError, Type


def generate_java(interfaces: list[Interface]) -> dict[str, str]:
    """Every generated file as `{path relative to the output root: text}`."""
    by_name = {i.name: i for i in interfaces}
    targets = channel_targets(interfaces)
    files: dict[str, str] = {}
    for interface in interfaces:
        names = Names(interface, by_name)
        for name, text in interface_files(names, interface.name in targets).items():
            files[f"{names.package.replace('.', '/')}/{name}.java"] = text
    return files


def channel_targets(interfaces: list[Interface]) -> set[str]:
    """The interfaces some `Channel<I>` of the compile carries."""
    found: set[str] = set()

    def walk(ty: Type) -> None:
        if ty.name == "Channel" and ty.args:
            found.add(ty.args[0].name)
        for arg in ty.args:
            walk(arg)

    for interface in interfaces:
        for m in interface.methods:
            for p in m.params:
                walk(p.ty)
        for s in interface.structs:
            for f in s.fields:
                walk(f.ty)
    return found


def supported(names: Names) -> list[Method]:
    return [m for m in names.interface.methods if not any(has_ring(p.ty, names.interface) for p in m.params)]


def interface_files(names: Names, channel_target: bool) -> dict[str, str]:
    """The files of one interface. A listener is generated when the interface has
    events or a `Channel` carries it: it holds the events and oneway methods."""
    interface = names.interface
    methods = supported(names)
    calls = [m for m in methods if not m.event]
    listened = [m for m in methods if m.oneway or m.event]
    has_listener = channel_target or any(m.event for m in methods)
    if not has_listener:
        listened = []
    check_names(names, methods, calls, listened)
    files: dict[str, str] = {}
    for s in interface.structs:
        files[s.name] = emit_record(names, s.name, s.fields, s.doc)
    for e in interface.enums:
        files[e.name] = java_file(names, [f"public enum {e.name} {{", "    " + ", ".join(constant(v) for v in e.variants), "}"])
    for m in methods:
        if not positional(m):
            files[f"{m.name}Args"] = emit_record(names, f"{m.name}Args", m.params, f"The arguments of `{m.name}`.")
        if not m.oneway and len(m.returns) > 1:
            files[f"{m.name}Reply"] = emit_record(names, f"{m.name}Reply", m.returns, f"The reply of `{m.name}`.")
    if calls:
        files[names.base] = emit_client(names, calls)
        files[f"{names.base}Server"] = emit_server(names, calls)
    if has_listener:
        files[f"{names.base}Listener"] = emit_listener(names, listened)
    if interface.errors:
        files[f"{names.base}Error"] = emit_error_enum(names)
        files[f"{names.base}Exception"] = emit_exception(names)
    if interface.topics:
        files[f"{names.base}Topics"] = emit_topics(names)
    files[names.codec] = emit_codec(names, methods, calls, listened, has_listener)
    return files


def check_names(names: Names, methods: list[Method], calls: list[Method], listened: list[Method]) -> None:
    """A declared struct or enum may not take a name the backend generates or imports."""
    base = names.base
    generated = {names.codec, f"{base}Error", f"{base}Exception", f"{base}Topics"}
    generated |= {base, f"{base}Server"} if calls else set()
    generated |= {f"{base}Listener"} if listened else set()
    generated |= {f"{m.name}Args" for m in methods if not positional(m)}
    generated |= {f"{m.name}Reply" for m in methods if not m.oneway and len(m.returns) > 1}
    if names.base in IMPORTED | JAVA_LANG:
        raise MidlError(f"{names.interface.name}: the Java class name {names.base!r} is taken by an import")
    for name in [s.name for s in names.interface.structs] + [e.name for e in names.interface.enums]:
        if name in generated or name in IMPORTED | JAVA_LANG:
            raise MidlError(f"{names.interface.name}: the type name {name!r} is taken by the Java backend")
    for s in names.interface.structs:
        if any(has_ring(f.ty, names.interface) for f in s.fields):
            raise MidlError(f"{names.interface.name}: struct {s.name} holds a Ring, which the Java backend has no type for")


def header_id(names: Names, method: Method) -> str:
    """The id the request header carries: the method's scope, else the interface."""
    value = method.scope_id if method.scope is not None else names.interface.id
    return f"0x{value:016x}L"


def deadline(method: Method) -> str:
    policy = method.policy
    if policy.never or policy.deadline_ms is None:
        return "lazy.internal.msg.Client.NEVER"
    return f"{policy.deadline_ms}L"


def method_id(method: Method) -> str:
    if not 0 <= method.method_id <= 0x7FFFFFFF:
        raise MidlError(f"{method.name}: method id {method.method_id} does not fit a Java int")
    return f"M_{constant(method.name)}"


def emit_client(names: Names, calls: list[Method]) -> str:
    interface, base = names.interface, names.base
    service, family = interface.service, interface.family
    body = javadoc(interface.docs, "") + [f"public final class {base} {{"]
    body.append(f"    public static final String INTERFACE = {java_string(interface.name)};")
    if service:
        body.append(f"    public static final String SERVICE = {java_string(service.name)};")
    if family:
        body.append(f"    public static final String SERVICE_PREFIX = {java_string(family.prefix)};")
    body += [
        "",
        "    private final lazy.internal.msg.Client client;",
        "",
        f"    private {base}(lazy.internal.msg.Client client) {{",
        "        this.client = client;",
        "    }",
        "",
    ]
    if service:
        body += [
            f"    /** A client of {service.name}; it is resolved on the first call. */",
            f"    public static {base} connect() {{",
            "        return connect(SERVICE);",
            "    }",
            "",
        ]
    if family:
        body += [
            f"    /** A client of the instance `{family.name}` names. */",
            f"    public static {base} connectInstance(String instance) {{",
            '        return connect(SERVICE_PREFIX + Wire.need(instance, "instance"));',
            "    }",
            "",
        ]
    body += [
        "    /** A client of the service registered as {@code name}. */",
        f"    public static {base} connect(String name) {{",
        f"        return new {base}(lazy.internal.msg.Client.connect(name, INTERFACE, {names.codec}.ERRORS));",
        "    }",
        "",
        "    /** The same connection with every call bounded by {@code millis}; 0 means no bound. */",
        f"    public {base} within(long millis) {{",
        f"        return new {base}(client.within(millis));",
        "    }",
        "",
    ]
    if service:
        body += [
            f"    /** Serves {{@code server}} as {service.name}. */",
            f"    public static lazy.msg.Registration serve({base}Server server) {{",
            "        return serve(SERVICE, server);",
            "    }",
            "",
        ]
    body += [
        "    /** Serves {@code server} under {@code name}; a name already served fails with EEXIST. */",
        f"    public static lazy.msg.Registration serve(String name, {base}Server server) {{",
        '        Wire.need(server, "server");',
        f"        return lazy.internal.msg.Server.serve(name, call -> {names.codec}.dispatch(server, call));",
        "    }",
    ]
    for m in calls:
        jname = member(m.name, 1 if not positional(m) else len(m.params))
        body += [""] + javadoc(m.doc, "    ")
        body.append(f"    public CompletionStage<{reply_type(names, m)}> {jname}({param_list(names, m)}) {{")
        reply = "r -> null" if m.oneway else f"{names.codec}::read{m.name}Reply"
        safe = "true" if m.policy.retry == "safe" else "false"
        body += [
            f"        return client.call({header_id(names, m)}, {names.codec}.{method_id(m)}, {java_string(m.name)},",
            f"                {deadline(m)}, {safe}, {'true' if m.oneway else 'false'},",
            f"                w -> {names.codec}.write{m.name}Args(w{', ' + arg_names(names, m) if m.params else ''}), {reply});",
            "    }",
        ]
    body.append("}")
    return java_file(names, body)


def emit_server(names: Names, calls: list[Method]) -> str:
    body = [f"/** What a service of {names.interface.name} implements: one method per call. */"]
    body.append(f"public interface {names.base}Server {{")
    for m in calls:
        jname = member(m.name, 1 if not positional(m) else len(m.params))
        body += javadoc(m.doc, "    ")
        body.append(f"    CompletionStage<{reply_type(names, m)}> {jname}({param_list(names, m)});")
        body.append("")
    body.append("}")
    return java_file(names, body)


def emit_listener(names: Names, methods: list[Method]) -> str:
    """One `void` method per event or oneway; abstract when it is the only one,
    so a lambda is a listener, else a default that ignores it."""
    lone = len(methods) == 1
    body = [f"/** The events of {names.interface.name}: what a listener receives and a remote one sends. */"]
    body.append(("@FunctionalInterface\n" if lone else "") + f"public interface {names.base}Listener {{")
    for m in methods:
        jname = member(m.name, 1 if not positional(m) else len(m.params))
        body += javadoc(m.doc, "    ")
        signature = f"void {jname}({param_list(names, m)})"
        body.append(f"    {signature};" if lone else f"    default {signature} {{}}")
        body.append("")
    body.append("}")
    return java_file(names, body)


def emit_codec(names: Names, methods: list[Method], calls: list[Method], listened: list[Method],
               has_listener: bool) -> str:
    interface, codec = names.interface, names.codec
    body = [
        f"/** The wire code of {interface.name}, for the generated classes and the runtime; not API. */",
        "@SuppressWarnings(\"unchecked\")",
        f"public final class {codec} {{",
        f"    public static final long INTERFACE_ID = 0x{interface.id:016x}L;",
    ]
    for m in methods:
        body.append(f"    public static final int {method_id(m)} = {m.method_id};")
    if interface.errors:
        body += [
            "    public static final lazy.internal.msg.Client.Errors ERRORS = (where, f) -> {",
            f"        {names.base}Error e = f.domain.isEmpty() ? {names.base}Error.of(f.code) : null;",
            f"        return e != null ? new {names.base}Exception(where, e, f.sentence, f.hint) : lazy.internal.msg.Client.ERRNO.of(where, f);",
            "    };",
        ]
    else:
        body.append("    public static final lazy.internal.msg.Client.Errors ERRORS = lazy.internal.msg.Client.ERRNO;")
    body += topic_codecs(names)
    body += ["", f"    private {codec}() {{}}", ""]
    for s in interface.structs:
        body += record_codec(names, s.name, s.name, s.fields)
    for m in methods:
        body += message_codecs(names, m)
    if calls:
        body += dispatch(names, calls)
    if has_listener:
        body += deliver(names, listened)
    body.append("}")
    return java_file(names, body)


def dispatch(names: Names, calls: list[Method]) -> list[str]:
    out = [
        f"    public static CompletionStage<WireWriter> dispatch({names.base}Server s, lazy.internal.msg.Server.Call c) {{",
        "        switch (c.method()) {",
    ]
    for m in calls:
        jname = member(m.name, 1 if not positional(m) else len(m.params))
        args = f"read{m.name}Args(c.args())"
        call = f"s.{jname}({decoded_args(names, m, args)})"
        write = "(w, v) -> {}" if m.oneway else f"{names.codec}::write{m.name}Reply"
        out += [f"            case {method_id(m)}: {{", f"                c.expect({header_id(names, m)});"]
        if positional(m) and m.params:
            out.append(f"                Object[] a = {args};")
        out += [f"                return lazy.internal.msg.Server.reply({call}, {write});", "            }"]
    out += ["            default: throw Wire.unknownMethod(c.method());", "        }", "    }", ""]
    return out


def deliver(names: Names, methods: list[Method]) -> list[str]:
    listener = f"{names.base}Listener"
    out = [
        "    /** Runs one received event on a listener. */",
        "    public static void deliver(Object target, int method, WireReader r) {",
        f"        {listener} l = ({listener}) target;",
        "        switch (method) {",
    ]
    for m in methods:
        jname = member(m.name, 1 if not positional(m) else len(m.params))
        args = f"read{m.name}Args(r)"
        out.append(f"            case {method_id(m)}: {{")
        if positional(m) and m.params:
            out.append(f"                Object[] a = {args};")
        out += [f"                l.{jname}({decoded_args(names, m, args)});", "                break;", "            }"]
    out += [
        "            default: break;",
        "        }",
        "    }",
        "",
        "    /** The listener a service sees for a decoded channel end: its methods send the events. */",
        f"    public static {listener} remote(Object raw) {{",
        "        return new Remote(Listeners.endpoint(raw));",
        "    }",
        "",
        f"    private static final class Remote implements {listener}, lazy.msg.Peer, Listeners.Remote {{",
        "        private final Listeners.Endpoint end;",
        "",
        "        Remote(Listeners.Endpoint end) {",
        "            this.end = end;",
        "        }",
    ]
    for m in methods:
        jname = member(m.name, 1 if not positional(m) else len(m.params))
        out += [
            "",
            "        @Override",
            f"        public void {jname}({param_list(names, m)}) {{",
            "            WireWriter w = new WireWriter();",
            f"            write{m.name}Args(w{', ' + arg_names(names, m) if m.params else ''});",
            f"            end.send({header_id(names, m)}, {method_id(m)}, w);",
            "        }",
        ]
    out += [
        "",
        "        @Override",
        "        public void close() {",
        "            end.close();",
        "        }",
        "",
        "        @Override",
        "        public CompletionStage<Void> closed() {",
        "            return end.closed();",
        "        }",
        "",
        "        @Override",
        "        public Listeners.Endpoint endpoint() {",
        "            return end;",
        "        }",
        "    }",
        "",
    ]
    return out

