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
| `kernel/src/ipc/shared/registry.rs` | `FENCES` queue for fence waits |
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

- Per **process** (keyed by `pml4`), not per thread: `pending`, `blocked`,
  `actions[NSIG]`, `infos[NSIG]`, `altstack`, `on_altstack`. `NSIG = 65`.
- `Disposition`: `Default`, `Ignore`, or `Handler { handler, flags, restorer,
  mask }`. `SIGKILL`/`SIGSTOP` are uncatchable (`UNCATCHABLE`).
- `default_action` classifies term / core / stop / cont / ignore.
- `kill` implements Linux pid encoding: pid, 0 (own group), -1 (all but init),
  negative (process group). `send_tid`/`kill` share `send_to_slot`.
- Fatal signals terminate the whole thread group (`terminate_process`); stop
  signals park every live thread until `SIGCONT`. `SIGCHLD` is recorded even
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
- Native frames are `[old_rip, old_rsp, old_rflags, sig, GP regs...]` and a bare
  `ret` returns to the interrupted instruction (native programs have no
  `sigreturn` syscall yet).

**Invariants / decisions**

- The blocked mask and pending set are process-wide, a documented simplification
  of Linux's per-thread model.
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
`SIGSEGV` on unresolvable faults, stop/cont, `SIGCHLD` to `wait4`. Missing:
`sigqueue` payloads, per-thread masks, `SIGTTIN`/`SIGTTOU`. The terminal
layer (`kernel/src/tty/`) sends `SIGINT`/`SIGQUIT`/`SIGTSTP` to the foreground
group and `SIGHUP` when a pty's master closes, `SIGWINCH` on a resize.
