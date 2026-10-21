use std::collections::HashSet;

use rhai_lazy::msg::api;
use rhai_lazy::msg::schema::{self, snake_case};

use crate::catalog::{Catalog, NodeId};
use crate::docs::{self, Block, ChipKind, Risk};
use crate::rhai_names::{
    alias, enum_const, has_sugar, module, module_usable, scriptable, sys_function, topic_helper,
    variant_const, wildcards,
};

/// `(name, arity)` of every `fn` in a generated module's source.
fn functions(source: &str) -> HashSet<(String, usize)> {
    source
        .lines()
        .filter_map(|line| line.strip_prefix("fn "))
        .filter_map(|rest| {
            let (name, tail) = rest.split_once('(')?;
            let (params, _) = tail.split_once(')')?;
            let arity = params.split(',').filter(|p| !p.trim().is_empty()).count();
            Some((name.to_string(), arity))
        })
        .collect()
}

fn constants(source: &str) -> HashSet<String> {
    source
        .lines()
        .filter_map(|line| line.strip_prefix("export const "))
        .filter_map(|rest| rest.split_once(' ').map(|(n, _)| n.to_string()))
        .collect()
}

#[test]
fn the_alias_rule_is_the_generators() {
    for module in api::modules() {
        assert_eq!(
            alias(module.interface),
            module.alias,
            "{}",
            module.interface
        );
    }
    // Interfaces with no module are the kernel scopes, and only those.
    let without: Vec<_> = schema::interfaces()
        .iter()
        .filter(|i| module(i).is_none())
        .map(|i| i.name)
        .collect();
    assert!(!without.is_empty());
    for name in &without {
        assert!(
            name.contains(".publish.")
                || name.contains(".subscribe.")
                || name.contains("policy")
                || name.contains("names.resolve")
                || name.contains("label.spawn"),
            "{name} has no sys module but is not a kernel scope"
        );
    }
    assert_eq!(alias("os.lazy.net.nic.v1"), "net_nic");
    assert_eq!(alias("os.lazy.confd.v1"), "confd");
    assert_eq!(alias("os.lazy.keyd.appsecret.v1"), "keyd_appsecret");
}

#[test]
fn a_method_is_scriptable_exactly_when_the_module_has_its_function() {
    let mut checked = 0;
    for interface in schema::interfaces() {
        let Some(module) = module(interface) else {
            continue;
        };
        let fns = functions(module.source);
        for method in interface.methods {
            let name = snake_case(method.name);
            let has = fns.contains(&(name.clone(), method.params.len()));
            assert_eq!(
                scriptable(interface, method),
                has,
                "{}.{} ({name}/{})",
                interface.name,
                method.name,
                method.params.len()
            );
            assert_eq!(
                sys_function(interface, method).is_some(),
                has && module_usable(interface),
                "{}.{}",
                interface.name,
                method.name
            );
            checked += 1;
        }
    }
    assert!(checked > 100, "checked {checked} methods");
}

#[test]
fn enum_constants_and_struct_zero_values_exist_in_the_module() {
    for interface in schema::interfaces() {
        let Some(module) = module(interface) else {
            continue;
        };
        let consts = constants(module.source);
        let fns = functions(module.source);
        for def in interface.enums {
            assert!(
                consts.contains(&enum_const(def.name)),
                "{} {}",
                interface.name,
                def.name
            );
            for variant in def.variants {
                assert!(
                    consts.contains(&variant_const(def.name, variant)),
                    "{} {} {variant}",
                    interface.name,
                    def.name
                );
            }
        }
        for def in interface.structs {
            assert!(
                fns.contains(&(format!("new_{}", snake_case(def.name)), 0)),
                "{} new_{}",
                interface.name,
                snake_case(def.name)
            );
        }
    }
}

#[test]
fn topic_helpers_are_found_and_exist() {
    let mut topics = 0;
    for interface in schema::interfaces() {
        let Some(module) = module(interface) else {
            continue;
        };
        let fns = functions(module.source);
        let consts = constants(module.source);
        for topic in interface.topics {
            let helper = topic_helper(interface, topic)
                .unwrap_or_else(|| panic!("{} {}", interface.name, topic.pattern));
            let stem = helper.helper;
            assert!(fns.contains(&(format!("on_{stem}"), 1)), "on_{stem}");
            assert!(
                fns.contains(&(format!("subscribe_{stem}"), 0)),
                "subscribe_{stem}"
            );
            let n = wildcards(topic.pattern);
            assert!(
                fns.contains(&(format!("publish_{stem}"), n + 1)),
                "publish_{stem}/{}",
                n + 1
            );
            assert!(consts.contains(&format!("{}_PATTERN", stem.to_uppercase())));
            assert_eq!(
                fns.contains(&(format!("{stem}_topic"), n)),
                n > 0,
                "{stem}_topic"
            );
            topics += 1;
        }
    }
    assert!(topics > 10);
}

#[test]
fn sugar_exists_unless_the_name_is_reserved() {
    let echo = schema::interface("os.lazy.echo.v1").unwrap();
    assert!(has_sugar(echo.method("Echo").unwrap()));
    for interface in schema::interfaces() {
        for method in interface.methods {
            let reserved = [
                "invoke",
                "invoke_oneway",
                "call",
                "interface",
                "service",
                "methods",
                "type_of",
            ]
            .contains(&snake_case(method.name).as_str());
            assert_eq!(has_sugar(method), !reserved);
        }
    }
}

#[test]
fn every_node_has_a_doc_page_with_a_title_and_the_reference_link() {
    let catalog = Catalog::from_schema();
    for node in catalog.nodes() {
        let blocks = docs::blocks(&catalog, node);
        assert!(
            matches!(blocks.first(), Some(Block::Title(_))),
            "{:?}",
            node.id
        );
        let link = blocks.iter().find_map(|b| match b {
            Block::Links(l) => Some(l),
            _ => None,
        });
        let link = link.unwrap_or_else(|| panic!("no links on {:?}", node.id));
        assert_eq!(
            link[0].path,
            format!("/docs/os/idl/{}.md", node.item.interface().name)
        );
        assert_eq!(link[1].path, "/docs/os/messenger.md");
    }
}

#[test]
fn the_reference_pages_linked_to_exist_in_the_repository() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/idl");
    if !dir.exists() {
        return;
    }
    for module in api::modules() {
        let path = dir.join(format!("{}.md", module.interface));
        assert!(path.exists(), "{} is linked but missing", path.display());
    }
}

#[test]
fn a_method_page_shows_signature_spellings_and_flags() {
    let catalog = Catalog::from_schema();
    let node = catalog
        .get(&NodeId::method("os.lazy.confd.v1", "Get"))
        .unwrap();
    let blocks = docs::blocks(&catalog, node);
    assert!(blocks
        .iter()
        .any(|b| matches!(b, Block::Signature(s) if s.starts_with("Get(path: String)"))));
    let codes: Vec<_> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Code { label, text } => Some((label.as_str(), text.as_str())),
            _ => None,
        })
        .collect();
    assert!(codes.contains(&("sys module", "sys::confd::get(path)")));
    assert!(codes.contains(&("connect", "msg::connect(\"os.lazy.confd.v1\").get(path)")));
    assert!(codes
        .iter()
        .any(|(l, t)| *l == "invoke" && t.contains("invoke(\"Get\", [path])")));

    let ready = catalog
        .get(&NodeId::method("os.lazy.messenger.topics.bell.v1", "Ready"))
        .unwrap();
    let blocks = docs::blocks(&catalog, ready);
    assert!(blocks
        .iter()
        .any(|b| matches!(b, Block::Chips(c) if c.iter().any(|c| c.text.starts_with("one-way")))));
    let bell = schema::interface("os.lazy.messenger.topics.bell.v1").unwrap();
    let spellings = docs::spellings(bell, bell.method("Ready").unwrap());
    assert!(spellings
        .iter()
        .any(|s| s.code.contains("invoke_oneway(\"Ready\"")));
}

#[test]
fn methods_with_objects_and_gate_only_methods_say_a_script_cannot_call_them() {
    let catalog = Catalog::from_schema();
    let mut with_objects = 0;
    for interface in schema::interfaces() {
        for method in interface.methods.iter().filter(|m| !m.objects.is_empty()) {
            let info = docs::method_info(interface, method);
            assert!(!info.scriptable && !info.objects.is_empty());
            assert!(info.spellings.is_empty());
            let node = catalog
                .get(&NodeId::method(interface.name, method.name))
                .unwrap();
            let blocks = docs::blocks(&catalog, node);
            assert!(
                blocks.iter().any(|b| matches!(b, Block::Chips(c)
                    if c.iter().any(|c| c.kind == ChipKind::Warning && c.text.contains("cannot send")))),
                "{}.{}",
                interface.name,
                method.name
            );
            with_objects += 1;
        }
    }
    assert!(with_objects > 3);
    let registry = schema::interface("os.lazy.messenger.registry.v1").unwrap();
    let connect = registry.method("Connect").unwrap();
    let info = docs::method_info(registry, connect);
    assert!(!info.scriptable && info.objects.is_empty());
    let node = catalog
        .get(&NodeId::method(registry.name, "Connect"))
        .unwrap();
    let blocks = docs::blocks(&catalog, node);
    assert!(blocks
        .iter()
        .any(|b| matches!(b, Block::Chips(c) if c.iter().any(|c| c.text.contains("kernel gate")))));
}

#[test]
fn risks_flag_the_calls_the_plan_names() {
    let init = schema::interface("os.lazy.init.v1").unwrap();
    assert!(docs::risks(init, init.method("Shutdown").unwrap()).contains(&Risk::StopsServices));
    assert!(
        docs::risks(init, init.method("RestartService").unwrap()).contains(&Risk::StopsServices)
    );
    assert!(!docs::risks(init, init.method("Services").unwrap()).contains(&Risk::StopsServices));
    let confd = schema::interface("os.lazy.confd.v1").unwrap();
    assert!(docs::risks(confd, confd.method("Get").unwrap()).is_empty());
    // Prose that merely mentions a reboot or a shutdown flags nothing (found in the guest:
    // confd.Info "survives a reboot" asked for a confirmation).
    assert!(docs::risks(confd, confd.method("Info").unwrap()).is_empty());
    let net =
        schema::interface("os.lazy.net.socket.v1").or_else(|| schema::interface("os.lazy.net.v1"));
    if let Some(net) = net.filter(|n| n.method("Shutdown").is_some()) {
        assert!(!docs::risks(net, net.method("Shutdown").unwrap()).contains(&Risk::StopsServices));
    }
    let logind = schema::interface("os.lazy.logind.v1").unwrap();
    assert!(docs::risks(logind, logind.method("Logout").unwrap()).contains(&Risk::StopsServices));
    for name in [
        "os.lazy.keyd.v1",
        "os.lazy.keyd.appsecret.v1",
        "os.lazy.accounts.v1",
        "os.lazy.elevd.v1",
    ] {
        let i = schema::interface(name).unwrap();
        assert!(docs::is_secret_interface(i), "{name}");
        assert!(docs::risks(i, &i.methods[0]).contains(&Risk::Secret));
    }
    let clip = schema::interface("os.lazy.clipboard.v1").unwrap();
    assert!(docs::risks(clip, clip.method("Request").unwrap()).contains(&Risk::ReadsClipboard));
    assert!(!docs::risks(clip, clip.method("Ping").unwrap()).contains(&Risk::ReadsClipboard));
    let timed = schema::interface("os.lazy.timed.v1").unwrap();
    assert!(docs::risks(timed, timed.method("SetTime").unwrap()).contains(&Risk::Admin));
}

#[test]
fn interface_topic_struct_and_enum_pages_carry_the_schema_text() {
    let catalog = Catalog::from_schema();
    let text = |id: NodeId| -> String {
        let node = catalog.get(&id).unwrap();
        format!("{:?}", docs::blocks(&catalog, node))
    };
    let timed = text(NodeId::interface("os.lazy.timed.v1"));
    assert!(timed.contains("Rhai module: sys::timed"));
    assert!(timed.contains("time-of-day service"));
    let topic = text(NodeId::topic("os.lazy.timed.v1", "time/tick"));
    assert!(topic.contains("subscribe:time/tick"));
    assert!(topic.contains("sys::timed::on_tick"));
    assert!(topic.contains("QoS: latest"));
    let health = text(NodeId::topic("os.lazy.healthd.v1", "system/health/+"));
    assert!(health.contains("sys::healthd::health_topic(value)"));
    let strukt = text(NodeId::struct_("os.lazy.timed.v1", "Tick"));
    assert!(strukt.contains("sys::timed::new_tick()"));
    assert!(strukt.contains("topic time/tick"));
    let en = text(NodeId::enum_("os.lazy.timed.v1", "NtpState"));
    assert!(en.contains("sys::timed::NTP_STATE_WAITING_FOR_NETWORK"));
    assert!(en.contains("1: \\\"WaitingForNetwork\\\""));
}

#[test]
fn the_print_module_cannot_be_written_so_it_has_no_sys_spelling() {
    let print = schema::interface("os.lazy.print.v1").unwrap();
    assert!(module(print).is_some());
    assert!(!module_usable(print));
    let open = print.method("Open").unwrap();
    assert!(sys_function(print, open).is_none());
    let spellings = docs::spellings(print, open);
    assert!(spellings.iter().all(|s| !s.code.contains("sys::")));
    assert!(spellings.iter().any(|s| s.label == "connect"));
    // Every other module is usable.
    for module in api::modules() {
        let interface = schema::interface(module.interface).unwrap();
        assert_eq!(
            module_usable(interface),
            module.alias != "print",
            "{}",
            module.alias
        );
    }
}
