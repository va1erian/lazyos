# Performance and responsiveness plan

Status: draft, 2026-10-03. P0 (the harness, `tools/perf/run.py`) and P1
(the kernel wake path) are built; their measurements, so far on the dev
profile under WHPX, are in [`perf/report.md`](perf/report.md) and
[`perf/history.md`](perf/history.md). P4 (network) steps 0-5 are built and
measured in [`perf/network.md`](perf/network.md) (`tools/net/bulk.py`): the
exit target is met under WHPX (Linux sockets about 70 MB/s out and 130-190
MB/s in, `connect` 0.5-1 ms); steps 6 and 7 are not done. P2, P3 and P5
onward are not started here.

This plan covers the whole system, kernel first. It comes from a code audit, so
every latency and throughput figure below is **derived from the code, not
measured**. Stage P0 exists to replace them with measurements before anything
else is changed.

Building the kernel optimized (release profile, LTO, no debug assertions) is
being done separately and is not a work item here. All baselines in P0 must be
taken on that optimized build, so P0 depends on it.

## 1. The core problem

Almost nothing in LazyOS is woken by the event it is waiting for. Work happens
when a 100 Hz tick comes round, and several of those waits are stacked on the
hot paths:

| Gap | Where | Cost |
|---|---|---|
| The timer is a periodic 100 Hz PIT; every sleep, timeout and deadline is in 10 ms ticks, rounded up | `arch/idt.rs:89`, `process/linux/time.rs:21` | A 1 ms sleep takes 10 to 20 ms; smoltcp's clock is 10 ms; animations run at one frame per tick |
| A wake does not preempt. A woken task runs at the next scheduler entry; on an idle CPU the halted task re-halts and the woken one waits for the next tick | `task/wait.rs:99-112`, `task/waiting.rs:62-76` | Up to 10 ms added to every IRQ-driven wake on an idle machine |
| Device IRQs for userspace drivers are delivered by a bottom half that runs only on native syscall entry and in the kernel mux loop (every 2 ticks). Not on the timer tick, not on Linux syscalls | `dev/irq.rs:94-105`, `process/gate.rs:87`, `mux.rs:45` | Up to 20 ms before a driver is even told about its interrupt |
| The raw input bus has no doorbell; `inputd` polls it every 2 ticks | `user/src/bin/inputd.rs:46` | Up to 20 ms per input event |
| `xuid` parks on its request endpoint for 2 ticks; a pointer event arriving on its other endpoint does not wake it | `user/src/bin/xuid.rs:271` | Up to 20 ms more per input event |
| Nothing wakes `netd` when an app reads or writes a socket; it moves one 16 KiB chunk per socket per direction per pass | `user/src/bin/netd.rs:290-302`, `netd/inet/flow.rs` | TCP capped near 1.6 MB/s (`docs/tls-plan.md:56` already states this) |
| Every syscall runs with interrupts off, and block I/O busy-waits inside it | `arch/linux.rs:292`, `block/virtio.rs:139-165` | The machine freezes for the length of each disk request; 50 to 120 ms stalls during installs (`input/ps2.rs:6-10`) |

Derived worst cases today: PS/2 mouse to cursor pixels about 40 ms (two stacked
2-tick polls), USB mouse about 50 ms (three timer-paced hops), TCP about
1.6 MB/s.

Secondary costs, in rough order of weight:

- **Present path.** The kernel copies each damage rect to the framebuffer one
  byte at a time, with interrupts off, under the console lock
  (`gfx.rs:116-167`, `console.rs:139-143`). The framebuffer is not mapped
  write-combining (no PAT or MTRR code exists).
- **Cursor damage.** A cursor move recomposes the bounding box of the old and
  new positions through every layer (`xuid/event.rs:57`), so a fast move
  redraws a large part of the screen. A click on a window does a full-screen
  repaint.
- **Scheduler cost.** Each scheduler entry makes several linear passes over
  all 256 task slots with interrupts off; there is no run queue and no timer
  queue (`task/sched.rs:218-274`).
- **Messenger cost.** A call copies the parcel 4 to 5 times each way and makes
  about 15 kernel heap allocations on a first-fit linked-list allocator; there
  is no direct handoff to the callee.
- **Network path.** About 7 copies per frame, a 16 KiB window with no scaling,
  no congestion control, no virtio offloads or interrupt suppression.
- **Storage.** virtio-blk is polled with one request in flight and a 64 KiB
  bounce buffer; an open file holds a path, so every read re-resolves it from
  the root; there is no page cache and exec copies every page.
- **Serial mirror.** Every terminal write is copied to the polled UART a byte
  at a time under a lock (`process/linux/io.rs:114-115`). Under a hypervisor
  each byte is a VM exit.
- **Userspace polling.** Every xui client wakes at 100 Hz; most services poll
  every 2 to 5 ticks; the Terminal reads its pty on a 100 ms timer; `init`
  staggers autostart apps with fixed 500 ms and 400 ms delays.

## 2. Principles

1. **Event-driven before faster.** Remove a wait before optimising the code
   that runs after it. Most of the gaps above are waits, not slow code.
2. **Measure first, and gate.** Each stage has an exit number taken by a
   script in `tools/`, on the optimized build, under KVM or WHPX. TCG numbers
   are informative only.
3. **Keep the tick ABI working.** Userspace passes deadlines as 100 Hz ticks
   everywhere. Finer time is added beside it, then callers migrate.
4. **Kernel changes ship with correctness and soak tests** under
   `kernel/src/tests/`, per `AGENTS.md`.

## 3. Stages

Order is by gap size. P1 and P2 are the kernel gaps that everything else sits
on; P3 (cursor) and P4 (network) are the user-visible payoffs.

### P0. Measurement

Nothing to compare against exists today except boot time
(`docs/perf/boot-time.md`) and informational IPC cycle counts.

- `tools/perf/run.py`, one command, writes `docs/perf/report.md`:
  - **Wake latency:** TSC at IRQ top half to TSC when the claimant task runs.
    Kernel histogram, printed as `PERF:` serial lines.
  - **Input to present:** stamp each raw input record with `monotonic_ns` at
    the IRQ (today it is tick resolution, `input/bus.rs:326`), and report the
    delta when the present syscall that moved the cursor returns. Driven by
    `qemu_qmp.mouse_move`, once on the default PS/2 path and once on the USB
    configuration of `tools/usb/run.py`. For USB the start stamp is taken at
    the xHCI transfer event, before `usbd` polls it: a stamp taken when
    `usbd` publishes the report would hide its polling delay.
  - **TCP throughput:** bulk send and receive against a host server through
    `--net`, both the Linux socket path and native `socket.v1`.
  - **IPC round trip:** promote the existing echo test's cycle count to a
    reported p50 and p99.
  - **Interrupts-off time:** worst and p99 syscall duration, extending the
    existing `PS2:GAP` record.
  - **Disk:** sequential read MB/s of a large file; exec time of a large
    binary.
- Exit: a baseline report committed, and the derived figures in section 1
  confirmed or corrected.

### P1. Kernel wake path

The largest gap, and mostly small changes.

1. **Reschedule on wake.** Add a `need_resched` flag set by `wake_task_with`
   when the woken task outranks the current one or the CPU is idle. Check it
   on return from every IRQ to user mode and in the `nap` loop, so a halted
   task yields as soon as another becomes runnable. A dedicated idle task
   replaces "the last blocked task halts".
2. **Deliver device IRQs at once.** Run `dev::intx::service()` on IRQ return
   instead of waiting for a native syscall or a mux frame. Design note to
   verify: syscalls run with interrupts off and locks are only held with
   interrupts off (the #382 rule), so an IRQ that is taken at all has
   interrupted user code or a halt, and the bottom half can take the channel
   locks safely. Until that is proven, the stopgap is to call it from the
   timer tick and from Linux syscall entry too.
3. **Input bus doorbell.** `bus::publish` wakes a consumer blocked in a new
   blocking form of the raw-bus `POLL` op. `inputd` drops `POLL_TICKS`.
4. **Wait on several endpoints.** A kernel wait-set (receive from any of N
   endpoints, later also fds). `xuid` waits on its request and shell-event
   endpoints together; `netd` uses the same primitive in P4. Defined in MIDL
   where it surfaces as an interface.
5. **Exit without waiting for a tick.** `exit` and `exit_group` yield instead
   of halting until the next tick (`process/mod.rs:322-325`).

Tests: wake-preempts-lower-class, IRQ-to-driver latency under an idle CPU,
wait-set correctness; soaks for millions of wake/yield cycles and IRQ storms
with a slow claimant.
Exit: p99 IRQ-to-driver wake under 200 µs on an idle machine (from up to
30 ms derived).

### P2. Timekeeping

1. **Nanosecond deadlines inside the kernel.** `Blocked { deadline }` becomes
   monotonic nanoseconds. A timer min-heap replaces the per-entry linear scan
   of all tasks. Tick-based ABI calls convert on entry.
2. **Finer hardware timer, step one.** Run the PIT at 1000 Hz with the
   100 Hz tick ABI derived from it. Cheap, and it cuts the rounding of
   finer-grained deadlines (step 4) from 10 ms to 1 ms. Deadlines passed in
   100 Hz ticks stay in 10 ms units until their callers migrate.
3. **Step two: LAPIC timer, one-shot.** Enable the local APIC and IOAPIC,
   program the timer for the next heap deadline (TSC-deadline when available),
   and stop ticking when idle. This also unblocks MSI-X (P4, P5) and SMP.
4. **Userspace time.** A native `sleep`/deadline call in nanoseconds (today
   userspace has no sleep and `park_tick` builds a channel pair to nap,
   `user/src/messenger/mod.rs:443-454`); Linux `nanosleep`, `poll`, `epoll`,
   `select` and futex timeouts stop rounding to ticks; `netd` feeds smoltcp a
   millisecond clock.

Tests: timer ordering, cancellation, wrap, sleeps of 100 µs to 10 s within
tolerance; a soak arming and cancelling millions of timers.
Exit: a 1 ms sleep returns within 1.2 ms; ping reports real sub-10 ms times.

### P3. Cursor and display

With P1 done the path is IRQ, `inputd`, `xuid`, present, with no timer in it.
Then make what runs cheaper:

1. **Cursor as an overlay.** `xuid` keeps the composed scene without the
   cursor; a move restores the old 11x11 area from the scene and draws the
   sprite at the new position. Two tiny presents, no recomposition. Until
   then: damage as two rects, not their bounding box.
2. **Fast present.** Row-wise copy when source and framebuffer formats match
   (have `xuid` compose in the framebuffer's native format so the copy is
   `rep movs`). Map the framebuffer write-combining through PAT. Run the copy
   with interrupts on and outside the console lock.
3. **Zero-copy scanout.** Map the framebuffer into `xuid` (the S8 follow-up in
   `display.rs:11-17`), removing the kernel copy altogether.
4. **Coalesce `inputd` pointer events in `xuid`** before repainting, so a
   stalled compositor does not replay a backlog one full repaint at a time.
5. **Smaller repaints.** Damage-only repaint on click, focus and raise
   instead of `repaint_full`; stop filling a window body before blitting over
   it (`xuid/render.rs:277`); clip title text by glyph, not by pixel.
6. **No blocking calls on the compositor loop.** `register_surface` and
   `set_focus` to `inputd` become one-way (today up to 200 ms).
7. **`usbd` interrupt-driven** (xHCI IRQ instead of 1-tick naps), as
   `docs/usb-hid-plan.md` risk 10 already wants.
8. **Clients.** `xui-app` blocks on its event endpoint instead of polling
   every tick per window; frame pacing by a 60 Hz timer from P2; animations
   by time, not by tick count.
9. Check the PS/2 sample rate: the wheel handshake leaves it at 80 Hz
   (`input/mouse.rs:160-163`); set it to 200 afterwards.

Verification is visual as well as numeric: `qemu_session.py` scripts with
screenshots, per `AGENTS.md`.
Exit: PS/2 mouse IRQ to cursor pixels p99 under 5 ms, and USB transfer event
to cursor pixels p99 under 5 ms; a cursor move never triggers a full-screen
present.

### P4. Network throughput

1. **Wake `netd` on socket activity.** The kernel inet pair notifies `netd`
   (through the P1 wait-set) when an app writes, reads or queues a control
   request. `netd` drops its 1-tick and 5-tick polls.
2. **Drain, don't chunk.** The pump loops until the pipe or the TCP buffer is
   exhausted instead of one 16 KiB chunk per pass; stop allocating a zeroed
   16 KiB `Vec` every pass.
3. **Bigger windows.** TCP buffers from 16 KiB to 256 KiB or more (window
   scaling follows from the buffer size in smoltcp); enable CUBIC; set the
   ACK delay explicitly now that the clock is fine-grained.
4. **Bigger syscall chunks.** Linux pipe and socket I/O moves at most 4 KiB
   per syscall (`process/linux/io.rs:19`); raise it.
5. **virtio-net features.** `EVENT_IDX` (interrupt suppression),
   `MRG_RXBUF`, checksum offload; MSI-X once P2 step 3 lands.
6. **Fewer copies.** Parse frames in place in the ring; then the shared-ring
   data plane between `netd` and apps already planned as N6
   (`docs/networking-plan.md:441`).

Verdict from the wire, as for all networking: a pcap-judged bulk transfer in
`tools/net/run.py`, with throughput recorded.
Exit: at least 50 MB/s TCP in each direction on the Linux socket path under
KVM or WHPX (from 1.6 MB/s); `connect` under 1 ms on a local link. Revise the
target after the P0 baseline.

Result (`tools/net/bulk.py`, [`perf/network.md`](perf/network.md); WHPX, dev
profile): the baseline was not 1.6 MB/s but 49-53 MB/s out and 58-60 MB/s in
(arriving frames woke `netd` far more often than its timer), with 10-13 ms
per `connect`. The doorbell (step 1) took `connect` to 0.5-1 ms (the first
one after a program starts 1-10 ms); the 256 KiB windows (step 3) took bulk
TCP to 66-87 MB/s out and 132-191 MB/s in; steps 2, 4 and 5 removed copies,
allocations, syscalls and a timer without a measurable change in throughput.
Step 6 is not done: under bulk load the card already raises about one
interrupt per 30 received frames and drops none, so `EVENT_IDX` has little to
save, and QEMU's user network is not expected to offer checksum offload
(unverified). Step 7 is not done. What limits the throughput now is unmeasured;
the 10 ms clock still governs delayed ACKs and retransmission until P2.

### P5. Interrupts-off time and storage

1. **Blocking block I/O.** virtio-blk becomes IRQ-driven; the caller parks
   instead of spinning, and other tasks run meanwhile. This needs sleeping
   locks for the VFS and ext2 volume, since spin locks may only be held with
   interrupts off. This is the largest single change in the plan.
2. **Queue depth and size.** More than one request in flight; no 64 KiB
   bounce copy.
3. **Open files hold an inode**, not a path; cache the block map; read runs
   of contiguous blocks in one request; widen read-ahead.
4. **Page cache**, then file-backed `mmap` and demand-paged exec with text
   shared between processes.
5. **Serial mirror.** Buffer terminal output to the UART and drain it from a
   low-priority path, or mirror only in debug images.

Exit: worst interrupts-off stretch under 1 ms during a package install (from
50 to 120 ms); sequential read and exec time at least 5x the P0 baseline.

### P6. Scheduler, IPC and memory cost

1. **Run queues per class** and the P2 timer heap replace the 256-slot scans;
   the signal sweep runs only when a signal is pending.
2. **Direct handoff** for synchronous Messenger calls: the caller switches
   straight to a callee blocked in `recv`.
3. **One copy per direction** for parcels: validate in place, decode once;
   index channels and transactions instead of scanning them.
4. **Allocator.** Route small `GlobalAlloc` sizes through the existing slab
   (`mem/slab.rs`) instead of the first-fit list.
5. **Per-object poll queues** instead of the single global `POLL` queue that
   wakes every `poll`/`select`/`epoll` waiter on any pipe event; hash the
   futex table.
6. **Address spaces.** Global pages for the kernel, PCID, COW without the
   zero-then-copy double write and with a refcount-1 shortcut, fault-around.
7. **vDSO clock** so `clock_gettime` is not a syscall.

Exit: Messenger round trip p50 under 10 µs (the target `docs/messenger.md`
already sets), with the benchmark gated in CI.

### P7. Userspace sweep

- Services block on events instead of polling every 2 to 5 ticks (`logd`,
  `init`, `confd`, `logind`, `clipboardd`, `healthd`, `sysmond`).
- `init` starts autostart apps on readiness, not after fixed 500 ms and
  400 ms delays; dependencies wait for "ready", not "spawned".
- Terminal reads its pty on readiness instead of a 100 ms timer, and repaints
  changed rows only.
- `messenger_async::block_on` parks instead of spinning.
- Native userspace (`xuid` and the drivers) is built `opt-level = "s"` for a
  soft-float target, so the compositor's pixel loops get no SSE. Coordinate
  with the optimized-build work: speed-optimise `xuid` at least, and
  investigate enabling SSE2 for native user programs.

Exit: an idle desktop makes fewer than 10 context switches per second.

### Later: SMP

Out of scope here. P2 step 3 (APIC) and P6 step 1 (run queues) are its
prerequisites.

## 4. Quick wins

Small, independent, and worth doing right after P0, ahead of their stages:

| Change | Stage | Expected effect |
|---|---|---|
| Yield from the `nap` loop when another task is runnable | P1.1 | Removes up to 10 ms from every wake on an idle CPU |
| Call `intx::service()` from the timer tick and Linux syscall entry | P1.2 | Halves worst-case driver IRQ delivery before the real fix |
| `exit` yields instead of halting to the next tick | P1.5 | Up to 10 ms per process exit; faster shell pipelines and boot |
| `netd` pump drains instead of one chunk per pass; 256 KiB TCP buffers | P4.2, P4.3 | Lifts the 1.6 MB/s cap several-fold on its own |
| Two-rect cursor damage | P3.1 | Fast cursor moves stop recomposing large areas |
| Row-wise present copy | P3.2 | Several times less time with interrupts off per present |
| PIT at 1000 Hz with the tick ABI derived | P2.2 | Sleeps and timeouts given in finer units round to 1 ms; tick-ABI waits stay at 10 ms until callers migrate |

## 5. Risks

- **Preemption on wake exposes races** that the "runs until it blocks"
  behaviour hides today. Mitigation: the soak suite, and landing P1.1 alone
  first.
- **IRQ-context bottom half** relies on the interrupts-off lock rule holding
  everywhere. One lock taken with interrupts on becomes a deadlock. Audit
  before relying on it.
- **Sleeping locks in the filesystem** (P5.1) touch every VFS path; do it
  after the cheaper stages have delivered.
- **APIC bring-up** differs across QEMU machine types, WHPX and real PCs
  (`docs/real-pc-boot-plan.md`); keep the PIT path as a fallback.
- **More wakeups can cost throughput.** Event-driven paths need batching
  (`EVENT_IDX`, coalescing) so a packet flood or a fast mouse does not turn
  into one context switch per event.
