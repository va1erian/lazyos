//! Loading child labels: the grammar, `OP_ACL_LOAD` and revocation.

use super::*;

/// The label grammar: the child form, its parent link and every way the
/// string can be wrong.
pub fn child_label_grammar() -> Result<(), String> {
    fresh_children()?;
    let (kind, name) = labels::parse(WEB).map_err(|_| "parse")?;
    check!(
        kind == Kind::Child && name == "org.test.browser/webprocess",
        "parsed as {kind:?} {name:?}"
    );
    check!(labels::parent_label(WEB) == Some(APP), "parent label");
    for plain in [
        APP,
        "dev:org.test.browser",
        "system:netdrv",
        "app:a.b/",
        "x",
    ] {
        check!(
            labels::parent_label(plain).is_none(),
            "{plain:?} has a parent label"
        );
    }

    // The app first: a child of a label nobody interned is refused, and the
    // refusal takes no slot.
    check!(
        labels::intern(WEB) == Err(LabelError::NoParent),
        "a child was interned before its app"
    );
    check!(labels::count() == 0, "a refused child changed the table");
    let app = labels::intern(APP).map_err(|_| "intern the app")?;
    let web = labels::intern(WEB).map_err(|_| "intern the child")?;
    check!(
        labels::kind_of(web) == Some(Kind::Child) && labels::kind_of(app) == Some(Kind::App),
        "kinds"
    );
    check!(labels::parent_of(web) == Some(app), "the parent link");
    check!(
        labels::parent_of(app).is_none() && labels::parent_of(0).is_none(),
        "an app or the unlabelled has a parent"
    );
    check!(labels::intern(WEB) == Ok(web), "re-interning a child");
    check!(
        labels::name_of(web).as_deref() == Some(WEB),
        "the id does not round-trip"
    );

    let long = format!("{APP}/{}", "a".repeat(labels::MAX_CHILD_NAME_BYTES + 1));
    let longest = format!("{APP}/{}", "a".repeat(labels::MAX_CHILD_NAME_BYTES));
    let bad = [
        "app:org.test.browser/",
        "app:org.test.browser/Web",
        "app:org.test.browser/we b",
        "app:org.test.browser/a/b",
        "app:org.test.browser//a",
        "app:org.test.browser/-a",
        "app:org.test.browser/_a",
        "app:org.test.browser/a.b",
        "app:org.test.browser/a:b",
        "app:/a",
        "app:.org/a",
        "dev:org.test.browser/a",
        "system:netdrv/a",
        long.as_str(),
    ];
    for label in bad {
        check!(
            labels::intern(label) == Err(LabelError::Malformed),
            "{label:?} was accepted"
        );
    }
    check!(labels::intern(&longest).is_ok(), "the longest child name");
    check!(
        labels::intern("app:org.test.browser/a_b-1").is_ok(),
        "underscore and digits"
    );
    // The longest app id plus the longest child fits a label.
    let id = format!(
        "app:{}",
        ["a".repeat(40), "b".repeat(40), "c".repeat(46)].join(".")
    );
    check!(id.len() == 4 + 128, "setup: id is {} bytes", id.len());
    labels::intern(&id).map_err(|_| "intern the longest app")?;
    let child = format!("{id}/{}", "z".repeat(labels::MAX_CHILD_NAME_BYTES));
    check!(
        child.len() <= labels::MAX_LABEL_BYTES && labels::intern(&child).is_ok(),
        "the longest child of the longest app ({} bytes) was refused",
        child.len()
    );
    Ok(())
}

/// Loading a child through `OP_ACL_LOAD`: the app first, its program, no
/// widening, and every malformed request refused without a trace.
pub fn child_load_rules() -> Result<(), String> {
    fresh_children()?;
    in_space(|| -> Result<(), String> {
        // No app label yet: refused, and the child takes no slot.
        let before = labels::count();
        check!(
            load_with_program(WEB, &[], Some(WEB_PATH))? == failed(ENOENT),
            "a child of an unloaded app was accepted"
        );
        check!(
            labels::count() == before && labels::lookup(WEB).is_none(),
            "a refused child interned a label"
        );

        let app = load_app()?;
        // A page process asks for nothing: bound, with no rules.
        check!(
            load_with_program(WEB, &[], Some(WEB_PATH))? == 0,
            "a child with no rules was refused"
        );
        let web = labels::lookup(WEB).ok_or("not interned")?;
        check!(
            children::program_of(web).as_deref() == Some(WEB_PATH)
                && acl::label_rule_count(web) == 0,
            "the binding or the rules are wrong"
        );
        check!(labels::parent_of(web) == Some(app), "link");

        // Widening is refused with `EACCES`, audited, and changes nothing.
        let wider = [any(SOCKETS), any(CLIPBOARD)];
        check!(
            load_with_program(NET, &wider, Some(NET_PATH))? == failed(EACCES),
            "a child wider than the app was accepted"
        );
        let event = *audit::recent(1).first().ok_or("no audit record")?;
        check!(
            !event.allow
                && event.reason_code == reason::CHILD_WIDENS_APP
                && event.interface_id == policy::LOADER_INTERFACE,
            "the refusal was audited as {event:?}"
        );
        check!(
            labels::lookup(NET).is_none() && children::count() == 1,
            "a refused child left a label or a binding"
        );
        // A wildcard the app does not hold is wider than any one of its rules.
        let wildcard = policy::wire::LabelRule {
            interface_id: u64::MAX,
            method: u32::MAX,
            allow: true,
        };
        check!(
            load_with_program(NET, &[wildcard], Some(NET_PATH))? == failed(EACCES),
            "a wildcard child was accepted"
        );
        // A deny rule never widens anything.
        let deny = policy::wire::LabelRule {
            interface_id: fnv1a64(CLIPBOARD),
            method: u32::MAX,
            allow: false,
        };
        check!(
            load_with_program(NET, &[deny, any(SOCKETS)], Some(NET_PATH))? == 0,
            "a child with a deny rule was refused"
        );
        check!(
            load_with_program(NET, &[any(SOCKETS), resolve(STACK)], Some(NET_PATH))? == 0,
            "the network process was refused"
        );

        // Malformed requests change nothing.
        let bound = children::count();
        let used = labels::count();
        for program in ["relative/x", "/a/../b", "/a//b", "/a/./b", "/a/", "/"] {
            let code = load_with_program(WEB, &[], Some(program))?;
            check!(code == failed(EINVAL), "program {program:?} -> {code:#x}");
        }
        check!(
            load_with_program(WEB, &[any(SOCKETS)], None)? == failed(EINVAL),
            "a child with rules and no program was accepted"
        );
        check!(
            load_with_program(APP, &[], Some("/system/bin/x"))? == failed(EINVAL),
            "a program on an app label was accepted"
        );
        check!(
            load_with_program("system:netdrv", &[], Some("/system/bin/x"))? == failed(EINVAL),
            "a program on a system label was accepted"
        );
        check!(
            load_with_program("app:org.test.browser/Web", &[], Some("/x"))? == failed(EINVAL),
            "a malformed child was accepted"
        );
        check!(
            children::count() == bound && labels::count() == used,
            "a refused load changed the bindings or the label table"
        );
        // Another app may bind the same path to its own child.
        check!(
            load_current(OTHER, &[any(DISPLAY)])? == 0,
            "load the other app"
        );
        check!(
            load_with_program(OTHER_WEB, &[], Some(WEB_PATH))? == 0,
            "the same path under another app was refused"
        );
        // A new program replaces the old one.
        check!(
            load_with_program(WEB, &[], Some("/system/bin/webprocess2"))? == 0,
            "rebind"
        );
        check!(
            children::program_of(web).as_deref() == Some("/system/bin/webprocess2"),
            "the program was not replaced"
        );

        // A table renamed in an upgrade: the program moves to the newer
        // child of the same app, and the older one can no longer be entered
        // and holds no rules. Another app's child with that path stays.
        let other_web = labels::lookup(OTHER_WEB).ok_or("other child")?;
        check!(
            load_with_program(WEB, &[any(SOCKETS)], Some(WEB_PATH))? == 0,
            "bind again"
        );
        check!(
            load_with_program("app:org.test.browser/renamed", &[], Some(WEB_PATH))? == 0,
            "the program was refused to the newer child"
        );
        let renamed = labels::lookup("app:org.test.browser/renamed").ok_or("renamed")?;
        check!(
            children::program_of(renamed).as_deref() == Some(WEB_PATH)
                && children::program_of(web).is_none()
                && acl::label_rule_count(web) == 0,
            "the program did not move, or the displaced child kept rules"
        );
        check!(
            children::program_of(other_web).as_deref() == Some(WEB_PATH),
            "another app's binding of the path moved"
        );
        let cred = Cred::new(UID, UID, CAPS, app, SESSION);
        check!(
            children::label_for_exec(&cred, WEB_PATH) == Some(renamed),
            "the program enters the older child"
        );
        Ok(())
    })
}

/// A child leaves with its own revocation (no rules, no program) and with the
/// app's: the bindings and the rules go, and nothing else is touched.
pub fn child_revocation() -> Result<(), String> {
    fresh_children()?;
    in_space(|| -> Result<(), String> {
        load_app()?;
        let (web, net) = load_children()?;
        check!(load_current(OTHER, &[any(DISPLAY)])? == 0, "other app");
        check!(
            load_with_program(OTHER_WEB, &[], Some(WEB_PATH))? == 0,
            "other child"
        );
        let other_web = labels::lookup(OTHER_WEB).ok_or("other child")?;
        check!(children::count() == 3, "bindings: {}", children::count());

        // One child, by itself.
        check!(
            load_with_program(WEB, &[], None)? == 0,
            "revoking a child was refused"
        );
        check!(
            children::program_of(web).is_none() && children::program_of(net).is_some(),
            "the wrong binding went"
        );
        check!(acl::label_rule_count(net) == 2, "a sibling lost rules");
        // Revoking a child that was never loaded interns nothing.
        let used = labels::count();
        check!(
            load_with_program("app:org.test.browser/never", &[], None)? == 0,
            "revoking an unknown child"
        );
        check!(labels::count() == used, "a revocation interned a label");

        // The app, which takes the rest of its children with it and no one
        // else's.
        check!(load_current(APP, &[])? == 0, "revoking the app");
        check!(
            children::program_of(net).is_none() && acl::label_rule_count(net) == 0,
            "a child outlived its app"
        );
        check!(
            children::program_of(other_web).as_deref() == Some(WEB_PATH),
            "another app's child went too"
        );
        check!(children::count() == 1, "bindings: {}", children::count());
        // An upgrade reloads the app and then the children: the app's own
        // non-empty load does not drop anything.
        load_app()?;
        load_children()?;
        load_app()?;
        check!(children::count() == 3, "an app reload dropped a child");
        Ok(())
    })
}
