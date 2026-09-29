"""midlc Rust backend: emits typed wire helpers over the libmessenger codec.

Part of the Messenger IDL compiler (issue #90); see `midlc.py` for the CLI. One
`encode_*`/`decode_*` pair is generated per struct and per method args/reply
record, nested into a module named after the interface.
"""

from __future__ import annotations

from midlc_model import Interface, Param, SCALARS, Type, snake_case

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


def emit_rust(interface: Interface) -> str:
    lines = [
        f"/// `{interface.name}` (interface id `{interface.id:#x}`).",
        f"pub mod {interface.module} {{",
        "    use alloc::vec::Vec;",
        "    // Not every interface needs every codec item (`Kind` is only used by nested values).",
        "    #[allow(unused_imports)]",
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
    if lines[-1] == "":
        lines.pop()
    lines.append("}")
    return "\n".join(lines)
