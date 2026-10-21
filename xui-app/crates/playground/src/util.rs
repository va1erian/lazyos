//! Small helpers shared by the model modules: type labels, Rhai literals and
//! the FNV-1a hash the run markers use.

use rhai_lazy::msg::schema::{Field, Interface, Ty};

/// A type as the IDL spells it: `Array<Entry>`, `Option<String>`, `Qos`.
pub fn ty_label(ty: Ty) -> String {
    match ty {
        Ty::Array(inner) => format!("Array<{}>", ty_label(*inner)),
        Ty::Option(inner) => format!("Option<{}>", ty_label(*inner)),
        Ty::Struct(name) | Ty::Enum(name) => name.to_string(),
        Ty::Channel => "Channel".to_string(),
        other => format!("{other:?}"),
    }
}

/// `name: Type, name: Type`, as a signature prints a field list.
pub fn field_list(fields: &[Field]) -> String {
    fields
        .iter()
        .map(|f| format!("{}: {}", f.name, ty_label(f.ty)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A method signature, `Echo(text: String, count: U32) -> (reply: String) oneway`.
pub fn signature(method: &rhai_lazy::msg::schema::Method) -> String {
    let tail = if method.oneway { " oneway" } else { "" };
    format!(
        "{}({}) -> ({}){tail}",
        method.name,
        field_list(method.params),
        field_list(method.returns)
    )
}

/// Whether `ty` is (or contains, through arrays, options and structs of
/// `interface`) a kernel object a script cannot send.
pub fn carries_object(interface: &Interface, ty: Ty) -> bool {
    carries_object_at(interface, ty, 0)
}

fn carries_object_at(interface: &Interface, ty: Ty, depth: u8) -> bool {
    if depth > 8 {
        return false;
    }
    match ty {
        Ty::Channel | Ty::Buffer => true,
        Ty::Array(inner) | Ty::Option(inner) => carries_object_at(interface, *inner, depth + 1),
        Ty::Struct(name) => interface.find_struct(name).is_some_and(|s| {
            s.fields
                .iter()
                .any(|f| carries_object_at(interface, f.ty, depth + 1))
        }),
        _ => false,
    }
}

/// `text` as a Rhai double-quoted string literal. Control characters use the
/// `\u` escape, so the literal stays on one line and reads back unchanged.
pub fn rhai_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether `name` is a Rhai keyword or reserved word. The generated API
/// appends `_` to a parameter named like one (`tools/midlc/midlc_rhai.py`).
pub fn is_rhai_reserved(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "true",
        "false",
        "let",
        "const",
        "if",
        "else",
        "switch",
        "do",
        "while",
        "until",
        "loop",
        "for",
        "in",
        "continue",
        "break",
        "return",
        "throw",
        "try",
        "catch",
        "fn",
        "private",
        "import",
        "export",
        "as",
        "global",
        "Fn",
        "call",
        "curry",
        "is_shared",
        "is_def_fn",
        "is_def_var",
        "this",
        "type_of",
        "print",
        "debug",
        "eval",
        "var",
        "static",
        "shared",
        "goto",
        "exit",
        "match",
        "case",
        "public",
        "protected",
        "new",
        "use",
        "with",
        "module",
        "package",
        "super",
        "thread",
        "spawn",
        "go",
        "await",
        "async",
        "sync",
        "yield",
        "default",
        "void",
        "null",
        "nil",
        "is",
    ];
    RESERVED.contains(&name)
}

/// A parameter name as the generated `sys::*` functions spell it.
pub fn param_name(name: &str) -> String {
    if is_rhai_reserved(name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// 64-bit FNV-1a. The run marker carries this of the script text: enough to
/// tell two scripts apart in a log, and it does not reveal the text.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Lowercase hexadecimal of `bytes`, no separators.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// An interface by reference, comparable (by wire id and name) so the form
/// model can derive `PartialEq`.
#[derive(Debug, Clone, Copy)]
pub struct Iface(pub &'static Interface);

impl PartialEq for Iface {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id && self.0.name == other.0.name
    }
}

impl Eq for Iface {}

impl std::ops::Deref for Iface {
    type Target = Interface;

    fn deref(&self) -> &Interface {
        self.0
    }
}
