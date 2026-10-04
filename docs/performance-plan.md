# Performance and responsiveness plan

Status: draft, 2026-10-03. P0 (the harness, `tools/perf/run.py`) and P1
(the kernel wake path) are built; their measurements, so far on the dev
profile under WHPX, are in [`perf/report.md`](perf/report.md) and
[`perf/history.md`](perf/history.md). P2 (timekeeping) is built: nanosecond
deadlines on a timer queue, a one-shot local APIC deadline timer beside the
100 Hz PIT tick (not the 1000 Hz PIT step, and not yet with the APIC as the
tick itself), exact Linux timeouts and a native nanosecond sleep; a 1 ms
sleep went from 10.0 ms to 1.04 ms (p50, WHPX, dev profile). P3 has the
cursor overlay (P3.1), the chunked row-copy present with ops 7/8 (P3.2;
write-combining through PAT already existed, bare metal only), pointer
coalescing and damage-only click repaints (P3.4, P3.5), one-way `inputd`
notes (P3.6), parked `xui-app` clients (P3.8) and the 200 Hz PS/2 rate
(P3.9); the P3 follow-ups (`perf/p3b-display-input`) add interrupt-driven
`usbd` (P3.7), the Terminal on pty readiness with row repaints, time-paced
60 Hz animation frames and nanosecond client timers (P3.8), and WC for a
switched mode's framebuffer; not done: zero-copy scanout (P3.3). P4
(network) steps 0-5 are built and measured in
[`perf/network.md`](perf/network.md) (`tools/net/bulk.py`): the exit target is
met under WHPX (Linux sockets about 70 MB/s out and 130-190 MB/s in,
`connect` 0.5-1 ms); steps 6 and 7 are not done. P2, P3 and P4 are merged on
`perf/integration`. P5 (storage) steps 1, 2, 3 and 5 are built on
`perf/p5-storage` (measured in [`perf/disk.md`](perf/disk.md),
`tools/perf/disk.py`): the worst interrupts-off stretch of a storage syscall
went from 1.0-1.6 s (disk bench) and 483 ms (desktop package install) to
about 1 ms; sequential read is 3.5-4x and exec 1.6-3x the baseline, short of
the 5x target (see P5). Step 4 (page cache) is not done. P6 (scheduler, IPC
and memory cost) is built on `perf/p6-sched-ipc`: run queues, same-class
wake preemption with a bounded minimum slice, direct handoff, one-copy
parcels with indexed channels, slab small allocations, keyed poll wakeups and
a hashed futex table, and COW without the zero-then-copy; a cross-process
Messenger round trip is 4-6 µs at the median (WHPX, dev profile), see "As
built" under P6. P7 (userspace sweep) is built on `perf/p7-userspace`,
measured by `tools/perf/idle.py` ([`perf/idle.md`](perf/idle.md)): an idle
desktop went from 401 to 70 context switches a second (WHPX, dev profile),
`init` and the services park on events (a child-exit doorbell, topic
doorbells), dependencies wait for `init.Ready`, and the desktop's apps open
on readiness (Terminal up 1.87 s to 0.86 s after QEMU start). The exit
target (under 10) is not met: about 50 of the 70 are LazyShell, `xuid` and
the Terminal (P3's area), the rest is listed in the P7 section.

This plan covers the whole system, kernel first. It comes from a code audit, so
every latency and throughput figure below is **derived from the code, not
measured**. Stage P0 exists to replace them with measurements before anything
else is changed.

Building the kernel optimized (release profile, LTO, no debug assertions)
was done separately (#553) and is not a work item here. All baselines in P0 must be
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

**As built** (details in `docs/architecture/tasks.md`, "Deadlines and the
timer queue"): steps 1 and 4 as planned. For 2 and 3, the PIT stays the
100 Hz tick and the local APIC timer, unused while the PIT ticks, runs
one-shot for the earliest deadline inside the current tick period
(`arch::event_timer`): sub-millisecond expiry with `TICKS`, the quantum and
the #344 catch-up untouched, and the APIC already brought up by the H2 path.
Not done: the 1000 Hz PIT step (unneeded), TSC-deadline mode, tickless idle,
the IOAPIC, and a deadline timer when the APIC timer is itself the tick (a
PC with a gated PIT, `LAZYOS_TIMER=lapic`), where deadlines still expire at
ticks. Measured (WHPX, dev profile): `PERF:sleep_1ms` p50 10.0 ms -> 1.04 ms,
p99 11.5 ms -> 1.9 ms; in `deadline_sleep_accuracy`, idle-CPU sleeps from
100 µs to 1 s land 20-80 µs late (median). `ping` to the gateway reports
`time=0 ms` (its RTT is under a millisecond; the reply carries whole ms).

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

**Follow-ups, as built** (WHPX, dev profile; same-hour A/B runs):

- The fast-move tail (600 moves 4 ms apart, `input_present` p99 11.6 to
  15.3 ms) was not the `PERF:` report: a switch/wake trace of the slow
  samples showed `xuid` blocked for two ticks in a theme-topic `NextEvent`
  call every 250 ms (`themefeed.rs`). It now polls (`EXPIRED_DEADLINE`):
  p99 0.25 ms. The report does cost 3-14 ms of polled serial
  (`PERF:report`), so it now waits for 200 ms of input quiet; no sample is
  overlapped since (`PERF:input_present_rpt`).
- The resize wireframe vanished under a client repaint (the Editor's caret):
  `compose` now draws the live outline (also on the P1 base, not new).
- Animations: 100 ms per phase whatever the frame rate, 60 Hz slots with
  nanosecond sleeps. `xui-app` timers are monotonic nanoseconds; the wait set
  takes `WAIT_DEADLINE_NS` and a descriptor doorbell `WAIT_FD`.
- Terminal: parks on its pty master (`watch_fd`), repaints changed rows only.
  Echo p50 50-71 ms -> 32-41 us; `cat` of 108 894 bytes 726-736 ms -> 99-104 ms
  (`term_perf.json`, `TERM:PERF`).
- `usbd` idles on the xHCI interrupt: `usb_input_present` (from the
  interrupt, `tools/perf/run.py --usb`) p50 4.8 ms / p99 10.4 ms -> 0.28 /
  0.64 ms. The USB exit target is met.
- A mode switch's framebuffer gets a 4 KiB window and the boot WC policy.
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

**As built** (branch `perf/p5-storage`; details in
`docs/architecture/block-devices.md`, `block-cache.md`, `filesystem.md`,
`tasks.md` and `boot.md`; harness `python tools/perf/disk.py`, history in
[`perf/disk.md`](perf/disk.md)):

- Step 3 first (P5.1): ext2 open files and the loader hold the inode
  (`ext2fs::FileHandle`: inode number plus an `i_generation` advanced on every
  allocation, so a stale handle answers `ENOENT` and never reads a reused
  inode) behind a VFS node (`fs/vfs/node.rs`); reads map blocks with a memo of
  the last pointer table and move runs of contiguous blocks (up to 1 MiB) as
  one transfer; read-ahead is 64 blocks within a window past the last miss;
  files of 8 MiB or more stream past the cache.
- Steps 1 and 2 (P5.2), as a lock audit allowed: every ext2 call reaches the
  device holding only `YieldMutex`es (mount tables, the volume gate) and
  spin locks reachable only through the gate, the same locks the USB block
  provider already parks under. So the ext2 gate holder passes
  `Wait::MaySleep` and parks; FAT, partition scans, boot mounts and the
  suite's own task spin as before. The wait is a deadline, not the interrupt:
  the legacy virtio-blk INTx line is shared with the NIC's user-space driver
  (line 11 on QEMU's machine), so the driver polls the used ring at deadlines
  (3/4 of the usual answer time, then 20-250 Âµs slices on the P2 one-shot
  timer) with interrupts asked off. Up to 8 requests of 256 KiB are in
  flight, DMA straight to and from the caller's buffers (no bounce copy);
  a request stuck 10 s resets the device. Long CPU stretches breathe at most
  every 50 Âµs (ext2 pieces and library pause points, loader chunks, staged
  user copies); user bytes are staged before any breath.
- Step 5 (P5.3): COM1 output goes through a 16 KiB ring, queued whole (order
  and atomicity kept) and drained 16 bytes per status poll; program output
  drains in 32-byte chunks with interrupts let in between.
- Not done: step 4 (page cache, file-backed `mmap`, demand-paged shared
  program text), an interrupt-driven virtio-blk, modern virtio.

Measured (WHPX, dev profile, same-hour alternating runs of the disk bench,
baseline then P5): sequential read 386-401 -> 1380-1655 MB/s (3.5-4x),
BusyBox read 283-428 -> 576-860 MB/s, spawn+wait of `busybox true` mean
4.2-5.7 -> 1.45-3.2 ms (median 2.2-4.4 -> 1.3-2.8), 64 MiB write 14-16 ->
21-27 MB/s (the host's sparse image file is the limit), 400 small files
566-610 -> 227-356 ms; worst interrupts-off stretch 0.98-1.23 s (a `write`)
-> 0.8-1.4 ms (now `wait4`/`exit_group`; storage syscalls 0.4-0.75 ms).
Desktop boot with the package install (`tools/perf/run.py`): 483 ms in
native `fsync` -> every storage syscall 0.5-1.05 ms (the worst run had a
1.05 ms `rename`); the remaining multi-millisecond stretches are IPC and
display. What limits the rest: exec is dominated by process creation and
teardown and by copying program text into fresh frames (step 4 would share
it); reads by the polled completion (no interrupt) and copies through the
kernel staging buffer; writes by the host.

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

**As built** (branch `perf/p6-sched-ipc`; details in
`docs/architecture/tasks.md`, `ipc-core.md`, `allocators.md`):

0. Measurement: `msgbench` (`user/src/bin/msgbench.rs`) is a real two-process
   benchmark (`PERF:msg_rt`, `PERF:msg_tput`), and the kernel reports
   `PERF:sched` (one scheduler entry), `PERF:wake_run` (any task-to-task wake
   until the woken task runs) and the switch and entry totals (since merged into P7's `PERF:wakeups`
   line; the harness takes an idle-desktop rate over an 8 s quiet window).
1. Run queues per class and a finished-task mask (`task/runq.rs`); the signal
   sweep first finds the processes with a deliverable signal and does nothing
   when there are none.
2. Same-class wake preemption when the woken task would be the next pick,
   bounded by charging by use (a task leaving early is refunded down to a
   tenth of a quantum) and a 1 ms minimum slice (a deserving wake inside it
   is deferred on the deadline timer); sleepers rejoin at most one stride
   behind. Direct handoff from a call to its callee and a reply to its caller.
3. Parcels: one copy in, validated in place (`libmessenger::ParcelView`),
   queued as is, one copy out; channels and transactions indexed by slot.
4. `GlobalAlloc` serves requests up to 2 KiB from slab classes whose pages
   come from the list heap.
5. `poll`/`select` record the pipes and pseudo-terminals they scan; those
   objects' events wake only interested waiters (other kinds and `epoll`
   still wake everyone). The futex table is 64 hashed buckets.
6. COW faults and `mprotect` privatization keep the frame when its refcount
   is 1 and copy into an unzeroed frame otherwise.

Measured (WHPX, dev profile, one desktop run each; `docs/perf/history.md`):
`msg_rt` p50 5.6 -> 4.1-5.7 µs, p99 9.8 -> 6.7-8.9 µs; one-way throughput
523k -> 0.81-1.0 M messages/s; `sched` p50 1.9 -> 1.0 µs, p99 38.6 ->
32-34 µs. The remaining round trip is two syscalls and two context switches
(CR3 reloads, no PCID). The scheduler entry's p90-p99 tail is not attributed
(selection now visits only runnable tasks; the tick path with its PS/2
service and device bottom half is the likely part). `sleep_1ms` p99 moved between
1.5 and 2.2 ms across runs (1.57 ms at the baseline; 2 samples of 200, not attributed: the same runs
show interrupts-off stretches of 25 ms and more, P5).
Not done: a CI workflow for the `msg_rt` gate (`tools/perf/run.py
--max-msg-rt-p50-us 10` is the gate; nothing runs it in CI), keyed wakeups for
eventfds, sockets, listeners and `epoll`, PCID, global
kernel pages, fault-around, the vDSO clock.

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

**Built** (`perf/p7-userspace`; `python tools/perf/idle.py`, WHPX, dev
profile, 30 s window after 30 s of boot):

| Step | What changed | Idle switches/s |
|---|---|---:|
| 0 | Baseline; per-task counters (`kernel/src/perf/wakeups.rs`, now `PERF:wakeups`) | 401 |
| 1 | `init` parks on its endpoint and a child-exit doorbell (`WAIT_CHILD`, `task/childbell.rs`) | 363 |
| 2 | Topic doorbells (`topics.Bell`); `logd`, `healthd`, `timed`, `sysmond`, `confd`, `clipboardd`, `logind` park on events | 119 |
| 3 | `init.Ready`: dependencies and the autostart wait for readiness, no fixed delays | 119 |
| 4 | `park_tick` and one-tick naps sleep (`sys::nap`); `block_on` parks; `call` stops zero-filling 16 KiB | 119 |
| - | The kernel task parks 250 ms instead of 20 ms while a compositor is bound | 70 |

What is left at idle (switches/s): LazyShell 25, `xuid` 12.5, the Terminal
11 (P3 territory: the shell's heartbeat and theme timers, the Terminal's pty
timer), `messengerd` 5.5 and `confd` 4.8 (answering `xuid`'s 25-tick theme
watch poll and the shell's 3 s theme re-read and 10 s launcher refresh; both
could park on a topic bell now), the kernel task 4, `inputd` 4, `init` 1.6
(the shell's `ListApps`), `audiod` 1 (its card retry and keepalive timer),
`sysmond` 0.6 (its 5 s publish), `timed` 0.4, `logd` 0.1 (the denial
fallback: the audit ring has no wake source, see below).

Not done in this stage: the Terminal's pty on readiness; a wake source for
fabric denials (the audit ring is written under locks a wake may not take,
so `logd` samples it on every wake and every 10 s); `audiod` without its
1 s timer; the extra `authorize_topic` syscall per publish. Native user
programs stay at `opt-level = "s"`: `opt-level = 2` measured slower to boot,
9% larger, and no better on `input_present` (`perf/boot-time.md`). SSE2 for
native user programs is feasible but not done: `x86_64-unknown-none` is a
soft-float target, and turning `soft-float` off per package is being phased
out by rustc (rust-lang/rust#162235), so it needs its own target spec built
with `-Zbuild-std` and a second artifact target; the kernel already saves
FXSAVE state per task.

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
