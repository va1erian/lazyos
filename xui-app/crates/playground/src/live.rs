//! The live side of the catalog: the registry's name table and the broker's
//! topic list, joined with what the schema declares.
//!
//! Both are plain rows the app copies out of the registry `List` and the
//! broker's `ListTopics`; nothing here talks to the fabric.

use std::fmt;

use rhai_lazy::msg::schema::{self, Interface, Topic};

use crate::catalog::Catalog;

/// One registered name (`os.lazy.messenger.registry.v1` `Entry`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryRow {
    pub name: String,
    pub object: u64,
    /// Task slot that owns the name.
    pub owner: u64,
    pub interfaces: Vec<u64>,
    pub lease_remaining: u64,
    pub owner_uid: u64,
    /// Label id of the owner; `0` is unlabelled.
    pub owner_label: u64,
    pub owner_session: u64,
}

/// One topic the broker has seen (`TopicInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopicRow {
    pub topic: String,
    pub subscribers: u64,
    pub retained: bool,
}

/// An interface id resolved through the schema.
#[derive(Debug, Clone, Copy)]
pub enum IfaceRef {
    Known(&'static Interface),
    /// A hash the table has no entry for (an app's own interface).
    Unknown(u64),
}

impl IfaceRef {
    /// The wire id, known or not.
    pub fn id(&self) -> u64 {
        match self {
            IfaceRef::Known(i) => i.id,
            IfaceRef::Unknown(id) => *id,
        }
    }
}

impl PartialEq for IfaceRef {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for IfaceRef {}

impl fmt::Display for IfaceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IfaceRef::Known(i) => f.write_str(i.name),
            IfaceRef::Unknown(id) => write!(f, "0x{id:x}"),
        }
    }
}

/// A registry row with its interface ids resolved.
#[derive(Debug, Clone)]
pub struct LiveService {
    pub row: RegistryRow,
    pub interfaces: Vec<IfaceRef>,
}

impl LiveService {
    /// The owner's label as a table cell: `-` when unlabelled.
    pub fn label_text(&self) -> String {
        match self.row.owner_label {
            0 => "-".to_string(),
            id => format!("#{id}"),
        }
    }

    /// The owner's session as a table cell: `system` before login.
    pub fn session_text(&self) -> String {
        match self.row.owner_session {
            0 => "system".to_string(),
            id => id.to_string(),
        }
    }

    /// The lease as a table cell: `permanent` when it never expires.
    pub fn lease_text(&self) -> String {
        match self.row.lease_remaining {
            0 => "permanent".to_string(),
            ticks => format!("{ticks} ticks"),
        }
    }

    pub fn serves(&self, interface: &Interface) -> bool {
        self.row.interfaces.contains(&interface.id)
    }
}

/// The registry joined with the schema, sorted by name.
#[derive(Debug, Clone, Default)]
pub struct LiveJoin {
    pub services: Vec<LiveService>,
}

impl LiveJoin {
    /// The services whose interface list names `interface`.
    pub fn services_of(&self, interface: &Interface) -> Vec<&LiveService> {
        self.services
            .iter()
            .filter(|s| s.serves(interface))
            .collect()
    }

    /// The `LIVE` badge: some registered name serves `interface`.
    pub fn is_live(&self, interface: &Interface) -> bool {
        self.services.iter().any(|s| s.serves(interface))
    }

    /// The service called `name`.
    pub fn service(&self, name: &str) -> Option<&LiveService> {
        self.services.iter().find(|s| s.row.name == name)
    }

    /// The registered name that owns task slot `slot` (a publisher id), or
    /// `None`: a slot with no name, or two names, is not guessed at.
    pub fn owner_name(&self, slot: u64) -> Option<&str> {
        let mut owners = self.services.iter().filter(|s| s.row.owner == slot);
        let first = owners.next()?;
        owners.next().is_none().then_some(first.row.name.as_str())
    }

    /// The rows whose name contains `text` (case-insensitive), for the
    /// Live tab's filter box.
    pub fn filter(&self, text: &str) -> Vec<&LiveService> {
        let needle = text.to_lowercase();
        self.services
            .iter()
            .filter(|s| {
                needle.is_empty()
                    || s.row.name.to_lowercase().contains(&needle)
                    || s.interfaces
                        .iter()
                        .any(|i| i.to_string().to_lowercase().contains(&needle))
            })
            .collect()
    }
}

/// A declared topic, with the interface that declares it.
#[derive(Debug, Clone, Copy)]
pub struct DeclaredTopic {
    pub interface: &'static Interface,
    pub topic: &'static Topic,
}

/// One row of the Topics tab: a declared pattern with the live topics under
/// it, or a live topic nothing declares.
#[derive(Debug, Clone)]
pub struct TopicEntry {
    /// The pattern, or the concrete name of an undeclared live topic.
    pub label: String,
    pub declared: Option<DeclaredTopic>,
    pub live: Vec<TopicRow>,
    pub subscribers: u64,
    /// Declared retained, or some live topic under it is.
    pub retained: bool,
    /// What a package would list to publish or subscribe: the strings the
    /// generated docs print in their Permissions column.
    pub permissions: Vec<String>,
}

impl TopicEntry {
    /// The payload type name, or `bytes` for an undeclared topic.
    pub fn payload(&self) -> &str {
        self.declared.map_or("bytes", |d| d.topic.payload)
    }

    /// The QoS name the declaration asks for, if any.
    pub fn qos(&self) -> Option<&'static str> {
        self.declared
            .map(|d| rhai_lazy::msg::topics::QOS[d.topic.qos as usize % 4])
    }
}

impl Catalog {
    /// Resolve the registry's rows through the schema.
    pub fn join_live(&self, rows: &[RegistryRow]) -> LiveJoin {
        let mut services: Vec<LiveService> = rows
            .iter()
            .map(|row| LiveService {
                row: row.clone(),
                interfaces: row
                    .interfaces
                    .iter()
                    .map(|&id| {
                        self.interface_by_id(id)
                            .map_or(IfaceRef::Unknown(id), IfaceRef::Known)
                    })
                    .collect(),
            })
            .collect();
        services.sort_by(|a, b| a.row.name.cmp(&b.row.name));
        LiveJoin { services }
    }

    /// Declared topics merged with the broker's list, sorted by label.
    pub fn join_topics(&self, live: &[TopicRow]) -> Vec<TopicEntry> {
        let mut entries: Vec<TopicEntry> = Vec::new();
        for interface in self.interfaces() {
            for topic in interface.topics {
                let rows: Vec<TopicRow> = live
                    .iter()
                    .filter(|r| schema::topic_matches(topic.pattern, &r.topic))
                    .cloned()
                    .collect();
                entries.push(TopicEntry {
                    label: topic.pattern.to_string(),
                    declared: Some(DeclaredTopic { interface, topic }),
                    subscribers: rows.iter().map(|r| r.subscribers).sum(),
                    retained: topic.retained || rows.iter().any(|r| r.retained),
                    permissions: permissions(topic.pattern),
                    live: rows,
                });
            }
        }
        for row in live {
            if self.declared_topic(&row.topic).is_none() {
                entries.push(TopicEntry {
                    label: row.topic.clone(),
                    declared: None,
                    subscribers: row.subscribers,
                    retained: row.retained,
                    permissions: permissions(&row.topic),
                    live: vec![row.clone()],
                });
            }
        }
        entries.sort_by(|a, b| a.label.cmp(&b.label));
        entries
    }
}

/// `publish:<pattern>` and `subscribe:<pattern>`, as a package manifest and
/// the generated docs spell them.
pub fn permissions(pattern: &str) -> Vec<String> {
    vec![format!("publish:{pattern}"), format!("subscribe:{pattern}")]
}
