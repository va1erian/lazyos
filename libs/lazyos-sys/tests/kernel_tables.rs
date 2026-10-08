//! The numbers in `lazyos-sys` against the kernel sources they mirror: the
//! native dispatch table (`kernel/src/process/gate.rs`), the Messenger op
//! table (`kernel/src/ipc/syscalls/abi.rs`) and the wait flags
//! (`kernel/src/ipc/channels/recv/waitset.rs`). A renumbering on either side
//! fails here instead of at runtime.

use std::collections::BTreeMap;
use std::path::PathBuf;

use lazyos_sys::{msg, nr};

fn kernel(path: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/src");
    std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("reading {path}: {e}"))
}

/// The arms of `native_dispatch`'s `match regs.rax`: number -> the handler
/// text up to the next arm (comments dropped).
fn native_arms() -> BTreeMap<u64, String> {
    let source = kernel("process/gate.rs");
    let start = source
        .find("regs.rax = match regs.rax {")
        .expect("native_dispatch's match");
    let end = start
        + source[start..]
            .find("_ => u64::MAX")
            .expect("the default arm");
    let mut arms: BTreeMap<u64, String> = BTreeMap::new();
    let mut current: Vec<u64> = Vec::new();
    for line in source[start..end].lines().skip(1) {
        let line = line.trim();
        if line.starts_with("//") || line.is_empty() {
            continue;
        }
        if let Some((pattern, handler)) = line.split_once("=>") {
            let numbers = parse_pattern(pattern.trim());
            if !numbers.is_empty() {
                current = numbers;
                for number in &current {
                    arms.insert(*number, handler.trim().to_string());
                }
                continue;
            }
        }
        for number in &current {
            arms.get_mut(number).unwrap().push_str(line);
        }
    }
    arms
}

/// `7`, `15..=22` or `28 | 30 | 32`; empty when `pattern` is none of those.
fn parse_pattern(pattern: &str) -> Vec<u64> {
    if let Some((low, high)) = pattern.split_once("..=") {
        return match (low.trim().parse::<u64>(), high.trim().parse::<u64>()) {
            (Ok(low), Ok(high)) => (low..=high).collect(),
            _ => Vec::new(),
        };
    }
    pattern
        .split('|')
        .map(|part| part.trim().parse::<u64>())
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default()
}

#[test]
fn every_syscall_number_reaches_its_kernel_handler() {
    let arms = native_arms();
    assert!(arms.len() > 30, "parsed only {} arms", arms.len());
    for &(number, handler) in nr::ALL {
        if number == nr::EXIT {
            // `exit` is handled before the match.
            assert!(kernel("process/gate.rs").contains("regs.rax == 0 {\n        exit("));
            continue;
        }
        let arm = arms
            .get(&number)
            .unwrap_or_else(|| panic!("the kernel does not dispatch syscall {number}"));
        assert!(
            arm.contains(handler),
            "syscall {number} goes to `{arm}`, expected `{handler}`"
        );
    }
}

#[test]
fn every_dispatched_number_has_a_name() {
    for number in native_arms().keys() {
        assert!(
            nr::ALL.iter().any(|&(known, _)| known == *number),
            "the kernel dispatches syscall {number}, which lazyos_sys::nr does not name"
        );
    }
}

/// `pub const NAME: TYPE = VALUE;` lines of `source`, by name.
fn constants(source: &str) -> BTreeMap<String, String> {
    source
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("pub const ")?;
            let (name, rest) = rest.split_once(':')?;
            let (_, value) = rest.split_once('=')?;
            Some((
                name.trim().to_string(),
                value.trim().trim_end_matches(';').to_string(),
            ))
        })
        .collect()
}

fn value_of(text: &str) -> u64 {
    let text = text.trim();
    if text == "u64::MAX" {
        return u64::MAX;
    }
    if let Some((base, shift)) = text.split_once("<<") {
        return value_of(base) << value_of(shift);
    }
    text.parse()
        .unwrap_or_else(|_| panic!("unparsed constant `{text}`"))
}

#[test]
fn the_messenger_ops_match_the_kernel_abi() {
    let table = constants(&kernel("ipc/syscalls/abi.rs"));
    let ours = [
        ("OP_CALL", msg::op::CALL),
        ("OP_REPLY", msg::op::REPLY),
        ("OP_SEND", msg::op::SEND),
        ("OP_RECV", msg::op::RECV),
        ("OP_CANCEL", msg::op::CANCEL),
        ("OP_CLOSE_ENDPOINT", msg::op::CLOSE_ENDPOINT),
        ("OP_CREATE_PAIR", msg::op::CREATE_PAIR),
        ("OP_STATS", msg::op::STATS),
        ("OP_BOOTSTRAP", msg::op::BOOTSTRAP),
        ("OP_CALL_BEGIN", msg::op::CALL_BEGIN),
        ("OP_CALL_AWAIT", msg::op::CALL_AWAIT),
        ("OP_TOTALS", msg::op::TOTALS),
        ("OP_REGISTER", msg::op::REGISTER),
        ("OP_RESOLVE", msg::op::RESOLVE),
        ("OP_UNREGISTER", msg::op::UNREGISTER),
        ("OP_LIST", msg::op::LIST),
        ("OP_AUTHORIZE_TOPIC", msg::op::AUTHORIZE_TOPIC),
        ("OP_ACL_LOAD", msg::op::ACL_LOAD),
        ("OP_WAIT", msg::op::WAIT),
        ("OP_CONNECT", msg::op::CONNECT),
        ("CLOSE_RELEASE", msg::op::CLOSE_RELEASE),
        ("RECV_SENDER_ID", msg::op::RECV_SENDER_ID),
        ("REGISTRY_TARGET_SELF", msg::REGISTRY_TARGET_SELF),
    ];
    for (name, value) in ours {
        let theirs = table
            .get(name)
            .unwrap_or_else(|| panic!("kernel has no {name}"));
        assert_eq!(value_of(theirs), value, "{name}");
    }
    let op_count = table.keys().filter(|name| name.starts_with("OP_")).count();
    assert_eq!(op_count, 20, "a new kernel op needs a name in msg::op");
}

#[test]
fn the_wait_flags_match_the_kernel() {
    let table = constants(&kernel("ipc/channels/recv/waitset.rs"));
    let ours = [
        ("MAX_WAIT_ENDPOINTS", msg::WAIT_MAX_ENDPOINTS as u64),
        ("WAIT_RAW_INPUT", msg::WAIT_RAW_INPUT),
        ("WAIT_DISPLAY_KEYS", msg::WAIT_DISPLAY_KEYS),
        ("WAIT_INET", msg::WAIT_INET),
        ("WAIT_CHILD", msg::WAIT_CHILD),
        ("WAIT_FD", msg::WAIT_FD),
        ("WAIT_DEADLINE_NS", msg::WAIT_DEADLINE_NS),
        ("WAIT_FD_SHIFT", u64::from(msg::WAIT_FD_SHIFT)),
        ("RAW_INPUT_READY", msg::RAW_INPUT_READY),
        ("DISPLAY_INPUT_READY", msg::DISPLAY_INPUT_READY),
        ("INET_READY", msg::INET_READY),
        ("CHILD_READY", msg::CHILD_READY),
        ("FD_READY", msg::FD_READY),
    ];
    for (name, value) in ours {
        let theirs = table
            .get(name)
            .unwrap_or_else(|| panic!("kernel has no {name}"));
        assert_eq!(value_of(theirs), value, "{name}");
    }
    let call = constants(&kernel("ipc/channels/recv/waitcall.rs"));
    assert_eq!(value_of(&call["WAIT_ITEM_CALL"]), msg::WAIT_ITEM_CALL);
}
