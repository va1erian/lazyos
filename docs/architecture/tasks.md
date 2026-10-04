# Tasks & scheduler

**What it is.** The task table, the strict-class stride scheduler, and the timer
ISR that performs context switches.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/task/mod.rs` (+ `sched`, `spawn`, `lifecycle`, `waiting`, `console`, `fdtable`, `fdtypes`, `fdops`, `fdio`, `memstate`, `stats`) | `Task`, task table, scheduler, spawn APIs, terminals, fds (split by responsibility into the submodules) |
| `kernel/src/task/switch.rs` | `timer_isr` naked stub (IDT vector 32) |
| `kernel/src/task/runq.rs` | per-class run queues and the finished-task mask (P6.1) |
| `kernel/src/task/preempt.rs` | reschedule on wake, the same-class rule, charging by use, the minimum slice, direct handoff (P1.1, P6.2) |
| `kernel/src/task/pollwait.rs` | keyed `poll`/`select` wakeups (P6.5); futexes hash their waiters (`process/linux/futex_queue.rs`, 64 buckets) |
| `kernel/src/arch/idt.rs` | installs the timer gate; `TICKS` counter |
| `kernel/src/tests/task_suite/`, `kernel/src/tests/sched_suite.rs` | scheduler/task test hooks (`task::harness`) |

**Task table** (`task/mod.rs`)

- `MAX_TASKS = 256` (64 until the application package system, 16 until issue
  #204). Slot bitmasks (`PENDING_RECLAIM`, the device `EXITED` mask) are
  `task::slotmask::SlotMask`es, and per-slot snapshots (`stats_snapshot`,
  `sysinfo`, `ipc::stats`) live on the heap, because a kernel stack is 48 KiB;
  slot 0 is the kernel task (`KERNEL_TASK`, the mux), 1.. are
  user programs/threads, each with a 48 KiB kernel stack (`KSTACKS`; 32 KiB
  until USB storage, whose I/O parks the writer deep inside ext2).
- `Kind`: `Native` (`int 0x80`) or `Linux` (`syscall`/`sysret`). `TaskState`:
  `Runnable`, `Blocked { wait: WaitKind, deadline: Option<u64> }`, `Done` (kept
  until reaped). `WakeReason`: `Woken`, `TimedOut`, `Interrupted`. The
  deadline is monotonic nanoseconds (`arch::clock::monotonic_ns`); see
  "Deadlines and the timer queue" below.
- Parentless tasks (`parent == 0`: kernel-started programs and
  `clone(CLONE_VM)` threads) have no `wait4` observer. The scheduler flags a
  finished one and `reclaim_pending` — run from a syscall entry or the mux
  loop, where the current task holds no heap lock — frees its slot, buffers
  and, when it was the address space's last user, its pages (issue #133).
  Children (`parent != 0`) stay zombies until their parent reaps them.
- A task's descriptors close when it exits, not when it is reaped (as on
  Linux): `finish` flags the slot and `close_exited_fds` drops its `fds`
  outside the task-table lock, at once in `finish` or, for a task the timer
  sweep killed, at the next `reclaim_pending`. A shell reading a `$(...)`
  substitution reaps the writer only after the pipe's end-of-file, so a
  zombie that kept its write end hung it.
- Per task: `pml4`, `kstack_top`/`rsp`, `class`/`weight`/`pass`, `cpu_ticks`,
  `parent`/`pgid`/`sid`, `heap_break`, `fs_base`, `fds` (an `FdTable`), `output`, `input`.

**Scheduling** (issue #58)

| Concept | Value |
|---|---|
| Classes | `Background < Normal < Interactive < Realtime` (strict order) |
| Default weights | 1 / 2 / 4 / 8 |
| Weight range | `MIN_WEIGHT = 1`, `MAX_WEIGHT = 32` |
| Quantum cost | `STRIDE_UNIT / weight`, `STRIDE_UNIT = 1024` |
| Selection | highest class with a `Runnable` task, then smallest virtual `pass` |
| Renormalize | subtract the minimum pass once it passes `PASS_CEILING = 1<<40` |

- Ties break round-robin after the current slot. A spawned task starts at
  `virtual_now`; a waking one rejoins no further behind it than one of its
  own strides (`sched::rejoin`), so it never claims catch-up quanta but a
  task that mostly sleeps is the next pick when it wakes (P6.2).
- Charging (P6.2): a selection charges a whole stride; a task that leaves
  the CPU early (parked, or preempted by a wake) gets back the unused part of
  that quantum, keeping at least a tenth (`preempt::refund`,
  `MIN_CHARGE_DIV`). CPU-bound tasks pay full strides as before.
- Run queues (P6.1, `task/runq.rs`): one slot mask per class of the
  `Runnable` tasks and one of the `Done` tasks, synced under the table lock
  at every state, class, insertion and removal change. Selection,
  `virtual_now`, the reclaim flagging, `wait4`'s reap and the signal sweep
  visit only those slots, so a scheduler entry costs the runnable count, not
  256; renormalization scans the table only when a charged pass reaches the
  ceiling. Test builds compare the masks with a full scan at every entry and
  panic on a difference (`runq_suite` also checks the pick against the old
  full scan over 20000 random transitions).
- The signal sweep first collects the address spaces with a deliverable
  signal (one pass over the registry, creating no entries) and does nothing
  when there are none; resume-time delivery checks the same first.
- Starvation bound: within a class a peer is selected at most
  `ceil(stride_i/stride_j)+1` times; worst case < 2100 ticks (~21 s at 100 Hz
  with 63 peers; it was < 500 ticks at 16 slots).
- `set_priority`/`set_weight` reset/clamp the fields; Linux `nice` is not wired
  to them yet (documented in `task/mod.rs`).

**Switch path**

1. PIT tick -> `timer_isr` pushes 15 GP registers, calls `schedule(rsp)`.
2. `schedule` acknowledges the PIC, bumps `TICKS`, saves `rsp`, charges
   `cpu_ticks`, expires due deadlines from the timer queue, sweeps signals (`signal::sweep`, handler
   frames only through the installed table), then picks the next task.
3. On a real switch: `mem::switch_to(pml4)`, sets TSS `RSP0` and
   `arch::linux::KERNEL_STACK`, restores `IA32_FS_BASE`, then delivers the
   resumed task's pending handler signals through its now-installed table
   (`signal::deliver_on_resume`, #375; if that ends the task, another is
   picked) and returns the new `rsp`; `timer_isr` pops the frame and
   `iretq`s into it. `mux::run` parks the kernel
   task with `task::idle(now + 2)`, so the mux cannot starve user tasks.

**Spawn APIs**

| Function | Use |
|---|---|
| `spawn(name, elf)` | kernel-started program; own group/session |
| `spawn_child(name, elf)` | child of the caller (supervision, `spawnv`) |
| `spawn_linux(name, elf, argv0)` | Linux ABI loader; own group/session |
| `spawn_thread(...)` | `clone(CLONE_VM)` thread: shares PML4, own stack/TLS |
| `spawn_fork()` | COW copy of the current address space, `rax = 0` in child |

- `build_user_frame`/`build_thread_frame` lay out 20 qwords in the exact pop
  order `timer_isr` uses (15 regs, RIP, CS, RFLAGS, RSP, SS).

**Terminal & fd surface**

- Output is per task and stripped of ANSI sequences; forked children write to
  the root ancestor's window (`root_index`). `Tab` cycles focus in `on_key`,
  Ctrl-C becomes `SIGINT` to the focused process group, and blocked readers park
  on `wait::TERMINAL`.
- fd table (`task/fdtable.rs`): a growable `FdTable` per task, starting with
  0/1/2 (the task terminal) and doubling on demand up to `limit.fd_max`
  descriptors (Linux's `RLIMIT_NOFILE`, 1024 by default;
  [limits.md](limits.md)). Growth is fallible: a full table (or an exhausted
  heap) fails the `open`/`dup` with `EMFILE`, and `dup2` to a descriptor at or
  past the limit is `EBADF`. The table's API (look up, install lowest, put,
  replace, take, fork and exec copies) is small on purpose, so `CLONE_FILES`
  sharing can wrap it. Entries removed from a table are dropped after the task
  table unlocks (a pipe's last end wakes a queue). `fd_open/close/read/size/
  seek/dup/dup2` are built on it; files on ext2 are read in place, ramfs and
  FAT files are whole-file snapshots with an offset.
- Program images are loaded before the task table is locked (they are
  streamed from disk); the slot is claimed after.

**Wakes and preemption** (docs/performance-plan.md P1)

- A wake (`wake_task_with`) raises `need_resched` (`task/preempt.rs`) when
  the woken task should run before the next tick: the current task is blocked
  or done (the CPU is idle; the resumed parked task halts in its wait loop,
  there is no idle task), the woken task is in a strictly higher class, or
  (P6.2) it is in the same class and deserves the CPU: the next selection
  would pick it. `preempt_point` yields on the flag at the end of every
  device interrupt (keyboard, mouse, PIC line stubs, the deadline timer) and
  every syscall (native gate, Linux `syscall` stub); every selection clears
  it.
- Minimum slice (P6.2): a selected task keeps the CPU against same-class
  wakes for 1 ms (`MIN_SLICE_NS`); a deserving wake inside it is deferred to
  its end, armed on the deadline timer. This guarantees progress (a task
  resumed with an interrupt pending used to be preempted before running an
  instruction, forever: `dev_irq_prompt_storm_slow_claimant` hung) and bounds
  same-class wake preemptions to one per millisecond of CPU; with the tenth
  minimum charge, a waker can preempt a CPU-bound peer at most ten times per
  peer quantum. `preempt_wake_suite::same_class` runs a 5 kHz waker against
  two hogs of its class: the waker completes about 320 sleeps/s instead of 50,
  the switch rate stays near 660/s, the hogs split evenly.
- Direct handoff (P6.2): a Messenger call that wakes its callee, and a reply
  that wakes its caller, record that task with `task::hand_off`; when the
  recording task parks next, the scheduler runs the partner directly
  (`preempt::take_handoff`) unless a higher class is runnable. The partner is
  charged its stride like any selection.
- `exit`, `exit_group`, signal termination and a fatal user fault end in
  `task::exit_cpu`, which yields at once instead of halting until a tick.
- `task::nap` marks the current task as napping (per task, so a task switched
  away mid-nap still counts when resumed); an interrupt that lands there or
  in user code may run the device bottom half in place (P1.2).
- Tests run real switches on ring-0 test threads (`task::kthread`,
  `LAZYOS_TESTS` only): `preempt_wake_suite`, `waitset_suite` and
  `dev_suite::irq_prompt`.

**Deadlines and the timer queue** (docs/performance-plan.md P2)

- Every timed wait is a deadline in monotonic nanoseconds. The tick-based
  entry points (`WaitQueue::wait`/`park`, `task::idle`, `wait_sleep`, the
  native ABI's tick deadlines, Messenger deadlines) keep their meaning by
  converting with `task::ticks_to_ns` (`tick * 10 ms`): `monotonic_ns` is
  `TICKS * 10 ms` plus less than one period, so a tick deadline passes on
  exactly the tick it names. `TICKS` itself, `clock` (native syscall 8) and
  every tick deadline still advance at 100 Hz.
- `task::timerq::TIMERS` is an indexed binary min-heap over task slots, one
  entry per blocked task with a deadline: `block_task` arms (or cancels) it,
  `wake_task_with` cancels it, both under the task table (lock order `TASKS`
  then `TIMERS`). `expire_deadlines` pops only the due entries; an entry
  whose task is no longer blocked with that same deadline (stopped, finished,
  replaced) is dropped. Scheduler entries no longer scan all 256 slots.
- The deadline timer (`arch::event_timer`): with the PIT as the tick, the
  local APIC timer (otherwise unused) runs one-shot on vector `0x31`, armed
  for the earliest deadline if it falls inside the current tick period; a
  later one (tick deadlines included) is armed by the tick that begins its
  period. `monotonic_ns` cannot pass the end of the period the last counted
  tick began, so arming sooner would fire, find nothing due and re-fire
  every 20 µs for as long as the PIT's interrupt is late. Its interrupt runs the same expiry and then the
  P1 preemption point, so a 1 ms sleep on an idle CPU returns tens of
  microseconds late under WHPX instead of at the next tick. It is
  calibrated against the TSC in a quarter tick; arming at most one period
  ahead bounds the APIC/TSC calibration error to 10 ms worth. While the
  tick (PIC line 0) is masked it does nothing. When the APIC timer is the
  tick (`LAZYOS_TIMER=lapic`, a PC with a gated PIT), or the APIC cannot be
  brought up, there is no deadline timer and deadlines are served at ticks
  (10 ms) as before; `LAZYOS_EVENT_TIMER=0` builds that kernel on purpose.
- Users: Linux `nanosleep`/`clock_nanosleep`, `poll`, `ppoll`, `select`,
  `pselect6`, `epoll_wait` and futex timeouts are exact nanosecond deadlines
  (no rounding to ticks); native syscall 34 (`process::timesys`) reads the
  clock and sleeps until a nanosecond deadline (`sys::monotonic_ns`,
  `sys::sleep_ns` in `user`).
- Tests: `deadline_suite` (queue order, cancellation, equal deadlines,
  past and far deadlines, a million-operation seeded soak against a model,
  exact expiry through the task table, the tick ABI, real sleeps from 100 µs
  to 1 s, and eight kernel threads arming and cancelling 3200 timers).

**Locks and preemption** (issue #382)

- Syscalls and ISRs run with interrupts off; the kernel task (the mux) runs
  with them on and is preempted by the timer (and, since P1.1, by an
  interrupt's preemption point, at the same instants). Its periodic services
  that take the serial port or the VFS (block stats, the flusher) run with
  interrupts off. A spin lock that both sides
  take must therefore always be held with interrupts off: a tick that
  preempts the mux while it holds the lock hands the CPU to a task whose
  syscall then spins on it with the timer masked, and the machine stops
  silently. The task table (`task::live`), the mouse state, the console
  (`console::with_framebuffer`) and the heap (`mem::heap`'s `IrqSafeHeap`
  wrapper around `LockedHeap`) follow this rule;
  `preempt_lock_suite` soaks the heap and console under real PIT ticks.
- The same holds inside a syscall that sleeps: a busy-wait must re-mask
  interrupts after each `hlt` before it touches a lock again. `task::nap`
  (`enable_and_hlt` then `cli`) and `task::poll_until` do this; native
  `read_char`, redirected stdin, a stopped task and `WaitQueue::wait` use
  them.
- A hang is diagnosed with an NMI (`arch::nmi`): QMP `inject-nmi` (sent by
  `qemu_session.py` on a timed-out gate, or the monitor's `nmi`) prints
  `HANG:` lines through a lock-free UART writer: the interrupted context, the
  held global locks, the context the last tick interrupted, every task's
  state and saved frame (`task::diag`), and raw ring-0 stack words
  (subtract the bootloader's logged `virtual_address_offset`, then
  `llvm-symbolizer` against the kernel ELF). WHPX drops injected NMIs; the
  session then keeps `info registers` and the words at `rsp`
  (`hang_registers.txt`). `tools/screenshot/boot_stress.py` boots an image
  many times to measure a hang rate.

**Poll waiters** (P6.5, `task/pollwait.rs`)

- `poll`, `select` and `epoll_wait` all park on `wait::POLL`. A `poll` or
  `select` scan records the objects it looked at (`task::poll_scan_begin`,
  then `fd_poll` notes each descriptor's `Fd::poll_keys`: a pipe, a socket's
  two pipes, a pseudo-terminal) and parks with `wait_poll_keyed_ns`; pipe and
  pseudo-terminal events wake only the waiters interested in that object
  (`notify_poll_key`). A scan that met any other kind, or more than 16 keys,
  is woken by everything, as is `epoll_wait` (it records nothing), and the
  other objects still notify everyone (`notify_poll`). `poll_keys_suite`
  checks the rules and a 200000-event soak.

**Status.** Working: preemptive RR-with-priorities demo, native/Linux spawn,
fork/threads, CPU accounting (`cpu_usage`), per-task fds, per-class run
queues. Not yet: SMP, per-CPU run queues, Linux nice mapping, keyed wakeups
for eventfds, sockets, listeners and `epoll`.
