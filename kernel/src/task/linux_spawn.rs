//! Creating Linux-ABI tasks from a static ELF image: the kernel's own
//! start-up path ([`spawn_linux`]) and the supervised-child path
//! ([`spawn_linux_child`], [`spawn_linux_child_env`]) that `spawn`/
//! credentialed-`spawn` take for a `linux:` command line
//! (`process::spawn_line`) and `spawnv` for the Linux personality.
//!
//! Split out of `task/mod.rs` so the two variants share one body instead of
//! growing that file.

use super::*;

/// Create a Linux task from a static ELF image. Returns its slot index.
///
/// The program is started by the kernel: it has no parent, leads its own
/// process group and session, and runs as root.
pub fn spawn_linux(name: &'static str, elf: &[u8], argv0: &str) -> Result<usize, &'static str> {
    spawn_linux_in(name, elf, &[argv0.as_bytes()], &[], None)
}

/// Create a kernel-started Linux task with a full `argv` (BusyBox's `sh -c`,
/// used by the ABI bench to run one command and print its result).
pub fn spawn_linux_args(
    name: &'static str,
    elf: &[u8],
    argv: &[&str],
) -> Result<usize, &'static str> {
    spawn_linux_in(name, elf, &as_bytes(argv), &[], None)
}

/// Create a Linux task that is a child of the calling task, with `argv`.
///
/// This is how the userspace `init` supervises a musl program (a desktop app):
/// the child inherits the supervisor's credentials, process group and session,
/// and its exit is reaped with `reap_child` exactly like a native child.
pub fn spawn_linux_child(
    name: &'static str,
    elf: &[u8],
    argv: &[&str],
) -> Result<usize, &'static str> {
    spawn_linux_in(name, elf, &as_bytes(argv), &[], Some(current()))
}

/// [`spawn_linux_child`] with byte-string `argv` and an environment (`spawnv`):
/// both reach the start stack exactly as given, one item per entry, without
/// their NUL terminators (the loader adds them).
pub fn spawn_linux_child_env(
    name: &'static str,
    elf: &[u8],
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Result<usize, &'static str> {
    spawn_linux_in(name, elf, argv, envp, Some(current()))
}

/// The bytes of each `&str` item.
fn as_bytes<'a>(items: &[&'a str]) -> Vec<&'a [u8]> {
    items.iter().map(|item| item.as_bytes()).collect()
}

/// Shared body: load `elf` into a fresh address space and register the task.
fn spawn_linux_in(
    name: &'static str,
    elf: &[u8],
    argv: &[&[u8]],
    envp: &[&[u8]],
    parent: Option<usize>,
) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;
    // Resolve the parent before any frame is allocated, so a bad parent
    // cannot leak a half-built address space.
    let (parent_slot, pgid, sid) = match parent {
        Some(parent) => {
            let parent_task = tasks[parent].as_ref().ok_or("no parent task")?;
            (parent, parent_task.pgid, parent_task.sid)
        }
        // A kernel-started program leads its own group and session.
        None => (0, index, index),
    };

    let pml4 = mem::new_user_table().ok_or("out of memory")?;
    let (entry, stack_top) = match user_process::linux::load(pml4, elf, argv, envp) {
        Ok(loaded) => loaded,
        Err(err) => {
            // A partially loaded image still owns its frames: release them.
            mem::free_user_table(pml4);
            return Err(err);
        }
    };

    let top = kstack_top(index);
    let rsp = build_user_frame(top, entry, stack_top);
    let class = PriorityClass::Normal;
    let pass = virtual_now(&tasks);

    // A supervised child starts with its supervisor's credentials, a
    // kernel-started program with the root default: the slot may still hold a
    // dead task's identity.
    match parent {
        Some(parent) => credentials::inherit(parent, index),
        None => credentials::reset_for_task(index),
    }
    // A new program starts with clean x87/SSE registers.
    fpu::reset(index);
    tasks[index] = Some(Task {
        name,
        kind: Kind::Linux,
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
        heap_break: 0,
        fs_base: 0,
        fds: new_fds(),
        fd_flags: [0; FD_COUNT],
        cwd: None,
        output: Vec::new(),
        input: VecDeque::new(),
    });
    register_bumps(
        pml4.as_u64(),
        user_process::linux::BRK_BASE,
        user_process::linux::MMAP_BASE,
    );
    Ok(index)
}
