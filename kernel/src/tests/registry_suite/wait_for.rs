//! `WaitFor`: a resolve that parks until the name registers (`ipc/registry/wait.rs`).
//!
//! The suite has one task, so the registrant is the nap hook
//! (`task::harness::set_nap_hook`): it runs each time the CPU wakes from the
//! halt the waiter sleeps in, and does what a peer would (register, interrupt).

use super::*;
use crate::ipc::registry::wait::{self, Ticket};
use crate::ipc::syscalls::OP_WAIT_FOR;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

mod gate;
mod limits;
mod signals;
mod soak;
mod stress;

/// The names the hook publishes, indexed by [`NAME`].
const NAMES: [&str; 8] = [
    "os.example.w0",
    "os.example.w1",
    "os.example.w2",
    "os.example.w3",
    "os.example.w4",
    "os.example.w5",
    "os.example.w6",
    "os.example.w7",
];
const OTHER: &str = "os.example.other";

const PLAN_NONE: u8 = 0;
/// Publish `NAMES[NAME]` at the first nap at or after [`AT`].
const PLAN_REGISTER: u8 = 1;
/// Publish [`OTHER`], then `NAMES[NAME]` three ticks later.
const PLAN_OTHER_FIRST: u8 = 2;
/// Interrupt the waiter (a signal that is not fatal), then publish.
const PLAN_INTERRUPT_FIRST: u8 = 3;
/// Withdraw the caller's permission, then publish.
const PLAN_REVOKE: u8 = 4;

static PLAN: AtomicU8 = AtomicU8::new(PLAN_NONE);
static STEP: AtomicU8 = AtomicU8::new(0);
static NAME: AtomicU8 = AtomicU8::new(0);
static AT: AtomicU64 = AtomicU64::new(0);
/// The tick [`OTHER`] was published at (0 until it was).
static OTHER_AT: AtomicU64 = AtomicU64::new(0);
/// The object and rights the hook publishes.
static OBJECT: AtomicU64 = AtomicU64::new(0);
static RIGHTS: AtomicU64 = AtomicU64::new(0);
/// The waiter's name policy, as its `allowed` closure reads it.
static PERMITTED: AtomicBool = AtomicBool::new(true);

fn publish(name: &str) {
    let object = OBJECT.load(Ordering::Acquire);
    let rights = RIGHTS.load(Ordering::Acquire) as u32;
    let _ = registry::register(
        task::KERNEL_TASK,
        name,
        HandleKind::Channel,
        rights,
        object,
        &[1],
        0,
    );
}

/// The peer: acts once its plan's tick has come.
fn act() {
    let now = task::ticks();
    if now < AT.load(Ordering::Acquire) {
        return;
    }
    let name = NAMES[NAME.load(Ordering::Acquire) as usize % NAMES.len()];
    match (PLAN.load(Ordering::Acquire), STEP.load(Ordering::Acquire)) {
        (PLAN_REGISTER, 0) | (PLAN_REVOKE, 0) => {
            if PLAN.load(Ordering::Acquire) == PLAN_REVOKE {
                PERMITTED.store(false, Ordering::Release);
            }
            STEP.store(1, Ordering::Release);
            publish(name);
        }
        (PLAN_OTHER_FIRST, 0) => {
            STEP.store(1, Ordering::Release);
            OTHER_AT.store(now.max(1), Ordering::Release);
            AT.store(now + 3, Ordering::Release);
            publish(OTHER);
        }
        (PLAN_INTERRUPT_FIRST, 0) => {
            STEP.store(1, Ordering::Release);
            AT.store(now + 2, Ordering::Release);
            task::harness::interrupt(task::harness::nap_hook_owner());
        }
        (PLAN_OTHER_FIRST, 1) | (PLAN_INTERRUPT_FIRST, 1) => {
            STEP.store(2, Ordering::Release);
            publish(name);
        }
        _ => {}
    }
}

/// Arrange for the peer to follow `plan` for `NAMES[name]` after `after` ticks.
fn later(plan: u8, name: usize, after: u64, endpoint: u64) -> Result<(), String> {
    let entry = handles::get(endpoint).map_err(friendly)?;
    OBJECT.store(entry.object_id, Ordering::Release);
    RIGHTS.store(u64::from(entry.rights), Ordering::Release);
    NAME.store(name as u8, Ordering::Release);
    OTHER_AT.store(0, Ordering::Release);
    PERMITTED.store(true, Ordering::Release);
    STEP.store(0, Ordering::Release);
    AT.store(task::ticks() + after, Ordering::Release);
    PLAN.store(plan, Ordering::Release);
    task::harness::set_nap_hook(Some(act));
    Ok(())
}

fn no_later() {
    PLAN.store(PLAN_NONE, Ordering::Release);
    task::harness::set_nap_hook(None);
}

/// `WaitFor` as the kernel task, with every waiter structure checked empty
/// afterwards by the caller.
fn wait_until(name: &str, ticks: u64) -> Result<u64, RegistryError> {
    let limit = Some(task::ticks() + ticks);
    wait::wait_for(task::KERNEL_TASK, name, limit, || {
        PERMITTED.load(Ordering::Acquire)
    })
}

/// Nothing is left parked, ticketed or opened for the kernel task.
fn nothing_waits() -> Result<(), String> {
    check!(
        registry::stats().waiters == 0,
        "{} waiter tickets leaked",
        registry::stats().waiters
    );
    check!(
        wait::queue_len() == 0,
        "{} tasks left on the wait queue",
        wait::queue_len()
    );
    Ok(())
}

/// One published endpoint: its pair and the callable side.
fn endpoint() -> Result<(u64, u64), String> {
    channels::create().map_err(friendly)
}

/// A name that is already registered answers at once, without a ticket
/// outliving the call and without parking.
pub fn wait_registered_answers_at_once() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    later(PLAN_NONE, 0, 0, callable)?;
    no_later();
    publish(NAMES[0]);
    let start = task::ticks();
    let handle = wait_until(NAMES[0], 100).map_err(reason)?;
    check!(
        task::ticks() <= start + 1,
        "an existing name waited {} ticks",
        task::ticks() - start
    );
    let entry = handles::get(handle).map_err(friendly)?;
    check!(
        entry.object_id == OBJECT.load(Ordering::Acquire),
        "the handle names another object: {entry:?}"
    );
    nothing_waits()?;
    // Through the gate: a registered name, no deadline, the handle in `value`.
    in_space(|| -> Result<(), String> {
        let request = string_parcel(registry::method::WAIT_FOR, NAMES[0])?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_WAIT_FOR, &args);
        check!(code == 0, "OP_WAIT_FOR of a registered name -> {code:#x}");
        let entry = handles::get(result.value).map_err(friendly)?;
        check!(
            entry.object_id == OBJECT.load(Ordering::Acquire),
            "the gate's handle names another object"
        );
        Ok(())
    })?;
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}

/// A name registered later wakes the waiter with a usable handle, long before
/// its deadline.
pub fn wait_then_register_wakes() -> Result<(), String> {
    fresh()?;
    let (service, callable) = endpoint()?;
    later(PLAN_REGISTER, 1, 3, callable)?;
    let start = task::ticks();
    let outcome = wait_until(NAMES[1], 500);
    no_later();
    let handle = outcome.map_err(reason)?;
    let waited = task::ticks() - start;
    check!(
        (3..100).contains(&waited),
        "woke after {waited} ticks, expected about 3"
    );
    // The handle works: a message through it reaches the service side.
    let hello = string_parcel(7, "hello")?;
    channels::send(handle, &hello).map_err(friendly)?;
    let got = channels::try_recv(service).map_err(friendly)?;
    check!(got.is_some(), "the resolved handle reached nobody");
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}

/// A registration of another name does not end the wait; a signal that is
/// not fatal ends it with `EINTR`, and the re-issued wait succeeds.
pub fn wait_ignores_other_names_and_signals() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    for plan in [PLAN_OTHER_FIRST, PLAN_INTERRUPT_FIRST] {
        later(plan, 2, 1, callable)?;
        let mut outcome = wait_until(NAMES[2], 500);
        if plan == PLAN_INTERRUPT_FIRST {
            // A signal ends the wait with EINTR (the syscall must return for
            // the handler frame); the client re-issues it.
            check!(
                outcome == Err(RegistryError::Interrupted),
                "a signal gave {outcome:?}"
            );
            outcome = wait_until(NAMES[2], 500);
        }
        no_later();
        let handle = outcome.map_err(reason)?;
        let other = OTHER_AT.load(Ordering::Acquire);
        if plan == PLAN_OTHER_FIRST {
            check!(other != 0, "the other name was never published");
            check!(
                task::ticks() >= other + 3,
                "the wait ended at {} on the other name's registration at {other}",
                task::ticks()
            );
        }
        channels::close_endpoint(handle).map_err(friendly)?;
        registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, NAMES[2]).map_err(reason)?;
        registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, OTHER).ok();
    }
    // Which slots a registration wakes is the name's alone.
    let ticket = Ticket::take(task::KERNEL_TASK, NAMES[3]).map_err(reason)?;
    check!(
        wait::waiting_slots(NAMES[3]) == [task::KERNEL_TASK]
            && wait::waiting_slots(NAMES[4]).is_empty(),
        "the table woke the wrong slots"
    );
    drop(ticket);
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}

/// A deadline ends the wait with the timeout, an already-expired one at once,
/// and the syscall reports `ETIMEDOUT`; the ticket is released each time.
pub fn wait_deadline_times_out() -> Result<(), String> {
    fresh()?;
    no_later();
    let start = task::ticks();
    check!(
        wait_until("os.example.never", 4) == Err(RegistryError::TimedOut),
        "a missing name did not time out"
    );
    let waited = task::ticks() - start;
    check!(
        (4..60).contains(&waited),
        "timed out after {waited} ticks, expected 4"
    );
    nothing_waits()?;
    let start = task::ticks();
    check!(
        wait::wait_for(task::KERNEL_TASK, "os.example.never", Some(1), || true)
            == Err(RegistryError::TimedOut)
            && task::ticks() <= start + 1,
        "an expired deadline parked"
    );
    nothing_waits()?;
    in_space(|| -> Result<(), String> {
        let request = string_parcel(registry::method::WAIT_FOR, "os.example.never")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            // Already expired: the suite's scratch address space would not
            // survive a park (the idle context switches it away).
            deadline: 1,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_WAIT_FOR, &args);
        check!(
            code == failed(errno::ETIMEDOUT) && result.status == -errno::ETIMEDOUT,
            "OP_WAIT_FOR past its deadline -> {code:#x}"
        );
        Ok(())
    })?;
    nothing_waits()
}

/// The name policy is applied when the wait starts and again when the name
/// appears: a refused name never parks, and a name the caller lost the right
/// to between the two stays refused, with no handle opened.
pub fn wait_policy_rechecked() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    let base = handles::count_for_task(task::KERNEL_TASK);
    // At the start: a labelled task without rules may not resolve a system
    // name, so its wait is refused at once (and learns nothing about it).
    let label = crate::ipc::labels::intern("app:com.example").map_err(|_| "label")?;
    credentials::set(
        task::KERNEL_TASK,
        credentials::Cred::new(1000, 1000, 0, label, 0),
    );
    in_space(|| -> Result<(), String> {
        let request = string_parcel(registry::method::WAIT_FOR, "os.lazy.timed")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            deadline: task::ticks() + 200,
            ..MsgArgs::default()
        };
        let start = task::ticks();
        let (code, _) = dispatch(OP_WAIT_FOR, &args);
        check!(code == failed(errno::EACCES), "refused name -> {code:#x}");
        check!(
            task::ticks() <= start + 1,
            "a refused wait parked for {} ticks",
            task::ticks() - start
        );
        Ok(())
    })?;
    credentials::reset_for_task(task::KERNEL_TASK);
    crate::ipc::labels::reset_for_tests();
    nothing_waits()?;
    // As the name appears: the permission is gone by then.
    later(PLAN_REVOKE, 5, 2, callable)?;
    let outcome = wait_until(NAMES[5], 500);
    no_later();
    check!(
        outcome == Err(RegistryError::Denied),
        "a revoked waiter got {outcome:?}"
    );
    check!(
        handles::count_for_task(task::KERNEL_TASK) == base,
        "a refused wait opened a handle ({} held)",
        handles::count_for_task(task::KERNEL_TASK)
    );
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}

/// A name withdrawn and registered again is waited for like a new one.
pub fn wait_unregister_reregister() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    later(PLAN_NONE, 6, 0, callable)?;
    no_later();
    publish(NAMES[6]);
    registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, NAMES[6]).map_err(reason)?;
    check!(
        wait_until(NAMES[6], 3) == Err(RegistryError::TimedOut),
        "a withdrawn name still answered"
    );
    later(PLAN_REGISTER, 6, 2, callable)?;
    let outcome = wait_until(NAMES[6], 500);
    no_later();
    outcome.map_err(reason)?;
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}

pub(crate) const WAIT_CASES: &[(&str, Test)] = &[
    (
        "ipc_registry_wait_registered_answers_at_once",
        wait_registered_answers_at_once,
    ),
    (
        "ipc_registry_wait_then_register_wakes",
        wait_then_register_wakes,
    ),
    (
        "ipc_registry_wait_ignores_other_names_and_signals",
        wait_ignores_other_names_and_signals,
    ),
    (
        "ipc_registry_wait_deadline_times_out",
        wait_deadline_times_out,
    ),
    ("ipc_registry_wait_policy_rechecked", wait_policy_rechecked),
    (
        "ipc_registry_wait_unregister_reregister",
        wait_unregister_reregister,
    ),
    (
        "ipc_registry_wait_limits_and_release",
        limits::wait_limits_and_release,
    ),
    (
        "ipc_registry_wait_killed_waiter_is_canceled",
        signals::killed_waiter_is_canceled,
    ),
    (
        "ipc_registry_wait_deferred_frame_returns_eintr",
        signals::deferred_frame_returns_eintr,
    ),
    (
        "ipc_registry_wait_teardown_then_slot_reuse",
        signals::teardown_then_slot_reuse,
    ),
    ("ipc_registry_wait_gate_errnos", gate::gate_errnos),
    ("ipc_registry_wait_labelled_gate", gate::labelled_gate),
    (
        "ipc_registry_wait_parked_herd_soak",
        stress::parked_herd_soak,
    ),
    ("ipc_registry_wait_soak", soak::wait_soak),
    (
        "ipc_registry_wait_table_churn_soak",
        soak::wait_table_churn_soak,
    ),
];
