//! A node as documentation: the blocks the doc pane draws.
//!
//! The text is the schema's own (`docs/messenger-playground-plan.md` D6): the
//! doc comments of the IDL, the typed signatures, and the Rhai spellings of a
//! method. Nothing is invented: a field without a doc comment has none shown.

use rhai_lazy::msg::schema::{Enum, Interface, Method, Struct, Topic};

use crate::catalog::{Catalog, Item, Node};
use crate::rhai_names::{
    alias, enum_const, has_sugar, module, module_usable, scriptable, sys_function, topic_helper,
    variant_const, wildcards,
};
use crate::util::{field_list, param_name, signature, ty_label};

/// The Markdown overview every interface page links to.
pub fn messenger_doc() -> String {
    format!("{}/messenger.md", fhs::docs::OS_DOCS)
}

/// Interfaces that handle credentials: a banner, and the app keeps no
/// arguments, replies or output for them (plan, "Guard rails").
const SECRET_INTERFACES: &[&str] = &[
    "os.lazy.keyd.v1",
    "os.lazy.keyd.appsecret.v1",
    "os.lazy.accounts.v1",
    "os.lazy.elevd.v1",
];

/// What a call can do that deserves a warning chip (plan, D7 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    /// Handles credentials or secrets.
    Secret,
    /// Stops or restarts services, apps or the machine.
    StopsServices,
    /// Reads what the user last copied.
    ReadsClipboard,
    /// Needs an administrator through `elevd`'s trusted prompt.
    Admin,
}

impl Risk {
    pub fn text(self) -> &'static str {
        match self {
            Risk::Secret => "handles credentials: nothing is kept for this interface",
            Risk::StopsServices => "can stop or restart services, apps or the machine",
            Risk::ReadsClipboard => "reads what you copied",
            Risk::Admin => "may ask an administrator to approve it on the trusted prompt",
        }
    }
}

/// Whether the interface carries the credentials banner.
pub fn is_secret_interface(interface: &Interface) -> bool {
    SECRET_INTERFACES.contains(&interface.name)
}

/// The risks of calling `method`, for chips and the confirmation step.
pub fn risks(interface: &Interface, method: &Method) -> Vec<Risk> {
    // An explicit list, not a search of the prose: "survives a reboot" (confd.Info) and "a
    // request during a shutdown" (init.Launch) are not calls that stop anything.
    const STOPS: &[(&str, &str)] = &[
        ("os.lazy.init.v1", "Stop"),
        ("os.lazy.init.v1", "Shutdown"),
        ("os.lazy.init.v1", "RestartService"),
        ("os.lazy.init.v1", "ReloadService"),
        ("os.lazy.init.v1", "RevertService"),
        ("os.lazy.init.v1", "RelaunchApp"),
        ("os.lazy.init.app.v1", "Quit"),
        ("os.lazy.lifecycle.v1", "Shutdown"),
        ("os.lazy.logind.v1", "Logout"),
    ];
    const CLIPBOARD_READS: &[&str] = &["Request", "Fetch", "Serialize", "Current"];
    let mut out = Vec::new();
    if is_secret_interface(interface) {
        out.push(Risk::Secret);
    }
    if STOPS.contains(&(interface.name, method.name)) {
        out.push(Risk::StopsServices);
    }
    if interface.name == "os.lazy.clipboard.v1" && CLIPBOARD_READS.contains(&method.name) {
        out.push(Risk::ReadsClipboard);
    }
    if mentions_elevd(method) {
        out.push(Risk::Admin);
    }
    out
}

/// The method's doc names `elevd` or an administrator.
pub fn mentions_elevd(method: &Method) -> bool {
    let doc = method.doc.to_lowercase();
    doc.contains("elevd") || doc.contains("administrator")
}

/// One kernel object a request carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub name: String,
    /// The interface a channel speaks, `None` for a shared buffer.
    pub channel: Option<String>,
}

/// Everything the doc pane says about a method besides its prose.
#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub signature: String,
    pub oneway: bool,
    pub objects: Vec<ObjectInfo>,
    /// A script can call it (no objects, not gate-only).
    pub scriptable: bool,
    pub risks: Vec<Risk>,
    pub spellings: Vec<Spelling>,
}

/// One way to write a call in Rhai.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spelling {
    pub label: &'static str,
    pub code: String,
}

/// The Rhai spellings of a call, the arguments written as the parameter
/// names: `msg::connect(..).get(path)`, `sys::confd::get(path)` and the
/// generic `invoke`.
pub fn spellings(interface: &Interface, method: &Method) -> Vec<Spelling> {
    if !scriptable(interface, method) {
        return Vec::new();
    }
    let args = method
        .params
        .iter()
        .map(|p| param_name(p.name))
        .collect::<Vec<_>>();
    let connect = format!("msg::connect(\"{}\")", interface.name);
    let mut out = Vec::new();
    if let Some(function) = sys_function(interface, method) {
        out.push(Spelling {
            label: "sys module",
            code: format!("{function}({})", args.join(", ")),
        });
    }
    if has_sugar(method) {
        out.push(Spelling {
            label: "connect",
            code: format!(
                "{connect}.{}({})",
                rhai_lazy::msg::schema::snake_case(method.name),
                args.join(", ")
            ),
        });
    }
    out.push(Spelling {
        label: "invoke",
        code: format!(
            "{connect}.{}(\"{}\", [{}])",
            if method.oneway {
                "invoke_oneway"
            } else {
                "invoke"
            },
            method.name,
            args.join(", ")
        ),
    });
    out
}

pub fn method_info(interface: &Interface, method: &Method) -> MethodInfo {
    MethodInfo {
        signature: signature(method),
        oneway: method.oneway,
        objects: method
            .objects
            .iter()
            .map(|o| ObjectInfo {
                name: o.name.to_string(),
                channel: o.channel.map(str::to_string),
            })
            .collect(),
        scriptable: scriptable(interface, method),
        risks: risks(interface, method),
        spellings: spellings(interface, method),
    }
}

/// The page of the generated reference for an interface.
pub fn idl_doc_path(interface: &Interface) -> String {
    format!("{}/idl/{}.md", fhs::docs::OS_DOCS, interface.name)
}

/// A chip's weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipKind {
    Info,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub kind: ChipKind,
    pub text: String,
}

/// One row of a field table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRow {
    pub name: String,
    pub ty: String,
    pub note: String,
}

/// A link to a Markdown page the Docs app opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLink {
    pub label: String,
    pub path: String,
}

/// One block of the doc pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Title(String),
    Heading(String),
    Paragraph(String),
    Signature(String),
    Code { label: String, text: String },
    Fields(Vec<FieldRow>),
    List(Vec<String>),
    Chips(Vec<Chip>),
    Links(Vec<DocLink>),
}

fn paragraphs(text: &str, into: &mut Vec<Block>) {
    for part in text.split("\n\n") {
        let part = part.trim();
        if !part.is_empty() {
            into.push(Block::Paragraph(part.to_string()));
        }
    }
}

fn field_rows(fields: &[rhai_lazy::msg::schema::Field]) -> Vec<FieldRow> {
    fields
        .iter()
        .map(|f| FieldRow {
            name: f.name.to_string(),
            ty: ty_label(f.ty),
            note: String::new(),
        })
        .collect()
}

fn links(interface: &Interface) -> Block {
    Block::Links(vec![
        DocLink {
            label: format!("{} reference", interface.name),
            path: idl_doc_path(interface),
        },
        DocLink {
            label: "Messenger".to_string(),
            path: messenger_doc(),
        },
    ])
}

/// The doc pane for `node`.
pub fn blocks(catalog: &Catalog, node: &Node) -> Vec<Block> {
    match node.item {
        Item::Interface(i) => interface_blocks(i),
        Item::Method(i, m) => method_blocks(i, m),
        Item::Struct(i, s) => struct_blocks(catalog, i, s),
        Item::Enum(i, e) => enum_blocks(i, e),
        Item::Topic(i, t) => topic_blocks(i, t),
    }
}

fn interface_blocks(interface: &Interface) -> Vec<Block> {
    let mut out = vec![Block::Title(interface.name.to_string())];
    if is_secret_interface(interface) {
        out.push(Block::Chips(vec![Chip {
            kind: ChipKind::Warning,
            text: "Credentials: arguments, replies and output of this interface are not kept"
                .to_string(),
        }]));
    }
    let mut facts = vec![format!(
        "Default service name: {}",
        interface.default_service()
    )];
    match module(interface) {
        Some(m) if !module_usable(interface) => facts.push(format!(
            "Rhai module sys::{} cannot be written ({} is a Rhai keyword): use msg::connect",
            m.alias, m.alias
        )),
        Some(m) => facts.push(format!("Rhai module: sys::{}", m.alias)),
        None => facts.push(format!(
            "No sys module (a kernel scope); alias would be sys::{}",
            alias(interface.name)
        )),
    }
    out.push(Block::List(facts));
    paragraphs(interface.doc, &mut out);
    out.push(Block::Heading("Methods".to_string()));
    out.push(Block::List(
        interface.methods.iter().map(signature).collect(),
    ));
    if !interface.topics.is_empty() {
        out.push(Block::Heading("Topics".to_string()));
        out.push(Block::List(
            interface
                .topics
                .iter()
                .map(|t| format!("{} : {}", t.pattern, t.payload))
                .collect(),
        ));
    }
    out.push(links(interface));
    out
}

fn method_blocks(interface: &Interface, method: &Method) -> Vec<Block> {
    let info = method_info(interface, method);
    let mut out = vec![
        Block::Title(format!("{}.{}", interface.name, method.name)),
        Block::Signature(info.signature.clone()),
    ];
    let mut chips = Vec::new();
    if info.oneway {
        chips.push(Chip {
            kind: ChipKind::Info,
            text: "one-way: sent, no reply (invoke_oneway)".to_string(),
        });
    }
    for object in &info.objects {
        chips.push(Chip {
            kind: ChipKind::Warning,
            text: match &object.channel {
                Some(i) => format!(
                    "carries a channel `{}` ({i}): a script cannot send it",
                    object.name
                ),
                None => format!(
                    "carries a shared buffer `{}`: a script cannot send it",
                    object.name
                ),
            },
        });
    }
    if !info.scriptable && info.objects.is_empty() {
        chips.push(Chip {
            kind: ChipKind::Warning,
            text: "served by the kernel gate only: not reachable from a script".to_string(),
        });
    }
    for risk in &info.risks {
        chips.push(Chip {
            kind: ChipKind::Warning,
            text: risk.text().to_string(),
        });
    }
    if !chips.is_empty() {
        out.push(Block::Chips(chips));
    }
    paragraphs(method.doc, &mut out);
    if !method.params.is_empty() {
        out.push(Block::Heading("Arguments".to_string()));
        out.push(Block::Fields(field_rows(method.params)));
    }
    if !method.returns.is_empty() {
        out.push(Block::Heading("Reply".to_string()));
        out.push(Block::Fields(field_rows(method.returns)));
    }
    for spelling in &info.spellings {
        out.push(Block::Code {
            label: spelling.label.to_string(),
            text: spelling.code.clone(),
        });
    }
    out.push(links(interface));
    out
}

fn struct_blocks(catalog: &Catalog, interface: &Interface, def: &Struct) -> Vec<Block> {
    let mut out = vec![Block::Title(format!("struct {}", def.name))];
    paragraphs(def.doc, &mut out);
    out.push(Block::Fields(field_rows(def.fields)));
    let used = catalog.used_by(interface, def.name);
    if !used.is_empty() {
        out.push(Block::Heading("Used by".to_string()));
        out.push(Block::List(used));
    }
    if module_usable(interface) {
        out.push(Block::Code {
            label: "zero value".to_string(),
            text: format!(
                "sys::{}::new_{}()",
                alias(interface.name),
                rhai_lazy::msg::schema::snake_case(def.name)
            ),
        });
    }
    out.push(links(interface));
    out
}

fn enum_blocks(interface: &Interface, def: &Enum) -> Vec<Block> {
    let mut out = vec![Block::Title(format!("enum {}", def.name))];
    out.push(Block::Paragraph(
        "A script writes the variant as a string; the wire carries its index.".to_string(),
    ));
    out.push(Block::List(
        def.variants
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{i}: \"{v}\""))
            .collect(),
    ));
    if module_usable(interface) {
        let a = alias(interface.name);
        out.push(Block::Code {
            label: "constants".to_string(),
            text: def
                .variants
                .iter()
                .map(|v| format!("sys::{a}::{}", variant_const(def.name, v)))
                .chain(std::iter::once(format!(
                    "sys::{a}::{}",
                    enum_const(def.name)
                )))
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }
    out.push(links(interface));
    out
}

fn topic_blocks(interface: &Interface, topic: &Topic) -> Vec<Block> {
    let qos = rhai_lazy::msg::topics::QOS[topic.qos as usize % 4];
    let mut out = vec![Block::Title(format!("topic {}", topic.pattern))];
    out.push(Block::List(vec![
        format!("Payload: {}", topic.payload),
        format!("QoS: {qos}"),
        format!("Retained: {}", if topic.retained { "yes" } else { "no" }),
        format!("Declared by {}", interface.name),
    ]));
    paragraphs(topic.doc, &mut out);
    out.push(Block::Heading("Permissions".to_string()));
    out.push(Block::List(crate::live::permissions(topic.pattern)));
    if let Some(helper) = topic_helper(interface, topic).filter(|_| module_usable(interface)) {
        let a = alias(interface.name);
        let placeholders = wildcards(topic.pattern);
        let mut lines = vec![
            format!("sys::{a}::on_{}(|e| print(e.payload))", helper.helper),
            format!("sys::{a}::subscribe_{}()", helper.helper),
            format!("sys::{a}::publish_{}(payload)", helper.helper),
        ];
        if placeholders > 0 {
            lines.push(format!(
                "sys::{a}::{}_topic({})",
                helper.helper,
                vec!["value"; placeholders].join(", ")
            ));
        }
        out.push(Block::Code {
            label: "helpers".to_string(),
            text: lines.join("\n"),
        });
    }
    out.push(Block::Code {
        label: "msg".to_string(),
        text: format!(
            "msg::on(\"{}\", |e| print(e.payload));\nmsg::run();",
            topic.pattern
        ),
    });
    out.push(links(interface));
    out
}

/// The fields of a method's request or reply, as one line (for a tooltip).
pub fn fields_line(fields: &[rhai_lazy::msg::schema::Field]) -> String {
    field_list(fields)
}
