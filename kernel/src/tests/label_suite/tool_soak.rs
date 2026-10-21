//! Soaks for developer-tool labels (`tool:<id>`): load/replace/revoke churn
//! over many labels, thousands of registrations, resolves and topic
//! authorizations from tool credentials interleaved with app credentials, the
//! label table churned up to its capacity, and a rule list at the per-label
//! bound with a trailing wildcard allow. Each ends with every table back where
//! it started.

use super::tool::{default_denied, tool_rules, DENIED};
use super::*;
use crate::ipc::acl::{reason, LoadError, Rule, ANY_INTERFACE, ANY_METHOD};
use crate::ipc::labels::Kind;
use crate::ipc::topics::fnv1a64;
use crate::ipc::{authorize, devspawn};

const EINVAL: i64 = 22;
const ENOMEM: i64 = 12;

/// An interface no list names, so only a wildcard can allow it.
const OPEN_IFACE: u64 = 0x7000;

pub(super) fn as_task<R>(slot: usize, f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    task::harness::switch_current(slot);
    let outcome = f();
    task::harness::switch_current(task::KERNEL_TASK);
    outcome
}

/// Close every handle `slot` holds, then require the table to be empty.
pub(super) fn drain(slot: usize) -> Result<(), String> {
    as_task(slot, || {
        for (handle, _) in handles::entries_for_task(slot) {
            let _ = channels::close_endpoint(handle);
        }
        Ok(())
    })?;
    check!(
        handles::count_for_task(slot) == 0,
        "slot {slot} still holds {} handles",
        handles::count_for_task(slot)
    );
    Ok(())
}

/// What the soak believes one label's rules are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Nothing loaded: default deny.
    Revoked,
    /// The shipped list.
    Full,
    /// The shipped list plus a deny for one more interface.
    Extra(u64),
}

fn rules_for(mode: Mode) -> Vec<Rule> {
    let mut denied = default_denied();
    match mode {
        Mode::Revoked => return Vec::new(),
        Mode::Full => {}
        Mode::Extra(id) => denied.push(id),
    }
    tool_rules(&denied)
}

/// Judge `label` the way the model says, for every denied interface, one open
/// interface and the extra one.
fn expect(label: u32, mode: Mode) -> Result<(), String> {
    let (open, code) = acl::evaluate_label(label, OPEN_IFACE, 1);
    match mode {
        Mode::Revoked => {
            check!(
                open.denied() && code == reason::LABEL_DEFAULT_DENY,
                "revoked label {label} judged {code}"
            );
        }
        Mode::Full | Mode::Extra(_) => {
            check!(
                !open.denied() && code == reason::ALLOWED_BY_RULE,
                "label {label} lost its wildcard ({code})"
            );
            for name in DENIED {
                let (verdict, code) = acl::evaluate_label(label, fnv1a64(name), 2);
                check!(
                    verdict.denied() && code == reason::EXPLICIT_DENY,
                    "label {label} reached {name} ({code})"
                );
            }
            if let Mode::Extra(id) = mode {
                check!(
                    acl::evaluate_label(label, id, 0).0.denied(),
                    "the extra deny {id:#x} of label {label} was ignored"
                );
            }
        }
    }
    Ok(())
}

/// Many tool labels loaded, replaced and revoked in turn (half through the real
/// loader), every neighbour unchanged, the label table and the rule store back
/// to empty at the end.
pub fn churn_load_replace_revoke() -> Result<(), String> {
    const TOOLS: usize = 24;
    const ITERATIONS: usize = 3000;
    fresh()?;
    let mut ids = Vec::new();
    let mut tasks = Vec::new();
    for index in 0..TOOLS {
        let label = format!("tool:soak.t{index}");
        ids.push(labels::intern(&label).map_err(|_| "intern")?);
        if index < 6 {
            tasks.push(labelled_task(&label, 1000 + index as u32, 0)?);
        }
    }
    let mut modes = vec![Mode::Revoked; TOOLS];
    in_space(|| -> Result<(), String> {
        for index in 0..ITERATIONS {
            let k = index % TOOLS;
            let next = match index % 5 {
                0 | 3 => Mode::Full,
                1 => Mode::Extra(0x9000 + (index % 16) as u64),
                _ => Mode::Revoked,
            };
            if index % 2 == 0 {
                let wire: Vec<policy::wire::LabelRule> = rules_for(next)
                    .iter()
                    .map(|rule| policy::wire::LabelRule {
                        interface_id: rule.interface_id,
                        method: rule.method,
                        allow: rule.allow,
                    })
                    .collect();
                let label = format!("tool:soak.t{k}");
                check!(
                    load_current(&label, &wire)? == 0,
                    "iteration {index}: load refused"
                );
            } else {
                acl::load_label(ids[k], &rules_for(next))
                    .map_err(|e| format!("iteration {index}: {e:?}"))?;
            }
            modes[k] = next;
            expect(ids[k], next)?;
            // A neighbour is untouched.
            let neighbour = (k + 1) % TOOLS;
            expect(ids[neighbour], modes[neighbour])?;
            if let Some(&slot) = tasks.get(k) {
                let denied = authorize(slot, fnv1a64(DENIED[0]), 1, index as u64).denied();
                check!(
                    denied,
                    "iteration {index}: a task reached a denied interface"
                );
            }
            if index % 250 == 249 {
                let expected: usize = modes.iter().map(|&m| rules_for(m).len()).sum();
                check!(
                    acl::label_rules_total() == expected,
                    "iteration {index}: {} rules stored, expected {expected}",
                    acl::label_rules_total()
                );
                check!(
                    labels::count() == TOOLS,
                    "iteration {index}: the label table grew to {}",
                    labels::count()
                );
            }
        }
        Ok(())
    })?;
    for &id in &ids {
        acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
    }
    check!(
        acl::label_rules_total() == 0,
        "{} rules leaked",
        acl::label_rules_total()
    );
    check!(labels::count() == TOOLS, "the label table changed");
    Ok(())
}

/// The label table filled to capacity with a mix of tool, app and dev labels,
/// each judged by its own rules; a full table refuses the next tool (and the
/// loader reports `ENOMEM`), nothing is evicted, and replacing and revoking
/// every list leaves the rule store empty. Three passes.
pub fn table_churn_to_capacity() -> Result<(), String> {
    fresh()?;
    let mut ids: Vec<(u32, Kind)> = Vec::new();
    for index in 0..labels::CAPACITY {
        let (label, kind) = match index % 3 {
            0 => (format!("tool:fill.t{index}"), Kind::Tool),
            1 => (format!("app:fill.a{index}"), Kind::App),
            _ => (format!("dev:fill.d{index}"), Kind::Dev),
        };
        ids.push((labels::intern(&label).map_err(|_| "fill")?, kind));
    }
    check!(labels::count() == labels::CAPACITY, "table not full");
    for pass in 0..3 {
        for &(id, kind) in &ids {
            let rules = match kind {
                Kind::Tool => tool_rules(&default_denied()),
                _ => vec![Rule {
                    actor: 0,
                    interface_id: OPEN_IFACE,
                    method: ANY_METHOD,
                    allow: true,
                }],
            };
            acl::load_label(id, &rules).map_err(|e| format!("pass {pass}: {e:?}"))?;
        }
        for &(id, kind) in &ids {
            check!(
                labels::kind_of(id) == Some(kind),
                "pass {pass}: kind of {id}"
            );
            let open = !acl::evaluate_label(id, OPEN_IFACE, 3).0.denied();
            let other = !acl::evaluate_label(id, 0x7001, 3).0.denied();
            let secret = !acl::evaluate_label(id, fnv1a64(DENIED[0]), 3).0.denied();
            let expected = match kind {
                Kind::Tool => (true, true, false),
                _ => (true, false, false),
            };
            check!(
                (open, other, secret) == expected,
                "pass {pass}: label {id} ({kind:?}) judged {:?}",
                (open, other, secret)
            );
        }
        check!(
            labels::intern("tool:fill.overflow") == Err(labels::Error::TableFull),
            "pass {pass}: a full table accepted a tool"
        );
        let loaded = in_space(|| load_current("tool:fill.overflow", &[]))?;
        check!(
            loaded == failed(ENOMEM),
            "pass {pass}: the loader's answer was {loaded:#x}"
        );
        // Replace every tool's list by one rule, then revoke everything.
        for &(id, kind) in ids.iter().filter(|(_, kind)| *kind == Kind::Tool) {
            let one = [Rule {
                actor: 0,
                interface_id: ANY_INTERFACE,
                method: ANY_METHOD,
                allow: true,
            }];
            acl::load_label(id, &one).map_err(|e| format!("{kind:?}: {e:?}"))?;
            check!(
                !acl::evaluate_label(id, fnv1a64(DENIED[0]), 3).0.denied(),
                "pass {pass}: a replaced list kept its denies"
            );
        }
        for &(id, _) in &ids {
            acl::load_label(id, &[]).map_err(|e| format!("{e:?}"))?;
        }
        check!(
            acl::label_rules_total() == 0,
            "pass {pass}: {} rules leaked",
            acl::label_rules_total()
        );
        check!(
            labels::count() == labels::CAPACITY,
            "pass {pass}: the table changed"
        );
    }
    fresh()
}

/// A list at the per-label bound: 255 distinct denies and the trailing
/// wildcard allow load, hold and replace repeatedly; one rule more is refused
/// with the old list intact; the kernel-wide budget refuses the label that
/// would exceed it and accepts it again once room is made.
pub fn rule_list_at_the_limit() -> Result<(), String> {
    const ROUNDS: usize = 200;
    fresh()?;
    let tool = labels::intern("tool:limit.t").map_err(|_| "intern")?;
    let denies = acl::MAX_RULES_PER_LABEL - 1;
    let ids: Vec<u64> = (0..denies as u64).map(|n| 0x5000 + n).collect();
    let full = tool_rules(&ids);
    check!(full.len() == acl::MAX_RULES_PER_LABEL, "list length");
    let tiny = tool_rules(&ids[..1]);

    for round in 0..ROUNDS {
        let (list, listed) = if round % 2 == 0 {
            (&full, denies)
        } else {
            (&tiny, 1)
        };
        acl::load_label(tool, list).map_err(|e| format!("round {round}: {e:?}"))?;
        check!(
            acl::label_rule_count(tool) == list.len(),
            "round {round}: {} rules",
            acl::label_rule_count(tool)
        );
        // First, last and a stride of the denies; an unlisted id; the wildcard.
        for position in (0..listed).step_by(37).chain([listed - 1]) {
            check!(
                acl::evaluate_label(tool, ids[position], 1).0.denied(),
                "round {round}: deny {position} not enforced"
            );
        }
        if listed < denies {
            check!(
                !acl::evaluate_label(tool, ids[denies - 1], 1).0.denied(),
                "round {round}: a replaced list kept a deny"
            );
        }
        check!(
            !acl::evaluate_label(tool, OPEN_IFACE, 1).0.denied(),
            "round {round}: the trailing allow-all was lost"
        );
        check!(
            acl::label_allowed_ids(tool).is_empty(),
            "round {round}: allowed ids before the wildcard"
        );
    }

    // One more than the bound, directly and through the loader.
    acl::load_label(tool, &full).map_err(|e| format!("{e:?}"))?;
    let mut too_many = full.clone();
    too_many.insert(0, too_many[0]);
    check!(
        acl::load_label(tool, &too_many) == Err(LoadError::TooManyRules),
        "an oversized list was accepted"
    );
    let wire: Vec<policy::wire::LabelRule> = too_many
        .iter()
        .map(|rule| policy::wire::LabelRule {
            interface_id: rule.interface_id,
            method: rule.method,
            allow: rule.allow,
        })
        .collect();
    let code = in_space(|| load_current("tool:limit.t", &wire))?;
    check!(code == failed(EINVAL), "the loader answered {code:#x}");
    check!(
        acl::label_rule_count(tool) == acl::MAX_RULES_PER_LABEL
            && acl::evaluate_label(tool, ids[denies - 1], 1).0.denied(),
        "a refused load changed the stored rules"
    );

    // The total budget: fill it with full lists, one label short of room.
    let labels_needed = acl::MAX_LABEL_RULES / acl::MAX_RULES_PER_LABEL;
    let mut others = Vec::new();
    for index in 1..labels_needed {
        let id = labels::intern(&format!("tool:limit.f{index}")).map_err(|_| "intern")?;
        acl::load_label(id, &full).map_err(|e| format!("filler {index}: {e:?}"))?;
        others.push(id);
    }
    check!(
        acl::label_rules_total() == acl::MAX_LABEL_RULES,
        "{} rules stored",
        acl::label_rules_total()
    );
    let late = labels::intern("tool:limit.late").map_err(|_| "intern")?;
    check!(
        acl::load_label(late, &tiny[..1]) == Err(LoadError::PolicyFull),
        "the budget was exceeded"
    );
    check!(
        acl::label_rule_count(late) == 0 && acl::label_rules_total() == acl::MAX_LABEL_RULES,
        "a refused load changed the store"
    );
    // Replacing a full list by a tiny one makes room; replacing in place never
    // needs more than the list's own old size.
    acl::load_label(tool, &tiny).map_err(|e| format!("{e:?}"))?;
    acl::load_label(late, &tiny).map_err(|e| format!("late after room: {e:?}"))?;
    acl::load_label(late, &[]).map_err(|e| format!("{e:?}"))?;
    acl::load_label(tool, &full).map_err(|e| format!("regrow: {e:?}"))?;
    check!(
        acl::load_label(tool, &full).is_ok(),
        "an in-place replace needed fresh room"
    );
    for id in others.iter().chain([&tool, &late]) {
        acl::load_label(*id, &[]).map_err(|e| format!("{e:?}"))?;
    }
    check!(acl::label_rules_total() == 0, "rules leaked");

    // The dev-spawn scope, the tool's own deny, survives every replacement.
    let scope = devspawn::SPAWN_SCOPE;
    acl::load_label(tool, &tool_rules(&default_denied())).map_err(|e| format!("{e:?}"))?;
    check!(
        acl::evaluate_label(tool, scope, 7).0.denied(),
        "the dev-spawn scope was reachable"
    );
    Ok(())
}
