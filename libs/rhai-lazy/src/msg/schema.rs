//! The run-time shape of every Messenger interface, and lookups over it.
//!
//! The data itself is generated from `idl/*.midl` by `midlc --schema` into
//! [`super::idl`]; this module only declares the types that table is built
//! from. Wire field ids are not stored: a parameter's id is its 1-based
//! position in its list, exactly as the compiled Rust codecs number them.

use alloc::string::String;

/// How one value travels. Named types resolve inside their own interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Bool,
    I32,
    I64,
    U32,
    U64,
    F64,
    String,
    Bytes,
    /// A channel end (`Channel<I>`): a kernel object a script cannot make.
    Channel,
    /// A shared buffer (`Buffer`, or a `Ring<...>` buffer): likewise.
    Buffer,
    Array(&'static Ty),
    Option(&'static Ty),
    Struct(&'static str),
    /// Travels as a `U32` variant index.
    Enum(&'static str),
}

/// One named, typed slot: a parameter, reply value or struct field.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    /// The wire field id (`= N` in the `.midl`, else the 1-based position).
    pub id: u16,
    pub ty: Ty,
}

#[derive(Debug, Clone, Copy)]
pub struct Method {
    pub name: &'static str,
    pub id: u32,
    pub oneway: bool,
    pub doc: &'static str,
    pub params: &'static [Field],
    pub returns: &'static [Field],
    /// Kernel objects the request carries (its `Channel<I>`, `Buffer` and
    /// `Ring<...>` parameters, nested ones included), in object-list order.
    pub objects: &'static [Object],
}

/// One declared object: a channel (`Some(interface)`, what its receiver
/// sends on it) or a shared buffer (`None`). `name` is the field path
/// (`config.pixels` for a nested one).
#[derive(Debug, Clone, Copy)]
pub struct Object {
    pub name: &'static str,
    pub channel: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub struct Struct {
    pub name: &'static str,
    pub doc: &'static str,
    pub fields: &'static [Field],
}

#[derive(Debug, Clone, Copy)]
pub struct Enum {
    pub name: &'static str,
    pub variants: &'static [&'static str],
}

/// A declared topic: a filter pattern (`+`/`#` wildcards) and its payload.
#[derive(Debug, Clone, Copy)]
pub struct Topic {
    pub pattern: &'static str,
    pub payload: &'static str,
    /// The broker's `Qos` index: latest, buffered, conflate, reliable.
    pub qos: u32,
    pub retained: bool,
    pub doc: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Interface {
    pub name: &'static str,
    pub id: u64,
    pub doc: &'static str,
    pub methods: &'static [Method],
    pub structs: &'static [Struct],
    pub enums: &'static [Enum],
    pub topics: &'static [Topic],
}

/// Every interface compiled from `idl/`.
pub fn interfaces() -> &'static [Interface] {
    super::idl::INTERFACES
}

/// The interface called `name` (`os.lazy.confd.v1`).
pub fn interface(name: &str) -> Option<&'static Interface> {
    interfaces().iter().find(|i| i.name == name)
}

/// `PascalCase` -> `snake_case`, the spelling scripts use for method sugar
/// (`ListTopics` -> `list_topics`), matching `midlc`'s own helper.
pub fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in name.char_indices() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

impl Interface {
    /// A method by its IDL name or its `snake_case` spelling.
    pub fn method(&self, name: &str) -> Option<&'static Method> {
        self.methods
            .iter()
            .find(|m| m.name == name || snake_case(m.name) == name)
    }

    pub fn method_by_id(&self, id: u32) -> Option<&'static Method> {
        self.methods.iter().find(|m| m.id == id)
    }

    pub fn find_struct(&self, name: &str) -> Option<&'static Struct> {
        self.structs.iter().find(|s| s.name == name)
    }

    pub fn find_enum(&self, name: &str) -> Option<&'static Enum> {
        self.enums.iter().find(|e| e.name == name)
    }

    /// The service name an interface is usually registered under: the
    /// interface name without its `.vN` suffix (`os.lazy.confd.v1` ->
    /// `os.lazy.confd`). Some services keep the suffix or another name, so
    /// `msg::connect` falls back to the full name and accepts an explicit one.
    pub fn default_service(&self) -> &'static str {
        match self.name.rsplit_once('.') {
            Some((base, version))
                if version.len() > 1
                    && version.starts_with('v')
                    && version[1..].bytes().all(|b| b.is_ascii_digit()) =>
            {
                base
            }
            _ => self.name,
        }
    }
}

/// Whether the concrete `topic` matches the filter `pattern` (`+` is one
/// segment, a trailing `#` any remaining segments, including none).
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    let mut filter = pattern.split('/');
    let mut name = topic.split('/');
    loop {
        match (filter.next(), name.next()) {
            (Some("#"), _) => return filter.next().is_none(),
            (Some("+"), Some(_)) => {}
            (Some(want), Some(got)) if want == got => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// The declared topic (and its interface) whose pattern matches `topic`.
pub fn declared_topic(topic: &str) -> Option<(&'static Interface, &'static Topic)> {
    interfaces().iter().find_map(|interface| {
        interface
            .topics
            .iter()
            .find(|t| topic_matches(t.pattern, topic))
            .map(|t| (interface, t))
    })
}
