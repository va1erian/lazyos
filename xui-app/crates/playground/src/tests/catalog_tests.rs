use rhai_lazy::msg::schema;

use crate::catalog::{Catalog, Kind, NodeId};
use crate::live::{IfaceRef, RegistryRow, TopicRow};

fn row(name: &str, owner: u64, interfaces: &[u64]) -> RegistryRow {
    RegistryRow {
        name: name.to_string(),
        owner,
        interfaces: interfaces.to_vec(),
        ..RegistryRow::default()
    }
}

#[test]
fn every_interface_is_a_root_with_all_its_items_as_children() {
    let catalog = Catalog::from_schema();
    let roots: Vec<_> = catalog.roots().collect();
    assert_eq!(roots.len(), schema::interfaces().len());
    let names: Vec<_> = roots.iter().map(|n| n.label.clone()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "roots are in name order");
    for root in roots {
        let i = catalog
            .interface_node(&root.label)
            .unwrap()
            .item
            .interface();
        let want = i.methods.len() + i.structs.len() + i.enums.len() + i.topics.len();
        assert_eq!(root.children.len(), want, "{}", i.name);
        for child in catalog.children(root) {
            assert_eq!(catalog.parent(child).unwrap().id, root.id);
            assert_eq!(catalog.get(&child.id).unwrap().label, child.label);
        }
    }
}

#[test]
fn node_ids_are_unique_and_stable() {
    let catalog = Catalog::from_schema();
    let mut ids: Vec<_> = catalog.nodes().iter().map(|n| n.id.clone()).collect();
    let before = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), before);
    let again = Catalog::from_schema();
    assert!(again
        .get(&NodeId::method("os.lazy.confd.v1", "Get"))
        .is_some());
    assert!(again
        .get(&NodeId::topic("os.lazy.timed.v1", "time/tick"))
        .is_some());
    assert!(again
        .get(&NodeId::struct_("os.lazy.timed.v1", "Tick"))
        .is_some());
    assert!(again
        .get(&NodeId::enum_("os.lazy.timed.v1", "NtpState"))
        .is_some());
}

#[test]
fn search_ranks_names_before_fields_before_docs() {
    let catalog = Catalog::from_schema();
    let hits = catalog.search("confd", 10);
    assert_eq!(hits[0].id, NodeId::interface("os.lazy.confd.v1"));
    let hits = catalog.search("get_zone", 5);
    assert_eq!(hits[0].id, NodeId::method("os.lazy.timed.v1", "GetZone"));
    let hits = catalog.search("GetZone", 5);
    assert_eq!(hits[0].id, NodeId::method("os.lazy.timed.v1", "GetZone"));
    // A field name finds the method that takes it.
    let hits = catalog.search("unix_secs", 50);
    assert!(hits
        .iter()
        .any(|h| h.id == NodeId::method("os.lazy.timed.v1", "SetTime")));
    // A doc word finds the text.
    let hits = catalog.search("retained time/tick", 50);
    assert!(hits
        .iter()
        .any(|h| h.id == NodeId::topic("os.lazy.timed.v1", "time/tick")));
}

#[test]
fn search_needs_every_term_and_ignores_case_and_empty_queries() {
    let catalog = Catalog::from_schema();
    assert!(catalog.search("", 10).is_empty());
    assert!(catalog.search("   ", 10).is_empty());
    assert!(catalog.search("zzzzqqqq", 10).is_empty());
    let both = catalog.search("CONFD changed", 100);
    assert!(!both.is_empty());
    let lower = catalog.search("confd changed", 100);
    assert_eq!(
        both.iter().map(|h| &h.id).collect::<Vec<_>>(),
        lower.iter().map(|h| &h.id).collect::<Vec<_>>()
    );
    assert!(catalog.search("confd zzzzqqqq", 10).is_empty());
    assert!(catalog.search("confd", 3).len() <= 3);
}

#[test]
fn visible_for_adds_every_ancestor() {
    let catalog = Catalog::from_schema();
    let hits = catalog.search("get_zone", 3);
    let visible = catalog.visible_for(&hits);
    assert!(visible.contains(&NodeId::interface("os.lazy.timed.v1")));
    assert!(visible.contains(&NodeId::method("os.lazy.timed.v1", "GetZone")));
}

#[test]
fn a_struct_knows_where_it_is_used() {
    let catalog = Catalog::from_schema();
    let registry = schema::interface("os.lazy.messenger.registry.v1").unwrap();
    let used = catalog.used_by(registry, "Entry");
    assert!(used.iter().any(|u| u == "List (reply)"), "{used:?}");
    let timed = schema::interface("os.lazy.timed.v1").unwrap();
    assert!(catalog
        .used_by(timed, "Tick")
        .iter()
        .any(|u| u == "topic time/tick"));
}

#[test]
fn live_rows_resolve_interface_ids_and_unknown_ids_show_as_hex() {
    let catalog = Catalog::from_schema();
    let timed = schema::interface("os.lazy.timed.v1").unwrap();
    let confd = schema::interface("os.lazy.confd.v1").unwrap();
    let rows = vec![
        row("zeta.app", 31, &[0xdead_beef]),
        row("os.lazy.timed", 12, &[timed.id]),
        row("os.lazy.confd", 11, &[confd.id, 0x1234]),
    ];
    let join = catalog.join_live(&rows);
    let names: Vec<_> = join.services.iter().map(|s| s.row.name.as_str()).collect();
    assert_eq!(names, ["os.lazy.confd", "os.lazy.timed", "zeta.app"]);
    assert!(join.is_live(timed));
    assert!(!join.is_live(schema::interface("os.lazy.echo.v1").unwrap()));
    assert_eq!(join.services_of(confd).len(), 1);
    let unknown = &join.services[2].interfaces[0];
    assert!(matches!(unknown, IfaceRef::Unknown(0xdead_beef)));
    assert_eq!(unknown.to_string(), "0xdeadbeef");
    assert_eq!(
        join.services[0].interfaces[0].to_string(),
        "os.lazy.confd.v1"
    );
    assert_eq!(join.services[0].interfaces[1].to_string(), "0x1234");
    assert_eq!(join.filter("timed").len(), 1);
    assert_eq!(join.filter("").len(), 3);
    assert_eq!(join.filter("0xdead").len(), 1);
}

#[test]
fn the_owner_of_a_slot_is_a_name_only_when_it_is_exactly_one() {
    let catalog = Catalog::from_schema();
    let join = catalog.join_live(&[row("a", 5, &[]), row("b", 6, &[]), row("c", 6, &[])]);
    assert_eq!(join.owner_name(5), Some("a"));
    assert_eq!(
        join.owner_name(6),
        None,
        "two names own slot 6: not guessed"
    );
    assert_eq!(join.owner_name(99), None);
}

#[test]
fn row_cells_read_like_the_table() {
    let catalog = Catalog::from_schema();
    let mut r = row("x", 1, &[]);
    r.owner_label = 7;
    r.owner_session = 2;
    r.lease_remaining = 40;
    let join = catalog.join_live(&[r, row("y", 2, &[])]);
    let x = join.service("x").unwrap();
    assert_eq!(x.label_text(), "#7");
    assert_eq!(x.session_text(), "2");
    assert_eq!(x.lease_text(), "40 ticks");
    let y = join.service("y").unwrap();
    assert_eq!(y.label_text(), "-");
    assert_eq!(y.session_text(), "system");
    assert_eq!(y.lease_text(), "permanent");
}

#[test]
fn topics_join_declared_patterns_with_live_rows() {
    let catalog = Catalog::from_schema();
    let live = vec![
        TopicRow {
            topic: "time/tick".into(),
            subscribers: 2,
            retained: true,
        },
        TopicRow {
            topic: "system/health/summary".into(),
            subscribers: 1,
            retained: true,
        },
        TopicRow {
            topic: "demo/pg/x".into(),
            subscribers: 3,
            retained: false,
        },
    ];
    let entries = catalog.join_topics(&live);
    let tick = entries.iter().find(|e| e.label == "time/tick").unwrap();
    assert_eq!(tick.subscribers, 2);
    assert_eq!(tick.payload(), "Tick");
    assert_eq!(tick.qos(), Some("latest"));
    assert!(tick.retained);
    assert_eq!(
        tick.permissions,
        ["publish:time/tick", "subscribe:time/tick"]
    );
    let health = entries
        .iter()
        .find(|e| e.label == "system/health/+")
        .unwrap();
    assert_eq!(health.live.len(), 1);
    assert_eq!(health.subscribers, 1);
    let demo = entries.iter().find(|e| e.label == "demo/pg/x").unwrap();
    assert!(demo.declared.is_none());
    assert_eq!(demo.payload(), "bytes");
    assert_eq!(demo.subscribers, 3);
    let labels: Vec<_> = entries.iter().map(|e| e.label.clone()).collect();
    let mut sorted = labels.clone();
    sorted.sort();
    assert_eq!(labels, sorted);
    // Every declared topic is listed, live or not.
    let declared: usize = schema::interfaces().iter().map(|i| i.topics.len()).sum();
    assert_eq!(
        entries.iter().filter(|e| e.declared.is_some()).count(),
        declared
    );
}

#[test]
fn kinds_name_their_groups_and_nodes_summarise_themselves() {
    let catalog = Catalog::from_schema();
    assert_eq!(Kind::Method.group(), "Methods");
    let node = catalog
        .get(&NodeId::method("os.lazy.echo.v1", "Echo"))
        .unwrap();
    assert!(catalog.summary(node).starts_with("Echo(text: String"));
    assert!(catalog.declared_topic("time/tick").is_some());
    assert!(catalog.declared_topic("nobody/declares/this").is_none());
}
