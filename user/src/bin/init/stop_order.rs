//! The order an orderly shutdown stops the manifest services in
//! (docs/shutdown.md, phase 3): dependents before their dependencies, and by
//! tier, so the services that hold durable state (`confd`, `logd`) stop after
//! every service that could still write to them, and `messengerd`, the
//! foundation, last of all.
//!
//! The rule is pure (a function of the rows' names, dependencies, tiers and
//! liveness), so the boot self-test proves it on a synthetic graph, cycles
//! included, without stopping anything.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::sys;

/// The tier of an ordinary service: stops first.
const TIER_SERVICE: u8 = 1;
/// Services holding durable state: they stop once every ordinary service is
/// gone, so nothing writes to them after their final flush.
const TIER_PERSIST: u8 = 2;
/// The fabric itself: last.
const TIER_FOUNDATION: u8 = 3;

/// The stop tier of manifest service `name`.
pub(super) fn tier(name: &str) -> u8 {
    match name {
        "confd" | "logd" => TIER_PERSIST,
        "messengerd" => TIER_FOUNDATION,
        _ => TIER_SERVICE,
    }
}

/// One row as the ordering sees it.
pub(super) struct Node<'a> {
    pub(super) name: &'a str,
    pub(super) deps: &'a [&'a str],
    pub(super) tier: u8,
    /// Holds a running task (running, or asked to stop and not yet gone).
    pub(super) live: bool,
    /// Already asked to stop.
    pub(super) stopping: bool,
}

/// Which rows may be asked to stop now, and whether the strict rule had to be
/// relaxed to find any.
pub(super) struct Ready {
    pub(super) rows: Vec<usize>,
    pub(super) relaxed: bool,
}

/// The rows to ask to stop now: live, not already stopping, with no live
/// dependent, and in the lowest live tier. When nothing qualifies while live
/// rows remain and none is stopping (a dependency across tiers or a cycle
/// would wait forever), the tier rule is dropped first and then the
/// dependency rule, so the shutdown always makes progress.
pub(super) fn ready(nodes: &[Node]) -> Ready {
    let Some(lowest) = nodes.iter().filter(|n| n.live).map(|n| n.tier).min() else {
        return Ready {
            rows: Vec::new(),
            relaxed: false,
        };
    };
    let strict = select(nodes, |index| {
        nodes[index].tier == lowest && !has_live_dependent(nodes, index)
    });
    if !strict.is_empty() || nodes.iter().any(|n| n.stopping && n.live) {
        return Ready {
            rows: strict,
            relaxed: false,
        };
    }
    let mut rows = select(nodes, |index| !has_live_dependent(nodes, index));
    if rows.is_empty() {
        rows = select(nodes, |_| true);
    }
    Ready {
        rows,
        relaxed: true,
    }
}

/// Live, not-yet-stopping rows passing `keep`.
fn select(nodes: &[Node], keep: impl Fn(usize) -> bool) -> Vec<usize> {
    (0..nodes.len())
        .filter(|&index| nodes[index].live && !nodes[index].stopping && keep(index))
        .collect()
}

/// Whether another live row names `nodes[index]` as a dependency.
fn has_live_dependent(nodes: &[Node], index: usize) -> bool {
    let name = nodes[index].name;
    nodes
        .iter()
        .enumerate()
        .any(|(other, node)| other != index && node.live && node.deps.contains(&name))
}

/// Stop a synthetic graph to the end, one round of [`ready`] at a time, as if
/// every stop completed at once. Returns the rounds, or `None` when it stalls.
fn simulate(nodes: &mut [Node]) -> Option<Vec<Vec<usize>>> {
    let mut rounds = Vec::new();
    while nodes.iter().any(|n| n.live) {
        let ready = ready(nodes);
        if ready.rows.is_empty() || rounds.len() > nodes.len() {
            return None;
        }
        for &index in &ready.rows {
            nodes[index].live = false;
        }
        rounds.push(ready.rows);
    }
    Some(rounds)
}

/// The round a row stopped in.
fn round_of(rounds: &[Vec<usize>], index: usize) -> usize {
    rounds
        .iter()
        .position(|round| round.contains(&index))
        .unwrap_or(usize::MAX)
}

/// The shutdown-order self-test: the boot manifest's real shape stops every
/// dependent before its dependency and the tiers in order, and a dependency
/// cycle still stops. Prints `INIT:SHUTDOWN:ORDER:PASS`.
pub(super) fn selftest_stop_order() {
    match check_order() {
        Ok(()) => sys::write_str("INIT:SHUTDOWN:ORDER:PASS\n"),
        Err(detail) => sys::write_str(&format!("INIT:SHUTDOWN:ORDER:FAIL {detail}\n")),
    }
}

fn node<'a>(name: &'a str, deps: &'a [&'a str]) -> Node<'a> {
    Node {
        name,
        deps,
        tier: tier(name),
        live: true,
        stopping: false,
    }
}

fn check_order() -> Result<(), String> {
    let mut graph = [
        node("messengerd", &[]),
        node("keyd", &["messengerd"]),
        node("confd", &["messengerd"]),
        node("timed", &["messengerd", "confd"]),
        node("inputd", &["confd"]),
        node("usbd", &["inputd"]),
        node("logd", &["messengerd"]),
        node("healthd", &["messengerd"]),
        node("flaky", &["healthd"]),
        node("mimed", &[]),
        node("pkgd", &["confd", "mimed"]),
    ];
    let rounds = simulate(&mut graph).ok_or("the manifest graph stalled")?;
    for (index, row) in graph.iter().enumerate() {
        for dep in row.deps {
            let dep_index = graph.iter().position(|n| n.name == *dep).unwrap_or(0);
            if round_of(&rounds, index) >= round_of(&rounds, dep_index) {
                return Err(format!("{} did not stop before {dep}", row.name));
            }
        }
        for (other, later) in graph.iter().enumerate() {
            if later.tier > row.tier && round_of(&rounds, other) <= round_of(&rounds, index) {
                return Err(format!("{} stopped before {}", later.name, row.name));
            }
        }
    }
    let mut cycle = [node("a", &["b"]), node("b", &["a"]), node("c", &["a"])];
    let rounds = simulate(&mut cycle).ok_or("a dependency cycle stalled")?;
    if round_of(&rounds, 2) != 0 {
        return Err(String::from("the cycle's dependent did not stop first"));
    }
    Ok(())
}
