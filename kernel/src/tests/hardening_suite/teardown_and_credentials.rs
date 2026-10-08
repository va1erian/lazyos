//! A refused display bind leaves no state, bind requires the
//! capability, children inherit credentials, and task teardown
//! releases fabric/quota state (including soaks).

use super::*;
use crate::tests::bufops;

/// A refused display request leaves no state behind and a bad event
/// buffer does not eat input.
pub fn display_bad_pointers_leave_no_state() -> Result<(), String> {
    fresh()?;
    let slot = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(slot);
    let buffers_before = shared::stats().buffers;

    {
        let _strict = Strict::on();
        let mut info = canary(crate::display::INFO_WORDS * 8);
        let code =
            process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
        check!(
            code == failed(EFAULT),
            "bind into kernel memory -> {code:#x}"
        );
        untouched(&info, "bind")?;
        check!(
            !crate::display::bound(),
            "a refused bind left the display bound"
        );
        check!(
            shared::stats().buffers == buffers_before,
            "a refused bind leaked the screen buffer"
        );
    }
    // A buffer op whose result block is kernel memory is refused before
    // anything is created (the gate validates the block first).
    bufops::in_space(|| {
        let mut out = canary(64);
        let code = bufops::create_into_kernel(4096, out.as_mut_ptr() as u64);
        check!(
            code == failed(EFAULT),
            "buffer_create into kernel memory -> {code:#x}"
        );
        untouched(&out, "buffer_create")?;
        check!(
            shared::stats().buffers == buffers_before,
            "a refused buffer_create leaked the buffer"
        );
        Ok(())
    })?;

    // Bind properly (kernel buffers are trusted outside the guard).
    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    {
        let _strict = Strict::on();
        let mut events = canary(16);
        let code = process::dispatch_for_test(
            12,
            crate::display::op::INPUT_POLL,
            events.as_mut_ptr() as u64,
            16,
        );
        check!(
            code == failed(EFAULT),
            "input_poll into kernel -> {code:#x}"
        );
        untouched(&events, "input_poll")?;
    }
    // The seeded pointer event survived the refused poll.
    let mut drain = [0u8; 16];
    let count = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        drain.as_mut_ptr() as u64,
        16,
    );
    check!(count == 1, "the refused poll ate the queued event: {count}");
    let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    crate::display::reset();
    shared::reset();
    Ok(())
}

/// Binding the display hands over every pixel and keystroke, so it needs
/// `CAP_SYS_ADMIN`; the `messenger` buffer ops stay open to every task.
pub fn display_bind_requires_capability() -> Result<(), String> {
    fresh()?;
    let slot = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(slot);
    credentials::set(slot, alice());

    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(
        code == failed(EPERM),
        "an unprivileged bind -> {code:#x} (expected -EPERM)"
    );
    check!(
        !crate::display::bound(),
        "an unprivileged task now owns the display"
    );

    bufops::in_space(|| {
        let (handle, _) = bufops::create(4096)?;
        check!(bufops::close(handle) == 0, "buffer_close failed");
        Ok(())
    })?;

    credentials::set(
        slot,
        Cred::new(1000, 1000, credentials::CAP_SYS_ADMIN, 0, 7),
    );
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "a CAP_SYS_ADMIN bind -> {code:#x}");
    check!(crate::display::bound(), "the privileged bind did not bind");
    process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    crate::display::reset();
    shared::reset();
    Ok(())
}

/// A child never holds more privilege than its creator, whichever path
/// made it, and a kernel-started program never inherits a dead task's
/// stale identity.
pub fn children_inherit_credentials() -> Result<(), String> {
    fresh()?;
    let parent = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(parent);
    credentials::set(parent, alice());

    let elf = service_suite::minimal_elf();
    let native = task::spawn_child("kid", &elf).map_err(to_string)?;
    check!(
        credentials::of(native) == alice(),
        "a native child of an unprivileged task holds {:?}",
        credentials::of(native)
    );
    let forked = task::spawn_fork().map_err(to_string)?;
    check!(
        credentials::of(forked) == alice(),
        "a forked child of an unprivileged task holds {:?}",
        credentials::of(forked)
    );

    // A slot freed by a dead root task must not hand its identity to the
    // next child of an unprivileged parent, nor a dead user's identity to a
    // program the kernel starts.
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::set(native, Cred::ROOT);
    credentials::set(forked, alice());
    task::harness::finish(native, 0);
    task::harness::finish(forked, 0);
    task::harness::switch_current(parent);
    task::harness::finish(parent, 0);
    task::harness::switch_current(task::KERNEL_TASK);
    while task::reap_child().is_some() {}
    credentials::set(native, Cred::ROOT);
    let started = task::spawn("boot", &elf).map_err(to_string)?;
    credentials::set(started, alice());
    task::harness::finish(started, 0);
    while task::reap_child().is_some() {}
    let again = task::spawn("boot2", &elf).map_err(to_string)?;
    check!(
        credentials::of(again) == Cred::ROOT,
        "a kernel-started program inherited a stale identity: {:?}",
        credentials::of(again)
    );
    Ok(())
}

/// Reaping a task tears down its Messenger handles, channels and buffer
/// mappings while its address space still exists, so a peer sees
/// `PeerDied`, per-uid charges return, and the slot's next tenant starts
/// with an empty table.
pub fn teardown_releases_fabric_state() -> Result<(), String> {
    fresh()?;
    let frames_before = mem::frame_stats().live();
    let fabric_before = crate::ipc::stats::snapshot();
    let handles_before = quota::usage(0, Resource::Handles);
    let kernel_mem_before = quota::usage(0, Resource::KernelMemory);
    let kernel_table = mem::kernel_table();

    let child = task::spawn_fork().map_err(to_string)?;
    let child_table =
        PhysAddr::new(task::harness::pml4(child).ok_or("the child has no address space")?);

    // As the child: a channel pair and a mapped shared buffer, created
    // inside the child's own address space.
    task::harness::switch_current(child);
    mem::switch_to(child_table);
    let (a, _b) = channels::create().map_err(|e| e.message().to_string())?;
    let buffer = shared::create(2 * 4096, shared::flags::READ | shared::flags::WRITE)
        .map_err(|e| e.message().to_string())?;
    shared::map(buffer).map_err(|e| e.message().to_string())?;
    mem::switch_to(kernel_table);
    task::harness::switch_current(task::KERNEL_TASK);

    // The kernel task holds a second handle to side `a`, exactly as a
    // registry resolve gives a client one, and can talk to the child.
    let object = handles::get_for_task(child, a)
        .map_err(|e| e.message().to_string())?
        .object_id;
    let mine = handles::open_for_task(
        task::KERNEL_TASK,
        handles::HandleKind::Channel,
        handles::rights::ALL,
        object,
    )
    .map_err(|e| e.message().to_string())?;
    let hello = {
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::ONE_WAY,
                interface_id: 0x77,
                method: 1,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: Vec::new(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).map_err(|e| e.message())?;
        bytes
    };
    check!(
        channels::send(mine, &hello).is_ok(),
        "the live peer refused a message"
    );

    task::harness::finish(child, 0);
    let reaped = task::reap_child().ok_or("the finished child was not reaped")?;
    check!(reaped.0 == child, "reaped slot {} not {child}", reaped.0);

    check!(
        handles::count_for_task(child) == 0,
        "the reaped slot still holds {} handles",
        handles::count_for_task(child)
    );
    check!(
        channels::send(mine, &hello) == Err(channels::Error::PeerDied),
        "a peer that died never reported PeerDied"
    );
    let fabric = crate::ipc::stats::snapshot();
    check!(
        fabric.buffers == fabric_before.buffers
            && fabric.buffer_mappings == fabric_before.buffer_mappings,
        "buffers {} -> {}, mappings {} -> {}",
        fabric_before.buffers,
        fabric.buffers,
        fabric_before.buffer_mappings,
        fabric.buffer_mappings
    );
    check!(
        quota::usage(0, Resource::KernelMemory) == kernel_mem_before,
        "the child's buffer charge was not returned"
    );
    // The slot's next tenant starts empty.
    let next = task::spawn_fork().map_err(to_string)?;
    check!(
        next == child && handles::get_for_task(next, a).is_err(),
        "slot {next} inherited the dead task's handles"
    );

    // Drop the kernel's own handle and every remaining task.
    handles::close(mine).map_err(|e| e.message().to_string())?;
    task::harness::finish(next, 0);
    while task::reap_child().is_some() {}
    check!(
        quota::usage(0, Resource::Handles) == handles_before,
        "handle charge {} -> {}",
        handles_before,
        quota::usage(0, Resource::Handles)
    );
    check!(
        mem::frame_stats().live() == frames_before,
        "frames {} -> {}",
        frames_before,
        mem::frame_stats().live()
    );
    Ok(())
}

/// Soak: 48 generations of fork / build fabric state / exit / reap keep
/// every registry, quota and frame counter flat.
pub fn soak_teardown_generations() -> Result<(), String> {
    fresh()?;
    let frames_before = mem::frame_stats().live();
    let fabric_before = crate::ipc::stats::snapshot();
    let kernel_table = mem::kernel_table();
    for round in 0..48u64 {
        let child = task::spawn_fork().map_err(to_string)?;
        let table = PhysAddr::new(task::harness::pml4(child).ok_or("no child table")?);
        task::harness::switch_current(child);
        mem::switch_to(table);
        for _ in 0..3 {
            channels::create().map_err(|e| e.message().to_string())?;
        }
        let buffer = shared::create(4096, shared::flags::READ | shared::flags::WRITE)
            .map_err(|e| e.message().to_string())?;
        shared::map(buffer).map_err(|e| e.message().to_string())?;
        mem::switch_to(kernel_table);
        task::harness::switch_current(task::KERNEL_TASK);
        task::harness::finish(child, round);
        check!(
            task::reap_child().map(|reaped| reaped.0) == Some(child),
            "round {round}: the child was not reaped"
        );
        let fabric = crate::ipc::stats::snapshot();
        check!(
            fabric.channels == fabric_before.channels
                && fabric.buffers == fabric_before.buffers
                && fabric.buffer_mappings == fabric_before.buffer_mappings,
            "round {round}: channels {} buffers {} mappings {}",
            fabric.channels,
            fabric.buffers,
            fabric.buffer_mappings
        );
        check!(
            quota::usage(0, Resource::Handles) == 0 && quota::usage(0, Resource::KernelMemory) == 0,
            "round {round}: quota not returned"
        );
    }
    check!(
        mem::frame_stats().live() == frames_before,
        "frame leak across 48 generations: {} -> {}",
        frames_before,
        mem::frame_stats().live()
    );
    Ok(())
}

/// A task that exits without unmapping gives its user-memory charge back,
/// and a release never takes more than its own address space charged.
pub fn user_memory_quota_released_on_exit() -> Result<(), String> {
    fresh()?;
    const MIB: u64 = 1 << 20;

    // Another address space of the same uid holds 5 MiB. Charged from the
    // kernel task, i.e. *without* the kernel task's own CR3 naming
    // `bystander`'s table: `charge_for_slot` must key the ledger by
    // `bystander`'s own PML4, not by whatever table is active right now,
    // or this charge would land nowhere `bystander`'s teardown can find.
    let bystander = task::spawn_fork().map_err(to_string)?;
    credentials::set(bystander, alice());
    quota::charge_for_slot(bystander, Resource::UserMemory, 5 * MIB).map_err(|e| e.to_string())?;

    // A second, unrelated address space of the same uid, again charged
    // without ever switching to its table.
    let child = task::spawn_fork().map_err(to_string)?;
    credentials::set(child, alice());
    let charged = quota::charge_for_slot(child, Resource::UserMemory, MIB);
    // Over-releasing (ELF segments and stacks were never charged) must not
    // eat into the bystander's 5 MiB.
    quota::release_for_slot(child, Resource::UserMemory, 4 * MIB);
    quota::charge_for_slot(child, Resource::UserMemory, 3 * MIB).map_err(|e| e.to_string())?;
    charged.map_err(|e| e.to_string())?;
    check!(
        quota::usage(1000, Resource::UserMemory) == 5 * MIB + 3 * MIB,
        "over-release took bytes it never charged: usage is {}",
        quota::usage(1000, Resource::UserMemory)
    );

    // The child exits without unmapping the 3 MiB it still holds.
    task::harness::finish(child, 0);
    task::reap_child().ok_or("the child was not reaped")?;
    check!(
        quota::usage(1000, Resource::UserMemory) == 5 * MIB,
        "an exited task stranded {} bytes of its uid's quota",
        quota::usage(1000, Resource::UserMemory) - 5 * MIB
    );

    // The bystander's charge (recorded while a different table was active)
    // is still keyed to its own space: reaping it releases exactly its
    // 5 MiB, proving the ledger followed the slot, not the active CR3.
    task::harness::finish(bystander, 0);
    task::reap_child().ok_or("the bystander was not reaped")?;
    check!(
        quota::usage(1000, Resource::UserMemory) == 0,
        "the bystander's charge did not release from its own space: {} left",
        quota::usage(1000, Resource::UserMemory)
    );
    quota::reset();
    Ok(())
}
