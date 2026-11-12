//! `OP_WAIT_FOR` through the native gate: the errno of every way out, the
//! refused non-self target, and a labelled task's name policy.

use super::*;

/// One `OP_WAIT_FOR` for `name` with `deadline` (and `target` in `txn_id`
/// unless it is the caller's own).
fn gate(name: &str, deadline: u64, target: Option<u64>) -> Result<(u64, MsgResult), String> {
    let request = string_parcel(registry::method::WAIT_FOR, name)?;
    write_bytes(REQUEST, &request);
    let args = MsgArgs {
        parcel_ptr: REQUEST,
        parcel_len: request.len() as u64,
        deadline,
        txn_id: target.unwrap_or(crate::ipc::syscalls::REGISTRY_TARGET_SELF),
        ..MsgArgs::default()
    };
    Ok(dispatch(OP_WAIT_FOR, &args))
}

/// The errno each outcome maps to: `ETIMEDOUT` (an expired deadline),
/// `EAGAIN` (the task already holds its one place), `EINTR` (a deferred
/// handler frame), `EINVAL` (a target that is not the caller), `ENOENT`
/// never (a wait is not a lookup), and the handle on success.
pub fn gate_errnos() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    let child = task::spawn_fork().map_err(to_string)?;
    in_space(|| -> Result<(), String> {
        let (code, _) = gate("os.example.none", 1, None)?;
        check!(code == failed(errno::ETIMEDOUT), "expired -> {code:#x}");
        let held = Ticket::take(task::KERNEL_TASK, "os.example.held").map_err(reason)?;
        let (code, result) = gate("os.example.none", 1, None)?;
        check!(
            code == failed(errno::EAGAIN) && result.status == -errno::EAGAIN,
            "a second wait of the task -> {code:#x}"
        );
        drop(held);
        // Not the caller's own slot: refused before any wait, with the
        // administrator's capability too.
        let (code, _) = gate("os.example.none", 1, Some(7))?;
        check!(code == failed(errno::EINVAL), "another target -> {code:#x}");
        nothing_waits()?;
        // A signal frame the sweep deferred: EINTR, no park.
        task::harness::switch_current(child);
        task::signal::harness::set_frame_deferred(child, true);
        let (code, _) = gate("os.example.none", 0, None)?;
        task::signal::harness::set_frame_deferred(child, false);
        task::harness::switch_current(task::KERNEL_TASK);
        check!(code == failed(errno::EINTR), "deferred frame -> {code:#x}");
        task::harness::finish(child, 0);
        // Success: the handle is in `value`.
        later(PLAN_NONE, 0, 0, callable)?;
        no_later();
        publish(NAMES[0]);
        let (code, result) = gate(NAMES[0], 0, None)?;
        check!(code == 0, "registered -> {code:#x}");
        handles::get(result.value).map_err(friendly)?;
        Ok(())
    })?;
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}

/// A labelled task: a name its label may not resolve is refused at once with
/// no ticket taken and no park, a name in its own namespace is waited for and
/// found, exactly as `Resolve` judges them.
pub fn labelled_gate() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    later(PLAN_NONE, 0, 0, callable)?;
    no_later();
    let label = crate::ipc::labels::intern("app:com.waitfor").map_err(|_| "label")?;
    // A label id can carry rules an earlier suite left: start with none.
    acl::load_label(label, &[]).map_err(|_| "label rules")?;
    credentials::set(
        task::KERNEL_TASK,
        credentials::Cred::new(1000, 1000, 0, label, 0),
    );
    let own = "app.com.waitfor.svc";
    registry::register(
        task::KERNEL_TASK,
        "os.lazy.timed",
        HandleKind::Channel,
        0x7,
        OBJECT.load(Ordering::Acquire),
        &[1],
        0,
    )
    .map_err(reason)?;
    publish(own);
    in_space(|| -> Result<(), String> {
        let start = task::ticks();
        // Registered, but not the label's to resolve: refused like `Resolve`.
        let (code, _) = gate("os.lazy.timed", 0, None)?;
        check!(code == failed(errno::EACCES), "denied name -> {code:#x}");
        check!(
            task::ticks() <= start + 1 && registry::stats().waiters == 0,
            "a refused wait took a place or time"
        );
        let (code, result) = gate(own, 0, None)?;
        check!(code == 0, "own namespace -> {code:#x}");
        handles::get(result.value).map_err(friendly)?;
        Ok(())
    })?;
    credentials::reset_for_task(task::KERNEL_TASK);
    crate::ipc::labels::reset_for_tests();
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}
