//! Register/resolve round-trip and duplicate handling, the unknown-
//! name error, and lease expiry / owner-death release.

use super::*;
use crate::arch::idt::TICKS;
use core::sync::atomic::Ordering;

/// Register/resolve round-trips a name and, more importantly, duplicates
/// the endpoint into the *resolver's* table: the child gets its own handle
/// to the object the owner published, and an echo call through that handle
/// reaches the owner's side. Owner death then releases the name.
pub fn register_resolve_roundtrip() -> Result<(), String> {
    fresh()?;

    // The child owns the service: it creates the channel, keeps the
    // receiving side and publishes the callable side under the name.
    let child = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(child);
    let (service, callable) = channels::create().map_err(friendly)?;
    let published = handles::get(callable).map_err(friendly)?;
    registry::register(
        child,
        "os.example.echo",
        published.kind,
        published.rights,
        published.object_id,
        &[0xfeed],
        0,
    )
    .map_err(reason)?;

    // The kernel resolves the name; the handle is open in *its* table, not
    // the owner's, and names the same object.
    task::harness::switch_current(task::KERNEL_TASK);
    let resolved = registry::resolve(task::KERNEL_TASK, "os.example.echo").map_err(reason)?;
    check!(
        resolved != callable,
        "resolve handed back the owner's own handle {callable}"
    );
    let copy = handles::get(resolved).map_err(friendly)?;
    check!(
        copy.object_id == published.object_id && copy.kind == published.kind,
        "the resolved handle names a different object"
    );
    check!(
        handles::count_for_task(task::KERNEL_TASK) == 1,
        "the resolver holds {} handles, expected 1",
        handles::count_for_task(task::KERNEL_TASK)
    );

    // Echo through the resolved name: the kernel calls, the owner answers.
    let request = string_parcel(7, "ping")?;
    let txn = channels::begin_call(resolved, 7, &request, None).map_err(friendly)?;
    check!(
        matches!(
            task::harness::state(task::KERNEL_TASK),
            Some(TaskState::Blocked { .. })
        ),
        "begin_call did not park the caller"
    );
    task::harness::switch_current(child);
    let message = channels::recv(service, None).map_err(friendly)?;
    check!(
        message.txn == Some(txn),
        "the request arrived with transaction {:?}",
        message.txn
    );
    check!(
        message.sender == task::KERNEL_TASK,
        "the sender is {}, expected {}",
        message.sender,
        task::KERNEL_TASK
    );
    channels::reply(txn, &request).map_err(friendly)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
    let reply = channels::await_reply(txn).map_err(friendly)?;
    check!(reply == request, "the echo reply changed in flight");

    // Owner death: the slot is gone after the reap, so the name is too.
    task::harness::finish(child, 0);
    check!(task::reap_child().is_some(), "the owner was not reapable");
    check!(
        registry::resolve(task::KERNEL_TASK, "os.example.echo") == Err(RegistryError::UnknownName),
        "the name outlived its owner"
    );
    check!(registry::list().is_empty(), "list kept a dead owner's name");
    Ok(())
}

/// An unknown name reports the dedicated, friendly error rather than a
/// generic lookup failure.
pub fn unknown_name_friendly() -> Result<(), String> {
    fresh()?;
    let error = registry::resolve(task::KERNEL_TASK, "no.such.service").unwrap_err();
    check!(
        error == RegistryError::UnknownName,
        "resolve of an unknown name returned {error:?}"
    );
    check!(
        error.message().contains("no service"),
        "the message is not friendly: {:?}",
        error.message()
    );
    check!(
        registry::stats().entries == 0,
        "a failed resolve left table entries"
    );
    Ok(())
}

/// A lease is a deadline: it survives before its tick and is pruned after
/// it, without any owner action.
pub fn lease_expiry_prunes() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = channels::create().map_err(friendly)?;
    let published = handles::get(callable).map_err(friendly)?;
    let before = task::ticks();
    registry::register(
        task::KERNEL_TASK,
        "os.example.lease",
        published.kind,
        published.rights,
        published.object_id,
        &[],
        2,
    )
    .map_err(reason)?;
    check!(registry::stats().leases == 1, "the lease was not recorded");
    registry::prune_at(before);
    check!(
        registry::list().len() == 1,
        "the lease expired before its deadline"
    );
    check!(
        registry::prune_at(before + 100) == 1,
        "prune did not remove the expired name"
    );
    check!(
        registry::resolve(task::KERNEL_TASK, "os.example.lease") == Err(RegistryError::UnknownName),
        "the expired name still resolved"
    );
    check!(
        registry::stats().expirations == 1,
        "the expiration was not counted"
    );
    Ok(())
}

/// A lease whose deadline would overflow the tick clock (`u64::MAX`) is
/// refused with `-EINVAL` instead of panicking under the dev profile's
/// overflow checks (or wrapping into the past and silently expiring). The
/// check runs before the table is touched, so re-registering an existing
/// name with an impossible lease leaves the old entry in place, and the
/// native syscall surface reports the same `EINVAL`.
pub fn lease_overflow_is_rejected() -> Result<(), String> {
    fresh()?;
    // The test harness runs with timer interrupts off, so the clock sits at
    // tick 0, where even `u64::MAX` fits. Move it forward by hand (restored on
    // every exit path) so the deadline really overflows.
    let saved = TICKS.load(Ordering::Relaxed);
    TICKS.store(saved.max(1), Ordering::Relaxed);
    let result = lease_overflow_body();
    TICKS.store(saved, Ordering::Relaxed);
    result
}

fn lease_overflow_body() -> Result<(), String> {
    let (_service, callable) = channels::create().map_err(friendly)?;
    let published = handles::get(callable).map_err(friendly)?;
    let register = |lease| {
        registry::register(
            task::KERNEL_TASK,
            "os.example.big-lease",
            published.kind,
            published.rights,
            published.object_id,
            &[],
            lease,
        )
    };
    register(0).map_err(reason)?;

    check!(
        register(u64::MAX) == Err(RegistryError::BadLease),
        "a u64::MAX lease was accepted"
    );
    check!(
        registry::resolve(task::KERNEL_TASK, "os.example.big-lease").is_ok(),
        "a refused lease dropped the existing name"
    );

    in_space(|| -> Result<(), String> {
        let (_s, endpoint) = channels::create().map_err(friendly)?;
        let request = register_parcel("os.example.sys.big-lease", endpoint, &[], u64::MAX)?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = dispatch(OP_REGISTER, &args);
        check!(
            code == failed(errno::EINVAL),
            "overflowing lease -> {code:#x}, expected -EINVAL"
        );
        check!(
            result.status == -errno::EINVAL,
            "the overflowing-lease status is {}",
            result.status
        );
        check!(
            registry::resolve(task::KERNEL_TASK, "os.example.sys.big-lease")
                == Err(RegistryError::UnknownName),
            "a refused lease left an entry behind"
        );
        Ok(())
    })
}

/// Owner death releases the name through both paths: the explicit
/// `release_owner` teardown hook, and lazy pruning when a task dies without
/// the hook running.
pub fn owner_death_releases() -> Result<(), String> {
    fresh()?;
    let child = task::spawn_fork().map_err(to_string)?;

    // Path 1: the hook a task teardown calls.
    task::harness::switch_current(child);
    let (_service, callable) = channels::create().map_err(friendly)?;
    let published = handles::get(callable).map_err(friendly)?;
    registry::register(
        child,
        "os.example.hooked",
        published.kind,
        published.rights,
        published.object_id,
        &[1],
        0,
    )
    .map_err(reason)?;
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        registry::list().len() == 1,
        "the child's registration is missing"
    );
    check!(
        registry::release_owner(child) == 1,
        "release_owner did not drop the child's name"
    );
    check!(
        registry::resolve(task::KERNEL_TASK, "os.example.hooked")
            == Err(RegistryError::UnknownName),
        "the released name still resolved"
    );

    // Path 2: the owner dies without teardown; the next access prunes.
    task::harness::switch_current(child);
    let (_service2, callable2) = channels::create().map_err(friendly)?;
    let published2 = handles::get(callable2).map_err(friendly)?;
    registry::register(
        child,
        "os.example.crashed",
        published2.kind,
        published2.rights,
        published2.object_id,
        &[1],
        0,
    )
    .map_err(reason)?;
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(child, 0);
    check!(
        registry::list().is_empty(),
        "prune kept a crashed owner's name"
    );
    check!(task::reap_child().is_some(), "the child was not reapable");
    Ok(())
}
