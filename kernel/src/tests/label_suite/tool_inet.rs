//! A developer tool and a raw `AF_INET` socket (`confine::may_open_inet`,
//! `docs/devtool-packages-plan.md` section 9, finding 2).
//!
//! The consent text promises "no network unless listed", and the Messenger
//! deny list binds only calls to the socket service. These tests pin the
//! other half: a tool's own native code (and every child, which inherits the
//! label) is refused `socket(AF_INET)` exactly when the label's rules deny the
//! socket interface, which is what a tool without `network = ["outbound"]`
//! compiles to; a tool with it, an unlabelled task and an app label keep what
//! they had. The gate is asked directly with the task made current, which is
//! what the Linux `socket` call does (`process/linux/inet.rs`).

use super::tool::{tool_rules, tool_task, DENIED, OTHER_TOOL, TOOL, UNLESS_NETWORK};
use super::tool_soak::as_task;
use super::*;
use crate::ipc::acl::{reason, Rule, ANY_METHOD};
use crate::ipc::confine;
use crate::ipc::topics::fnv1a64;

/// The doors a `network = ["outbound"]` declaration opens: the stand-in's
/// `UnlessNetwork` list, which `check_interfaces.py` compares with
/// `devtool::DENY`.
const NETWORK: [&str; 3] = UNLESS_NETWORK;

/// A plain tool's compiled rules: every deny, then the wildcard allow.
fn plain_rules() -> Vec<Rule> {
    tool_rules(&DENIED.iter().map(|name| fnv1a64(name)).collect::<Vec<_>>())
}

/// A tool that declared `network = ["outbound"]`: the same list without the
/// three network entries.
fn declared_rules() -> Vec<Rule> {
    let denied: Vec<u64> = DENIED
        .iter()
        .filter(|name| !NETWORK.contains(name))
        .map(|name| fnv1a64(name))
        .collect();
    tool_rules(&denied)
}

/// Run `body` on a clean world and leave the confine gate's memory clean:
/// a label that held the confinement marker stays confined for the boot, and
/// label ids are reused by the next test.
fn scoped(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    fresh()?;
    confine::reset_for_tests();
    let outcome = body();
    confine::reset_for_tests();
    outcome
}

fn inet(slot: usize) -> Result<bool, String> {
    as_task(slot, || Ok(confine::may_open_inet()))
}

fn serve(slot: usize) -> Result<bool, String> {
    as_task(slot, || Ok(confine::may_serve()))
}

/// What the socket gate says for tasks of each kind of label.
pub fn inet_follows_the_tool_rules() -> Result<(), String> {
    scoped(|| {
        let (plain, plain_id) = tool_task(TOOL, 1000)?;
        let (declared, declared_id) = tool_task(OTHER_TOOL, 1000)?;
        acl::load_label(declared_id, &declared_rules()).map_err(|e| format!("{e:?}"))?;

        // A tool without the declaration is refused, audited and traced.
        check!(!inet(plain)?, "a tool without `network` opened AF_INET");
        // ... as the explicit deny rule it hit, not as a default deny.
        last_denial(plain_id, reason::EXPLICIT_DENY)?;
        // One with it is not, and the other tool's verdict is its own.
        check!(inet(declared)?, "a tool with `network` was refused AF_INET");
        check!(!inet(plain)?, "the declaration of another tool leaked");
        // The Messenger doors agree with the socket gate, door by door.
        for name in NETWORK {
            let id = fnv1a64(name);
            check!(
                acl::evaluate_label(plain_id, id, 1).0.denied(),
                "{name} reachable without the declaration"
            );
            check!(
                !acl::evaluate_label(declared_id, id, 1).0.denied(),
                "{name} refused to a tool that declared the network"
            );
        }
        // A tool is not confined (files, programs and signals stay the user's):
        // `serve` is only asked after `socket` succeeded, and is not narrowed.
        check!(
            serve(plain)? && serve(declared)?,
            "a tool was narrowed beyond sockets"
        );
        check!(
            !confine::confined(plain_id) && !confine::confined(declared_id),
            "a tool became confined"
        );

        // Every task of the label, a child included, follows the rules: loading
        // the declaration changes the verdict for tasks that already run.
        let child = labelled_task(TOOL, 1000, 0)?;
        check!(!inet(child)?, "a child of a plain tool opened AF_INET");
        acl::load_label(plain_id, &declared_rules()).map_err(|e| format!("{e:?}"))?;
        check!(
            inet(plain)? && inet(child)?,
            "the declaration did not reach running tasks"
        );
        // And removing it takes the socket away again.
        acl::load_label(plain_id, &plain_rules()).map_err(|e| format!("{e:?}"))?;
        check!(
            !inet(plain)? && !inet(child)?,
            "the socket survived the rules that denied it"
        );
        // A revoked tool (an empty list) is default-denied: it loses sockets with
        // everything else, however it got them.
        acl::load_label(declared_id, &[]).map_err(|e| format!("{e:?}"))?;
        check!(!inet(declared)?, "a revoked tool kept its socket");
        last_denial(declared_id, reason::LABEL_DEFAULT_DENY)?;
        acl::load_label(declared_id, &declared_rules()).map_err(|e| format!("{e:?}"))?;
        check!(inet(declared)?, "a re-loaded declaration was ignored");
        Ok(())
    })
}

/// Everything that is not a tool is as it was: unlabelled tasks (every
/// service) and unconfined apps open sockets with or without rules, a
/// confined label needs the socket rule.
pub fn inet_gate_unchanged_for_other_labels() -> Result<(), String> {
    scoped(|| {
        let unlabelled = labelled_task("", 1000, 0)?;
        check!(inet(unlabelled)?, "an unlabelled task lost AF_INET");
        check!(serve(unlabelled)?, "an unlabelled task lost listen");

        // An app label is not confined by its rules alone: with none loaded, or a
        // list that does not name the socket, it still opens sockets (its
        // `network` entry is consent text).
        let app = labelled_task(super::tool::APP, 1000, 0)?;
        let app_id = credentials::of(app).label_id;
        check!(inet(app)?, "an unconfined app lost AF_INET");
        acl::load_label(
            app_id,
            &[Rule {
                actor: 0,
                interface_id: fnv1a64("os.lazy.confd.v1"),
                method: ANY_METHOD,
                allow: true,
            }],
        )
        .map_err(|e| format!("{e:?}"))?;
        check!(inet(app)?, "an unconfined app with rules lost AF_INET");

        // A confined label (the marker rule) needs the socket interface.
        let confined_task = labelled_task(super::tool::OTHER_APP, 1000, 0)?;
        let confined_id = credentials::of(confined_task).label_id;
        let marker = Rule {
            actor: 0,
            interface_id: filepolicy::confine_id(),
            method: ANY_METHOD,
            allow: true,
        };
        acl::load_label(confined_id, &[marker]).map_err(|e| format!("{e:?}"))?;
        check!(!inet(confined_task)?, "a confined label opened AF_INET");
        check!(!serve(confined_task)?, "a confined label may listen");
        acl::load_label(
            confined_id,
            &[
                marker,
                Rule {
                    actor: 0,
                    interface_id: fnv1a64(NETWORK[0]),
                    method: ANY_METHOD,
                    allow: true,
                },
            ],
        )
        .map_err(|e| format!("{e:?}"))?;
        check!(inet(confined_task)?, "the socket rule did not open AF_INET");
        Ok(())
    })
}

/// The gate churned: sixteen tasks on four tool labels, whose rules go
/// through plain, declared and revoked 20 000 times, every task of every
/// label judged against the model after each change, and the rule store and
/// label table back where they started.
pub fn inet_gate_soak() -> Result<(), String> {
    scoped(|| {
        const LABELS: usize = 4;
        const PER_LABEL: usize = 4;
        const ROUNDS: usize = 20_000;
        let mut ids = Vec::new();
        let mut tasks: Vec<Vec<usize>> = Vec::new();
        for index in 0..LABELS {
            let name = format!("tool:inet.t{index}");
            ids.push(labels::intern(&name).map_err(|_| "intern")?);
            let mut group = Vec::new();
            for _ in 0..PER_LABEL {
                group.push(labelled_task(&name, 1000, 0)?);
            }
            tasks.push(group);
        }
        let baseline = labels::count();
        // 0 revoked, 1 plain, 2 declared; every label starts revoked.
        let mut modes = [0u8; LABELS];
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for round in 0..ROUNDS {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let label = (state % LABELS as u64) as usize;
            let mode = ((state >> 20) % 3) as u8;
            let rules = match mode {
                0 => Vec::new(),
                1 => plain_rules(),
                _ => declared_rules(),
            };
            acl::load_label(ids[label], &rules).map_err(|e| format!("round {round}: {e:?}"))?;
            modes[label] = mode;
            // The changed label, one task of it; every label, every task, on a
            // stride (all of them each round would make the soak the gate's cost).
            for (other, group) in tasks.iter().enumerate() {
                let expected = modes[other] == 2;
                let probe: &[usize] = if other == label || round % 97 == 0 {
                    group
                } else {
                    &group[..1]
                };
                for &slot in probe {
                    check!(
                        inet(slot)? == expected,
                        "round {round}: label {other} in mode {} judged wrongly",
                        modes[other]
                    );
                }
            }
            if round % 500 == 499 {
                let stored: usize = modes
                    .iter()
                    .map(|&m| match m {
                        0 => 0,
                        1 => plain_rules().len(),
                        _ => declared_rules().len(),
                    })
                    .sum();
                check!(
                    acl::label_rules_total() == stored,
                    "round {round}: {} rules stored, expected {stored}",
                    acl::label_rules_total()
                );
                check!(
                    labels::count() == baseline,
                    "round {round}: label table grew"
                );
            }
        }
        for &id in &ids {
            acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
        }
        check!(
            acl::label_rules_total() == 0,
            "{} rules leaked",
            acl::label_rules_total()
        );
        // Rules that never named a wildcard on a tool do not make it confined.
        check!(
            ids.iter().all(|&id| !confine::confined(id)),
            "a tool became confined during the churn"
        );
        Ok(())
    })
}
