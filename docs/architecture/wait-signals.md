# Wait queues & signals

**What it is.** The blocking primitive every sleep/poll/wait syscall shares
(`WaitQueue`) and the POSIX-ish signal layer, including Linux `rt_sigframe`
delivery.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/task/wait.rs` | `WaitQueue`, `TERMINAL`, `CHILD_EXIT`, `SLEEP`, `SLOT`, `POLL` |
| `kernel/src/task/signal.rs` (+ `signal/*.rs`) | Signal state, delivery, frames, `SIGSEGV` hook; constants, types, control, frames, delivery in submodules |
| `kernel/src/ipc/channels.rs` | `MESSENGER` queue over the same primitive |
| `kernel/src/process/linux/sig.rs` | `rt_sigaction` family |
| `kernel/src/process/linux/futex.rs` | `futex` dispatch |

**WaitQueue model** (`wait.rs`, issue #57)

- A FIFO of task slots keyed by event kind; `park` registers + marks `Blocked`,
  `wait` parks and yields via `switch::yield_now` (vector `0x81`, `yield_isr`,
  which counts no tick and sends no EOI; issue #338), and
  `notify` / `notify_one` / `notify_all` move waiters back to `Runnable`.
- The timer's deadline sweep (`task::expire_deadlines`) marks expired waiters
  `TimedOut`; no separate timer callback exists.
- Locking: the queue lock is always taken before the task table (never after);
  callers hold no locks and run with interrupts disabled across register-then-park.
- `WaitKind`: `Futex`, `Terminal`, `ChildExit`, `Sleep`, `Pipe`, `UnixAccept`,
  `Poll`, `Signal`, `Slot`. Only
  `SIGCONT` wakes `Signal`; the others are advisory (`take_wake_reason` re-checks).

**Signal state** (`signal.rs`, issue #60)

- Split as POSIX threads split it (`signal/registry.rs`, issue #772). Per
  **process** (`Signals`, keyed by `pml4`, a `Vec`): the dispositions
  `actions[NSIG]`, the shared `pending` set of process-directed signals (and
  its `infos[NSIG]`), `group_exit`. Per **thread** (`ThreadSig`, a fixed
  `[ThreadSig; MAX_TASKS]` in `.bss` indexed by task slot): `blocked`, the
  thread's own `pending` set (and `infos[NSIG]`), `altstack`, `on_altstack`,
  `suspend_restore`. `NSIG = 65`. Locks: task table, then the process table,
  then the thread table. A slot's `ThreadSig` is reset when its task is
  created (`thread_born`, `fork_inherit`), so a recycled slot starts clean; a
  thread starts with its creator's mask and no alternate stack, a `fork` child
  with the calling thread's mask and alternate stack, `execve` keeps the mask
  and pending set (`exec_thread`).
- `Disposition`: `Default`, `Ignore`, or `Handler { handler, flags, restorer,
  mask }`. `SIGKILL`/`SIGSTOP` are uncatchable (`UNCATCHABLE`).
- `SIGKILL` and a task parked inside the kernel (#558): a task whose saved
  frame is a user frame is ended where it stands; one blocked, stopped or
  mid-call in the kernel may hold a lock or request slot (an ext2 gate, a block
  provider request), so it only gets `SIGKILL` pending and a
  `WakeReason::Interrupted` wake, unwinds through its syscall and dies at the
  syscall return or in the sweep. Kernel waits that would park again check
  `signal::killed(slot)`; a killed requester waiting on a block provider
  naps one tick at a time until its request completes (never abandoned).
  `execve` carries a pending kill over to the new image.
- `default_action` classifies term / core / stop / cont / ignore.
- `kill` implements Linux pid encoding: pid, 0 (own group), -1 (all but init),
  negative (process group). `kill` and the kernel's signals (`SIGCHLD`, the
  terminal's) are **process-directed** (`send_to_slot`): the bit goes in the
  shared set and one live thread that does not block it is woken (the target
  slot first, else the lowest slot, Linux's `complete_signal`); whichever such
  thread reaches a delivery boundary first takes it, and it stays pending
  while every thread blocks it. `tkill`/`tgkill`/`SIGPIPE` are
  **thread-directed** (`send_to_thread`/`send_tid`): the bit goes in the
  target's own set, only the target is woken, and no other thread takes it,
  whatever its mask. A synchronous fault (`deliver_fault`) is the faulting
  thread's too: its own mask decides. Delivery (`next_deliverable`) looks at
  the thread's set plus the shared one, less the thread's mask; `SIGKILL`
  first. `rt_sigpending` reports that union restricted to the thread's mask.
- Fatal signals terminate the whole thread group (`terminate_process`, and
  the timer sweep's `finish_sweep` ends the rest of the group of a thread it
  killed); stop signals park every live thread until `SIGCONT`. `SIGCHLD` is recorded even
  under its default-ignore disposition (`post_sigchld`).

**Delivery boundaries**

| Boundary | Path | Covers |
|---|---|---|
| Linux syscall return | `deliver_linux` (`signal/deliver.rs`) | any Linux task; result recorded as `rax` in the frame |
| Native syscall return | `deliver_native` (`signal/native.rs`) | native `int 0x80` tasks: a pending default-fatal signal (term/core) ends the task before it returns to user mode, and a Messenger `recv` woken by one returns instead of parking again, so a service blocked in `recv` dies on `SIGTERM` (the orderly shutdown, [../shutdown.md](../shutdown.md)) rather than waiting for a tick to catch it in user mode |
| Scheduler sweep | `sweep` (`signal/sweep.rs`) | native `int 0x80` tasks and Linux tasks preempted in user mode: default actions for all of them, handler frames only for the interrupted task's own process (the installed page table is the only one the frame can be written through) |
| Scheduler resume | `deliver_on_resume` (`signal/sweep.rs`) | the task the scheduler switches to, once its table is installed: handler frames the sweep had to leave pending (#375) |
| Page fault | `deliver_fault` (`signal/fault.rs`) | `SIGSEGV` with `SEGV_MAPERR`/`SEGV_ACCERR` |

- Linux frames follow `struct rt_sigframe`: restorer pointer, `ucontext_t`
  (with `sigcontext`), `siginfo_t`; `parse_linux_frame` reverses it for
  `rt_sigreturn`. `SA_SIGINFO`, `SA_ONSTACK`, `SA_NODEFER`, `SA_RESETHAND` are
  honored; `SA_RESTORER` is informational. `SA_RESTART`: when a restartable
  syscall (the transfers, waits and lock calls; not `poll`/`select`/
  `epoll_wait`/sleeps) returns `-EINTR` and the first handler to run carries
  the flag, `deliver_linux_restartable` rewinds the frame to the `syscall`
  instruction (`rip - 2`, `rax` = the number), so `rt_sigreturn` issues the
  call again with its saved arguments. The saved registers are read only when
  a handler needs a frame.
- The frame's `uc_mcontext.fpstate` points at a 64-byte-aligned copy of the
  interrupted task's FP state placed above the `rt_sigframe`
  (`signal/fpframe.rs`): with `XSAVE` in use the whole 832-byte standard-format
  area (legacy region, header, `YMM_Hi128` at offset 576) followed by
  `FP_XSTATE_MAGIC2`, with `struct _fpx_sw_bytes` (`FP_XSTATE_MAGIC1`,
  `extended_size = xstate_size + 4`, `xfeatures`, `xstate_size`) in the legacy
  region's software bytes at offset 464, the layout musl, glibc, libgcc and
  debuggers read; on an `FXSAVE`-only CPU the 512 bytes with the same bytes
  (`xstate_size = 512`) and no trailing magic. The handler starts from the
  power-on state, as on Linux. `rt_sigreturn` restores the area only when its
  markers are consistent (magics, sizes, `xfeatures` within `XCR0`, 64-byte
  alignment); otherwise it restores the legacy 512 bytes and resets the AVX
  state, and a header or `MXCSR` naming bits the CPU does not accept is
  masked, never passed to `XRSTOR` (issue #822).
- Native frames are `[old_rip, old_rsp, old_rflags, sig, GP regs...]` and a bare
  `ret` returns to the interrupted instruction (native programs have no
  `sigreturn` syscall yet).

**Invariants / decisions**

- A process-directed signal can wake the target thread and be taken by a
  sibling first (the sweep delivers to whichever runnable thread it meets).
  The woken thread must not see that as an interruption: when a call is
  interrupted (`-EINTR`) and no handler ran on its thread, `deliver_linux_restartable`
  rewinds the frame to the `syscall` (`rewind_unhandled`) for the restartable
  calls, and the sleeps and readiness waits (`nanosleep`/`clock_nanosleep`,
  `ppoll`/`pselect6`, `epoll_wait`, `pause`) check `wait_interrupted` and wait
  on. `rt_sigreturn` clears `on_altstack` (`returned_to`) once the restored
  `rsp` is outside the alternate stack, so every `SA_ONSTACK` signal enters it.
- Kernel signal sets use bit `1 << sig`; Linux `sigset_t` uses bit `sig - 1`.
  `linux_sigset_to_kernel`/`kernel_to_linux_sigset` translate at every Linux
  ABI boundary (`rt_sigaction` `sa_mask`, `rt_sigprocmask` set/oldset, and the
  `ucontext.uc_sigmask` of `build_linux_frame`/`parse_linux_frame`). Signal 64
  (`SIGRTMAX`) has no kernel `u64` bit and is dropped safely; the native
  (`int 0x80`) path never translates.
- Signal paths run in IRQ/scheduler context and therefore do not allocate
  (`SlotList` is a fixed stack array).
- Lock order is task table -> signal registry, always.
- A signal to a zombie is dropped; a blocked signal stays pending; a handler
  resets `blocked` composition through `arm_handler`.

**Status.** Working: `kill`/`tkill`/`tgkill`, masks, alt stacks, handlers,
`SIGSEGV` on unresolvable faults, stop/cont, `SIGCHLD` to `wait4`, per-thread
masks and thread-directed signals. Missing: `sigqueue` payloads,
`rt_sigtimedwait`, `SIGTTIN`/`SIGTTOU`. Tests: the signal suite
(`signal_suite/`) and `linux_compat_suite/threadsig*` (per-thread masks,
`tgkill` targeting, process signals landing on an unblocked thread, wake
targeting, `rt_sigpending`, faults, group exit, slot recycling, a 3000-signal
soak), plus the `threadsig` ABI fixture (JavaScriptCore's stop-the-world:
8 threads, `pthread_kill`, 1000 rounds). The terminal
layer (`kernel/src/tty/`) sends `SIGINT`/`SIGQUIT`/`SIGTSTP` to the foreground
group and `SIGHUP` when a pty's master closes, `SIGWINCH` on a resize.
