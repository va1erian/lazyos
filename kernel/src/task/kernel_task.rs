//! The kernel task: slot 0, the multiplexer that runs in ring 0.

use super::*;

/// Register the kernel task (the multiplexer running in ring 0).
pub fn register_kernel() {
    let mut tasks = TASKS.lock();
    signal::thread_born(KERNEL_TASK, None);
    tasks[KERNEL_TASK] = Some(Task {
        name: "kernel",
        kind: Kind::Native,
        pml4: mem::kernel_table().as_u64(),
        kstack_top: 0,
        kstack: None,
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
        fds: FdTable::standard(),
        cwd: None,
        linux: LinuxExtras::default(),
        output: Vec::new(),
        input: VecDeque::new(),
        guard: guard::State::new(),
    });
    super::runq::sync(&tasks, KERNEL_TASK);
}
