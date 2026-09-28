//! The audited transition gate: only `CAP_SETUID` holders may stamp a
//! task, never toward more privilege, and every attempt reaches the
//! audit ring. Credential transitions (issue #101); the native `creds`
//! syscall is covered by [`syscall_gate`], and the login service builds
//! on the same API for the real console path.

use super::*;
use crate::ipc::credentials::{self, Cred, TransitionError};
use crate::ipc::{acl, audit};
use crate::process::cred_op;

/// Scratch user space for the syscall test: one page for the request
/// block, one for the read-back block.
const SPACE: u64 = 0x0040_0000;

const SPACE_PAGES: u64 = 2;

const CRED: u64 = SPACE;

const OUT: u64 = SPACE + 0x100;

/// Refusals as the syscall returns them in `rax`.
const EPERM: i64 = 1;

const EACCES: i64 = 13;

const EINVAL: i64 = 22;

fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Every transition test starts from the bring-up state: root kernel task,
/// every slot reset, empty policy, empty audit ring, tracing off.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in 0..task::MAX_TASKS {
        credentials::reset_for_task(slot);
    }
    acl::load(&[]);
    audit::reset();
    audit::set_trace(false);
}

/// Run `f` with [`SPACE`] mapped into a fresh address space installed as
/// CR3, exactly as a real `creds` syscall from a user task would find it.
fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    let kernel = mem::kernel_table();
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(table, SPACE, SPACE + SPACE_PAGES * 4096).map_err(to_string)?;
    mem::switch_to(table);
    let outcome = f();
    mem::switch_to(kernel);
    mem::free_user_table(table);
    outcome
}

/// Write a credential block into the installed scratch space.
fn write_cred(va: u64, cred: Cred) {
    let words = cred.to_words();
    // Safety: the scratch pages are mapped writable while installed.
    unsafe { core::ptr::copy_nonoverlapping(words.as_ptr(), va as *mut u64, words.len()) };
}

/// Read a credential block from the installed scratch space.
fn read_cred(va: u64) -> Cred {
    let mut words = [0u64; 5];
    // Safety: the scratch pages are mapped readable while installed.
    unsafe { core::ptr::copy_nonoverlapping(va as *const u64, words.as_mut_ptr(), 5) };
    Cred::from_words(words)
}

/// A task without `CAP_SETUID` cannot stamp another task; with it, the
/// same request reaches the target. Both outcomes are audited.
pub fn transition_requires_cap() -> Result<(), String> {
    fresh();
    let child = task::spawn_child("cred", &service_suite::minimal_elf()).map_err(to_string)?;
    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    let before = audit::count();
    let requested = Cred::new(2000, 200, 0, 0, 7);
    check!(
        credentials::transition(task::current(), child, requested)
            == Err(TransitionError::NotPrivileged),
        "a task without CAP_SETUID stamped another task"
    );
    check!(
        credentials::of(child) == Cred::ROOT,
        "the refused stamp changed the target: {:?}",
        credentials::of(child)
    );
    check!(audit::count() == before + 1, "the refusal was not audited");
    let event = *audit::recent(1)
        .first()
        .ok_or("the refusal left no audit event")?;
    check!(
        !event.allow
            && event.interface_id == credentials::AUDIT_INTERFACE
            && event.reason_code == credentials::reason::TRANSITION_NOT_PRIVILEGED
            && event.txn_id == child as u64
            && event.uid == 1000,
        "the refusal record is wrong: {event:?}"
    );

    // With the capability the same request succeeds and reaches the target.
    credentials::set(
        task::current(),
        Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0),
    );
    check!(
        credentials::transition(task::current(), child, requested) == Ok(requested),
        "a CAP_SETUID holder was refused a legal stamp"
    );
    check!(
        credentials::of(child) == requested,
        "the target did not receive the stamp: {:?}",
        credentials::of(child)
    );
    let event = *audit::recent(1)
        .first()
        .ok_or("the allowed stamp left no audit event")?;
    check!(
        event.allow && event.reason_code == credentials::reason::TRANSITION_ALLOWED,
        "the allowed stamp record is wrong: {event:?}"
    );

    // Reading another task's identity is the same privilege; reading your
    // own is always allowed.
    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    check!(
        credentials::read(task::current(), child) == Err(TransitionError::NotPrivileged),
        "an unprivileged task read another task's identity"
    );
    check!(
        credentials::read(task::current(), task::current()) == Ok(credentials::of(task::current())),
        "a task could not read its own identity"
    );
    Ok(())
}

/// Widening is refused even with `CAP_SETUID`: uid 0 needs a root actor,
/// and capability bits never flow up. Refusals are audited, and a downgrade
/// still works.
pub fn transition_rejects_widening() -> Result<(), String> {
    fresh();
    let actor = Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0);
    credentials::set(task::current(), actor);
    let before = audit::count();
    check!(
        credentials::transition(task::current(), task::current(), Cred::new(0, 0, 0, 0, 3))
            == Err(TransitionError::Widening),
        "a non-root actor minted uid 0"
    );
    check!(
        credentials::transition(
            task::current(),
            task::current(),
            Cred::new(
                2000,
                200,
                credentials::CAP_SETUID | credentials::CAP_NET_RAW,
                0,
                3
            )
        ) == Err(TransitionError::Widening),
        "a transition granted a capability the actor lacks"
    );
    check!(
        credentials::of(task::current()) == actor,
        "a refused widening changed the actor: {:?}",
        credentials::of(task::current())
    );
    check!(
        audit::count() == before + 2,
        "the widening refusals were not audited"
    );
    check!(
        audit::recent(1).first().is_some_and(|event| {
            !event.allow && event.reason_code == credentials::reason::TRANSITION_WIDENING
        }),
        "the last widening refusal is not in the audit ring"
    );

    // Toward less privilege is exactly what the gate is for; keep the
    // capability so the next check reaches the target rule.
    let downgraded = Cred::new(1000, 100, credentials::CAP_SETUID, 4, 9);
    check!(
        credentials::transition(task::current(), task::current(), downgraded) == Ok(downgraded),
        "a legal downgrade was refused"
    );
    check!(
        credentials::of(task::current()) == downgraded,
        "the downgrade did not apply"
    );

    // An unknown target is refused with its own audited reason.
    let before = audit::count();
    check!(
        credentials::transition(task::current(), task::MAX_TASKS, downgraded)
            == Err(TransitionError::BadTarget),
        "an out-of-range target was accepted"
    );
    check!(
        audit::count() == before + 1,
        "the bad-target refusal was not audited"
    );
    check!(
        audit::recent(1).first().is_some_and(|event| {
            !event.allow && event.reason_code == credentials::reason::TRANSITION_BAD_TARGET
        }),
        "the bad-target record is missing"
    );

    // The kernel task may only restamp itself: a service cannot rewrite the
    // multiplexer's identity.
    let child = task::spawn_child("guard", &service_suite::minimal_elf()).map_err(to_string)?;
    task::harness::switch_current(child);
    credentials::set(child, Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0));
    check!(
        credentials::transition(child, task::KERNEL_TASK, Cred::new(1000, 100, 0, 0, 0))
            == Err(TransitionError::BadTarget),
        "a service restamped the kernel task"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    Ok(())
}

/// The native gate: `set`/`get` read and write the 40-byte block, and the
/// refusals arrive as `-errno` (`-EPERM`, `-EACCES`, `-EINVAL`).
pub fn syscall_gate() -> Result<(), String> {
    fresh();
    in_space(|| -> Result<(), String> {
        let me = task::current();

        // Without the capability the stamp is refused with `-EPERM`.
        credentials::set(me, Cred::new(1000, 100, 0, 0, 0));
        write_cred(CRED, Cred::new(2000, 200, 0, 0, 5));
        let code = process::dispatch_for_test(10, cred_op::SET, me as u64, CRED);
        check!(code == failed(EPERM), "set without the cap -> {code:#x}");
        check!(
            credentials::of(me) == Cred::new(1000, 100, 0, 0, 0),
            "the refused set changed the caller"
        );

        // With it, `set` applies and `get` returns the same block.
        credentials::set(me, Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0));
        let stamped = Cred::new(2000, 200, 0, 0, 5);
        let code = process::dispatch_for_test(10, cred_op::SET, u64::MAX, CRED);
        check!(code == 0, "set with the cap -> {code:#x}");
        check!(
            credentials::of(me) == stamped,
            "the syscall stamp did not apply"
        );
        let code = process::dispatch_for_test(10, cred_op::GET, u64::MAX, OUT);
        check!(code == 0, "get with the cap -> {code:#x}");
        check!(
            read_cred(OUT) == stamped,
            "get returned the wrong block: {:?}",
            read_cred(OUT)
        );

        // Widening is `-EACCES` even with the capability.
        credentials::set(me, Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0));
        write_cred(CRED, Cred::new(0, 0, 0, 0, 6));
        let code = process::dispatch_for_test(10, cred_op::SET, u64::MAX, CRED);
        check!(code == failed(EACCES), "widening set -> {code:#x}");

        // An unknown op is `-EINVAL`.
        let code = process::dispatch_for_test(10, 9, 0, 0);
        check!(code == failed(EINVAL), "unknown op -> {code:#x}");
        Ok(())
    })
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "ipc_credentials_transition_requires_cap",
        transition_requires_cap,
    ),
    (
        "ipc_credentials_transition_rejects_widening",
        transition_rejects_widening,
    ),
    ("ipc_credentials_syscall_gate", syscall_gate),
];
