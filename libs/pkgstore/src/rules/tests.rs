use super::*;

fn manifest(permissions: &str) -> Manifest {
    manifest_with("", permissions)
}

/// A manifest with `resident = true` and `permissions`.
fn resident(permissions: &str) -> Manifest {
    manifest_with("resident = true\n", permissions)
}

fn manifest_with(entry: &str, permissions: &str) -> Manifest {
    let text = format!(
        "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\nversion = \"1.0.0\"\n\
         [entry]\nbinary = \"bin/app.elf\"\n{entry}[permissions]\n{permissions}"
    );
    lazypkg::parse_manifest(&text).expect("valid")
}

#[test]
fn resident_compiles_the_tray_init_app_and_tray_topic_rules() {
    let rules = compile(&resident("")).unwrap();
    let resolve = |name: &str| allow(resolve_scope::INTERFACE_ID, fnv1a32(name));
    let sub = |segment: &str| allow(subscribe_scope::INTERFACE_ID, fnv1a32(segment));
    let topics_id = topics::INTERFACE_ID;
    let expected = [
        allow(fnv1a64("os.lazy.shell.tray.v1"), ANY_METHOD),
        resolve("os.lazy.shell.tray.v1"),
        resolve("os.lazy.shell.tray"),
        allow(fnv1a64("os.lazy.init.app.v1"), ANY_METHOD),
        resolve("os.lazy.init.app.v1"),
        resolve("os.lazy.init.app"),
        sub("session"),
        sub("+"),
        sub("shell"),
        sub("tray"),
        allow(topics_id, topics::METHOD_SUBSCRIBE),
        allow(topics_id, topics::METHOD_UNSUBSCRIBE),
        allow(topics_id, topics::METHOD_NEXTEVENT),
        allow(topics_id, topics::METHOD_ACK),
        allow(topics_id, topics::METHOD_STATS),
        resolve("os.lazy.messenger.topics.v1"),
        resolve("os.lazy.messenger.topics"),
    ];
    assert_eq!(rules, expected);
    // `installed` loads them too, from the manifest and nowhere else.
    assert!(installed(&resident("")).unwrap().starts_with(&expected));
    assert!(!installed(&manifest(""))
        .unwrap()
        .contains(&allow(fnv1a64("os.lazy.shell.tray.v1"), ANY_METHOD)));
}

#[test]
fn resident_adds_no_duplicate_of_what_the_manifest_lists() {
    let listed = "interfaces = [\"os.lazy.init.app.v1\", \"os.lazy.shell.tray.v1\"]\n\
                  topics = [\"subscribe:session/+/shell/tray\"]\n";
    // The same rules as the resident app that lists nothing, in its order.
    let both = compile(&resident(listed)).unwrap();
    for (index, rule) in both.iter().enumerate() {
        assert!(!both[..index].contains(rule), "duplicate {rule:?}");
    }
    assert_eq!(both, compile(&manifest(listed)).unwrap());
    let mut sorted = both.clone();
    let mut implied = compile(&resident("")).unwrap();
    sorted.sort_by_key(|rule| (rule.interface_id, rule.method));
    implied.sort_by_key(|rule| (rule.interface_id, rule.method));
    assert_eq!(sorted, implied);
}

#[test]
fn a_non_resident_manifest_compiles_as_before() {
    let text = "interfaces = [\"os.lazy.display.v1\"]\n";
    let plain = compile(&manifest(text)).unwrap();
    assert_eq!(
        compile(&manifest_with("resident = false\n", text)).unwrap(),
        plain
    );
    assert_eq!(plain.len(), 3);
}

fn allow(interface_id: u64, method: u32) -> LabelRule {
    LabelRule {
        interface_id,
        method,
        allow: true,
    }
}

#[test]
fn every_installed_app_may_report_its_failure_to_init() {
    let rules = installed(&manifest("")).unwrap();
    assert_eq!(rules, baseline());
    assert!(rules.contains(&allow(
        fnv1a64("os.lazy.init.v1"),
        init::METHOD_REPORTFAILURE
    )));
    assert!(rules.contains(&allow(resolve_scope::INTERFACE_ID, fnv1a32("os.lazy.init"))));
    // Nothing else of `init`: no launching, stopping or shutting down.
    let init_rules = rules
        .iter()
        .filter(|rule| rule.interface_id == fnv1a64("os.lazy.init.v1"))
        .count();
    assert_eq!(init_rules, 1);
    // On top of what the manifest asks for, never instead of it.
    let display = installed(&manifest(
        "interfaces = [\"os.lazy.display.v1\"]
",
    ))
    .unwrap();
    assert!(display.starts_with(
        &compile(&manifest(
            "interfaces = [\"os.lazy.display.v1\"]
"
        ))
        .unwrap()
    ));
}

#[test]
fn the_label_is_app_colon_system_name() {
    assert_eq!(label("org.lazy.demo"), "app:org.lazy.demo");
}

#[test]
fn no_permissions_compile_to_no_rules() {
    assert_eq!(compile(&manifest("")).unwrap(), Vec::new());
}

#[test]
fn an_interface_allows_every_method_and_each_service_name() {
    let rules = compile(&manifest("interfaces = [\"os.lazy.display.v1\"]\n")).unwrap();
    let id = fnv1a64("os.lazy.display.v1");
    assert_eq!(
        rules,
        [
            allow(id, ANY_METHOD),
            allow(resolve_scope::INTERFACE_ID, fnv1a32("os.lazy.display.v1")),
            allow(resolve_scope::INTERFACE_ID, fnv1a32("os.lazy.display")),
        ]
    );
}

#[test]
fn the_exact_rule_list_for_a_realistic_manifest() {
    let rules = compile(&manifest(
        "interfaces = [\"os.lazy.display.v1\", \"os.lazy.input.v1\"]\n\
         topics = [\"subscribe:system/events/open/+\", \"publish:app/org.lazy.demo/#\"]\n",
    ))
    .unwrap();
    let resolve = |name: &str| allow(resolve_scope::INTERFACE_ID, fnv1a32(name));
    let sub = |segment: &str| allow(subscribe_scope::INTERFACE_ID, fnv1a32(segment));
    let topics_id = topics::INTERFACE_ID;
    let expected = [
        allow(fnv1a64("os.lazy.display.v1"), ANY_METHOD),
        resolve("os.lazy.display.v1"),
        resolve("os.lazy.display"),
        allow(fnv1a64("os.lazy.input.v1"), ANY_METHOD),
        resolve("os.lazy.input.v1"),
        resolve("os.lazy.input"),
        sub("system"),
        sub("events"),
        sub("open"),
        sub("+"),
        // The app's own namespace needs no segment rules, only the broker.
        allow(topics_id, topics::METHOD_PUBLISH),
        allow(topics_id, topics::METHOD_SUBSCRIBE),
        allow(topics_id, topics::METHOD_UNSUBSCRIBE),
        allow(topics_id, topics::METHOD_NEXTEVENT),
        allow(topics_id, topics::METHOD_ACK),
        allow(topics_id, topics::METHOD_STATS),
        resolve("os.lazy.messenger.topics.v1"),
        resolve("os.lazy.messenger.topics"),
    ];
    assert_eq!(rules, expected);
}

#[test]
fn publish_only_does_not_get_the_subscribe_methods() {
    let rules = compile(&manifest("topics = [\"publish:app/org.lazy.demo/x\"]\n")).unwrap();
    let topics_id = topics::INTERFACE_ID;
    assert!(rules.contains(&allow(topics_id, topics::METHOD_PUBLISH)));
    assert!(!rules.contains(&allow(topics_id, topics::METHOD_SUBSCRIBE)));
    assert!(!rules.contains(&allow(topics_id, topics::METHOD_LISTTOPICS)));
    // Nothing but the broker: no segment rules for the own namespace.
    assert!(rules
        .iter()
        .all(|rule| rule.interface_id != publish_scope::INTERFACE_ID));
}

#[test]
fn publishing_to_another_namespace_needs_segment_rules() {
    let rules = compile(&manifest(
        "topics = [\"publish:session/1/clipboard/changed\"]\n",
    ))
    .unwrap();
    let publish = |segment: &str| allow(publish_scope::INTERFACE_ID, fnv1a32(segment));
    for segment in ["session", "1", "clipboard", "changed"] {
        assert!(rules.contains(&publish(segment)), "{segment}");
    }
    assert!(!rules.contains(&allow(subscribe_scope::INTERFACE_ID, fnv1a32("session"))));
}

#[test]
fn a_session_topic_compiles_to_the_plus_segment() {
    // The kernel checks the caller's own session segment as `+`.
    let rules = compile(&manifest("topics = [\"publish:session/+/selection\"]\n")).unwrap();
    let publish = |segment: &str| allow(publish_scope::INTERFACE_ID, fnv1a32(segment));
    for segment in ["session", "+", "selection"] {
        assert!(rules.contains(&publish(segment)), "{segment}");
    }
}

#[test]
fn a_service_whose_name_differs_gets_both_names() {
    let rules = compile(&manifest("interfaces = [\"os.lazy.accounts.v1\"]\n")).unwrap();
    let resolve = |name: &str| allow(resolve_scope::INTERFACE_ID, fnv1a32(name));
    assert!(rules.contains(&resolve("os.lazy.accountsd")));
    assert!(rules.contains(&resolve("os.lazy.accounts")));
}

#[test]
fn network_outbound_allows_the_socket_interface_and_files_compile_to_nothing() {
    let rules = compile(&manifest("network = [\"outbound\"]\n")).unwrap();
    assert!(rules.contains(&allow(fnv1a64("os.lazy.net.socket.v1"), ANY_METHOD)));
    assert!(rules.contains(&allow(
        resolve_scope::INTERFACE_ID,
        fnv1a32("os.lazy.net.stack")
    )));
    let files = compile(&manifest("files = [\"write:$HOME/x\"]\n")).unwrap();
    assert!(files.is_empty());
}

#[test]
fn every_rule_is_an_allow_and_there_are_no_duplicates() {
    let rules = compile(&manifest(
        "interfaces = [\"os.lazy.display.v1\", \"os.lazy.display.v1\", \"os.lazy.keyd.v1\"]\n\
         topics = [\"subscribe:system/events/+\", \"subscribe:system/events/open\"]\n",
    ))
    .unwrap();
    assert!(rules.iter().all(|rule| rule.allow));
    for (index, rule) in rules.iter().enumerate() {
        assert!(!rules[..index].contains(rule), "duplicate {rule:?}");
    }
}

#[test]
fn too_many_rules_is_refused() {
    let interfaces: Vec<String> = (0..120).map(|index| format!("\"x.y{index}.v1\"")).collect();
    let text = format!("interfaces = [{}]\n", interfaces.join(", "));
    match compile(&manifest(&text)) {
        Err(CompileError::TooManyRules { rules }) => assert_eq!(rules, 360),
        other => panic!("expected TooManyRules, got {other:?}"),
    }
}

#[test]
fn service_names_strip_the_version() {
    assert_eq!(
        service_names("os.lazy.confd.v1"),
        ["os.lazy.confd.v1", "os.lazy.confd"]
    );
    assert_eq!(service_names("weird"), ["weird"]);
    assert_eq!(
        service_names("os.lazy.net.socket.v1"),
        [
            "os.lazy.net.socket.v1",
            "os.lazy.net.socket",
            "os.lazy.net.stack"
        ]
    );
    // The mixer's control interface is reached on the mixer's name.
    assert_eq!(
        service_names("os.lazy.audio.mixer.v1"),
        [
            "os.lazy.audio.mixer.v1",
            "os.lazy.audio.mixer",
            "os.lazy.audio"
        ]
    );
}

#[test]
fn the_clipboard_brings_its_copy_and_paste_scopes() {
    let rules = compile(&manifest("interfaces = [\"os.lazy.clipboard.v1\"]")).unwrap();
    for scope in ["os.lazy.clipboard.write.v1", "os.lazy.clipboard.read.v1"] {
        assert!(
            rules.contains(&allow(fnv1a64(scope), ANY_METHOD)),
            "{scope}"
        );
    }
    let rules = compile(&manifest("interfaces = [\"os.lazy.display.v1\"]")).unwrap();
    assert!(!rules.contains(&allow(fnv1a64("os.lazy.clipboard.write.v1"), ANY_METHOD)));
}
