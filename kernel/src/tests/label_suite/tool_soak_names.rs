//! Soak: thousands of registrations, resolves and topic authorizations from
//! `tool:` credentials interleaved with `app:` credentials, tools revoked and
//! reloaded on the way and tool tasks dying mid-registration
//! (`tool_soak.rs` has the label and rule-store churn).

use super::tool::{default_denied, tool_rules};
use super::tool_soak::{as_task, drain};
use super::*;
use crate::ipc::topics;

const EACCES: i64 = 13;

const PAIRS: usize = 4;
const NAME_CYCLES: usize = 2500;

/// One cycle for pair `k`: tool task `k` and app task `k`, registrations,
/// resolves and topic authorizations that must be allowed or refused, with a
/// peer tool and peer app interleaved.
fn name_cycle(
    index: usize,
    tools: &[usize],
    apps: &[usize],
    denials: &mut u64,
) -> Result<(), String> {
    let k = index % PAIRS;
    let (tool, app) = (tools[k], apps[k]);
    let (peer_tool, peer_app) = (tools[(k + 1) % PAIRS], apps[(k + 1) % PAIRS]);
    let name = format!("soak.tool{k}.svc");
    let in_task = |slot: usize, f: &dyn Fn() -> Result<u64, String>| as_task(slot, || f());

    let code = in_task(tool, &|| {
        let (code, pair) = register_current(&name)?;
        close_pair(pair);
        Ok(code)
    })?;
    check!(code == 0, "cycle {index}: tool register -> {code:#x}");
    check!(registry::list().len() == 1, "cycle {index}: registry size");

    check!(
        in_task(peer_tool, &|| resolve_current(&name))? == 0,
        "cycle {index}: a peer tool could not resolve"
    );
    for slot in [peer_app, app] {
        let code = in_task(slot, &|| resolve_current(&name))?;
        check!(
            code == failed(EACCES),
            "cycle {index}: an app resolved a tool's name -> {code:#x}"
        );
        *denials += 1;
    }
    // An app cannot take a free name, a tool cannot take reserved ones.
    let code = in_task(app, &|| {
        let (code, pair) = register_current(&format!("soak.app{k}.free"))?;
        close_pair(pair);
        Ok(code)
    })?;
    check!(
        code == failed(EACCES),
        "cycle {index}: an app took a free name"
    );
    *denials += 1;
    for reserved in [
        format!("os.lazy.soak{k}"),
        format!("app.soak.app{k}.svc"),
        "os.lazy.net.nic/eth9".into(),
    ] {
        let code = in_task(tool, &|| {
            let (code, pair) = register_current(&reserved)?;
            close_pair(pair);
            Ok(code)
        })?;
        check!(
            code == failed(EACCES),
            "cycle {index}: a tool took {reserved} -> {code:#x}"
        );
        *denials += 1;
    }
    // The app's own name, interleaved, is fine for the app and not the tool.
    let own = format!("app.soak.app{k}.svc");
    let code = in_task(app, &|| {
        let (code, pair) = register_current(&own)?;
        close_pair(pair);
        Ok(code)
    })?;
    check!(
        code == 0,
        "cycle {index}: an app lost its namespace -> {code:#x}"
    );
    check!(
        in_task(app, &|| unregister_current(&own))? == 0,
        "cycle {index}: app unregister"
    );

    // Topics.
    let (publish, subscribe) = (topics::MODE_PUBLISH, topics::MODE_SUBSCRIBE);
    let allowed =
        |slot, mode, topic: &str| topics::authorize(slot, mode, topic, index as u64).is_ok();
    check!(
        allowed(tool, publish, &format!("soak/tool{k}/x"))
            && allowed(tool, subscribe, "system/events/#")
            && allowed(tool, publish, &format!("app/soak.tool{k}/own")),
        "cycle {index}: a tool lost an open topic"
    );
    check!(
        allowed(app, publish, &format!("app/soak.app{k}/own"))
            && !allowed(app, publish, &format!("soak/tool{k}/x")),
        "cycle {index}: an app's topics"
    );
    *denials += 1;
    for (slot, mode, topic) in [
        (tool, publish, format!("app/soak.app{k}/x")),
        (tool, subscribe, String::from("app/#")),
        (tool, publish, String::from("system/events/soak")),
    ] {
        check!(
            !allowed(slot, mode, &topic),
            "cycle {index}: a tool reached {topic}"
        );
        *denials += 1;
    }

    // A tool revoked mid-soak loses everything but its own namespaces.
    if index % 50 == 49 {
        let id = credentials::of(tool).label_id;
        let peer_id = credentials::of(peer_tool).label_id;
        acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
        // Resolving is judged by the resolver's rules, not the owner's.
        check!(
            in_task(peer_tool, &|| resolve_current(&name))? == 0,
            "cycle {index}: peer resolve after the owner's revoke"
        );
        acl::load_label(peer_id, &[]).map_err(|e| format!("{e:?}"))?;
        let code = in_task(peer_tool, &|| resolve_current(&name))?;
        check!(
            code == failed(EACCES),
            "cycle {index}: a revoked peer resolved"
        );
        *denials += 1;
        check!(
            allowed(tool, publish, &format!("app/soak.tool{k}/own")),
            "cycle {index}: revoking removed the namespace"
        );
        for slot in [tool, peer_tool] {
            let id = credentials::of(slot).label_id;
            acl::load_label(id, &tool_rules(&default_denied())).map_err(|e| format!("{e:?}"))?;
        }
    }

    check!(
        in_task(tool, &|| unregister_current(&name))? == 0,
        "cycle {index}: tool unregister"
    );
    check!(
        registry::list().is_empty(),
        "cycle {index}: name left behind"
    );
    for slot in [tool, app, peer_tool, peer_app] {
        drain(slot)?;
    }
    Ok(())
}

/// A tool task registers a name and exits without unregistering.
fn tool_exits_mid_registration(index: usize) -> Result<(), String> {
    let k = index % PAIRS;
    let child = labelled_task(&format!("tool:soak.t{k}"), 3000, 0)?;
    let code = as_task(child, || {
        let (code, _pair) = register_current(&format!("soak.exit{k}"))?;
        Ok(code)
    })?;
    check!(code == 0, "exit cycle {index}: register -> {code:#x}");
    task::harness::finish(child, 0);
    check!(
        registry::list().is_empty(),
        "exit cycle {index}: a dead tool's name survived"
    );
    check!(
        task::reap_child().is_some(),
        "exit cycle {index}: not reapable"
    );
    check!(
        handles::count_for_task(child) == 0,
        "exit cycle {index}: the dead task kept handles"
    );
    Ok(())
}

/// Thousands of registrations, resolves and topic authorizations from tool
/// credentials interleaved with app credentials, tools revoked and reloaded on
/// the way and tool tasks dying mid-registration.
pub fn names_and_topics() -> Result<(), String> {
    fresh()?;
    let mut tools = Vec::new();
    let mut apps = Vec::new();
    for index in 0..PAIRS {
        let tool = labelled_task(&format!("tool:soak.tool{index}"), 1000 + index as u32, 0)?;
        acl::load_label(
            credentials::of(tool).label_id,
            &tool_rules(&default_denied()),
        )
        .map_err(|e| format!("{e:?}"))?;
        tools.push(tool);
        apps.push(labelled_task(
            &format!("app:soak.app{index}"),
            2000 + index as u32,
            0,
        )?);
    }
    let denials_before = audit::denials();
    let mut denials = 0u64;
    let labels_before = labels::count();
    in_space(|| -> Result<(), String> {
        for index in 0..NAME_CYCLES {
            name_cycle(index, &tools, &apps, &mut denials)?;
            if index % 125 == 124 {
                // Dying tools carry the same labels as the live ones.
                for k in 0..PAIRS {
                    let id = labels::intern(&format!("tool:soak.t{k}")).map_err(|_| "intern")?;
                    acl::load_label(id, &tool_rules(&default_denied()))
                        .map_err(|e| format!("{e:?}"))?;
                }
                tool_exits_mid_registration(index)?;
            }
        }
        Ok(())
    })?;
    check!(
        registry::list().is_empty(),
        "names leaked: {}",
        registry::list().len()
    );
    let stats = registry::stats();
    check!(
        stats.entries == 0 && stats.leases == 0,
        "registry stats {stats:?}"
    );
    let audited = audit::denials() - denials_before;
    check!(
        audited >= denials,
        "only {audited} denials audited, expected at least {denials}"
    );
    // The four `tool:soak.t<k>` labels of the exit cycles are the only growth.
    check!(
        labels::count() == labels_before + PAIRS,
        "the label table is at {} (expected {})",
        labels::count(),
        labels_before + PAIRS
    );
    check!(audit::count() > 0, "audit ring is empty");
    Ok(())
}
