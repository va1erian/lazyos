"""midlc Java backend: declared errors and topics.

Part of the Messenger IDL compiler; see `midlc_java.py`. Emits `<Base>Error`
and `<Base>Exception` for an `errors` block, `<Base>Topics` with a typed
`Topic` (and a `...Filter` for a pattern with placeholders) per topic, and the
topic payload codecs `<Base>Codec` holds.
"""

from __future__ import annotations

from midlc_java_codec import java_file
from midlc_java_names import Names, constant, ident, java_string, javadoc, lower_camel
from midlc_model import Topic, Type


def emit_error_enum(names: Names) -> str:
    base = names.base
    rows = [
        f"    {constant(e.name)}({e.code}, {java_string(e.name)}, {java_string((e.doc or e.name).strip().splitlines()[0])})"
        for e in names.interface.errors
    ]
    body = [
        f"/** The errors {names.interface.name} declares. */",
        f"public enum {base}Error {{",
        ",\n".join(rows) + ";",
        "",
        "    private final int code;",
        "    private final String name, sentence;",
        "",
        f"    {base}Error(int code, String name, String sentence) {{",
        "        this.code = code;",
        "        this.name = name;",
        "        this.sentence = sentence;",
        "    }",
        "",
        "    public int code() {",
        "        return code;",
        "    }",
        "",
        "    /** The name the IDL gives it. */",
        "    public String idlName() {",
        "        return name;",
        "    }",
        "",
        "    /** Its sentence: the first line of its doc comment. */",
        "    public String sentence() {",
        "        return sentence;",
        "    }",
        "",
        "    /** The error with {@code code}, or null. */",
        f"    public static {base}Error of(int code) {{",
        f"        for ({base}Error e : values()) {{",
        "            if (e.code == code) return e;",
        "        }",
        "        return null;",
        "    }",
        "}",
    ]
    return java_file(names, body)


def emit_exception(names: Names) -> str:
    base = names.base
    body = [
        f"/** A declared error of {names.interface.name}; thrown by a service, caught by a client. */",
        f"public class {base}Exception extends lazy.msg.MessengerException {{",
        f"    private final {base}Error error;",
        "",
        f"    public {base}Exception({base}Error error) {{",
        "        this(error, null);",
        "    }",
        "",
        f"    public {base}Exception({base}Error error, String sentence) {{",
        '        this("", error, sentence, null);',
        "    }",
        "",
        "    /** As the runtime reports it: where it failed, the peer's sentence and hint. */",
        f"    public {base}Exception(String where, {base}Error error, String sentence, String hint) {{",
        '        super(where, Wire.need(error, "error").code(), sentence == null || sentence.isEmpty() ? error.sentence() : sentence, hint, error.idlName());',
        "        this.error = error;",
        "    }",
        "",
        f"    public {base}Error error() {{",
        "        return error;",
        "    }",
        "}",
    ]
    return java_file(names, body)


def topic_name(topic: Topic) -> str:
    return lower_camel(topic.suffix)


def topic_params(topic: Topic) -> list[str]:
    return [ident(lower_camel(p.rust_name)) for p in topic.params]


def emit_topics(names: Names) -> str:
    body = [f"/** The topics {names.interface.name} declares. */", f"public final class {names.base}Topics {{"]
    body.append(f"    private {names.base}Topics() {{}}")
    for t in names.interface.topics:
        payload = names.java_type(type_of(names, t.payload), True)
        params = topic_params(t)
        signature = ", ".join(f"String {p}" for p in params)
        values = "".join(f", {p}" for p in params)
        codec = f"{names.codec}.TOPIC_{constant(t.suffix)}"
        qos = f"lazy.msg.Qos.{t.qos.upper()}"
        retained = "true" if t.retained else "false"
        name = topic_name(t)
        body += [""] + javadoc(t.doc or f"`{t.source}`.", "    ")
        body += [
            f"    public static lazy.msg.Topic<{payload}> {name}({signature}) {{",
            f"        return new lazy.msg.Topic<>(TopicNames.name({java_string(t.name)}{values}), {qos}, {retained}, {codec});",
            "    }",
        ]
        if params:
            body += [
                "",
                f"    /** `{t.source}` with a null value for any segment: a wildcard. */",
                f"    public static lazy.msg.TopicFilter<{payload}> {name}Filter({signature}) {{",
                f"        return new lazy.msg.TopicFilter<>(TopicNames.filter({java_string(t.name)}{values}), {qos}, {codec});",
                "    }",
            ]
    body.append("}")
    return java_file(names, body)


def type_of(names: Names, payload: str) -> Type:
    return Type(payload, enum=payload in names.enums)


def topic_codecs(names: Names) -> list[str]:
    out = []
    for t in names.interface.topics:
        struct = t.payload in names.structs
        jt = names.java_type(type_of(names, t.payload), True)
        out += [
            f"    public static final lazy.internal.msg.Topics.Codec<{jt}> TOPIC_{constant(t.suffix)} = new lazy.internal.msg.Topics.Codec<{jt}>() {{",
            "        @Override",
            f"        public byte[] encode({jt} v) {{",
            "            WireWriter w = new WireWriter();",
            f"            write{t.payload}(w, v);" if struct else "            w.variant(1, v);",
            "            return w.toBytes();",
            "        }",
            "",
            "        @Override",
            f"        public {jt} decode(WireReader r) {{",
        ]
        if struct:
            out.append(f"            return read{t.payload}(r);")
        else:
            out += [
                f"            {jt} v = {t.payload}.values()[0];",
                "            while (r.next()) {",
                f'                if (r.id() == 1) v = r.variant({t.payload}.values(), "{t.payload}");',
                "            }",
                "            return v;",
            ]
        out += ["        }", "    };"]
    return out
