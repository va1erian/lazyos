//! Per-session topics for labelled tasks (issue #488): a manifest's
//! `session/+/<topic>` grants the app's own session only.

use super::*;
use crate::ipc::acl::{reason, Rule};
use crate::ipc::topics;

/// Generations of the soak (each re-labels the task into a new session).
const SOAK_GENERATIONS: u64 = 4000;

fn allow(interface_id: u64, segment: &str) -> Rule {
    Rule {
        actor: 0,
        interface_id,
        method: topics::segment_method(segment),
        allow: true,
    }
}

/// A labelled task in `session`, its label granted `publish:session/+/selection`
/// and `subscribe:session/+/selection` the way `pkgstore::rules` compiles them.
fn granted_task(session: u64) -> Result<(usize, u32), String> {
    let slot = labelled_task("app:com.files", 1000, 0)?;
    let id = credentials::of(slot).label_id;
    credentials::set(slot, Cred::new(1000, 1000, 0, id, session));
    let mut rules = Vec::new();
    for interface in [topics::PUBLISH_INTERFACE, topics::SUBSCRIBE_INTERFACE] {
        for segment in ["session", "+", "selection"] {
            rules.push(allow(interface, segment));
        }
    }
    acl::load_label(id, &rules).map_err(|_| "load failed")?;
    Ok((slot, id))
}

/// The own session is reachable through the `+` grant; another session, a
/// wildcard, a padded id and an ungranted tail are refused and audited; an
/// unlabelled task keeps the literal per-segment check.
pub fn own_session_only() -> Result<(), String> {
    fresh()?;
    let (x, x_id) = granted_task(7)?;
    let (publish, subscribe) = (topics::MODE_PUBLISH, topics::MODE_SUBSCRIBE);

    check!(
        topics::authorize(x, publish, "session/7/selection", 1) == Ok(3),
        "the app could not publish its own session's selection"
    );
    check!(
        topics::authorize(x, subscribe, "session/7/selection", 1).is_ok(),
        "the app could not subscribe to its own session's selection"
    );
    check!(
        topics::authorize(x, publish, "session/8/selection", 2) == Err(topics::Error::Denied),
        "the app published into another session"
    );
    last_denial(x_id, reason::LABEL_DEFAULT_DENY)?;
    check!(
        topics::authorize(x, subscribe, "session/+/selection", 2) == Err(topics::Error::Denied),
        "a wildcard filter crossed into other sessions"
    );
    check!(
        topics::authorize(x, subscribe, "session/#", 2) == Err(topics::Error::Denied),
        "a tail filter crossed into other sessions"
    );
    check!(
        topics::authorize(x, publish, "session/07/selection", 3) == Err(topics::Error::Denied),
        "a zero-padded id stood in for the own session"
    );
    check!(
        topics::authorize(x, publish, "session/7/clipboard/changed", 3)
            == Err(topics::Error::Denied),
        "an ungranted topic of the own session was allowed"
    );
    // `+` is not a literal grant: a topic segment spelled `+` stays invalid.
    check!(
        topics::authorize(x, publish, "session/+/selection", 3) == Err(topics::Error::BadName),
        "a wildcard publish topic was accepted"
    );
    // Session 0 (no login yet) is only its own, like any other.
    let (zero, _) = granted_task(0)?;
    check!(
        topics::authorize(zero, publish, "session/0/selection", 4).is_ok(),
        "a session-0 task could not reach its own session"
    );
    check!(
        topics::authorize(zero, publish, "session/7/selection", 4) == Err(topics::Error::Denied),
        "a session-0 task reached session 7"
    );

    // Unlabelled tasks: literal segments, unchanged.
    let legacy = labelled_task("", 1000, 0)?;
    check!(
        topics::authorize(legacy, publish, "session/8/selection", 5).is_ok(),
        "an unlabelled task lost its bootstrap access to session topics"
    );
    Ok(())
}

/// Thousands of session changes: the verdict follows the task's current
/// session every time, and the audit ring keeps recording.
pub fn session_soak() -> Result<(), String> {
    fresh()?;
    let (x, x_id) = granted_task(1)?;
    let mut own = String::new();
    let mut other = String::new();
    for generation in 1..=SOAK_GENERATIONS {
        credentials::set(x, Cred::new(1000, 1000, 0, x_id, generation));
        own.clear();
        other.clear();
        let _ =
            core::fmt::Write::write_fmt(&mut own, format_args!("session/{generation}/selection"));
        let _ = core::fmt::Write::write_fmt(
            &mut other,
            format_args!("session/{}/selection", generation + 1),
        );
        check!(
            topics::authorize(x, topics::MODE_PUBLISH, &own, generation).is_ok(),
            "generation {generation}: the own session was refused"
        );
        check!(
            topics::authorize(x, topics::MODE_PUBLISH, &other, generation)
                == Err(topics::Error::Denied),
            "generation {generation}: the next session was allowed"
        );
    }
    last_denial(x_id, reason::LABEL_DEFAULT_DENY)?;
    Ok(())
}
