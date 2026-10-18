# Locking rules and lock order

**What it is.** The kernel's mutual exclusion, in one place. Today it rests
on one machine-wide invariant — *at most one CPU executes kernel code,
because syscalls and handlers run with interrupts off* — plus the spin locks
below, whose orders are documented where they are declared and consolidated
here. The SMP plan ([`smp-plan.md`](../smp-plan.md)) keeps every rule on this
page true while it replaces the invariant's mechanism: per-CPU state first
(S8.1, `arch::percpu`), then a kernel entry lock (S8.3), then one subsystem
at a time.

**Key files**

| Path | Role |
|---|---|
| `task/mod.rs` (`TASKS`) | The task table: the scheduler's big lock; the wake paths' anchor |
| `task/wait.rs` | Wait queues and their order against the task table |
| `ipc/channels.rs`, `ipc/registry.rs`, `ipc/handles.rs` | The Messenger registries and their nesting |
| `mem/heap.rs`, `mem/frames.rs` | The allocator locks and the interrupts-off rule that makes them safe |
| `task/relax.rs`, `task/yieldlock.rs` | The parking locks (the VFS tables, ext2 volumes) |
| `tools/sync/check_locks.py` | The ratchet: raw `spin` locks may only be removed |

## The conventions

1. **Kernel code runs with interrupts off** (the `int 0x80` gate is an
   interrupt gate; `IA32_FMASK` clears IF on the Linux `syscall`;
   `without_interrupts` covers the driver sections that run from the
   preemptible kernel task). A tick therefore never interrupts a lock
   holder. Per-syscall stretches are bounded by `irq_window` poll points and
   accounted by `arch::irqoff` per CPU.
2. **Never hold a lock across a park.** Parking takes the task table
   (`block_task`) and switches tasks; a lock held across it is held while
   arbitrary other kernel code runs. The one exception is the
   [`YieldMutex`](#the-parking-locks), which exists to be parked inside —
   and its `can_block` rule (no task table held, stack headroom) applies to
   everything a parking critical section itself calls.
3. **Every spin lock's holder makes progress**: a contender never spins
   forever on a holder that needs the CPU back. This is what the
   interrupts-off convention (1) and the YieldMutex's park strategy give the
   single CPU; the SMP plan's entry lock (S8.3) and `sync::Lock` watchdog
   preserve it per subsystem as the kernel goes multicore.

## The orders

Written `A → B` for "A may be held while B is taken" (and B must never be
held while A is taken, at any depth):

| Order | Where | Rule |
|---|---|---|
| wait queue → task table | `task/wait.rs:24-27` | A wait queue is always locked *before* the task table, never after (`notify` takes the queue lock and then updates `TASKS`). No lock of any kind is held across `WaitQueue::wait`; the call is made with interrupts disabled so the register-then-block sequence cannot be split. |
| `CHANNELS` → `REGISTRY` / handle tables | `ipc/channels.rs:28-33` | `CHANNELS` is held only for registry mutations and is **released before `MESSENGER` is notified** (the wait-queue order above must not nest inside it). Transfer work (`shared`, handle table) happens either before the channel lock is taken or after it is dropped: the only nesting is `CHANNELS → REGISTRY`/handle table while a message is queued with its references taken. |
| slab → heap list | `mem/heap.rs:41-42` | A size-class slab takes its page from the list allocator. Growth releases the heap lock before taking the frame allocator's (`FRAMES`): the heap lock is never held while `FRAMES` is. |
| `CHANNELS` → (release) → wait queue | `ipc/channels.rs:28-33` | Wakeups are advisory and happen after the channel lock is dropped, so the Messenger locks and the wait queues never nest. |

Locks not listed never nest (each is a leaf: held only inside one function,
no allocation, no syscall inside).

## The parking locks

`YieldMutex` (the two VFS tables `FS`/`ABI_FS`, and each ext2 volume's
`volio::Gate`) is held **across parks**: its holder waits for a user-space
block provider (`usbd`) or virtio-blk inside the critical section. Its
`Yield` relax strategy makes a contender park for 200 µs instead of spin,
which is sound because on one CPU a contended lock's holder is always parked
or preempted *here*. The premise is single-CPU; the SMP plan's S8.6 replaces
these two locks with sleeplocks (park on a wait queue, wake the next
waiter) and retires the strategy. Until then, the locks stay on the ratchet
baseline (`tools/sync/check_locks.py`) and their holders keep the rule that
their own syscall paths never take one.

## Per-CPU state (S8.1)

The entry paths' state — the current task slot, the preemption bookkeeping,
the interrupts-off span, the interrupt-window flag, the Linux syscall entry
stacks — is per CPU (`arch::percpu`), addressed through `%gs`. Per-CPU state
takes no lock: it is only touched by its own CPU (its owner), and the
`percpu_suite` pins the isolation a second CPU's untouched area
demonstrates. Machine-wide data that a future remote waker will write into
another CPU's area keeps its atomic types; see the plan's S8.3 for the wake
IPI that will use them.

## Status

Working; the orders are enforced by review, not by code — the SMP plan adds
lockdep in S8.3 (`kernel/src/sync`, with the entry lock: a cycle in the
orders above panics with both traces), at which point this page and the
panic agree by construction.
