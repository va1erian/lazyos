# Tasks & scheduler

**What it is.** The task table, the strict-class stride scheduler, and the timer
ISR that performs context switches.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/task/mod.rs` | `Task`, task table, scheduler, spawn APIs, terminals, fds |
| `kernel/src/task/switch.rs` | `timer_isr` naked stub (IDT vector 32) |
| `kernel/src/arch/idt.rs` | installs the timer gate; `TICKS` counter |
| `kernel/src/tests.rs` | scheduler/task test hooks (`task::harness`) |

**Task table** (`task/mod.rs`)

- `MAX_TASKS = 16`; slot 0 is the kernel task (`KERNEL_TASK`, the mux), 1.. are
  user programs/threads, each with a 32 KiB kernel stack (`KSTACKS`).
- `Kind`: `Native` (`int 0x80`) or `Linux` (`syscall`/`sysret`). `TaskState`:
  `Runnable`, `Blocked { wait: WaitKind, deadline: Option<u64> }`, `Done` (kept
  until reaped). `WakeReason`: `Woken`, `TimedOut`, `Interrupted`.
- Parentless tasks (`parent == 0`: kernel-started programs and
  `clone(CLONE_VM)` threads) have no `wait4` observer. The scheduler flags a
  finished one and `reclaim_pending` — run from a syscall entry or the mux
  loop, where the current task holds no heap lock — frees its slot, buffers
  and, when it was the address space's last user, its pages (issue #133).
  Children (`parent != 0`) stay zombies until their parent reaps them.
- Per task: `pml4`, `kstack_top`/`rsp`, `class`/`weight`/`pass`, `cpu_ticks`,
  `parent`/`pgid`/`sid`, `heap_break`, `fs_base`, `fds[16]`, `output`, `input`.

**Scheduling** (issue #58)

| Concept | Value |
|---|---|
| Classes | `Background < Normal < Interactive < Realtime` (strict order) |
| Default weights | 1 / 2 / 4 / 8 |
| Weight range | `MIN_WEIGHT = 1`, `MAX_WEIGHT = 32` |
| Quantum cost | `STRIDE_UNIT / weight`, `STRIDE_UNIT = 1024` |
| Selection | highest class with a `Runnable` task, then smallest virtual `pass` |
| Renormalize | subtract the minimum pass once it passes `PASS_CEILING = 1<<40` |

- Ties break round-robin after the current slot. Spawn/wake uses `virtual_now` so
  a newly runnable task does not claim catch-up quanta.
- Starvation bound: within a class a peer is selected at most
  `ceil(stride_i/stride_j)+1` times; worst case < 500 ticks (~5 s at 100 Hz).
- `set_priority`/`set_weight` reset/clamp the fields; Linux `nice` is not wired
  to them yet (documented in `task/mod.rs`).

**Switch path**

1. PIT tick -> `timer_isr` pushes 15 GP registers, calls `schedule(rsp)`.
2. `schedule` acknowledges the PIC, bumps `TICKS`, saves `rsp`, charges
   `cpu_ticks`, expires deadlines, sweeps signals, then picks the next task.
3. On a real switch: `mem::switch_to(pml4)`, sets TSS `RSP0` and
   `arch::linux::KERNEL_STACK`, restores `IA32_FS_BASE`, returns the new `rsp`;
   `timer_isr` pops the frame and `iretq`s into it. `mux::run` parks the kernel
   task with `task::idle(now + 2)`, so the mux cannot starve user tasks.

**Spawn APIs**

| Function | Use |
|---|---|
| `spawn(name, elf)` | kernel-started program; own group/session |
| `spawn_child(name, elf)` | child of the caller (supervision, syscall 6) |
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
- fd table: `fd_open/close/read/size/seek/dup/dup2`, 16 slots; 0/1/2 are the
  task terminal; files are whole-file buffers with an offset.

**Status.** Working: preemptive RR-with-priorities demo, native/Linux spawn,
fork/threads, CPU accounting (`cpu_usage`), per-task fds. Not yet: SMP, per-CPU
run queues, Linux nice mapping.
