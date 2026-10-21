//! Developer-tool labels: the service-name verdicts (`tool:<id>`,
//! `kernel/src/ipc/policy.rs`): what a tool may register and resolve, the
//! interface-domain waiver and the friendly refusal text. The label table and
//! the rule engine are in `tool.rs`.

use super::tool::*;
use super::*;
use crate::ipc::acl::{reason, Rule, ANY_INTERFACE, ANY_METHOD};
use crate::ipc::policy::NameOp;
use crate::ipc::topics::{fnv1a32, fnv1a64};

const EACCES: i64 = 13;

/// The service-name matrix: a tool registers what is not reserved, with
/// `ALLOWED_BY_NAMESPACE`; everything reserved stays reserved, whatever the
/// uid and capabilities; an app (the control) still gets only its own.
pub fn register_verdicts() -> Result<(), String> {
    fresh()?;
    let (tool, id) = tool_task(TOOL, 1000)?;
    let app = labelled_task(APP, 1000, 0)?;
    let legacy = labelled_task("", 1000, 0)?;

    let allowed = [
        "demo.pirate.echo",
        "echo",
        "example.service",
        "pirate/chat",
        "app.demo.pirate.svc",
        "oslazy.x",
        "os.lazyx",
    ];
    for name in allowed {
        check!(
            name_reason(tool, NameOp::Register, name)? == Ok(reason::ALLOWED_BY_NAMESPACE),
            "{name} was not allowed by namespace for a tool"
        );
    }
    let reserved = [
        "os.lazy.confd",
        "os.lazy.echo",
        "os.lazy.net.nic",
        "os.lazy.net.nic/eth0",
        "os.lazy.net.wifi",
        "os.lazy.net.wifi.hw/wlan0",
        "app.com.other.svc",
        "app.demo.pirate.a.b",
        "app.demo.pirate",
        "app.demo.pirates.svc",
        "app.demo.svc",
    ];
    for name in reserved {
        check!(
            name_reason(tool, NameOp::Register, name)? == Err(reason::RESERVED_NAMESPACE),
            "{name} was not reserved against a tool"
        );
        last_denial(id, reason::RESERVED_NAMESPACE)?;
    }
    // The label wins over the uid and the capabilities.
    credentials::set(tool, Cred::new(0, 0, credentials::CAP_ALL, id, 0));
    for name in ["os.lazy.timed", "os.lazy.net.nic/eth1", "app.com.other.svc"] {
        check!(
            name_reason(tool, NameOp::Register, name)? == Err(reason::RESERVED_NAMESPACE),
            "a root tool reached {name}"
        );
    }
    check!(
        name_reason(tool, NameOp::Register, "root.free")? == Ok(reason::ALLOWED_BY_NAMESPACE),
        "a root tool lost the free names"
    );
    credentials::set(tool, Cred::new(1000, 1000, 0, id, 0));

    // Controls: an app gets its own namespace and nothing else, an unlabelled
    // task keeps the bootstrap window for free names.
    check!(
        name_reason(app, NameOp::Register, "example.service")? == Err(reason::OUTSIDE_NAMESPACE),
        "an app registered a free name"
    );
    check!(
        name_reason(legacy, NameOp::Register, "example.service")? == Ok(reason::BOOTSTRAP_ALLOW),
        "an unlabelled task lost the bootstrap window"
    );
    // The tool's own `app.<id>.*` is the app of the same id's too: ownership
    // is by id, so `tool:demo.pirate` and `app:demo.pirate` name the same
    // namespace (pkgd must not install both).
    check!(
        name_reason(app, NameOp::Register, "app.demo.pirate.svc")?
            == Ok(reason::ALLOWED_BY_NAMESPACE),
        "the app of the same id lost its namespace"
    );
    Ok(())
}

/// End to end through `OP_REGISTER`/`OP_RESOLVE`: a tool registers a free
/// name, a peer tool resolves it by rule, `os.lazy.*` is refused with
/// `EACCES` and leaves the registry untouched.
pub fn register_through_the_syscalls() -> Result<(), String> {
    fresh()?;
    let (tool, _) = tool_task(TOOL, 1000)?;
    let (peer, _) = tool_task(OTHER_TOOL, 1000)?;
    in_space(|| -> Result<(), String> {
        let run = |slot: usize, name: &str, register: bool| -> Result<u64, String> {
            task::harness::switch_current(slot);
            let code = if register {
                let (code, pair) = register_current(name)?;
                close_pair(pair);
                code
            } else {
                resolve_current(name)?
            };
            task::harness::switch_current(task::KERNEL_TASK);
            Ok(code)
        };
        check!(
            run(tool, "demo.pirate.echo", true)? == 0,
            "a tool could not register a free name"
        );
        check!(registry::list().len() == 1, "not registered");
        check!(
            run(peer, "demo.pirate.echo", false)? == 0,
            "a peer tool could not resolve a free name"
        );
        check!(
            run(tool, "os.lazy.echo", true)? == failed(EACCES),
            "a tool registered in os.lazy.*"
        );
        check!(registry::list().len() == 1, "a refusal touched the table");
        task::harness::switch_current(tool);
        let gone = unregister_current("demo.pirate.echo")?;
        task::harness::switch_current(task::KERNEL_TASK);
        check!(gone == 0, "unregister");
        check!(registry::list().is_empty(), "name left behind");
        for slot in [tool, peer] {
            task::harness::switch_current(slot);
            for (handle, _) in handles::entries_for_task(slot) {
                let _ = channels::close_endpoint(handle);
            }
            task::harness::switch_current(task::KERNEL_TASK);
        }
        Ok(())
    })
}

/// Resolving is judged by the tool's rules like an app's: the wildcard covers
/// it, a name-level deny ahead of it does not, and a revoked tool resolves
/// only its own namespace.
pub fn resolve_verdicts() -> Result<(), String> {
    fresh()?;
    let (tool, id) = tool_task(TOOL, 1000)?;
    for name in ["os.lazy.confd", "demo.other.echo", "app.com.other.svc"] {
        check!(
            name_reason(tool, NameOp::Resolve, name)? == Ok(reason::ALLOWED_BY_RULE),
            "{name} was not resolvable through the wildcard"
        );
    }
    let secret = Rule {
        actor: 0,
        interface_id: policy::RESOLVE_SCOPE,
        method: fnv1a32("os.lazy.keyd"),
        allow: false,
    };
    acl::load_label(id, &[secret, allow_rule(ANY_INTERFACE, ANY_METHOD)])
        .map_err(|e| format!("{e:?}"))?;
    check!(
        name_reason(tool, NameOp::Resolve, "os.lazy.keyd")? == Err(reason::EXPLICIT_DENY),
        "a name-level deny lost to the wildcard"
    );
    check!(
        name_reason(tool, NameOp::Resolve, "os.lazy.keyd2")? == Ok(reason::ALLOWED_BY_RULE),
        "the name deny covered another name"
    );
    acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
    check!(
        name_reason(tool, NameOp::Resolve, "os.lazy.confd")? == Err(reason::LABEL_DEFAULT_DENY),
        "a revoked tool resolved a foreign name"
    );
    check!(
        name_reason(tool, NameOp::Resolve, "app.demo.pirate.svc")?
            == Ok(reason::ALLOWED_BY_NAMESPACE),
        "a revoked tool lost its own namespace"
    );
    Ok(())
}

/// The interface-domain rule (issue #495) does not apply to a tool, but a
/// registration that lies about its ids is still refused; for an app the rule
/// is unchanged.
pub fn interface_domain_waived() -> Result<(), String> {
    fresh()?;
    let (tool, tool_id) = tool_task(TOOL, 1000)?;
    let app = labelled_task(APP, 1000, 0)?;
    let app_id = credentials::of(app).label_id;
    let echo = "os.lazy.echo.v1";
    let echo_id = fnv1a64(echo);
    let own = "demo.pirate.chat.v1";
    let names = |list: &[&str]| -> Vec<String> { list.iter().map(|n| String::from(*n)).collect() };

    check!(
        policy::check_interfaces(tool, &[echo_id], &names(&[echo])).is_ok(),
        "a tool could not serve os.lazy.echo.v1"
    );
    check!(
        policy::check_interfaces(tool, &[echo_id, fnv1a64(own)], &names(&[echo, own])).is_ok(),
        "a tool could not serve two foreign-domain interfaces"
    );
    check!(
        policy::check_interfaces(tool, &[echo_id], &[]).is_ok(),
        "a tool had to spell its interfaces"
    );
    // A lying pair, or the wrong number of names, is refused for a tool too.
    let lie = policy::check_interfaces(tool, &[echo_id], &names(&[own]));
    check!(
        lie.map_err(|d| d.reason_code) == Err(reason::UNNAMED_INTERFACE),
        "a lying interface name passed for a tool"
    );
    last_denial(tool_id, reason::UNNAMED_INTERFACE)?;
    let count = policy::check_interfaces(tool, &[echo_id, fnv1a64(own)], &names(&[echo]));
    check!(
        count.map_err(|d| d.reason_code) == Err(reason::UNNAMED_INTERFACE),
        "a short name list passed for a tool"
    );

    // The control: the same registration by an app is refused as before.
    let foreign = policy::check_interfaces(app, &[echo_id], &names(&[echo]));
    check!(
        foreign.map_err(|d| d.reason_code) == Err(reason::FOREIGN_INTERFACE),
        "an app served a foreign interface"
    );
    last_denial(app_id, reason::FOREIGN_INTERFACE)?;
    let unnamed = policy::check_interfaces(app, &[echo_id], &[]);
    check!(
        unnamed.map_err(|d| d.reason_code) == Err(reason::UNNAMED_INTERFACE),
        "an app served unnamed interfaces"
    );
    check!(
        policy::check_interfaces(app, &[fnv1a64("demo.pirate.chat.v1")], &names(&[own])).is_ok(),
        "an app lost its own domain"
    );
    Ok(())
}
/// The friendly refusal names what a tool may use.
pub fn explain_names_the_tool_namespace() -> Result<(), String> {
    fresh()?;
    let (_, id) = tool_task(TOOL, 1000)?;
    let text = policy::explain(id, reason::RESERVED_NAMESPACE);
    check!(
        text.contains("developer tool")
            && text.contains("app.demo.pirate.<name>")
            && text.contains("app/demo.pirate/"),
        "the text does not name the tool namespace: {text}"
    );
    let app = labels::intern(OTHER_APP).map_err(|_| "intern")?;
    check!(
        !policy::explain(app, reason::RESERVED_NAMESPACE).contains("developer tool"),
        "an app was explained as a tool"
    );
    Ok(())
}
