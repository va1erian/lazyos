//! A child shaped as `pkgd` loads it: the confine marker plus the file rule
//! ids of its `files`, under an app that holds the same file ids and not the
//! marker (`libs/pkgstore/src/rules.rs`).

use super::*;
use crate::ipc::{acl::ANY_METHOD, confine};

/// An allow rule for the raw id `interface_id`, any method.
fn id_rule(interface_id: u64) -> policy::wire::LabelRule {
    policy::wire::LabelRule {
        interface_id,
        method: ANY_METHOD,
        allow: true,
    }
}

/// The compiled ids of `files` as the file gate sees them, marker included.
fn compiled(files: &[&str]) -> Result<Vec<u64>, String> {
    filepolicy::compile(files).map_err(|(index, error)| {
        format!("rule {index} {:?}: {}", files.get(index), error.message())
    })
}

/// The file ids a child may name only when its app holds them too: the
/// marker only narrows, so it is never part of what the app must hold.
pub fn child_file_rules_are_covered() -> Result<(), String> {
    fresh_children()?;
    confine::reset_for_tests();
    // Label ids restart with every test, and a label that held the marker
    // stays confined for the "boot": leave nothing for the next test's ids.
    let outcome = file_rules_body();
    confine::reset_for_tests();
    outcome
}

fn file_rules_body() -> Result<(), String> {
    in_space(|| -> Result<(), String> {
        let marker = filepolicy::confine_id();
        let fonts = compiled(&["read:/system/share/fonts"])?;
        check!(
            fonts.contains(&marker) && fonts.len() > 1,
            "setup: the compiled rules are {fonts:?}"
        );
        let held: Vec<u64> = fonts.iter().copied().filter(|id| *id != marker).collect();

        // The app: its Messenger rules and the file ids, never the marker
        // (it stays unconfined).
        let mut app_rules = vec![any(DISPLAY), any(SOCKETS)];
        app_rules.extend(held.iter().map(|id| id_rule(*id)));
        check!(
            load_current(APP, &app_rules)? == 0,
            "the app's rules were refused"
        );
        let app = labels::lookup(APP).ok_or("the app label was not interned")?;
        check!(!confine::confined(app), "the app was confined");

        // The child, as pkgstore compiles it: marker plus the same ids.
        let child_rules: Vec<_> = fonts.iter().map(|id| id_rule(*id)).collect();
        check!(
            load_with_program(WEB, &child_rules, Some(WEB_PATH))? == 0,
            "a child holding the marker and its app's file ids was refused"
        );
        let web = labels::lookup(WEB).ok_or("the child was not interned")?;
        check!(
            acl::label_rule_count(web) == child_rules.len()
                && children::program_of(web).as_deref() == Some(WEB_PATH),
            "the child was not bound with its rules"
        );
        check!(
            confine::confined(web) && !confine::confined(app),
            "the marker did not confine the child alone"
        );

        // The marker by itself is a narrowing and covered by nothing.
        check!(
            load_with_program(NET, &[id_rule(marker)], Some(NET_PATH))? == 0,
            "a child with only the marker was refused"
        );

        // A file id the app does not hold is wider than the app: EACCES,
        // audited, and no label or binding is left.
        let resolv = compiled(&["read:/etc/resolv.conf"])?;
        // (the stat-only ancestor ids are shared with the fonts' own)
        let unheld = resolv
            .iter()
            .copied()
            .find(|id| *id != marker && !held.contains(id))
            .ok_or("setup: every id of the other rule is held")?;
        let wider: Vec<_> = fonts.iter().copied().chain([unheld]).map(id_rule).collect();
        let bound = children::count();
        check!(
            load_with_program(
                "app:org.test.browser/files",
                &wider,
                Some("/system/bin/files")
            )? == failed(EACCES),
            "a child naming a file id its app does not hold was accepted"
        );
        let event = *audit::recent(1).first().ok_or("no audit record")?;
        check!(
            !event.allow
                && event.reason_code == reason::CHILD_WIDENS_APP
                && event.interface_id == policy::LOADER_INTERFACE,
            "the refusal was audited as {event:?}"
        );
        check!(
            labels::lookup("app:org.test.browser/files").is_none() && children::count() == bound,
            "a refused child left a label or a binding"
        );
        Ok(())
    })
}
