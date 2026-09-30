"""midlc Rust backend: emits typed wire helpers over the libmessenger codec.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI. One
`encode_*`/`decode_*` pair is generated per struct and per method args/reply
record, nested into a module named after the interface.
"""

from __future__ import annotations

from midlc_model import Interface, Param, SCALARS, Topic, Type, snake_case
from midlc_topics import qos_rust_expr

# Topic codegen lives here; the shared Rust runtime (`TOPIC_SUPPORT`) is in
# `midlc_topic_support` to keep this file small.

# ---------------------------------------------------------------------------
# Rust code generation
# ---------------------------------------------------------------------------

RUST_TYPE = {
    "String": "alloc::string::String",
    "Bytes": "alloc::vec::Vec<u8>",
    "Handle": "u64",
    "Buffer": "libmessenger::BufferDesc",
}


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


def payload_kind(interface: Interface, topic: Topic) -> str:
    """`"struct"` when the topic names a struct, else `"enum"` (checked by the
    parser, which rejects a payload that is neither)."""
    return "struct" if any(s.name == topic.payload for s in interface.structs) else "enum"


def emit_topic(interface: Interface, topic: Topic) -> list[str]:
    """One declared topic: its constants plus typed name/codec/publish/subscribe
    helpers built over [`TOPIC_SUPPORT`]."""
    suffix = topic.suffix
    upper = suffix.upper()
    kind = payload_kind(interface, topic)
    param_names = [p.rust_name for p in topic.params]
    name_sig = ", ".join(f"{name}: &str" for name in param_names)
    name_args = ", ".join(param_names)
    value_param = "value: u32" if kind == "enum" else f"value: &{topic.payload}"
    retained = str(topic.retained).lower()

    lines: list[str] = []
    lines += emit_doc_lines(topic.doc, indent="    ")
    retained_note = ", retained" if topic.retained else ""
    lines.append(f"    /// The declared `{topic.name}` topic (`{topic.payload}`, `{topic.qos}`{retained_note}).")
    lines.append(f'    pub const TOPIC_{upper}: &str = "{topic.name}";')
    lines.append(f"    /// The `{topic.name}` delivery policy.")
    lines.append(f"    pub const TOPIC_{upper}_QOS: u32 = {qos_rust_expr(topic.qos)};")
    lines.append(f"    /// Whether `{topic.name}` publishes are retained.")
    lines.append(f"    pub const TOPIC_{upper}_RETAINED: bool = {retained};")
    lines.append("")
    lines.append(f"    /// Build the concrete `{topic.name}` name; each wildcard takes one literal segment.")
    lines.append(f"    pub fn name_{suffix}({name_sig}) -> Result<String, topics::TopicError> {{")
    lines.append(f"        topics::build(TOPIC_{upper}, &[{name_args}], topics::Mode::Publish)")
    lines.append("    }")
    lines.append("")
    if kind == "struct":
        lines.append(f"    /// Encode a `{topic.payload}` payload for `{topic.name}`.")
        lines.append(f"    pub fn encode_{suffix}(value: &{topic.payload}) -> Result<Vec<u8>, Error> {{")
        lines.append(f"        encode_{snake_case(topic.payload)}(value)")
        lines.append("    }")
        lines.append("")
        lines.append(f"    /// Decode a `{topic.name}` payload; malformed bytes are an error.")
        lines.append(f"    pub fn decode_{suffix}(body: &[u8]) -> Result<{topic.payload}, Error> {{")
        lines.append(f"        decode_{snake_case(topic.payload)}(body)")
        lines.append("    }")
    else:
        lines.append(f"    /// Encode a `{topic.payload}` payload for `{topic.name}` (travels as a `U32`).")
        lines.append(f"    pub fn encode_{suffix}(value: u32) -> Result<Vec<u8>, Error> {{")
        lines.append("        let mut target = Encoder::new();")
        lines.append("        target.u32(1, value)?;")
        lines.append("        Ok(target.finish())")
        lines.append("    }")
        lines.append("")
        lines.append(f"    /// Decode a `{topic.name}` payload (a `U32`); malformed bytes are an error.")
        lines.append(f"    pub fn decode_{suffix}(body: &[u8]) -> Result<u32, Error> {{")
        lines.append("        let mut value = 0u32;")
        lines.append("        let mut decoder = Decoder::new(body);")
        lines.append("        while let Some(field) = decoder.next()? {")
        lines.append("            if field.id == 1 {")
        lines.append("                value = field.as_u32()?;")
        lines.append("            }")
        lines.append("        }")
        lines.append("        Ok(value)")
        lines.append("    }")
    lines.append("")
    publish_params = ", ".join(["publisher: &mut P"] + [f"{name}: &str" for name in param_names] + [value_param])
    lines.append(f"    /// Publish a typed `{topic.payload}` on `{topic.name}`.")
    lines.append(f"    pub fn publish_{suffix}<P>({publish_params}) -> Result<u64, P::Error>")
    lines.append("    where")
    lines.append("        P: topics::Publish,")
    lines.append("        P::Error: From<topics::TopicError>,")
    lines.append("    {")
    lines.append(f"        let topic = name_{suffix}({name_args}).map_err(P::Error::from)?;")
    lines.append(f"        let payload = encode_{suffix}(value)")
    lines.append("            .map_err(|error| P::Error::from(topics::TopicError::Encode(error)))?;")
    lines.append(f"        publisher.publish_topic(&topic, &payload, TOPIC_{upper}_RETAINED)")
    lines.append("    }")
    lines.append("")
    subscribe_params = ", ".join(["subscriber: &mut S"] + [f"{name}: &str" for name in param_names])
    lines.append(f"    /// Subscribe to `{topic.name}` with its declared QoS.")
    lines.append(f"    pub fn subscribe_{suffix}<S>({subscribe_params}) -> Result<S::Subscription, S::Error>")
    lines.append("    where")
    lines.append("        S: topics::Subscribe,")
    lines.append("        S::Error: From<topics::TopicError>,")
    lines.append("    {")
    lines.append(f"        let filter = topics::build(TOPIC_{upper}, &[{name_args}], topics::Mode::Subscribe)")
    lines.append("            .map_err(S::Error::from)?;")
    lines.append(f"        subscriber.subscribe_topic(&filter, TOPIC_{upper}_QOS)")
    lines.append("    }")
    return lines


def emit_topic_table(interfaces: list[Interface]) -> str:
    """The crate-level table of every declared topic, with the permission
    strings derived from each pattern (never hand-typed)."""
    lines = [
        "/// Every topic declared across the compiled `.midl` files (issue #307).",
        "#[rustfmt::skip]",
        "pub static DECLARED_TOPICS: &[topics::TopicDecl] = &[",
    ]
    for interface in interfaces:
        for topic in interface.topics:
            lines.append("    topics::TopicDecl {")
            lines.append(f'        interface: "{interface.name}",')
            lines.append(f'        name: "{topic.name}",')
            lines.append(f'        payload: "{topic.payload}",')
            lines.append(f"        qos: {qos_rust_expr(topic.qos)},")
            lines.append(f"        retained: {str(topic.retained).lower()},")
            lines.append(f'        publish_permission: "publish:{topic.name}",')
            lines.append(f'        subscribe_permission: "subscribe:{topic.name}",')
            lines.append("    },")
    lines.append("];")
    lines.append("")
    lines.append("/// The declared topic whose pattern matches the concrete `topic`.")
    lines.append("#[rustfmt::skip]")
    lines.append("pub fn declared_topic(topic: &str) -> Option<&'static topics::TopicDecl> {")
    lines.append("    DECLARED_TOPICS.iter().find(|decl| topics::matches(decl.name, topic))")
    lines.append("}")
    return "\n".join(lines) + "\n"


def emit_rust(interface: Interface) -> str:
    lines = [
        f"/// `{interface.name}` (interface id `{interface.id:#x}`).",
        "#[rustfmt::skip]",
        f"pub mod {interface.module} {{",
        "    use alloc::vec::Vec;",
        "    #[allow(unused_imports)]",
        "    use alloc::string::String;",
        "    // Not every interface needs every codec item (`Kind` is only used by nested values).",
        "    #[allow(unused_imports)]",
        "    use libmessenger::{Decoder, Encoder, Error, Kind};",
        "    // Only interfaces that declare topics use the shared topic runtime.",
        "    #[allow(unused_imports)]",
        "    use super::topics;",
        "",
        "    /// The interface id: the FNV-1a hash of the `.vN` interface name.",
        f"    pub const INTERFACE_ID: u64 = {interface.id:#x};",
        "",
    ]
    # Enums travel as `U32` on the wire; the variant indices are emitted as
    # constants so callers never hand-type a discriminant.
    for enum in interface.enums:
        for index, variant in enumerate(enum.variants):
            lines.append(f"    /// `{enum.name}::{variant}` wire value.")
            lines.append(f"    pub const {snake_case(enum.name).upper()}_{snake_case(variant).upper()}: u32 = {index};")
        lines.append("")
    for struct in interface.structs:
        lines += emit_struct(struct.name, struct.fields, struct.doc).splitlines()
        lines.append("")
    for method in interface.methods:
        lines.append(f"    /// `{method.name}` method id.")
        lines.append(f"    pub const METHOD_{method.name.upper()}: u32 = {method.method_id};")
    lines.append("")
    for method in interface.methods:
        # A method with neither arguments nor results emits no item, so its
        # doc comment must not be left dangling onto the next method's item.
        if method.params or method.returns:
            lines += emit_doc_lines(method.doc, indent="    ")
        if method.params:
            lines += emit_message(method.name, "args", method.params).splitlines()
            lines.append("")
        if method.returns:
            lines += emit_message(method.name, "reply", method.returns).splitlines()
            lines.append("")
    for topic in interface.topics:
        lines += emit_topic(interface, topic)
        lines.append("")
    if lines[-1] == "":
        lines.pop()
    lines.append("}")
    return "\n".join(lines)
