//! Developer-tool labels: what a revoke (or a rule list never loaded) takes
//! away (`kernel/src/ipc/tool.rs`, `holds_rules`). A `tool:` label holds two
//! things an `app:` label of the same id does not, the free service names and
//! the waiver of the interface-domain rule, and both exist only while rules are
//! loaded for it, so `pkgd`'s revoke ends them for a process that keeps
//! running. What an `app:` label has implicitly (its own `app.<id>.*` names
//! with the domain rule, its own `app/<id>/` topics) stays, for parity. The
//! label table and rule engine cases are in `tool.rs`, the soak at the end of
//! this file.

use super::tool::*;
use super::tool_soak::{as_task, drain};
use super::*;
use crate::ipc::acl::reason;
use crate::ipc::policy::NameOp;
use crate::ipc::tool::holds_rules;
use crate::ipc::topics::{self, fnv1a64};

const EACCES: i64 = 13;

const ECHO: &str = "os.lazy.echo.v1";
const OWN_NAME: &str = "app.demo.pirate.svc";

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| String::from(*n)).collect()
}

/// The reason a registration of `iface` (spelled `name`) is refused, if it is.
fn interface_verdict(slot: usize, name: &str) -> Option<u32> {
    policy::check_interfaces(slot, &[fnv1a64(name)], &names(&[name]))
        .err()
        .map(|d| d.reason_code)
}

/// The tool's service-name and interface verdicts in one of its three states:
/// `loaded` (`true`: the shipped rules) or without rules.
fn expect_state(slot: usize, id: &str, loaded: bool, when: &str) -> Result<(), String> {
    let (own_name, own_domain) = (format!("app.{id}.svc"), format!("{id}.chat.v1"));
    let free = name_reason(slot, NameOp::Register, "demo.pirate.free")?;
    if loaded {
        check!(
            free == Ok(reason::ALLOWED_BY_NAMESPACE),
            "{when}: a tool with rules lost the free names ({free:?})"
        );
    } else {
        check!(
            free == Err(reason::OUTSIDE_NAMESPACE),
            "{when}: a tool without rules got a free name ({free:?})"
        );
    }
    // Parity with an `app:` label: the own namespace needs no rules.
    check!(
        name_reason(slot, NameOp::Register, &own_name)? == Ok(reason::ALLOWED_BY_NAMESPACE),
        "{when}: the tool lost its own app.<id>.* namespace"
    );
    for reserved in ["os.lazy.confd", "app.com.other.svc", "os.lazy.net.nic/eth0"] {
        check!(
            name_reason(slot, NameOp::Register, reserved)? == Err(reason::RESERVED_NAMESPACE),
            "{when}: {reserved} was not reserved against the tool"
        );
    }
    check!(
        interface_verdict(slot, &own_domain).is_none(),
        "{when}: the tool lost its own interface domain"
    );
    let foreign = interface_verdict(slot, ECHO);
    if loaded {
        check!(foreign.is_none(), "{when}: the domain waiver is gone");
    } else {
        check!(
            foreign == Some(reason::FOREIGN_INTERFACE),
            "{when}: a tool without rules served a foreign interface ({foreign:?})"
        );
    }
    let unnamed = policy::check_interfaces(slot, &[fnv1a64(ECHO)], &[]);
    check!(
        unnamed.is_ok() == loaded,
        "{when}: unnamed interfaces: allowed={}, loaded={loaded}",
        unnamed.is_ok()
    );
    Ok(())
}

fn expect_topics(slot: usize, loaded: bool, when: &str) -> Result<(), String> {
    let (publish, subscribe) = (topics::MODE_PUBLISH, topics::MODE_SUBSCRIBE);
    let allowed = |mode, topic: &str| topics::authorize(slot, mode, topic, 7).is_ok();
    check!(
        allowed(publish, "app/demo.pirate/x") && allowed(subscribe, "app/demo.pirate/#"),
        "{when}: the tool lost its own topics"
    );
    check!(
        allowed(publish, "demo/pirate/free") == loaded && allowed(subscribe, "demo/#") == loaded,
        "{when}: open topics follow the rules (loaded={loaded})"
    );
    check!(
        !allowed(publish, "app/com.other/x") && !allowed(subscribe, "app/#"),
        "{when}: another app's tree was reachable"
    );
    Ok(())
}

/// A tool's name, interface and topic grants over its life: rules never
/// loaded, loaded, revoked, reloaded; a peer's revoke touches nobody else; the
/// refusals are audited; the syscalls agree with the verdicts.
pub fn revoked_tool_registrations() -> Result<(), String> {
    fresh()?;
    let tool = labelled_task(TOOL, 1000, 0)?;
    let id = credentials::of(tool).label_id;
    let (peer, peer_id) = tool_task(OTHER_TOOL, 1000)?;
    let app = labelled_task(APP, 1000, 0)?;

    check!(!holds_rules(id), "rules before any load");
    expect_state(tool, "demo.pirate", false, "never loaded")?;
    expect_topics(tool, false, "never loaded")?;
    // The audited refusal names the tool, and says what an app may use.
    name_reason(tool, NameOp::Register, "demo.pirate.free")?
        .err()
        .ok_or("free name")?;
    last_denial(id, reason::OUTSIDE_NAMESPACE)?;
    check!(
        !policy::explain(id, reason::OUTSIDE_NAMESPACE).contains("developer tool"),
        "a tool without rules was explained as a tool"
    );

    acl::load_label(id, &tool_rules(&default_denied())).map_err(|e| format!("{e:?}"))?;
    check!(holds_rules(id), "no rules after the load");
    expect_state(tool, "demo.pirate", true, "loaded")?;
    expect_topics(tool, true, "loaded")?;
    check!(
        policy::explain(id, reason::RESERVED_NAMESPACE).contains("developer tool"),
        "a tool with rules lost its explanation"
    );

    acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
    check!(!holds_rules(id), "rules after the revoke");
    expect_state(tool, "demo.pirate", false, "revoked")?;
    expect_topics(tool, false, "revoked")?;
    // The peer, never revoked, kept everything; the app control is unchanged.
    expect_state(peer, "demo.other", true, "peer of a revoked tool")?;
    check!(holds_rules(peer_id), "the revoke reached the peer");
    check!(
        name_reason(app, NameOp::Register, "demo.pirate.free")? == Err(reason::OUTSIDE_NAMESPACE)
            && interface_verdict(app, ECHO) == Some(reason::FOREIGN_INTERFACE),
        "the app control changed"
    );

    acl::load_label(id, &tool_rules(&default_denied())).map_err(|e| format!("{e:?}"))?;
    expect_state(tool, "demo.pirate", true, "reloaded")?;
    expect_topics(tool, true, "reloaded")?;
    // Only a tool kind counts: rules of an app are not a tool's grant.
    check!(
        !holds_rules(0) && !holds_rules(credentials::of(app).label_id),
        "an unlabelled task or an app held tool rules"
    );
    Ok(())
}

/// `Register` through `OP_REGISTER` as `slot`: the syscall's return value.
fn register_as(slot: usize, name: &str) -> Result<u64, String> {
    as_task(slot, || {
        let (code, pair) = register_current(name)?;
        close_pair(pair);
        Ok(code)
    })
}

/// End to end: the syscall follows the verdicts, a name registered before the
/// revoke stays until its owner removes it, and the foreign-interface waiver
/// is checked on the wire too.
pub fn revoked_tool_syscalls() -> Result<(), String> {
    fresh()?;
    let tool = labelled_task(TOOL, 1000, 0)?;
    let id = credentials::of(tool).label_id;
    in_space(|| -> Result<(), String> {
        check!(
            register_as(tool, "demo.pirate.free")? == failed(EACCES),
            "a tool whose rules were never loaded registered a free name"
        );
        check!(registry::list().is_empty(), "a refusal touched the table");
        acl::load_label(id, &tool_rules(&default_denied())).map_err(|e| format!("{e:?}"))?;
        check!(register_as(tool, "demo.pirate.free")? == 0, "loaded: free name");
        let foreign = as_task(tool, || {
            let (code, pair) = register_current_with("demo.pirate.echo", &[fnv1a64(ECHO)], &[ECHO])?;
            close_pair(pair);
            Ok(code)
        })?;
        check!(foreign == 0, "loaded: foreign interface -> {foreign:#x}");
        acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
        check!(registry::list().len() == 2, "the revoke dropped a name");
        check!(
            register_as(tool, "demo.pirate.again")? == failed(EACCES),
            "a revoked tool registered a free name"
        );
        let foreign = as_task(tool, || {
            let (code, pair) = register_current_with(OWN_NAME, &[fnv1a64(ECHO)], &[ECHO])?;
            close_pair(pair);
            Ok(code)
        })?;
        check!(
            foreign == failed(EACCES),
            "a revoked tool served a foreign interface -> {foreign:#x}"
        );
        check!(register_as(tool, OWN_NAME)? == 0, "revoked: own namespace");
        for name in ["demo.pirate.free", "demo.pirate.echo", OWN_NAME] {
            let gone = as_task(tool, || unregister_current(name))?;
            check!(gone == 0, "unregister {name} after the revoke -> {gone:#x}");
        }
        check!(registry::list().is_empty(), "names left behind");
        drain(tool)
    })
}

const TOOLS: usize = 4;
const CYCLES: usize = 3000;

/// Registrations from tool credentials while their rules are loaded, revoked
/// and reloaded in a pseudo-random order (every state change goes through the
/// real store), with labels whose rules are never loaded and apps interleaved:
/// every verdict follows the state at that instant and nothing leaks.
pub fn revoke_registration_churn() -> Result<(), String> {
    fresh()?;
    let mut tools = Vec::new();
    let mut idle = Vec::new();
    for k in 0..TOOLS {
        tools.push(labelled_task(&format!("tool:rv.t{k}"), 1000 + k as u32, 0)?);
        idle.push(labelled_task(&format!("tool:rv.n{k}"), 1100 + k as u32, 0)?);
    }
    let app = labelled_task("app:rv.app", 2000, 0)?;
    let mut loaded = [false; TOOLS];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut denials = 0u64;
    let before = audit::denials();
    in_space(|| -> Result<(), String> {
        for cycle in 0..CYCLES {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let k = (seed % TOOLS as u64) as usize;
            let id = credentials::of(tools[k]).label_id;
            if (seed >> 8) % 3 == 0 {
                loaded[k] = !loaded[k];
                let rules = if loaded[k] {
                    tool_rules(&default_denied())
                } else {
                    Vec::new()
                };
                acl::load_label(id, &rules).map_err(|e| format!("{e:?}"))?;
            }
            for (j, &slot) in tools.iter().enumerate() {
                let name = format!("rv.free{j}");
                let code = register_as(slot, &name)?;
                if loaded[j] {
                    check!(code == 0, "cycle {cycle}: loaded tool {j} -> {code:#x}");
                    check!(as_task(slot, || unregister_current(&name))? == 0, "unregister");
                } else {
                    check!(
                        code == failed(EACCES),
                        "cycle {cycle}: revoked tool {j} registered a free name -> {code:#x}"
                    );
                    denials += 1;
                }
                let own = format!("app.rv.t{j}.svc");
                check!(register_as(slot, &own)? == 0, "cycle {cycle}: own name {j}");
                check!(as_task(slot, || unregister_current(&own))? == 0, "unregister own");
            }
            // The same credentials in the other two roles never get a free name.
            for slot in [idle[k], app] {
                let code = register_as(slot, "rv.free.other")?;
                check!(code == failed(EACCES), "cycle {cycle}: {slot} -> {code:#x}");
                denials += 1;
            }
            check!(registry::list().is_empty(), "cycle {cycle}: name left behind");
            if cycle % 100 == 99 {
                for slot in tools.iter().chain(&idle).chain([&app]) {
                    drain(*slot)?;
                }
            }
        }
        Ok(())
    })?;
    check!(
        audit::denials() - before >= denials,
        "only {} denials audited, expected {denials}",
        audit::denials() - before
    );
    let stats = registry::stats();
    check!(stats.entries == 0 && stats.leases == 0, "registry stats {stats:?}");
    Ok(())
}
