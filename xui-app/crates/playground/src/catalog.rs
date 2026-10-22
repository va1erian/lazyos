//! The browsable tree over the interface table, and its full-text search.
//!
//! Pure data in, data out: [`Catalog`] is built from `rhai_lazy`'s generated
//! `schema` table (so a new `.midl` shows up without a change here), and the
//! live registry and topic rows are joined by [`live`](super::live) on top of
//! it. Nodes are addressed by a stable [`NodeId`] string, so a selection
//! survives a rebuild.

use std::collections::HashMap;

use rhai_lazy::msg::schema::{self, Enum, Field, Interface, Method, Struct, Topic};

use crate::util::{signature, ty_label};

/// What a node stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Interface,
    Method,
    Struct,
    Enum,
    Topic,
}

impl Kind {
    /// The label of the group a kind's nodes are listed under.
    pub fn group(self) -> &'static str {
        match self {
            Kind::Interface => "Interfaces",
            Kind::Method => "Methods",
            Kind::Struct => "Structs",
            Kind::Enum => "Enums",
            Kind::Topic => "Topics",
        }
    }
}

/// A node's schema item.
#[derive(Debug, Clone, Copy)]
pub enum Item {
    Interface(&'static Interface),
    Method(&'static Interface, &'static Method),
    Struct(&'static Interface, &'static Struct),
    Enum(&'static Interface, &'static Enum),
    Topic(&'static Interface, &'static Topic),
}

impl Item {
    pub fn kind(&self) -> Kind {
        match self {
            Item::Interface(_) => Kind::Interface,
            Item::Method(..) => Kind::Method,
            Item::Struct(..) => Kind::Struct,
            Item::Enum(..) => Kind::Enum,
            Item::Topic(..) => Kind::Topic,
        }
    }

    pub fn interface(&self) -> &'static Interface {
        match *self {
            Item::Interface(i)
            | Item::Method(i, _)
            | Item::Struct(i, _)
            | Item::Enum(i, _)
            | Item::Topic(i, _) => i,
        }
    }

    /// The doc comment the IDL carries for the item (an enum has none).
    pub fn doc(&self) -> &'static str {
        match *self {
            Item::Interface(i) => i.doc,
            Item::Method(_, m) => m.doc,
            Item::Struct(_, s) => s.doc,
            Item::Enum(..) => "",
            Item::Topic(_, t) => t.doc,
        }
    }
}

/// A stable identifier for a node: the interface name, then `#method:Get`,
/// `#struct:Value`, `#enum:Kind` or `#topic:<pattern>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn interface(name: &str) -> Self {
        NodeId(name.to_string())
    }

    pub fn method(interface: &str, method: &str) -> Self {
        NodeId(format!("{interface}#method:{method}"))
    }

    pub fn struct_(interface: &str, name: &str) -> Self {
        NodeId(format!("{interface}#struct:{name}"))
    }

    pub fn enum_(interface: &str, name: &str) -> Self {
        NodeId(format!("{interface}#enum:{name}"))
    }

    pub fn topic(interface: &str, pattern: &str) -> Self {
        NodeId(format!("{interface}#topic:{pattern}"))
    }
}

/// One tree node.
#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub label: String,
    pub item: Item,
    /// Index of the parent in [`Catalog::nodes`] (`None` for an interface).
    pub parent: Option<usize>,
    /// Indices of the children, in schema order: methods, structs, enums, topics.
    pub children: Vec<usize>,
}

impl Node {
    pub fn kind(&self) -> Kind {
        self.item.kind()
    }
}

/// What part of a node a search term matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matched {
    Name,
    Field(String),
    Type(String),
    Doc,
}

/// One search result.
#[derive(Debug, Clone)]
pub struct Hit {
    pub id: NodeId,
    pub score: u32,
    /// The best match of the first term, to show why the node is listed.
    pub matched: Matched,
}

/// Lowercased text a node is searched in.
#[derive(Default)]
struct Haystack {
    name: String,
    fields: Vec<String>,
    types: Vec<String>,
    doc: String,
}

/// The tree, its index and its search index.
pub struct Catalog {
    interfaces: &'static [Interface],
    nodes: Vec<Node>,
    by_id: HashMap<NodeId, usize>,
    by_interface: HashMap<&'static str, usize>,
    hay: Vec<Haystack>,
    /// Interface node indices in name order.
    roots: Vec<usize>,
}

fn field_texts(fields: &[Field], into: &mut Haystack) {
    for field in fields {
        into.fields.push(field.name.to_lowercase());
        into.types.push(ty_label(field.ty).to_lowercase());
    }
}

impl Catalog {
    /// The catalog of every interface compiled from `idl/`.
    pub fn from_schema() -> Self {
        Self::new(schema::interfaces())
    }

    /// A catalog over an explicit table (tests, a trimmed view).
    pub fn new(interfaces: &'static [Interface]) -> Self {
        let mut catalog = Catalog {
            interfaces,
            nodes: Vec::new(),
            by_id: HashMap::new(),
            by_interface: HashMap::new(),
            hay: Vec::new(),
            roots: Vec::new(),
        };
        let mut sorted: Vec<&'static Interface> = interfaces.iter().collect();
        sorted.sort_by_key(|i| i.name);
        for interface in sorted {
            catalog.add_interface(interface);
        }
        catalog
    }

    fn push(&mut self, node: Node, hay: Haystack) -> usize {
        let index = self.nodes.len();
        self.by_id.insert(node.id.clone(), index);
        if let Some(parent) = node.parent {
            self.nodes[parent].children.push(index);
        }
        self.nodes.push(node);
        self.hay.push(hay);
        index
    }

    fn add_interface(&mut self, interface: &'static Interface) {
        let mut hay = Haystack {
            name: interface.name.to_lowercase(),
            doc: interface.doc.to_lowercase(),
            ..Haystack::default()
        };
        for method in interface.methods {
            hay.fields.push(method.name.to_lowercase());
        }
        let root = self.push(
            Node {
                id: NodeId::interface(interface.name),
                label: interface.name.to_string(),
                item: Item::Interface(interface),
                parent: None,
                children: Vec::new(),
            },
            hay,
        );
        self.by_interface.insert(interface.name, root);
        self.roots.push(root);
        for method in interface.methods {
            let mut hay = Haystack {
                name: format!(
                    "{} {}",
                    method.name.to_lowercase(),
                    schema::snake_case(method.name)
                ),
                doc: method.doc.to_lowercase(),
                ..Haystack::default()
            };
            field_texts(method.params, &mut hay);
            field_texts(method.returns, &mut hay);
            self.push(
                Node {
                    id: NodeId::method(interface.name, method.name),
                    label: method.name.to_string(),
                    item: Item::Method(interface, method),
                    parent: Some(root),
                    children: Vec::new(),
                },
                hay,
            );
        }
        for def in interface.structs {
            let mut hay = Haystack {
                name: def.name.to_lowercase(),
                doc: def.doc.to_lowercase(),
                ..Haystack::default()
            };
            field_texts(def.fields, &mut hay);
            self.push(
                Node {
                    id: NodeId::struct_(interface.name, def.name),
                    label: def.name.to_string(),
                    item: Item::Struct(interface, def),
                    parent: Some(root),
                    children: Vec::new(),
                },
                hay,
            );
        }
        for def in interface.enums {
            let mut hay = Haystack {
                name: def.name.to_lowercase(),
                ..Haystack::default()
            };
            hay.fields = def.variants.iter().map(|v| v.to_lowercase()).collect();
            self.push(
                Node {
                    id: NodeId::enum_(interface.name, def.name),
                    label: def.name.to_string(),
                    item: Item::Enum(interface, def),
                    parent: Some(root),
                    children: Vec::new(),
                },
                hay,
            );
        }
        for topic in interface.topics {
            let hay = Haystack {
                name: topic.pattern.to_lowercase(),
                doc: topic.doc.to_lowercase(),
                types: vec![topic.payload.to_lowercase()],
                ..Haystack::default()
            };
            self.push(
                Node {
                    id: NodeId::topic(interface.name, topic.pattern),
                    label: topic.pattern.to_string(),
                    item: Item::Topic(interface, topic),
                    parent: Some(root),
                    children: Vec::new(),
                },
                hay,
            );
        }
    }

    pub fn interfaces(&self) -> &'static [Interface] {
        self.interfaces
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The interface nodes, sorted by name.
    pub fn roots(&self) -> impl Iterator<Item = &Node> {
        self.roots.iter().map(|&i| &self.nodes[i])
    }

    pub fn get(&self, id: &NodeId) -> Option<&Node> {
        self.by_id.get(id).map(|&i| &self.nodes[i])
    }

    /// The node of the interface called `name`.
    pub fn interface_node(&self, name: &str) -> Option<&Node> {
        self.by_interface.get(name).map(|&i| &self.nodes[i])
    }

    pub fn children<'a>(&'a self, node: &'a Node) -> impl Iterator<Item = &'a Node> {
        node.children.iter().map(|&i| &self.nodes[i])
    }

    pub fn parent(&self, node: &Node) -> Option<&Node> {
        node.parent.map(|i| &self.nodes[i])
    }

    /// The interface with this wire id, if the table has it.
    pub fn interface_by_id(&self, id: u64) -> Option<&'static Interface> {
        self.interfaces.iter().find(|i| i.id == id)
    }

    /// The declared topic (and its interface) whose pattern matches the
    /// concrete `topic`.
    pub fn declared_topic(&self, topic: &str) -> Option<(&'static Interface, &'static Topic)> {
        self.interfaces.iter().find_map(|interface| {
            interface
                .topics
                .iter()
                .find(|t| schema::topic_matches(t.pattern, topic))
                .map(|t| (interface, t))
        })
    }

    /// Where a struct is used: the methods taking or returning it and the
    /// structs and topics that hold it. Labels like `Get (reply)`.
    pub fn used_by(&self, interface: &Interface, struct_name: &str) -> Vec<String> {
        let mentions = |fields: &[Field]| {
            fields.iter().any(|f| {
                ty_label(f.ty).contains(&format!("<{struct_name}>"))
                    || ty_label(f.ty) == struct_name
            })
        };
        let mut out = Vec::new();
        for method in interface.methods {
            if mentions(method.params) {
                out.push(format!("{} (argument)", method.name));
            }
            if mentions(method.returns) {
                out.push(format!("{} (reply)", method.name));
            }
        }
        for def in interface.structs {
            if def.name != struct_name && mentions(def.fields) {
                out.push(format!("struct {}", def.name));
            }
        }
        for topic in interface.topics {
            if topic.payload == struct_name {
                out.push(format!("topic {}", topic.pattern));
            }
        }
        out
    }

    /// Search names, field names, type names and docs. Every whitespace
    /// separated term must match somewhere; the best matches come first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for (node, hay) in self.nodes.iter().zip(&self.hay) {
            let mut total = 0;
            let mut first = None;
            let mut all = true;
            for term in &terms {
                match score_term(hay, term) {
                    Some((score, matched)) => {
                        total += score;
                        first.get_or_insert(matched);
                    }
                    None => {
                        all = false;
                        break;
                    }
                }
            }
            if all {
                // A method or struct is more useful than its interface when
                // both match equally, an interface than a topic's doc text.
                let bonus = match node.kind() {
                    Kind::Interface => 3,
                    Kind::Method => 2,
                    _ => 1,
                };
                hits.push(Hit {
                    id: node.id.clone(),
                    score: total * 10 + bonus,
                    matched: first.unwrap_or(Matched::Name),
                });
            }
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        hits.truncate(limit);
        hits
    }

    /// The nodes to show for a search: the hits and every ancestor, so the
    /// tree can expand to them.
    pub fn visible_for(&self, hits: &[Hit]) -> std::collections::HashSet<NodeId> {
        let mut set = std::collections::HashSet::new();
        for hit in hits {
            let mut at = self.by_id.get(&hit.id).copied();
            while let Some(index) = at {
                if !set.insert(self.nodes[index].id.clone()) {
                    break;
                }
                at = self.nodes[index].parent;
            }
        }
        set
    }

    /// The one-line summary a tree row shows beside a node.
    pub fn summary(&self, node: &Node) -> String {
        match node.item {
            Item::Interface(i) => {
                format!("{} methods, {} topics", i.methods.len(), i.topics.len())
            }
            Item::Method(_, m) => signature(m),
            Item::Struct(_, s) => format!("{} fields", s.fields.len()),
            Item::Enum(_, e) => e.variants.join(", "),
            Item::Topic(_, t) => t.payload.to_string(),
        }
    }
}

fn score_term(hay: &Haystack, term: &str) -> Option<(u32, Matched)> {
    let mut best: Option<(u32, Matched)> = None;
    let mut offer = |score: u32, matched: Matched| {
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, matched));
        }
    };
    if hay.name.split_whitespace().any(|w| w == term) {
        offer(100, Matched::Name);
    } else if hay
        .name
        .split(['.', '/', ' ', '_'])
        .any(|w| w.starts_with(term))
    {
        offer(80, Matched::Name);
    } else if hay.name.contains(term) {
        offer(60, Matched::Name);
    }
    if let Some(field) = hay.fields.iter().find(|f| f.contains(term)) {
        offer(40, Matched::Field(field.clone()));
    }
    if let Some(ty) = hay.types.iter().find(|t| t.contains(term)) {
        offer(30, Matched::Type(ty.clone()));
    }
    if hay.doc.contains(term) {
        offer(20, Matched::Doc);
    }
    best
}
