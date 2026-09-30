//! Task creation: kernel task, spawn, threads, fork and initial frames.

use super::*;

/// Register the kernel task (the multiplexer running in ring 0).
pub fn register_kernel() {
    let mut tasks = TASKS.lock();
    tasks[KERNEL_TASK] = Some(Task {
        name: "kernel",
        kind: Kind::Native,
        pml4: mem::kernel_table().as_u64(),
        kstack_top: 0,
        rsp: 0,
        state: TaskState::Runnable,
        // The multiplexer serves input and painting: an interactive workload.
        // `mux::run` parks it between frames, which bounds its CPU share.
        class: PriorityClass::Interactive,
        weight: PriorityClass::Interactive.default_weight(),
        pass: 0,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid: 0,
        parent: 0,
        pgid: KERNEL_TASK,
        sid: KERNEL_TASK,
        exit_status: 0,
        heap_break: 0,
        fs_base: 0,
        fds: new_fds(),
        fd_flags: [0; FD_COUNT],
        cwd: None,
        output: Vec::new(),
        input: VecDeque::new(),
    });
}
/// Create a user task from an ELF image. Returns its slot index.
///
/// The program is started by the kernel: it has no parent and leads its own
/// process group and session.
pub fn spawn(name: &'static str, elf: &[u8]) -> Result<usize, &'static str> {
    spawn_in_space(name, elf, None)
}

/// Create a user task that is a child of the calling task. Returns its slot.
///
/// This is the supervision primitive the userspace `init` (issue #93) builds
/// on: the child's `parent` names the supervisor, so its exit is reaped with
/// [`reap_child`] and wakes a [`wait_child_exit`] sleeper. The child inherits
/// the supervisor's process group and session (it is not a session leader),
/// exactly as a service started by `init` should be.
pub fn spawn_child(name: &'static str, elf: &[u8]) -> Result<usize, &'static str> {
    spawn_in_space(name, elf, Some(current()))
}

/// Why a native spawn failed, so callers can pick the errno that matches
/// (`EAGAIN` for a full table, `ENOMEM`, `ENOEXEC` for an image that will not
/// load) instead of collapsing every failure into one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// Every task slot is in use.
    NoSlot,
    /// No frame for the address space or its pages.
    NoMemory,
    /// The calling task is not in the table.
    NoParent,
    /// The ELF image is malformed or asks for an unloadable layout.
    BadImage(&'static str),
}

impl SpawnError {
    /// A short human-readable reason (the string the untyped spawns return).
    pub fn message(self) -> &'static str {
        match self {
            SpawnError::NoSlot => "no free task slot",
            SpawnError::NoMemory => "out of memory",
            SpawnError::NoParent => "no parent task",
            SpawnError::BadImage(reason) => reason,
        }
    }
}

/// Where a spawned native task's standard streams come from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Stdio {
    /// The kernel terminal (the historical behaviour of `spawn`).
    Terminal,
    /// A copy of the parent's descriptor table, minus `FD_CLOEXEC` entries:
    /// what `execve` of a native program from a shell needs so the program's
    /// output follows the shell's redirections and pipes.
    InheritFrom(usize),
}

/// Create a native child of the calling task that inherits its descriptors
/// (see [`Stdio::InheritFrom`]). This is the Linux `execve` path for native
/// programs (issue #315): the shell's fork child spawns the program, waits for
/// it and exits with its status.
pub fn spawn_child_inheriting_fds(name: &'static str, elf: &[u8]) -> Result<usize, SpawnError> {
    let parent = current();
    spawn_native(name, elf, Some(parent), Stdio::InheritFrom(parent))
}

/// Shared implementation of [`spawn`] and [`spawn_child`]: load a native ELF
/// into a fresh address space and register it as a runnable task. `parent` is
/// `None` for a kernel-started program (its own group/session leader) or the
/// slot of the supervisor starting a child.
pub(super) fn spawn_in_space(
    name: &'static str,
    elf: &[u8],
    parent: Option<usize>,
) -> Result<usize, &'static str> {
    spawn_native(name, elf, parent, Stdio::Terminal).map_err(SpawnError::message)
}

/// Classify a loader failure: frame exhaustion is `NoMemory`, anything else
/// means the image itself is unloadable.
fn load_error(reason: &'static str) -> SpawnError {
    if reason.contains("out of memory") || reason.starts_with("failed to") {
        SpawnError::NoMemory
    } else {
        SpawnError::BadImage(reason)
    }
}

/// The body behind every native spawn; see [`spawn_in_space`].
pub(super) fn spawn_native(
    name: &'static str,
    elf: &[u8],
    parent: Option<usize>,
    stdio: Stdio,
) -> Result<usize, SpawnError> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or(SpawnError::NoSlot)?;

    let pml4 = mem::new_user_table().ok_or(SpawnError::NoMemory)?;
    // A load failure returns while the guard is live, releasing the whole
    // partially built address space instead of leaking its frames.
    let guard = mem::UserTableGuard::new(pml4);
    let entry = user_process::load_image(guard.table(), elf).map_err(load_error)?;
    let (parent_slot, pgid, sid) = match parent {
        Some(parent) => {
            let parent_task = tasks[parent].as_ref().ok_or(SpawnError::NoParent)?;
            (parent, parent_task.pgid, parent_task.sid)
        }
        // A program started by the kernel leads its own group and session
        // (pid == pgid == sid); a supervised child inherits its supervisor's.
        None => (0, index, index),
    };
    // Every fallible step is behind us: keep the address space.
    guard.commit();
    trace::clear(index);

    let top = kstack_top(index);
    let rsp = build_user_frame(top, entry, user_process::USER_STACK_TOP - 16);
    let class = PriorityClass::Normal;
    let pass = virtual_now(&tasks);

    // A supervised child starts with its supervisor's credentials (never the
    // root default), a kernel-started program with the root default: the slot
    // may still hold a dead task's identity.
    match parent {
        Some(parent) => credentials::inherit(parent, index),
        None => credentials::reset_for_task(index),
    }
    // Cloned only now, after the last error return: dropping a cloned pipe end
    // on a failure path would take a wait-queue lock under `TASKS`.
    let fds = match stdio {
        Stdio::Terminal => new_fds(),
        Stdio::InheritFrom(from) => tasks[from]
            .as_ref()
            .map(|source| clone_fds_exec(&source.fds, &source.fd_flags))
            .unwrap_or_else(new_fds),
    };
    // A new program starts with clean x87/SSE registers, not the slot's
    // previous owner's.
    fpu::reset(index);
    tasks[index] = Some(Task {
        name,
        kind: Kind::Native,
        pml4: pml4.as_u64(),
        kstack_top: top,
        rsp,
        state: TaskState::Runnable,
        class,
        weight: class.default_weight(),
        pass,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid: 0,
        parent: parent_slot,
        pgid,
        sid,
        exit_status: 0,
        heap_break: user_process::USER_HEAP_BASE,
        fs_base: 0,
        fds,
        fd_flags: [0; FD_COUNT],
        cwd: None,
        output: Vec::new(),
        input: VecDeque::new(),
    });
    Ok(index)
}

/// Create a Linux thread that shares the current task's address space.
///
/// The child resumes at the caller's `syscall` return address with `rax = 0`,
/// on its own user stack (`user_rsp`) and its own `%fs` TLS (`fs_base`), as
/// `clone(CLONE_VM | ...)` requires.
pub fn spawn_thread(
    name: &'static str,
    user_rsp: u64,
    fs_base: u64,
    clear_child_tid: u64,
) -> Result<usize, &'static str> {
    // A non-canonical `%fs` base would fault on `wrmsr` in the context-switch
    // path (task/mod.rs context switch), which cannot return an error; refuse
    // it here so the infallible MSR write is safe by construction (issue #222).
    if !valid_fs_base(fs_base) {
        return Err("non-canonical user fs base");
    }
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;
    let parent = tasks[current()].as_ref().ok_or("no parent task")?;
    let pml4 = parent.pml4;
    // A thread stays in its process's group and session (#59: threads do not
    // get a new one), so only a process can create a group or session.
    let (pgid, sid) = (parent.pgid, parent.sid);
    // Threads inherit their creator's scheduling class and weight, like
    // Linux threads share a nice value.
    let (class, weight) = (parent.class, parent.weight);
    // A thread starts in its creator's directory (later `chdir`s are per task).
    let cwd = parent.cwd.clone();
    let context = crate::arch::linux::user_context();

    let top = kstack_top(index);
    let rsp = build_thread_frame(top, &context, user_rsp);
    let pass = virtual_now(&tasks);

    // A thread runs with its creator's credentials, not the slot's leftovers.
    credentials::inherit(current(), index);
    trace::clear(index);
    // Like its registers, a thread's floating-point state starts as its
    // creator's.
    fpu::inherit_live(index);
    tasks[index] = Some(Task {
        name,
        kind: Kind::Linux,
        pml4,
        kstack_top: top,
        rsp,
        state: TaskState::Runnable,
        class,
        weight,
        pass,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid,
        parent: 0,
        pgid,
        sid,
        exit_status: 0,
        heap_break: 0,
        fs_base,
        fds: new_fds(),
        fd_flags: [0; FD_COUNT],
        cwd,
        output: Vec::new(),
        input: VecDeque::new(),
    });
    Ok(index)
}

/// Create a `clone(CLONE_VM)` child that is *not* a thread: the vfork child
/// musl's `posix_spawn` builds (`CLONE_VM|CLONE_VFORK|SIGCHLD`). It resumes at
/// the caller's `syscall` return address with `rax = 0` on `user_rsp`, after
/// `dup2`-ing stdio and `execve`-ing.
///
/// The address space is a copy-on-write clone, not a true shared table (a
/// vfork child on LazyOS would otherwise share the parent's `exit_group`
/// thread group, so its `_exit` fallback would kill the parent). The
/// posix_spawn child only reads the argument block before `execve`, and the
/// parent synchronises through musl's status pipe, so a clone is semantically
/// sufficient.
pub fn spawn_vfork(user_rsp: u64) -> Result<usize, &'static str> {
    spawn_fork_inner(Some(user_rsp))
}

/// Fork the current Linux process: a new task with a deep copy of its address
/// space. Returns the child's slot (the parent's `fork` result); the child's
/// frame resumes at the parent's return address with `rax = 0`.
pub fn spawn_fork() -> Result<usize, &'static str> {
    spawn_fork_inner(None)
}

/// Shared [`spawn_fork`]/[`spawn_vfork`] body. `user_rsp` overrides the
/// child's resume stack (`clone` provides one); `None` resumes on the parent's.
pub(super) fn spawn_fork_inner(user_rsp: Option<u64>) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;
    let parent_index = current();
    let parent = tasks[parent_index].as_ref().ok_or("no parent task")?;
    let pml4 = parent.pml4;
    let fs_base = parent.fs_base;
    // `fork` inherits the parent's process group and session. Forking *from
    // the kernel task* (only the test harness does) starts a fresh leader:
    // init has no session of its own to hand down.
    let (pgid, sid) = if parent_index == KERNEL_TASK {
        (index, index)
    } else {
        (parent.pgid, parent.sid)
    };
    // `fork` inherits the parent's scheduling class and weight, like Linux.
    let (class, weight) = (parent.class, parent.weight);
    let (brk, mmap_next) = bump_for_pml4(pml4);
    let context = crate::arch::linux::user_context();
    let fds = clone_fds(&parent.fds);
    // `fork` inherits the parent's `FD_CLOEXEC` flags (they are per-descriptor,
    // and `execve` in the child closes whatever they mark).
    let fd_flags = parent.fd_flags;
    // ...and the working directory: the child gets its own reference, so a
    // `chdir` in either process never moves the other.
    let cwd = parent.cwd.clone();
    let pass = virtual_now(&tasks);

    // `fork` is only valid inside a user address space: the kernel task's table
    // holds low-half bootloader mappings (framebuffer, boot data) that are not
    // ours to share or copy-on-write. Give the child a fresh table there (the
    // test harness forks from the kernel task to exercise bookkeeping).
    //
    // The test must go through the *slot*, not `mem::kernel_table()`: that
    // helper reports the active `CR3`, which inside the fork syscall is the
    // parent's table, so comparing against it made every fork take the
    // fresh-table path and left the child without the parent's pages.
    let child_table = if parent_index == KERNEL_TASK {
        mem::new_user_table()
    } else {
        mem::clone_user_table(PhysAddr::new(pml4))
    }
    .ok_or("out of memory (fork)")?;
    let top = kstack_top(index);
    let rsp = build_thread_frame(top, &context, user_rsp.unwrap_or(context.rsp));

    // `fork`/`vfork` children inherit the parent's credentials; the slot may
    // still hold a dead task's (possibly root) identity.
    credentials::inherit(parent_index, index);
    trace::clear(index);
    // The child resumes with the parent's registers, floating point included.
    fpu::inherit_live(index);
    tasks[index] = Some(Task {
        name: "fork",
        kind: Kind::Linux,
        pml4: child_table.as_u64(),
        kstack_top: top,
        rsp,
        state: TaskState::Runnable,
        class,
        weight,
        pass,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid: 0,
        parent: parent_index,
        pgid,
        sid,
        exit_status: 0,
        heap_break: 0,
        fs_base,
        fds,
        fd_flags,
        cwd,
        output: Vec::new(),
        input: VecDeque::new(),
    });
    drop(tasks);

    register_bumps(child_table.as_u64(), brk, mmap_next);
    // POSIX `fork` inherits dispositions, the blocked mask and the alternate
    // stack; pending signals do not cross the fork.
    signal::fork_inherit(pml4, child_table.as_u64());
    Ok(index)
}

/// Lay out a thread's first ring-3 frame from the parent's saved user context:
/// same registers (but `rax = 0`, the child's return from `clone`), same RIP,
/// and the child's own stack pointer.
pub(super) fn build_thread_frame(
    kstack_top: u64,
    ctx: &crate::arch::linux::UserContext,
    user_rsp: u64,
) -> u64 {
    let selectors = gdt::selectors();
    // Register order must match `timer_isr`'s pop order (r15 .. rax).
    let regs = [
        ctx.r15, ctx.r14, ctx.r13, ctx.r12, ctx.rflags, ctx.r10, ctx.r9, ctx.r8, ctx.rbp, ctx.rdi,
        ctx.rsi, ctx.rdx, ctx.rip, ctx.rbx, 0, // rax: the child sees clone() return 0
    ];
    let base = kstack_top - FRAME_WORDS * 8;
    // Safety: writing within this task's kernel stack.
    unsafe {
        let frame = base as *mut u64;
        for (i, value) in regs.iter().enumerate() {
            core::ptr::write_volatile(frame.add(i), *value);
        }
        core::ptr::write_volatile(frame.add(15), ctx.rip); // RIP (after syscall)
        core::ptr::write_volatile(frame.add(16), selectors.user_code as u64); // CS
        core::ptr::write_volatile(frame.add(17), ctx.rflags | 0x200); // RFLAGS (IF set)
        core::ptr::write_volatile(frame.add(18), user_rsp); // RSP
        core::ptr::write_volatile(frame.add(19), selectors.user_data as u64); // SS
    }
    base
}

/// Lay out a fresh ring-3 entry frame on a kernel stack and return its RSP.
///
/// Layout (low to high) matches `timer_isr`'s pop order: 15 general registers,
/// then RIP, CS, RFLAGS, RSP, SS.
pub(super) fn build_user_frame(kstack_top: u64, entry: u64, user_rsp: u64) -> u64 {
    let selectors = gdt::selectors();
    let base = kstack_top - FRAME_WORDS * 8;
    // Safety: writing within this task's kernel stack.
    unsafe {
        let frame = base as *mut u64;
        for i in 0..15 {
            core::ptr::write_volatile(frame.add(i), 0); // general registers
        }
        core::ptr::write_volatile(frame.add(15), entry); // RIP
        core::ptr::write_volatile(frame.add(16), selectors.user_code as u64); // CS
        core::ptr::write_volatile(frame.add(17), 0x202); // RFLAGS (IF set)
        core::ptr::write_volatile(frame.add(18), user_rsp); // RSP
        core::ptr::write_volatile(frame.add(19), selectors.user_data as u64); // SS
    }
    base
}
