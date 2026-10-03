//! Soak for development-label spawns (issue #529): thousands of IDE spawns
//! into a `dev:` label, each handed a pipe as its stdout/stderr and reaped,
//! interleaved with refused spawns (no rule, revoked label, widened identity)
//! and approval reloads, leave frames, kernel memory, task slots, pipes, the
//! label table and the label rule budget where they started.

use super::dev::*;
use super::*;
use crate::ipc::acl::{self, ANY_METHOD};
use crate::ipc::{labels, pipe};
use crate::process::spawnv::STDIO_TERMINAL;

const CYCLES: u32 = 3_000;
/// Allocator slack over the soak: slab caches keep partial pages.
const SLACK: usize = 16 * 1024;

/// One spawn the soak expects to be refused, by cycle number.
fn refused_variant(cycle: u32) -> Req {
    let mut request = dev_request(DEV);
    match cycle % 3 {
        0 => request.label = b"dev:org.test.never".to_vec(),
        1 => request.cred = Cred::new(0, UID, 0, 0, SESSION),
        _ => request.cred = Cred::new(UID, UID, credentials::CAP_SETUID, 0, SESSION),
    }
    request
}

/// Spawn into the dev label with a fresh pipe as stdout and stderr, check the
/// child, reap it, and drain the pipe to EOF.
fn allowed_cycle(dev: u32) -> Result<(), String> {
    let (read, write) = make_pipe()?;
    let code = dev_request(DEV).call_stdio([STDIO_TERMINAL, write as u64, write as u64]);
    let slot = spawned(code)?;
    check!(
        credentials::of(slot).label_id == dev,
        "the child was not in the dev label"
    );
    check!(task::fd_close(write), "close the write end");
    reap(slot)?;
    let mut byte = [0u8; 1];
    check!(
        matches!(task::fd_stream_read(read, &mut byte), Ok(0)),
        "no EOF after the child was reaped"
    );
    check!(task::fd_close(read), "close the read end");
    Ok(())
}

/// The soak itself; see the module docs.
pub fn soak_dev_label_cycles() -> Result<(), String> {
    fresh_labels()?;
    let dev = approve(DEV)?;
    become_ide(Some(ANY_METHOD))?;
    let outcome = (|| {
        let labels_before = labels::count();
        let rules_before = acl::label_rules_total();
        let pipes_before = pipe::Pipe::live();
        // One warm-up cycle so lazily created caches are not counted as leaks.
        allowed_cycle(dev)?;
        let before = usage();
        for cycle in 0..CYCLES {
            allowed_cycle(dev).map_err(|error| format!("cycle {cycle}: {error}"))?;
            if cycle % 4 == 0 {
                let code = refused_variant(cycle).call();
                check!(
                    code == failed(EACCES),
                    "cycle {cycle}: a bad spawn returned {code:#x}"
                );
            }
            if cycle % 64 == 0 {
                // pkgd re-approving (or revoking then approving) the label
                // swaps its rules; neither leaks rule budget.
                acl::load_label(dev, &[]).map_err(|e| format!("{e:?}"))?;
                let code = dev_request(DEV).call();
                check!(
                    code == failed(EACCES),
                    "cycle {cycle}: a revoked label was entered ({code:#x})"
                );
                approve(DEV)?;
            }
        }
        no_leak(before, usage(), SLACK, "dev-label soak")?;
        check!(
            labels::count() == labels_before,
            "the label table grew from {labels_before} to {}",
            labels::count()
        );
        check!(
            acl::label_rules_total() == rules_before,
            "label rules grew from {rules_before} to {}",
            acl::label_rules_total()
        );
        check!(
            pipe::Pipe::live() == pipes_before,
            "pipes leaked: {pipes_before} -> {}",
            pipe::Pipe::live()
        );
        Ok(())
    })();
    credentials::reset_for_task(task::current());
    outcome
}
